use std::task::Poll;

use crate::vendor::testkit::{Corpus, LoopbackServer};

use super::*;
use crate::vendor::nntp::{BodyExpectation, TlsMode};

const ARTICLE: usize = 16 * 1024;

fn pool_on(server: &LoopbackServer, max_connections: usize, timeout: Duration) -> Pool {
    let endpoint = Endpoint::new(
        "127.0.0.1",
        server.addr().port(),
        TlsMode::Plain,
        None,
        timeout,
    )
    .unwrap();
    let config = PoolConfig {
        max_connections,
        pipeline_depth: 4,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    Pool::new(endpoint, config, Downloads::new(128)).unwrap()
}

async fn served(articles: usize) -> (LoopbackServer, Vec<String>) {
    let corpus = Corpus::generate(93, articles, ARTICLE);
    let ids = corpus
        .articles
        .iter()
        .map(|a| a.message_id.clone())
        .collect();
    let server = LoopbackServer::start(corpus.articles).await.unwrap();
    (server, ids)
}

fn cache_tag(reader: u64, decoded_len: Option<u64>) -> Tag {
    Tag {
        workload: Workload::Cache,
        lane: Some(Lane::reader(reader)),
        expected: BodyExpectation {
            decoded_len,
            ..BodyExpectation::NONE
        },
        ..Tag::UNTRACKED
    }
}

async fn still_waiting<F: Future + Unpin>(future: &mut F) -> bool {
    tokio::time::sleep(Duration::from_millis(50)).await;
    poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx).is_pending())).await
}

