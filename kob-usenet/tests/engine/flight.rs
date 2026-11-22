use kob_usenet::vendor::nntp::Workload;
use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{OutputReader, Purpose};
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, output, server};

async fn whole(mut reader: OutputReader) -> Vec<u8> {
    let mut all = Vec::new();
    reader.read_to_end(&mut all).await.unwrap();
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn play_and_cache_readers_of_one_output_fetch_each_article_once() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let name = "Show.S01E01.mkv";
    let expected = fixture.data(name);
    let articles = fixture
        .files
        .iter()
        .find(|f| f.name == name)
        .unwrap()
        .articles
        .len();

    for order in [
        [Purpose::Play, Purpose::Cache],
        [Purpose::Cache, Purpose::Play],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        let index = output(&release, name);
        let before = nntp.body_commands();
        let first = engine.open(&release.key, index, order[0]).unwrap();
        let second = engine.open(&release.key, index, order[1]).unwrap();
        let (a, b) = tokio::join!(whole(first), whole(second));
        assert!(a == expected && b == expected, "{order:?}");
        assert_eq!(nntp.body_commands() - before, articles, "{order:?}");
    }
}

#[tokio::test]
async fn a_warm_reader_fetches_on_the_warm_workload() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let release = import(&engine, &fixture.standard_nzb(), None).await;
    let index = output(&release, "Show.S01E02.mkv");
    let all = whole(engine.open(&release.key, index, Purpose::Warm).unwrap()).await;
    assert!(all == fixture.data("Show.S01E02.mkv"));
    let stats = &engine.stats()[0];
    let attempts = |w: Workload| stats.workloads[w as usize].attempts.iter().sum::<u64>();
    assert!(attempts(Workload::Warm) > 0);
    assert_eq!(attempts(Workload::Stream), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cache_reader_trailing_playback_further_than_read_ahead_reads_from_memory() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config =
        kob_usenet::EngineConfig::new(dir.path().to_path_buf(), vec![server("a", &nntp, 0)]);
    config.read_ahead_bytes = 32 * 1024;
    let engine = kob_usenet::Engine::new(config).await.unwrap();
    let release = import(&engine, &fixture.standard_nzb(), None).await;
    let index = output(&release, "Show.S01E01.mkv");
    let played = whole(engine.open(&release.key, index, Purpose::Play).unwrap()).await;
    let before = nntp.body_commands();
    let cached = whole(engine.open(&release.key, index, Purpose::Cache).unwrap()).await;
    assert!(played == cached && cached == fixture.data("Show.S01E01.mkv"));
    assert_eq!(nntp.body_commands(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playback_that_joined_a_stalled_cache_fetch_still_gets_its_bytes() {
    let fixture = Fixture::season();
    let healthy = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let release = {
        let engine = engine(dir.path(), vec![server("a", &healthy, 0)]).await;
        import(&engine, &fixture.standard_nzb(), None).await
    };
    let stalling =
        LoopbackServer::start_stalling(fixture.articles(), 2, std::time::Duration::from_secs(3))
            .await
            .unwrap();
    let mut config =
        kob_usenet::EngineConfig::new(dir.path().to_path_buf(), vec![server("a", &stalling, 0)]);
    config.provider_timeout = std::time::Duration::from_millis(500);
    let engine = kob_usenet::Engine::new(config).await.unwrap();
    let index = output(&release, "Show.S01E01.mkv");
    let cache = engine.open(&release.key, index, Purpose::Cache).unwrap();
    let cache = tokio::spawn(whole(cache));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let played = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        whole(engine.open(&release.key, index, Purpose::Play).unwrap()),
    )
    .await
    .expect("playback finished");
    assert!(played == fixture.data("Show.S01E01.mkv"));
    assert!(cache.await.unwrap() == fixture.data("Show.S01E01.mkv"));
}
