use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustls::ServerConfig;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_rustls::TlsAcceptor;

use crate::vendor::testkit::Article;

/// Simulated provider latency and per-connection bandwidth.
#[derive(Debug, Clone, Copy, Default)]
pub struct Impairment {
    /// Delay before the greeting and before each AUTHINFO reply.
    pub handshake_delay: Duration,
    /// Delay before each BODY response.
    pub response_delay: Duration,
    /// Article body bytes per second per connection; `None` is unlimited.
    pub bandwidth: Option<u64>,
}

const BODY_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct Behavior {
    close_after: Option<usize>,
    max_connections: usize,
    impairment: Impairment,
    /// Zero-based BODY command (counted across connections) delayed by the duration;
    /// with `true`, after half its body is sent.
    stall: Option<(usize, Duration, bool)>,
    /// Connections beyond `max_connections` get no greeting instead of a 502.
    hang: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            close_after: None,
            max_connections: usize::MAX,
            impairment: Impairment::default(),
            stall: None,
            hang: false,
        }
    }
}

/// NNTP server on `127.0.0.1` serving a fixed set of articles.
///
/// Accepts any AUTHINFO, answers BODY with 222 or 430, STAT with 223 or 430, DATE with
/// 111, QUIT with 205 and anything else with 500. Pipelined commands are answered in
/// request order. Stops accepting when dropped.
pub struct LoopbackServer {
    addr: SocketAddr,
    accept: Mutex<Option<JoinHandle<()>>>,
    refusing: Mutex<Option<TcpSocket>>,
    acceptor: Acceptor,
    open: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    stats: Arc<AtomicUsize>,
    stats_miss: Arc<AtomicBool>,
}

/// Per message-id: BODY replies still to stall, for how long, and how many were.
type Stalls = Arc<Mutex<HashMap<String, (usize, Duration, usize)>>>;

struct HeldBody {
    arrived: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
    /// Body bytes sent before holding; `None` holds the status line too.
    after: Option<usize>,
}

type Holds = Arc<Mutex<HashMap<String, Vec<HeldBody>>>>;

#[derive(Clone)]
struct Acceptor {
    articles: Arc<HashMap<String, Vec<u8>>>,
    tls: Option<TlsAcceptor>,
    behavior: Behavior,
    open: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    stats: Arc<AtomicUsize>,
    stats_miss: Arc<AtomicBool>,
    stalls: Stalls,
    holds: Holds,
}

impl Acceptor {
    fn spawn(self, listener: TcpListener) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let articles = Arc::clone(&self.articles);
                let tls = self.tls.clone();
                let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                let counted = Counted(Arc::clone(&self.open));
                let limit = Limit {
                    behavior: self.behavior,
                    rejected: now > self.behavior.max_connections,
                    bodies: Arc::clone(&self.bodies),
                    stats: Arc::clone(&self.stats),
                    stats_miss: Arc::clone(&self.stats_miss),
                    stalls: Arc::clone(&self.stalls),
                    holds: Arc::clone(&self.holds),
                };
                // A failed connection only affects the test that owns it.
                tokio::spawn(async move {
                    stream.set_nodelay(true)?;
                    match tls {
                        None => serve_counted(stream, articles, limit, counted).await,
                        Some(tls) => {
                            let stream = tls.accept(stream).await?;
                            serve_counted(stream, articles, limit, counted).await
                        }
                    }
                });
            }
        })
    }
}

impl LoopbackServer {
    pub async fn start(articles: impl IntoIterator<Item = Article>) -> io::Result<Self> {
        Self::start_with(articles, None, Behavior::default()).await
    }

