#![allow(clippy::unwrap_used)] // Test helpers outside `#[test]` fns; panics are the assertions.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kob_usenet::vendor::nntp::{
    Client, Downloads, Endpoint, Pool, PoolConfig, Provider, TlsMode, Workload,
};
use kob_usenet::vendor::testkit::{Corpus, Impairment, LoopbackServer};
use tokio::task::JoinSet;

/// A typical posted article, about 700 KB.
const ARTICLE_SIZE: usize = 700 * 1024;
const ARTICLES: usize = 48;
const CONNECTIONS: usize = 8;
const DEPTH: usize = 4;
const BANDWIDTH: usize = 8_000_000;
/// A remote provider: ~100 ms before each reply, ~8 MB/s per connection.
const IMPAIRMENT: Impairment = Impairment {
    handshake_delay: Duration::from_millis(100),
    response_delay: Duration::from_millis(100),
    bandwidth: Some(BANDWIDTH as u64),
};

struct Fixture {
    corpus: Arc<Corpus>,
    client: Arc<Client>,
    _server: LoopbackServer,
}

async fn fixture() -> Fixture {
    let corpus = Arc::new(Corpus::generate(1025, ARTICLES, ARTICLE_SIZE));
    let server = LoopbackServer::start_impaired(corpus.articles.clone(), IMPAIRMENT)
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
    Fixture {
        corpus,
        client,
        _server: server,
    }
}

/// Queue downloads keeping every request slot busy until dropped; counts bytes.
struct QueueLoad {
    stop: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    tasks: JoinSet<()>,
}

impl QueueLoad {
    fn start(fixture: &Fixture) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let mut tasks = JoinSet::new();
        for worker in 0..CONNECTIONS * DEPTH * 2 {
            let (client, corpus) = (Arc::clone(&fixture.client), Arc::clone(&fixture.corpus));
            let (stop, bytes) = (Arc::clone(&stop), Arc::clone(&bytes));
            tasks.spawn(async move {
                let mut index = worker;
                while !stop.load(Ordering::Relaxed) {
                    let id = &corpus.articles[index % ARTICLES].message_id;
                    let body = client.body(id, Workload::Queue, None).await.unwrap();
                    bytes.fetch_add(body.data.len() as u64, Ordering::Relaxed);
                    index += CONNECTIONS * DEPTH * 2;
                }
            });
        }
        Self { stop, bytes, tasks }
    }

    /// MB/s while `during` runs.
    async fn throughput<T>(&self, during: impl Future<Output = T>) -> (f64, T) {
        let (before, started) = (self.bytes.load(Ordering::Relaxed), Instant::now());
        let output = during.await;
        let bytes = self.bytes.load(Ordering::Relaxed) - before;
        let bytes = usize::try_from(bytes).unwrap();
        (megabytes(bytes) / started.elapsed().as_secs_f64(), output)
    }

    async fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        while let Some(task) = self.tasks.join_next().await {
            task.unwrap();
        }
    }
}

fn megabytes(bytes: usize) -> f64 {
    f64::from(u32::try_from(bytes).unwrap()) / 1e6
}

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    sorted[(sorted.len() * p).div_ceil(100).max(1) - 1]
}

/// Seeks while Queue work saturates the pool: each fetches the segment at the seek point
/// and the three after it, as a reader's window does. Returns every read's latency.
async fn seek_under_load(fixture: &Fixture, seeks: usize) -> Vec<Duration> {
    let mut latencies = Vec::new();
    for seek in 0..=seeks {
        let mut window = JoinSet::new();
        for segment in 0..4 {
            let client = Arc::clone(&fixture.client);
            let id = fixture.corpus.articles[(seek * 7 + segment) % ARTICLES]
                .message_id
                .clone();
            window.spawn(async move {
                let started = Instant::now();
                client.body(&id, Workload::Stream, None).await.unwrap();
                started.elapsed()
            });
        }
        while let Some(latency) = window.join_next().await {
            // Untimed: playback starts with the pool still full of queued downloads.
            if seek > 0 {
                latencies.push(latency.unwrap());
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    latencies.sort_unstable();
    latencies
}

#[tokio::test]
async fn playback_reads_are_not_queued_behind_a_saturating_download() {
    let fixture = fixture().await;
    let id = &fixture.corpus.articles[0].message_id;
    fixture
        .client
        .body(id, Workload::Stream, None)
        .await
        .unwrap();
    let started = Instant::now();
    fixture
        .client
        .body(id, Workload::Stream, None)
        .await
        .unwrap();
    let idle = started.elapsed();
    let load = QueueLoad::start(&fixture);
    let (alone, ()) = load
        .throughput(tokio::time::sleep(Duration::from_secs(2)))
        .await;
    let (shared, reads) = load.throughput(seek_under_load(&fixture, 10)).await;
    load.stop().await;
    let (p50, p95, p99) = (
        percentile(&reads, 50),
        percentile(&reads, 95),
        percentile(&reads, 99),
    );
    eprintln!(
        "stream reads={} p50={p50:?} p95={p95:?} p99={p99:?}; queue {alone:.1} MB/s alone, \
         {shared:.1} MB/s while seeking",
        reads.len()
    );
    // Relative to a read on an idle connection (~0.3 s), so a slow runner does not fail
    // it: behind four queued articles a read takes about three of those.
    assert!(p99 < idle * 5 / 2, "p99 {p99:?}, idle read {idle:?}");
    assert!(
        shared < alone * 0.8,
        "the queue kept {shared:.1} of {alone:.1} MB/s while playback was active"
    );
}

#[tokio::test]
async fn queue_work_pipelines_fully_once_playback_has_gone_quiet() {
    let fixture = fixture().await;
    let load = QueueLoad::start(&fixture);
    let id = &fixture.corpus.articles[0].message_id;
    fixture
        .client
        .body(id, Workload::Stream, None)
        .await
        .unwrap();
    // Past the bound's linger after the last playback request.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let mut deepest = [0usize; CONNECTIONS];
    let sampled = Instant::now();
    while sampled.elapsed() < Duration::from_secs(1) {
        let stats = fixture.client.stats();
        assert_eq!(stats[0].connections.len(), CONNECTIONS);
        for (deepest, connection) in deepest.iter_mut().zip(&stats[0].connections) {
            *deepest = (*deepest).max(connection.requests);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (throughput, ()) = load
        .throughput(tokio::time::sleep(Duration::from_secs(2)))
        .await;
    load.stop().await;
    eprintln!("queue alone {throughput:.1} MB/s, deepest per connection {deepest:?}");
    // Background work always leaves one connection idle for playback.
    let pipelined = CONNECTIONS - 1;
    assert!(
        deepest.iter().filter(|&&depth| depth >= DEPTH).count() >= pipelined,
        "{deepest:?}"
    );
    let transfer = megabytes(ARTICLE_SIZE) / megabytes(BANDWIDTH);
    let ceiling =
        megabytes(pipelined * ARTICLE_SIZE) / (IMPAIRMENT.response_delay.as_secs_f64() + transfer);
    assert!(
        throughput > ceiling * 0.9,
        "{throughput:.1} of {ceiling:.1} MB/s"
    );
}
