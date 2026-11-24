#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_usenet::vendor::nntp::{
    Client, ConnectionPath, ConnectionRoute, Downloads, Endpoint, Error, Pool, PoolConfig,
    Provider, TlsMode, Usage, Workload,
};
use kob_usenet::vendor::testkit::{Article, Corpus, Impairment, LoopbackServer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const ARTICLE: usize = 16 * 1024;
const TICKS_PER_SECOND: usize = 4;
const KEPT: usize = 4;

fn corpus() -> Corpus {
    Corpus::generate(93, 32, ARTICLE)
}

fn endpoint(port: u16, tls: TlsMode) -> Endpoint {
    Endpoint::new("127.0.0.1", port, tls, None, Duration::from_secs(5)).unwrap()
}

fn provider(endpoint: Endpoint, connections: usize, depth: usize, priority: u32) -> Provider {
    let config = PoolConfig {
        max_connections: connections,
        pipeline_depth: depth,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    Provider {
        pool: Pool::new(endpoint, config, Downloads::new(connections * depth)).unwrap(),
        key: format!("p{priority}"),
        name: format!("p{priority}"),
        priority,
        backbone: None,
    }
}

fn client_on(server: &LoopbackServer, connections: usize) -> Arc<Client> {
    let endpoint = endpoint(server.addr().port(), TlsMode::Plain);
    Arc::new(Client::new([provider(endpoint, connections, 4, 0)]))
}

fn read(client: &Arc<Client>, id: &str, workload: Workload) -> JoinHandle<Result<(), Error>> {
    let (client, id) = (Arc::clone(client), id.to_owned());
    tokio::spawn(async move { client.body(&id, workload, None).await.map(drop) })
}

/// Bodies moved at once, so that many connections are warm and idle.
async fn warm(client: &Arc<Client>, articles: &[Article]) {
    let reads: Vec<_> = articles
        .iter()
        .map(|a| read(client, &a.message_id, Workload::Stream))
        .collect();
    for read in reads {
        read.await.unwrap().unwrap();
    }
}

/// A playback read the provider sits on: a viewer stays active until released.
async fn viewer(
    client: &Arc<Client>,
    server: &LoopbackServer,
    id: &str,
) -> (JoinHandle<Result<(), Error>>, oneshot::Sender<()>) {
    let (arrived, release) = server.hold_article(id);
    let read = read(client, id, Workload::Stream);
    timeout(Duration::from_secs(1), arrived)
        .await
        .unwrap()
        .unwrap();
    (read, release)
}

async fn stats_over(server: &LoopbackServer, span: Duration) -> usize {
    let before = server.stat_commands();
    sleep(span).await;
    server.stat_commands() - before
}

#[tokio::test]
async fn keep_warm_runs_only_while_a_viewer_is_active_and_stops_after() {
    let corpus = corpus();
    let server = LoopbackServer::start(corpus.articles.clone())
        .await
        .unwrap();
    let client = client_on(&server, 16);

    read(&client, &corpus.articles[20].message_id, Workload::Queue)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stats_over(&server, Duration::from_millis(700)).await,
        0,
        "download-only work keeps nothing warm"
    );

    warm(&client, &corpus.articles[..3]).await;
    let (held, release) = viewer(&client, &server, &corpus.articles[3].message_id).await;
    assert!(stats_over(&server, Duration::from_millis(1100)).await >= TICKS_PER_SECOND);
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
    sleep(Duration::from_millis(2300)).await;
    assert_eq!(
        stats_over(&server, Duration::from_millis(700)).await,
        0,
        "nobody is watching"
    );
    assert!(client.stats()[0].keep_warm_stats > 0);
}

#[tokio::test]
async fn keep_warm_holds_the_named_count_and_interval_and_opens_nothing() {
    let corpus = corpus();
    let server = LoopbackServer::start(corpus.articles.clone())
        .await
        .unwrap();
    let client = client_on(&server, 16);
    warm(&client, &corpus.articles[..8]).await;
    let (held, release) = viewer(&client, &server, &corpus.articles[8].message_id).await;
    sleep(Duration::from_millis(300)).await;
    let open = server.open_connections();
    let sent = stats_over(&server, Duration::from_secs(1)).await;
    let wanted = KEPT * TICKS_PER_SECOND;
    assert!(
        (wanted - 4..=wanted + 4).contains(&sent),
        "{sent} STATs in a second"
    );
    assert_eq!(server.open_connections(), open);
    assert!(client.stats()[0].warm_connections >= KEPT);
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_seek_takes_a_kept_connection_after_its_last_body_has_aged() {
    let corpus = corpus();
    let server = LoopbackServer::start(corpus.articles.clone())
        .await
        .unwrap();
    let client = client_on(&server, 16);
    warm(&client, &corpus.articles[..3]).await;
    let (held, release) = viewer(&client, &server, &corpus.articles[3].message_id).await;
    sleep(Duration::from_millis(1400)).await;
    let before = client.stream_stages().snapshot()[0].path(ConnectionPath::Warm);
    read(&client, &corpus.articles[4].message_id, Workload::Stream)
        .await
        .unwrap()
        .unwrap();
    let stream = client.stream_stages().snapshot().remove(0);
    assert_eq!(stream.path(ConnectionPath::Warm), before + 1);
    assert_eq!(stream.route(ConnectionRoute::Busy), 0);
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_backup_tier_is_never_kept_warm() {
    let corpus = corpus();
    let missing = &corpus.articles[0];
    let primary = LoopbackServer::start(corpus.articles[1..].to_vec())
        .await
        .unwrap();
    let backup = LoopbackServer::start(corpus.articles.clone())
        .await
        .unwrap();
    let client = Arc::new(Client::new([
        provider(endpoint(primary.addr().port(), TlsMode::Plain), 16, 4, 0),
        provider(endpoint(backup.addr().port(), TlsMode::Plain), 16, 4, 1),
    ]));
    read(&client, &missing.message_id, Workload::Stream)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(backup.body_commands(), 1);
    warm(&client, &corpus.articles[1..4]).await;
    let (held, release) = viewer(&client, &primary, &corpus.articles[4].message_id).await;
    sleep(Duration::from_millis(900)).await;
    assert!(primary.stat_commands() > 0);
    assert_eq!(backup.stat_commands(), 0);
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_keep_warm_miss_touches_no_health_memory_or_quota() {
    let corpus = corpus();
    let server = LoopbackServer::start(corpus.articles.clone())
        .await
        .unwrap();
    server.miss_stats(true);
    let usage = Arc::new(Usage::default());
    let endpoint = endpoint(server.addr().port(), TlsMode::Plain).with_usage(Arc::clone(&usage));
    let client = Arc::new(Client::new([provider(endpoint, 16, 4, 0)]));
    warm(&client, &corpus.articles[..3]).await;
    let (held, release) = viewer(&client, &server, &corpus.articles[3].message_id).await;
    sleep(Duration::from_millis(300)).await;
    let used = usage.used();
    let sent = stats_over(&server, Duration::from_millis(800)).await;
    assert!(sent > 0);
    assert_eq!(usage.used(), used, "keep-warm replies are not quota");
    let before = client.stats().remove(0);
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
    for article in &corpus.articles[..3] {
        client
            .body(&article.message_id, Workload::Stream, None)
            .await
            .unwrap();
    }
    let after = client.stats().remove(0);
    assert_eq!((after.trips, after.connect_failures), (0, 0));
    assert_eq!(client.missing_hits(), 0);
    assert_eq!(
        before.workloads[Workload::ALL
            .iter()
            .position(|w| *w == Workload::Stream)
            .unwrap()]
        .attempts
        .iter()
        .sum::<u64>()
            + 3,
        after.workloads[0].attempts.iter().sum::<u64>() - 1,
        "only the released read and the three reads were counted"
    );
}

#[tokio::test]
async fn a_real_request_is_answered_correctly_and_promptly_beside_keep_warm() {
    let corpus = corpus();
    let delay = Duration::from_millis(40);
    let server = LoopbackServer::start_impaired(
        corpus.articles.clone(),
        Impairment {
            response_delay: delay,
            ..Impairment::default()
        },
    )
    .await
    .unwrap();
    let client = client_on(&server, 16);
    warm(&client, &corpus.articles[..4]).await;
    let (held, release) = viewer(&client, &server, &corpus.articles[4].message_id).await;
    for round in 0..12 {
        sleep(Duration::from_millis(83)).await;
        let article = &corpus.articles[5 + round];
        let started = Instant::now();
        let body = client
            .body(&article.message_id, Workload::Stream, None)
            .await
            .unwrap();
        assert_eq!(body.data.len(), ARTICLE);
        assert!(started.elapsed() < delay * 4, "{:?}", started.elapsed());
    }
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_second_command_goes_out_while_the_first_body_is_being_read() {
    let corpus = corpus();
    let (first, second) = (corpus.articles[0].clone(), corpus.articles[1].clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (a, b) = (first.clone(), second.clone());
    let provider_side = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"200 ready\r\n").await.unwrap();
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains(&a.message_id)
        );
        let (head, tail) = a.wire.split_at(a.wire.len() / 2);
        write.write_all(b"222 0 <a> body\r\n").await.unwrap();
        write.write_all(head).await.unwrap();
        let next = timeout(Duration::from_secs(1), lines.next_line())
            .await
            .expect("the second command waited for the first body to finish")
            .unwrap()
            .unwrap();
        assert!(next.contains(&b.message_id));
        write.write_all(tail).await.unwrap();
        write.write_all(b"222 0 <b> body\r\n").await.unwrap();
        write.write_all(&b.wire).await.unwrap();
        let _ = lines.next_line().await;
    });
    let client = Arc::new(Client::new([provider(
        endpoint(port, TlsMode::Plain),
        1,
        4,
        0,
    )]));
    let head = read(&client, &first.message_id, Workload::Stream);
    sleep(Duration::from_millis(60)).await;
    let behind = client.body(&second.message_id, Workload::Stream, None);
    let body = timeout(Duration::from_secs(3), behind)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.data.len(), ARTICLE);
    head.await.unwrap().unwrap();
    drop(client);
    provider_side.await.unwrap();
}

#[tokio::test]
async fn pipelined_requests_over_tls_stay_in_order_when_written_mid_body() {
    let corpus = corpus();
    let server = LoopbackServer::start_tls(corpus.articles.clone())
        .await
        .unwrap();
    let endpoint = endpoint(server.addr().port(), TlsMode::Unverified);
    let client = Arc::new(Client::new([provider(endpoint, 1, 4, 0)]));
    let reads: Vec<_> = corpus.articles[..24]
        .iter()
        .map(|article| {
            let (client, id) = (Arc::clone(&client), article.message_id.clone());
            tokio::spawn(async move {
                let body = client.body(&id, Workload::Stream, None).await.unwrap();
                assert_eq!(body.data.len(), ARTICLE);
            })
        })
        .collect();
    for read in reads {
        read.await.unwrap();
    }
}