    /// Delays handshakes and responses and throttles bodies like a remote provider.
    pub async fn start_impaired(
        articles: impl IntoIterator<Item = Article>,
        impairment: Impairment,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            impairment,
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Greets connections beyond `max_connections` open at once with
    /// `502 too many connections` and closes them.
    pub async fn start_limited(
        articles: impl IntoIterator<Item = Article>,
        max_connections: usize,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            max_connections,
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Every connection is closed abruptly after answering `bodies` BODY commands,
    /// discarding any further pipelined requests.
    pub async fn start_dropping(
        articles: impl IntoIterator<Item = Article>,
        bodies: usize,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            close_after: Some(bodies),
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Holds the reply to the `nth` (zero-based) BODY command received for `delay`,
    /// stalling that connection like a provider stuck on one article.
    pub async fn start_stalling(
        articles: impl IntoIterator<Item = Article>,
        nth: usize,
        delay: Duration,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            stall: Some((nth, delay, false)),
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Like [`start_stalling`](Self::start_stalling), but sends half the body first.
    pub async fn start_stalling_midway(
        articles: impl IntoIterator<Item = Article>,
        nth: usize,
        delay: Duration,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            stall: Some((nth, delay, true)),
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Like [`start_stalling`](Self::start_stalling), but connections beyond
    /// `max_connections` open at once are accepted and never greeted.
    pub async fn start_hanging(
        articles: impl IntoIterator<Item = Article>,
        max_connections: usize,
        nth: usize,
        delay: Duration,
    ) -> io::Result<Self> {
        let behavior = Behavior {
            max_connections,
            stall: Some((nth, delay, false)),
            hang: true,
            ..Behavior::default()
        };
        Self::start_with(articles, None, behavior).await
    }

    /// Implicit TLS with a fresh self-signed certificate for `localhost`.
    pub async fn start_tls(articles: impl IntoIterator<Item = Article>) -> io::Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .map_err(io::Error::other)?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
        let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .and_then(|b| {
                b.with_no_client_auth()
                    .with_single_cert(vec![cert.cert.der().clone()], key)
            })
            .map_err(io::Error::other)?;
        Self::start_with(
            articles,
            Some(TlsAcceptor::from(Arc::new(config))),
            Behavior::default(),
        )
        .await
    }

    async fn start_with(
        articles: impl IntoIterator<Item = Article>,
        tls: Option<TlsAcceptor>,
        behavior: Behavior,
    ) -> io::Result<Self> {
        let articles: Arc<HashMap<String, Vec<u8>>> = Arc::new(
            articles
                .into_iter()
                .map(|a| (a.message_id, a.wire))
                .collect(),
        );
        let listener =
            reserve(SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)))?.listen(1024)?;
        let addr = listener.local_addr()?;
        let acceptor = Acceptor {
            articles,
            tls,
            behavior,
            open: Arc::default(),
            peak: Arc::default(),
            bodies: Arc::default(),
            stats: Arc::default(),
            stats_miss: Arc::default(),
            stalls: Arc::default(),
            holds: Arc::default(),
        };
        Ok(Self {
            addr,
            accept: Mutex::new(Some(acceptor.clone().spawn(listener))),
            refusing: Mutex::default(),
            open: Arc::clone(&acceptor.open),
            peak: Arc::clone(&acceptor.peak),
            bodies: Arc::clone(&acceptor.bodies),
            stats: Arc::clone(&acceptor.stats),
            stats_miss: Arc::clone(&acceptor.stats_miss),
            acceptor,
        })
    }

    /// Stops listening, so new connects are refused; open connections keep serving.
    pub async fn refuse(&self) {
        // Reserved before the listener closes, so the port is never free.
        *self.refusing.lock().unwrap_or_else(PoisonError::into_inner) = reserve(self.addr).ok();
        let accept = self
            .accept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(accept) = accept {
            accept.abort();
            let _ = accept.await;
        }
    }

    /// Listens again on the same port after [`refuse`](Self::refuse).
    pub async fn resume(&self) -> io::Result<()> {
        let reserved = self
            .refusing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // Listens on the reserved socket: closing it first would free the port to anyone.
        let listener = match reserved {
            Some(socket) => socket.listen(1024)?,
            None => reserve(self.addr)?.listen(1024)?,
        };
        let accept = self.acceptor.clone().spawn(listener);
        if let Some(old) = self
            .accept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(accept)
        {
            old.abort();
        }
        Ok(())
    }

    /// BODY commands received across all connections.
    #[must_use]
    pub fn body_commands(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }

    /// STAT commands received across all connections.
    #[must_use]
    pub fn stat_commands(&self) -> usize {
        self.stats.load(Ordering::SeqCst)
    }

    /// Answers every STAT with 430 while on, whatever the articles.
    pub fn miss_stats(&self, on: bool) {
        self.stats_miss.store(on, Ordering::SeqCst);
    }

    /// Holds the next BODY reply until released; returns its arrival and release signals.
    pub fn hold_article(&self, message_id: &str) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        self.hold(message_id, None)
    }

    /// Sends the next BODY reply's status line and its first `bytes` body bytes, then
    /// holds the rest until released; returns its arrival and release signals. Holds
    /// placed before that reply all apply to it, in order of `bytes`.
    pub fn hold_article_after(
        &self,
        message_id: &str,
        bytes: usize,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        self.hold(message_id, Some(bytes))
    }

    fn hold(
        &self,
        message_id: &str,
        after: Option<usize>,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (arrived, arrival) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.acceptor
            .holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(message_id.to_owned())
            .or_default()
            .push(HeldBody {
                arrived,
                release: released,
                after,
            });
        (arrival, release)
    }

    /// Holds the next `times` BODY replies for `message_id` for `delay` each.
    pub fn stall_article(&self, message_id: &str, times: usize, delay: Duration) {
        self.acceptor
            .stalls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(message_id.to_owned(), (times, delay, 0));
    }

    /// BODY replies for `message_id` stalled so far by [`stall_article`](Self::stall_article).
    #[must_use]
    pub fn stalled(&self, message_id: &str) -> usize {
        self.acceptor
            .stalls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(message_id)
            .map_or(0, |&(_, _, stalled)| stalled)
    }

    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Connections open right now.
    #[must_use]
    pub fn open_connections(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }

    /// Most connections that were ever open at the same time.
    #[must_use]
    pub fn peak_connections(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        if let Some(accept) = self
            .accept
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            accept.abort();
        }
    }
}

