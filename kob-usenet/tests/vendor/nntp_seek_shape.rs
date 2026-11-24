#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::core::trace::Probe;
use kob_usenet::vendor::nntp::{
    AssignLoad, Client, ConnectionPath, ConnectionRoute, Downloads, Endpoint, Error, Pool,
    PoolConfig, Provider, StreamStage, TlsMode, Workload, WorkloadStageSnapshot,
};
use kob_usenet::vendor::testkit::{Corpus, LoopbackServer};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const ARTICLE: usize = 16 * 1024;
const PROMPT: Duration = Duration::from_secs(1);
const CACHE_READER: u64 = 1_000_001;
const OLD_SEEK: u64 = 1_000_002;
const NEW_SEEK: u64 = 1_000_003;
/// More than a cache reader starts with in flight, so the next request is held.
const PACER_BOUND: u32 = 21 << 20;

struct Shape {
    client: Arc<Client>,
    server: LoopbackServer,
    ids: Vec<String>,
    releases: Vec<oneshot::Sender<()>>,
}

impl Shape {
    async fn new(connections: usize, depth: usize, warm: usize) -> Self {
        let corpus = Corpus::generate(91, 32, ARTICLE);
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
            max_connections: connections,
            pipeline_depth: depth,
            idle_timeout: Duration::from_secs(60),
            min_connections: 0,
        };
        let pool = Pool::new(endpoint, config, Downloads::new(connections * depth)).unwrap();
        let client = Arc::new(Client::new([Provider {
            pool,
            key: "loopback".into(),
            name: "loopback".into(),
            priority: 0,
            backbone: None,
        }]));
        client.prewarm(warm).await;
        Self {
            client,
            server,
            ids: corpus
                .articles
                .iter()
                .map(|a| a.message_id.clone())
                .collect(),
            releases: Vec::new(),
        }
    }

    fn read(&self, at: usize, workload: Workload, reader: u64) -> JoinHandle<Result<(), Error>> {
        self.read_as(at, workload, probe(reader))
    }

    fn read_as(
        &self,
        at: usize,
        workload: Workload,
        probe: Probe,
    ) -> JoinHandle<Result<(), Error>> {
        let (client, id) = (Arc::clone(&self.client), self.ids[at].clone());
        tokio::spawn(async move { client.body(&id, workload, Some(&probe)).await.map(drop) })
    }

    /// Leaves one idle connection that has just moved a body.
    async fn warmed(&self) {
        finishes(
            self.read(31, Workload::Cache, CACHE_READER),
            "the warming read",
        )
        .await;
    }

    /// A read whose body the provider sits on, already sent when this returns.
    async fn held(
        &mut self,
        at: usize,
        workload: Workload,
        reader: u64,
    ) -> JoinHandle<Result<(), Error>> {
        let (arrived, release) = self.server.hold_article(&self.ids[at]);
        self.releases.push(release);
        let read = self.read(at, workload, reader);
        timeout(PROMPT, arrived).await.unwrap().unwrap();
        read
    }

    fn stream(&self) -> WorkloadStageSnapshot {
        self.client
            .stream_stages()
            .snapshot()
            .into_iter()
            .find(|s| s.workload == Workload::Stream)
            .unwrap()
    }

    fn release(&mut self) {
        for release in self.releases.drain(..) {
            let _ = release.send(());
        }
    }
}

fn probe(reader: u64) -> Probe {
    Probe {
        trace: None,
        wait: None,
        watch: None,
        segment: 0,
        hedge: false,
        size: ARTICLE as u32,
        reader,
    }
}

async fn finishes(read: JoinHandle<Result<(), Error>>, what: &str) {
    timeout(PROMPT, read)
        .await
        .unwrap_or_else(|_| panic!("{what} stayed queued with connections free"))
        .unwrap()
        .unwrap();
}

async fn abandon(read: JoinHandle<Result<(), Error>>) {
    read.abort();
    let _ = read.await;
}

#[tokio::test]
async fn a_seek_goes_to_a_free_connection_past_the_last_seeks_abandoned_read_ahead() {
    for warm in [8, 3] {
        let mut shape = Shape::new(8, 4, warm).await;
        let cache = shape.held(0, Workload::Cache, CACHE_READER).await;
        for at in 1..=2 {
            let stale = shape.held(at, Workload::Stream, OLD_SEEK).await;
            abandon(stale).await;
        }
        let seek = shape.read(3, Workload::Stream, NEW_SEEK);
        finishes(seek, "the seek").await;

        let stream = shape.stream();
        assert_eq!(stream.route(ConnectionRoute::Busy), 0, "{warm} warm");
        assert_eq!(stream.stage(StreamStage::Total).count(), 1);
        assert!(shape.server.peak_connections() <= 8);
        shape.release();
        finishes(cache, "the cache read").await;
    }
}

#[tokio::test]
async fn of_abandoned_read_ahead_only_the_one_command_on_the_wire_is_answered() {
    let mut shape = Shape::new(1, 4, 1).await;
    let head = shape.held(0, Workload::Stream, OLD_SEEK).await;
    let queued = [
        shape.read(1, Workload::Stream, OLD_SEEK),
        shape.read(2, Workload::Stream, OLD_SEEK),
    ];
    tokio::time::sleep(Duration::from_millis(30)).await;
    for read in queued {
        abandon(read).await;
    }
    shape.release();
    finishes(head, "the head read").await;
    finishes(shape.read(3, Workload::Stream, NEW_SEEK), "the seek").await;
    assert_eq!(
        shape.server.body_commands(),
        3,
        "the head, the one command sent during it, and the seek"
    );
}

