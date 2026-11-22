#![allow(clippy::unwrap_used)] // Test helpers outside `#[test]` fns; panics are the assertions.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kob_usenet::vendor::nntp::{
    BodyExpectation, Client, Downloads, Endpoint, Pool, PoolConfig, Provider, TlsMode,
};
use kob_usenet::vendor::stream::{Budget, Layout, Segment, SegmentReader};
use kob_usenet::vendor::testkit::{Article, Impairment, LoopbackServer, post};
use tokio::io::AsyncReadExt;

const ARTICLE_SIZE: usize = 256 * 1024;
const ARTICLES: usize = 96;
const CONNECTIONS: usize = 4;
const DEPTH: usize = 4;
/// ~50 ms before each reply, 4 MB/s per connection: two streams want more than the
/// provider has.
const IMPAIRMENT: Impairment = Impairment {
    handshake_delay: Duration::from_millis(20),
    response_delay: Duration::from_millis(50),
    bandwidth: Some(4_000_000),
};
const HEAD_START: Duration = Duration::from_millis(500);
const MEASURE: Duration = Duration::from_secs(3);

fn file(prefix: &str, seed: u8) -> Vec<Article> {
    let data: Vec<u8> = (0..ARTICLES * ARTICLE_SIZE)
        .map(|i| u8::try_from(i % 251).unwrap() ^ seed)
        .collect();
    post(prefix, &format!("{prefix}.mkv"), &data, ARTICLE_SIZE)
}

fn layout(articles: &[Article]) -> Arc<Layout> {
    Arc::new(Layout::new(
        articles
            .iter()
            .map(|a| Segment {
                message_id: a.message_id.clone(),
                size: ARTICLE_SIZE as u64,
                expected: BodyExpectation::NONE,
            })
            .collect(),
    ))
}

/// Reads `layout` to the end, counting bytes into `read`.
fn stream(
    client: &Arc<Client>,
    layout: Arc<Layout>,
    budget: &Budget,
    read: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()> {
    let mut reader = SegmentReader::new(Arc::clone(client), layout, 64)
        .with_budget(budget.clone())
        .strict(true);
    tokio::spawn(async move {
        let mut buf = vec![0; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            read.fetch_add(n as u64, Ordering::Relaxed);
        }
    })
}

#[tokio::test]
async fn two_streams_under_contention_share_the_provider() {
    let (first, second) = (file("a", 0), file("b", 0x5A));
    let server = LoopbackServer::start_impaired(
        first.iter().chain(&second).cloned().collect::<Vec<_>>(),
        IMPAIRMENT,
    )
    .await
    .unwrap();
    let endpoint = Endpoint::new(
        "127.0.0.1",
        server.addr().port(),
        TlsMode::Plain,
        None,
        Duration::from_secs(10),
    )
    .unwrap();
    let config = PoolConfig {
        max_connections: CONNECTIONS,
        pipeline_depth: DEPTH,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    let pool = Pool::new(endpoint, config, Downloads::new(CONNECTIONS * DEPTH)).unwrap();
    let client = Arc::new(Client::new([Provider {
        pool,
        key: "loopback".into(),
        name: "loopback".into(),
        priority: 0,
        backbone: None,
    }]));
    client.prewarm(CONNECTIONS).await;
    let budget = Budget::new(256 << 20);

    let (a, b) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let early = stream(&client, layout(&first), &budget, Arc::clone(&a));
    tokio::time::sleep(HEAD_START).await;
    let late = stream(&client, layout(&second), &budget, Arc::clone(&b));
    let (a0, b0) = (a.load(Ordering::Relaxed), b.load(Ordering::Relaxed));
    tokio::time::sleep(MEASURE).await;
    let (a1, b1) = (
        a.load(Ordering::Relaxed) - a0,
        b.load(Ordering::Relaxed) - b0,
    );
    early.abort();
    late.abort();

    let total = a1 + b1;
    eprintln!("first {a1} B, second {b1} B in {MEASURE:?}");
    assert!(total > 0);
    for (name, bytes) in [("first", a1), ("second", b1)] {
        assert!(
            bytes * 100 >= total * 35,
            "{name} stream got {bytes} of {total} bytes"
        );
    }
}
