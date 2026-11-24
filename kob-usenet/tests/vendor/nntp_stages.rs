#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::nntp::{
    AssignLoad, Client, ConnectionRoute, Downloads, Endpoint, Pool, PoolConfig, Provider,
    StageMetrics, StreamStage, TlsMode, Workload, WorkloadStageSnapshot,
};
use kob_usenet::vendor::testkit::{Corpus, Impairment, LoopbackServer};

const ARTICLE: usize = 64 * 1024;
const REPLY_DELAY: Duration = Duration::from_millis(60);
const HANDSHAKE_DELAY: Duration = Duration::from_millis(15);
const ADDITIVE: [StreamStage; 8] = [
    StreamStage::Retry,
    StreamStage::Admission,
    StreamStage::ConnectionWait,
    StreamStage::SendQueue,
    StreamStage::PipelineWait,
    StreamStage::ProviderFirstByte,
    StreamStage::BodyTransfer,
    StreamStage::Decode,
];

async fn delayed(connections: usize, depth: usize) -> (Arc<Corpus>, Client, LoopbackServer) {
    let corpus = Arc::new(Corpus::generate(77, 6, ARTICLE));
    let server = LoopbackServer::start_impaired(
        corpus.articles.clone(),
        Impairment {
            handshake_delay: HANDSHAKE_DELAY,
            response_delay: REPLY_DELAY,
            bandwidth: None,
        },
    )
    .await
    .unwrap();
    let client = client_of(&server, connections, depth);
    (corpus, client, server)
}

fn client_of(server: &LoopbackServer, connections: usize, depth: usize) -> Client {
    let endpoint = Endpoint::new(
        "127.0.0.1",
        server.addr().port(),
        TlsMode::Plain,
        None,
        Duration::from_secs(5),
    )
    .unwrap();
    let config = PoolConfig {
        max_connections: connections,
        pipeline_depth: depth,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    let pool = Pool::new(endpoint, config, Downloads::new(connections * depth)).unwrap();
    Client::new([Provider {
        pool,
        key: "loopback".into(),
        name: "loopback".into(),
        priority: 0,
        backbone: None,
    }])
}

fn of(metrics: &StageMetrics, workload: Workload) -> WorkloadStageSnapshot {
    metrics
        .snapshot()
        .into_iter()
        .find(|s| s.workload == workload)
        .unwrap()
}

fn additive_sum(snapshot: &WorkloadStageSnapshot) -> f64 {
    ADDITIVE
        .iter()
        .map(|&stage| snapshot.stage(stage).sum_seconds())
        .sum()
}

#[tokio::test]
async fn a_delivered_read_partitions_its_time_into_stages() {
    let (corpus, client, _server) = delayed(2, 1).await;
    let id = &corpus.articles[0].message_id;
    client.body(id, Workload::Stream, None).await.unwrap();
    client.body(id, Workload::Stream, None).await.unwrap();

    let metrics = client.stream_stages();
    let stream = of(&metrics, Workload::Stream);
    let total = stream.stage(StreamStage::Total);
    assert_eq!(total.count(), 2);
    assert_eq!(stream.route(ConnectionRoute::New), 1);
    assert_eq!(stream.route(ConnectionRoute::Idle), 1);
    assert_eq!(stream.abandoned, 0);
    assert_eq!(stream.body_bytes, 2 * ARTICLE as u64);
    assert_eq!(stream.load(AssignLoad::BusyConnections).count(), 2);
    assert_eq!(stream.stage(StreamStage::Connect).count(), 1);
    assert!(stream.stage(StreamStage::Auth).count() == 1);
    assert!(
        stream.stage(StreamStage::ProviderFirstByte).sum_seconds()
            >= 2.0 * REPLY_DELAY.as_secs_f64() * 0.9
    );
    let drift = (additive_sum(&stream) - total.sum_seconds()).abs();
    assert!(drift < 0.002, "stages miss the total by {drift}s");
    assert_eq!(
        of(&metrics, Workload::Cache)
            .stage(StreamStage::Total)
            .count(),
        0
    );
}

#[tokio::test]
async fn cache_reads_are_labelled_apart_and_a_pipelined_read_counts_as_busy() {
    let (corpus, client, _server) = delayed(1, 4).await;
    let client = Arc::new(client);
    client
        .body(&corpus.articles[0].message_id, Workload::Cache, None)
        .await
        .unwrap();
    let reads: Vec<_> = corpus.articles[1..4]
        .iter()
        .map(|article| {
            let (client, id) = (Arc::clone(&client), article.message_id.clone());
            tokio::spawn(async move { client.body(&id, Workload::Stream, None).await })
        })
        .collect();
    for read in reads {
        read.await.unwrap().unwrap();
    }
    let metrics = client.stream_stages();
    assert_eq!(
        of(&metrics, Workload::Cache)
            .stage(StreamStage::Total)
            .count(),
        1
    );
    let stream = of(&metrics, Workload::Stream);
    assert_eq!(stream.stage(StreamStage::Total).count(), 3);
    assert!(stream.route(ConnectionRoute::Busy) >= 1);
    assert!(stream.load(AssignLoad::RequestsAhead).sum >= 1);
}

#[tokio::test]
async fn a_read_abandoned_before_its_reply_is_counted_and_not_timed() {
    let (corpus, client, _server) = delayed(1, 1).await;
    let id = corpus.articles[0].message_id.clone();
    let _ = tokio::time::timeout(
        REPLY_DELAY / 4 + HANDSHAKE_DELAY * 4,
        client.body(&id, Workload::Stream, None),
    )
    .await;
    let metrics = client.stream_stages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while of(&metrics, Workload::Stream).abandoned == 0 {
        assert!(tokio::time::Instant::now() < deadline, "never counted");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        of(&metrics, Workload::Stream)
            .stage(StreamStage::Total)
            .count(),
        0
    );
}

#[tokio::test]
async fn a_retried_read_keeps_its_stages_partitioned() {
    let corpus = Corpus::generate(78, 3, ARTICLE);
    let server = LoopbackServer::start_dropping(corpus.articles.clone(), 1)
        .await
        .unwrap();
    let client = client_of(&server, 1, 1);
    for article in &corpus.articles[..2] {
        client
            .body(&article.message_id, Workload::Stream, None)
            .await
            .unwrap();
    }
    let metrics = client.stream_stages();
    let stream = of(&metrics, Workload::Stream);
    let total = stream.stage(StreamStage::Total);
    assert_eq!(total.count(), 2);
    assert_eq!(stream.abandoned, 1, "the attempt on the dropped connection");
    assert!(stream.stage(StreamStage::Retry).sum > 0);
    let routes: u64 = stream.routes.iter().map(|(_, n)| n).sum();
    assert_eq!(routes, total.count() + stream.abandoned);
    let drift = (additive_sum(&stream) - total.sum_seconds()).abs();
    assert!(drift < 0.002, "stages miss the total by {drift}s");
}