struct Limit {
    behavior: Behavior,
    rejected: bool,
    bodies: Arc<AtomicUsize>,
    stats: Arc<AtomicUsize>,
    stats_miss: Arc<AtomicBool>,
    stalls: Stalls,
    holds: Holds,
}

async fn delay(duration: Duration) {
    if !duration.is_zero() {
        sleep(duration).await;
    }
}

// Bound but not listening, so connects are refused and no other server is handed the port.
// Address reuse is on only for the bind, past connections on the port. Port reuse lets this
// server's own sockets overlap while it hands the port over; a socket without it cannot
// take the port, and the kernel never picks a bound port for
// a bind to port 0.
fn reserve(addr: SocketAddr) -> io::Result<TcpSocket> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.set_reuseport(true)?;
    socket.bind(addr)?;
    socket.set_reuseaddr(false)?;
    Ok(socket)
}

struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Uncounts before the socket closes: a client that saw the close may connect again at once,
// and must not be counted alongside the connection it replaced.
async fn serve_counted<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    articles: Arc<HashMap<String, Vec<u8>>>,
    limit: Limit,
    counted: Counted,
) -> io::Result<()> {
    let result = serve(&mut stream, articles, limit).await;
    drop(counted);
    result
}

async fn serve<S: AsyncRead + AsyncWrite>(
    stream: S,
    articles: Arc<HashMap<String, Vec<u8>>>,
    limit: Limit,
) -> io::Result<()> {
    let (mut read, mut write) = tokio::io::split(stream);
    if limit.rejected && limit.behavior.hang {
        tokio::io::copy(&mut read, &mut tokio::io::sink()).await?;
        return Ok(());
    }
    if limit.rejected {
        write.write_all(b"502 too many connections\r\n").await?;
        return write.flush().await;
    }
    let impairment = limit.behavior.impairment;
    let mut lines = BufReader::new(read).lines();
    let mut remaining = limit.behavior.close_after.unwrap_or(usize::MAX);
    delay(impairment.handshake_delay).await;
    write.write_all(b"200 loopback ready\r\n").await?;

    while let Some(line) = lines.next_line().await? {
        let (command, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match command.to_ascii_uppercase().as_str() {
            "AUTHINFO" => {
                delay(impairment.handshake_delay).await;
                write.write_all(b"281 authentication accepted\r\n").await?;
            }
            "BODY" if remaining == 0 => return write.flush().await,
            "BODY" => {
                remaining -= 1;
                let nth = limit.bodies.fetch_add(1, Ordering::SeqCst);
                let stall = limit
                    .behavior
                    .stall
                    .filter(|&(stalled, _, _)| stalled == nth);
                if let Some((_, duration, false)) = stall {
                    delay(duration).await;
                }
                delay(impairment.response_delay).await;
                let id = arg.trim().trim_start_matches('<').trim_end_matches('>');
                let (before, mut midway): (Vec<_>, Vec<_>) = limit
                    .holds
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(id)
                    .unwrap_or_default()
                    .into_iter()
                    .partition(|hold| hold.after.is_none());
                for hold in before {
                    let _ = hold.arrived.send(());
                    let _ = hold.release.await;
                }
                midway.sort_by_key(|hold| hold.after);
                let held = limit
                    .stalls
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_mut(id)
                    .filter(|(left, _, _)| *left > 0)
                    .map(|(left, duration, stalled)| {
                        *left -= 1;
                        *stalled += 1;
                        *duration
                    });
                if let Some(duration) = held {
                    delay(duration).await;
                }
                if let Some(wire) = articles.get(id) {
                    write
                        .write_all(format!("222 0 <{id}> body follows\r\n").as_bytes())
                        .await?;
                    let mut wire = wire.as_slice();
                    let mut sent = 0;
                    for hold in midway {
                        let at = hold.after.unwrap_or(0).clamp(sent, sent + wire.len());
                        let (head, tail) = wire.split_at(at - sent);
                        write.write_all(head).await?;
                        write.flush().await?;
                        let _ = hold.arrived.send(());
                        let _ = hold.release.await;
                        (wire, sent) = (tail, at);
                    }
                    if let Some((_, duration, true)) = stall {
                        let (head, tail) = wire.split_at(wire.len() / 2);
                        write.write_all(head).await?;
                        write.flush().await?;
                        delay(duration).await;
                        wire = tail;
                    }
                    match impairment.bandwidth {
                        None => write.write_all(wire).await?,
                        Some(rate) => {
                            // Pace against a deadline so timer rounding does not accumulate.
                            let start = Instant::now();
                            let mut sent = 0u128;
                            for chunk in wire.chunks(BODY_CHUNK) {
                                write.write_all(chunk).await?;
                                sent += chunk.len() as u128;
                                let nanos = sent * 1_000_000_000 / u128::from(rate.max(1));
                                let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
                                sleep_until(start + Duration::from_nanos(nanos)).await;
                            }
                        }
                    }
                } else {
                    write.write_all(b"430 no such article\r\n").await?;
                }
            }
            "STAT" => {
                limit.stats.fetch_add(1, Ordering::SeqCst);
                delay(impairment.response_delay).await;
                let id = arg.trim().trim_start_matches('<').trim_end_matches('>');
                let reply = if articles.contains_key(id) && !limit.stats_miss.load(Ordering::SeqCst)
                {
                    format!("223 0 <{id}>\r\n")
                } else {
                    "430 no such article\r\n".to_owned()
                };
                write.write_all(reply.as_bytes()).await?;
            }
            "DATE" => write.write_all(b"111 20261013000000\r\n").await?,
            "QUIT" => {
                write.write_all(b"205 closing connection\r\n").await?;
                break;
            }
            _ => write.write_all(b"500 unsupported command\r\n").await?,
        }
        write.flush().await?;
    }
    write.flush().await
}
