use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

const CHUNK: usize = 16 * 1024;

// One bottleneck shared by every connection: each takes a chunk's turn at a time, so busy
// connections split it evenly, as flows do on a real link.
pub struct Link {
    bytes_per_second: u64,
    one_way: Duration,
    free_at: Mutex<Instant>,
}

impl Link {
    pub fn new(bytes_per_second: u64, round_trip: Duration) -> Arc<Self> {
        Arc::new(Self {
            bytes_per_second,
            one_way: round_trip / 2,
            free_at: Mutex::new(Instant::now()),
        })
    }

    fn transmit(&self, bytes: usize) -> Instant {
        let cost = Duration::from_secs_f64(bytes as f64 / self.bytes_per_second as f64);
        let mut free_at = self.free_at.lock().unwrap();
        let done = (*free_at).max(Instant::now()) + cost;
        *free_at = done;
        done
    }

    pub async fn front(self: Arc<Self>, upstream: SocketAddr) -> SocketAddr {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let link = Arc::clone(&self);
                tokio::spawn(async move {
                    if let Ok(server) = TcpStream::connect(upstream).await {
                        link.carry(client, server).await;
                    }
                });
            }
        });
        addr
    }

    async fn carry(self: Arc<Self>, client: TcpStream, server: TcpStream) {
        client.set_nodelay(true).unwrap();
        server.set_nodelay(true).unwrap();
        let (mut client_read, client_write) = client.into_split();
        let (mut server_read, server_write) = server.into_split();
        let (up, up_rx) = mpsc::unbounded_channel();
        let (down, down_rx) = mpsc::unbounded_channel();
        let one_way = self.one_way;
        tokio::spawn(deliver(up_rx, server_write));
        tokio::spawn(deliver(down_rx, client_write));
        tokio::spawn(async move {
            let mut buf = vec![0; CHUNK];
            while let Ok(n @ 1..) = client_read.read(&mut buf).await {
                if up
                    .send((Instant::now() + one_way, buf[..n].to_vec()))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut buf = vec![0; CHUNK];
        while let Ok(n @ 1..) = server_read.read(&mut buf).await {
            let sent = self.transmit(n);
            sleep_until(sent).await;
            if down.send((sent + one_way, buf[..n].to_vec())).is_err() {
                break;
            }
        }
    }
}

async fn deliver(
    mut queue: mpsc::UnboundedReceiver<(Instant, Vec<u8>)>,
    mut to: tokio::net::tcp::OwnedWriteHalf,
) {
    while let Some((at, bytes)) = queue.recv().await {
        sleep_until(at).await;
        if to.write_all(&bytes).await.is_err() {
            break;
        }
    }
}
