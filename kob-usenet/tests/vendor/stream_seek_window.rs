#![allow(clippy::unwrap_used)]

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::nntp::{
    BodyExpectation, Client, Downloads, Endpoint, Pool, PoolConfig, Provider, TlsMode, Workload,
};
use kob_usenet::vendor::stream::{Budget, Layout, Segment, SegmentReader};
use kob_usenet::vendor::testkit::{Corpus, LoopbackServer};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const ARTICLE: usize = 722 * 1024;
const ARTICLES: usize = 48;
const PREFETCH: usize = 44;
const SEEK_READ: usize = 2 << 20;

async fn reader() -> (SegmentReader, LoopbackServer) {
    let (reader, _, server) = parts().await;
    (reader, server)
}

async fn parts() -> (SegmentReader, Arc<Client>, LoopbackServer) {
    let corpus = Corpus::generate(92, ARTICLES, ARTICLE);
    let server = LoopbackServer::start(corpus.articles.clone())
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
        max_connections: 16,
        pipeline_depth: 4,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    let pool = Pool::new(endpoint, config, Downloads::new(64)).unwrap();
    let client = Arc::new(Client::new([Provider {
        pool,
        key: "loopback".into(),
        name: "loopback".into(),
        priority: 0,
        backbone: None,
    }]));
    let layout = Arc::new(Layout::new(
        corpus
            .articles
            .iter()
            .map(|a| Segment {
                message_id: a.message_id.clone(),
                size: ARTICLE as u64,
                expected: BodyExpectation::NONE,
            })
            .collect(),
    ));
    let reader = SegmentReader::new(Arc::clone(&client), layout, PREFETCH);
    (reader, client, server)
}

async fn settled(server: &LoopbackServer) -> usize {
    let mut seen = server.body_commands();
    loop {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let now = server.body_commands();
        if now == seen {
            return now;
        }
        seen = now;
    }
}

#[tokio::test]
async fn a_seek_fetches_little_past_what_it_reads_until_the_reader_proves_sequential() {
    let (mut reader, server) = reader().await;
    reader
        .seek(SeekFrom::Start(10 * ARTICLE as u64))
        .await
        .unwrap();
    let mut buf = vec![0; SEEK_READ];
    reader.read_exact(&mut buf).await.unwrap();
    let fetched = settled(&server).await;
    assert!(fetched <= 8, "a 2 MB seek asked for {fetched} articles");

    let mut rest = vec![0; 16 << 20];
    reader.read_exact(&mut rest).await.unwrap();
    let fetched = settled(&server).await;
    assert!(
        fetched >= ARTICLES - 12,
        "sequential reading opened only {fetched}"
    );
}

#[tokio::test]
async fn a_download_runs_beside_a_cache_reader_whose_consumer_has_stopped() {
    let corpus = Corpus::generate(92, ARTICLES, ARTICLE);
    let (reader, client, server) = parts().await;
    let mut reader = reader
        .with_workload(Workload::Cache)
        .with_budget(Budget::new(4 * ARTICLE));
    reader.read_exact(&mut [0; 16]).await.unwrap();
    let fetched = settled(&server).await;
    assert!(
        fetched < ARTICLES / 2,
        "the budget bounds the window: {fetched}"
    );
    tokio::time::timeout(
        Duration::from_secs(1),
        client.body(
            &corpus.articles[ARTICLES - 1].message_id,
            Workload::Queue,
            None,
        ),
    )
    .await
    .expect("a stopped reader holds no download")
    .unwrap();
}