#[tokio::test]
async fn cache_takes_a_free_connection_while_playback_is_busy() {
    let mut shape = Shape::new(8, 1, 8).await;
    let cache = shape.held(0, Workload::Cache, CACHE_READER).await;
    let first = shape.held(1, Workload::Stream, OLD_SEEK).await;
    let second = shape.held(2, Workload::Stream, OLD_SEEK).await;
    let third = shape.read(3, Workload::Stream, OLD_SEEK);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let more_cache = shape.read(4, Workload::Cache, CACHE_READER);
    finishes(more_cache, "the cache read").await;
    finishes(third, "the third playback read").await;
    shape.release();
    for read in [cache, first, second] {
        finishes(read, "a released read").await;
    }
}

async fn still_waiting(read: &JoinHandle<Result<(), Error>>, what: &str) {
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(!read.is_finished(), "{what} did not wait");
}

#[tokio::test]
async fn a_download_starts_nothing_while_a_viewer_request_is_in_flight() {
    for workload in [Workload::Stream, Workload::Cache] {
        let mut shape = Shape::new(8, 4, 8).await;
        let viewer = shape.held(0, workload, OLD_SEEK).await;
        let job = shape.read(1, Workload::Queue, 0);
        still_waiting(&job, "the download").await;
        assert_eq!(shape.server.body_commands(), 1, "{workload:?}");
        shape.release();
        finishes(viewer, "the viewer read").await;
        finishes(job, "the download").await;
    }
}

#[tokio::test]
async fn a_download_waits_for_a_cache_request_the_pacer_holds() {
    let mut shape = Shape::new(8, 4, 8).await;
    let (arrived, release) = shape.server.hold_article(&shape.ids[0]);
    shape.releases.push(release);
    let filling = shape.read_as(
        0,
        Workload::Cache,
        Probe {
            size: PACER_BOUND,
            ..probe(CACHE_READER)
        },
    );
    timeout(PROMPT, arrived).await.unwrap().unwrap();
    let paced = shape.read(1, Workload::Cache, CACHE_READER);
    still_waiting(&paced, "the paced cache read").await;
    let job = shape.read(2, Workload::Queue, 0);
    still_waiting(&job, "the download").await;
    assert_eq!(shape.server.body_commands(), 1);
    shape.release();
    finishes(filling, "the cache read").await;
    finishes(paced, "the paced cache read").await;
    finishes(job, "the download").await;
}

#[tokio::test]
async fn a_seek_takes_an_idle_connection_that_just_moved_data() {
    let shape = Shape::new(8, 4, 8).await;
    shape.warmed().await;
    finishes(shape.read(1, Workload::Stream, NEW_SEEK), "the seek").await;
    let stream = shape.stream();
    assert_eq!(stream.path(ConnectionPath::Warm), 1);
    assert_eq!(stream.route(ConnectionRoute::Idle), 1);
}

#[tokio::test]
async fn a_seek_waits_behind_one_warm_body_rather_than_take_a_cold_connection() {
    let mut shape = Shape::new(8, 4, 8).await;
    shape.warmed().await;
    let cache = shape.held(1, Workload::Cache, CACHE_READER).await;
    let seek = shape.read(2, Workload::Stream, NEW_SEEK);
    still_waiting(&seek, "the seek").await;
    let stream = shape.stream();
    assert_eq!(stream.path(ConnectionPath::Warm), 1);
    assert_eq!(stream.route(ConnectionRoute::Busy), 1);
    assert_eq!(stream.load(AssignLoad::RequestsAhead).sum, 1);
    shape.release();
    finishes(cache, "the cache read").await;
    finishes(seek, "the seek").await;
}

#[tokio::test]
async fn a_seek_never_waits_behind_an_abandoned_body_or_a_second_one() {
    let mut shape = Shape::new(8, 4, 8).await;
    shape.warmed().await;
    let stale = shape.held(1, Workload::Cache, CACHE_READER).await;
    abandon(stale).await;
    finishes(
        shape.read(2, Workload::Stream, NEW_SEEK),
        "a seek past an abandoned body",
    )
    .await;
    assert_eq!(shape.stream().route(ConnectionRoute::Busy), 0);
    shape.release();

    let mut shape = Shape::new(8, 4, 8).await;
    shape.warmed().await;
    let cache = shape.held(1, Workload::Cache, CACHE_READER).await;
    let behind = shape.read(2, Workload::Stream, NEW_SEEK);
    still_waiting(&behind, "the first seek").await;
    finishes(
        shape.read(3, Workload::Stream, NEW_SEEK),
        "a seek past a full warm path",
    )
    .await;
    let stream = shape.stream();
    assert_eq!(stream.route(ConnectionRoute::Busy), 1);
    assert_eq!(stream.path(ConnectionPath::Cold), 1);
    shape.release();
    finishes(cache, "the cache read").await;
    finishes(behind, "the first seek").await;
}

#[tokio::test]
async fn an_active_stream_keeps_warm_connections_within_the_cap() {
    for (limit, wanted) in [(3, 1), (8, 3), (16, 5), (49, 9)] {
        let shape = Shape::new(limit, 4, 0).await;
        let play = shape.read(0, Workload::Stream, OLD_SEEK);
        finishes(play, "the playback read").await;
        let deadline = tokio::time::Instant::now() + PROMPT;
        while shape.server.open_connections() < wanted && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(shape.server.open_connections(), wanted, "limit {limit}");
        assert!(shape.server.peak_connections() <= limit);
    }
}