#[tokio::test]
async fn cache_requests_turned_away_by_a_connect_give_back_their_pace_charge() {
    let (server, ids) = served(31).await;
    let pool =
        pool_on(&server, 40, Duration::from_secs(1)).with_account(Arc::new(Semaphore::new(1)));
    let (arrived, release) = server.hold_article(&ids[0]);
    let first = {
        let (pool, id) = (pool.clone(), ids[0].clone());
        tokio::spawn(async move {
            pool.fetch(Command::Body, &id, &cache_tag(1, None))
                .await
                .is_ok()
        })
    };
    timeout(Duration::from_secs(2), arrived)
        .await
        .unwrap()
        .unwrap();
    let rest: Vec<_> = ids[1..]
        .iter()
        .map(|id| {
            let (pool, id) = (pool.clone(), id.clone());
            tokio::spawn(async move {
                let _ = pool
                    .fetch(Command::Body, &id, &cache_tag(1, Some(750_000)))
                    .await;
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(700)).await;
    release.send(()).unwrap();
    assert!(first.await.unwrap());
    let all = async {
        for request in rest {
            request.await.unwrap();
        }
    };
    let finished = timeout(Duration::from_secs(5), all).await;
    let (in_flight, _) = pool.pace.during_seek();
    assert!(
        finished.is_ok(),
        "cache requests parked on their own charge, {in_flight} bytes still counted"
    );
    assert_eq!(in_flight, 0);
}

#[tokio::test]
async fn a_cancelled_cache_connect_gives_back_its_charge_and_wakes_the_next_request() {
    let (server, _) = served(1).await;
    let account = Arc::new(Semaphore::new(0));
    let pool = pool_on(&server, 4, Duration::from_secs(5)).with_account(account);
    let mut flight = Flight::of(&pool.pace, 21_000_000, Lane::reader(1).0);
    let mut connecting = Box::pin(pool.worker(
        Workload::Cache,
        false,
        None,
        Some(Lane::reader(1)),
        &mut flight,
    ));
    assert!(still_waiting(&mut connecting).await);
    assert_eq!(pool.pace.during_seek().0, 21_000_000);
    drop(connecting);
    let woken = pool.changed.notified();
    tokio::pin!(woken);
    woken.as_mut().enable();
    drop(flight);
    assert_eq!(pool.pace.during_seek().0, 0);
    assert_eq!(
        pool.pace.landed_in_window(),
        0,
        "a refund is not throughput"
    );
    timeout(Duration::from_millis(100), woken)
        .await
        .expect("waiting requests hear of the refund");
}

#[tokio::test]
async fn a_new_cache_reader_starts_while_another_holds_the_bound_and_they_share_it() {
    let (server, _) = served(1).await;
    let pool = pool_on(&server, 8, Duration::from_secs(5));
    let (one, two) = (Lane::reader(1), Lane::reader(2));
    let mut flight = Flight::of(&pool.pace, 21_000_000, one.0);
    let held = pool
        .worker(Workload::Cache, false, None, Some(one), &mut flight)
        .await
        .ok()
        .flatten()
        .expect("the first reader gets a connection");

    let mut next_of_first = Flight::of(&pool.pace, 1_000_000, one.0);
    let mut parked =
        Box::pin(pool.worker(Workload::Cache, false, None, Some(one), &mut next_of_first));
    assert!(
        still_waiting(&mut parked).await,
        "the bound holds the first reader"
    );

    let mut first_of_second = Flight::of(&pool.pace, 1_000_000, two.0);
    let started = timeout(
        Duration::from_millis(500),
        pool.worker(
            Workload::Cache,
            false,
            None,
            Some(two),
            &mut first_of_second,
        ),
    )
    .await
    .expect("a new reader does not queue behind the first one's read-ahead");
    assert!(matches!(started, Ok(Some(_))));
    assert!(
        still_waiting(&mut parked).await,
        "the first reader, over its share, keeps waiting"
    );

    let mut over_share = Flight::of(&pool.pace, 20_000_000, two.0);
    let second = pool
        .worker(Workload::Cache, false, None, Some(two), &mut over_share)
        .await;
    assert!(matches!(second, Ok(Some(_))), "still under its share");
    let mut past_share = Flight::of(&pool.pace, 1_000_000, two.0);
    let mut stopped =
        Box::pin(pool.worker(Workload::Cache, false, None, Some(two), &mut past_share));
    assert!(
        still_waiting(&mut stopped).await,
        "past its share the second reader waits too"
    );
    drop(held);
}

async fn two_kept(pool: &Pool, ids: &[String]) -> crate::vendor::nntp::viewers::Viewing {
    pool.set_keep_warm(true);
    pool.lock().throttle = Some((2, Instant::now()));
    let viewing = pool.downloads.viewers().enter();
    pool.note_viewer(&ids[0]);
    let (a, b) = tokio::join!(
        pool.fetch(Command::Body, &ids[0], &Tag::UNTRACKED),
        pool.fetch(Command::Body, &ids[1], &Tag::UNTRACKED)
    );
    assert!(a.is_ok() && b.is_ok());
    tokio::time::sleep(KEEP_WARM_EVERY + Duration::from_millis(200)).await;
    let state = pool.lock();
    let kept = state.workers.iter().filter(|w| w.link.kept()).count();
    assert_eq!((state.workers.len(), kept), (2, 2));
    drop(state);
    viewing
}

#[tokio::test]
async fn a_cache_read_evicts_a_kept_connection_when_nothing_else_is_usable() {
    let (server, ids) = served(8).await;
    let pool = pool_on(&server, 8, Duration::from_secs(5));
    let viewing = two_kept(&pool, &ids).await;
    drop(viewing);
    let got = timeout(
        Duration::from_secs(2),
        pool.fetch(Command::Body, &ids[2], &cache_tag(1, None)),
    )
    .await;
    assert!(got.is_ok(), "cache read hung behind kept connections");
}

#[tokio::test]
async fn a_queue_read_evicts_a_kept_connection_after_the_viewer_leaves() {
    let (server, ids) = served(8).await;
    let pool = pool_on(&server, 8, Duration::from_secs(5));
    let viewing = two_kept(&pool, &ids).await;
    tokio::time::sleep(STREAM_LINGER + Duration::from_millis(100)).await;
    let tag = Tag {
        workload: Workload::Queue,
        ..Tag::UNTRACKED
    };
    let mut queued = Box::pin(pool.fetch(Command::Body, &ids[2], &tag));
    assert!(still_waiting(&mut queued).await, "a viewer holds the queue");
    pool.lock().throttle = Some((2, Instant::now()));
    drop(viewing);
    let got = timeout(STREAM_LINGER + Duration::from_secs(2), &mut queued).await;
    assert!(got.is_ok(), "download stayed parked after the viewer left");
}

#[tokio::test]
async fn a_kept_connection_leaving_the_set_wakes_waiters() {
    let (server, ids) = served(8).await;
    let pool = pool_on(&server, 8, Duration::from_secs(5));
    let viewing = two_kept(&pool, &ids).await;
    drop(viewing);
    let woken = pool.changed.notified();
    tokio::pin!(woken);
    woken.as_mut().enable();
    timeout(STREAM_LINGER + KEEP_WARM_EVERY * 4, woken)
        .await
        .expect("the slot's release is announced");
}

#[tokio::test]
async fn a_waiter_wakes_when_a_throttled_pool_may_grow_again() {
    let (server, _) = served(1).await;
    let pool = pool_on(&server, 4, Duration::from_secs(5));
    let mut flight = Flight::default();
    let first = pool
        .worker(Workload::Queue, true, None, None, &mut flight)
        .await
        .ok()
        .flatten()
        .expect("a first connection");
    pool.lock().throttle = Some((
        1,
        Instant::now() - THROTTLE_STEP + Duration::from_millis(300),
    ));
    let mut second = Flight::default();
    let grown = timeout(
        Duration::from_secs(2),
        pool.worker(Workload::Queue, true, None, None, &mut second),
    )
    .await;
    assert!(
        matches!(grown, Ok(Ok(Some(_)))),
        "regrowth went unnoticed by a parked request"
    );
    drop(first);
}

#[tokio::test]
async fn a_new_cache_reader_gets_bytes_promptly_beside_a_large_cache_ahead() {
    let corpus = Corpus::generate(94, 41, ARTICLE);
    let ids: Vec<String> = corpus
        .articles
        .iter()
        .map(|a| a.message_id.clone())
        .collect();
    let impairment = crate::vendor::testkit::Impairment {
        response_delay: Duration::from_millis(120),
        ..Default::default()
    };
    let server = LoopbackServer::start_impaired(corpus.articles, impairment)
        .await
        .unwrap();
    let endpoint = Endpoint::new(
        "127.0.0.1",
        server.addr().port(),
        TlsMode::Plain,
        None,
        Duration::from_secs(5),
    )
    .unwrap();
    let config = PoolConfig {
        max_connections: 4,
        pipeline_depth: 1,
        idle_timeout: Duration::from_secs(60),
        min_connections: 0,
    };
    let pool = Pool::new(endpoint, config, Downloads::new(4)).unwrap();
    let read = |reader: u64, id: String| {
        let pool = pool.clone();
        tokio::spawn(async move {
            let tag = Tag {
                probe: Some(crate::vendor::core::trace::Probe {
                    trace: None,
                    wait: None,
                    watch: None,
                    segment: 0,
                    hedge: false,
                    size: ARTICLE as u32,
                    reader,
                }),
                ..cache_tag(reader, None)
            };
            pool.fetch(Command::Body, &id, &tag).await.is_ok()
        })
    };
    let backlog: Vec<_> = ids[1..].iter().map(|id| read(1, id.clone())).collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    assert!(read(2, ids[0].clone()).await.unwrap());
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(700),
        "the new reader waited {waited:?} behind the other's backlog"
    );
    for request in backlog {
        request.abort();
    }
}
