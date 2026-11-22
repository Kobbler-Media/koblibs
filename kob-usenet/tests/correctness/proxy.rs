use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

type Counts = Arc<Mutex<HashMap<String, usize>>>;

pub struct BodyCounter {
    addr: SocketAddr,
    counts: Counts,
    accept: JoinHandle<()>,
}

impl BodyCounter {
    pub async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counts = Counts::default();
        let seen = Arc::clone(&counts);
        let accept = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    let (client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();
                    let replies = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut server_read, &mut client_write).await;
                    });
                    let mut lines = BufReader::new(client_read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Some(id) = line
                            .split_once(' ')
                            .filter(|(c, _)| c.eq_ignore_ascii_case("BODY"))
                            .map(|(_, id)| id.trim().trim_matches(['<', '>']).to_owned())
                        {
                            *seen.lock().unwrap().entry(id).or_default() += 1;
                        }
                        if server_write
                            .write_all(format!("{line}\r\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    let _ = server_write.shutdown().await;
                    let _ = replies.await;
                });
            }
        });
        Self {
            addr,
            counts,
            accept,
        }
    }

    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn take(&self) -> HashMap<String, usize> {
        std::mem::take(&mut *self.counts.lock().unwrap())
    }
}

impl Drop for BodyCounter {
    fn drop(&mut self) {
        self.accept.abort();
    }
}
