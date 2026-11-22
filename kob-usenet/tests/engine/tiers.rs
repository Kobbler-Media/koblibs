use std::time::Duration;

use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, EngineConfig, Purpose};
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, output, server};

#[tokio::test]
async fn the_backup_tier_serves_only_articles_the_primary_lacks() {
    let fixture = Fixture::season();
    let missing = fixture.files[1].articles[5].message_id.clone();
    for (primary_articles, backup_bodies) in
        [(fixture.articles(), 0), (fixture.without(&missing), 1)]
    {
        let primary = LoopbackServer::start(primary_articles).await.unwrap();
        let backup = LoopbackServer::start(fixture.articles()).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(
            dir.path(),
            vec![
                server("primary", &primary, 0),
                server("backup", &backup, 99),
            ],
        )
        .await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        for out in &release.outputs {
            let mut all = Vec::new();
            engine
                .open(&release.key, out.index, Purpose::Play)
                .unwrap()
                .read_to_end(&mut all)
                .await
                .unwrap();
            assert!(all == fixture.data(&out.path), "{}", out.path);
        }
        assert_eq!(backup.body_commands(), backup_bodies, "missing {missing}");
        let rescued = engine
            .stats()
            .into_iter()
            .find(|s| s.key == "backup")
            .unwrap();
        assert_eq!(rescued.priority, 99);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Primary {
    Refused,
    Stalled,
    ReconfiguredToRefused,
    Lacks,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_primary_that_is_down_or_stalled_never_spends_the_backup() {
    let fixture = Fixture::season();
    let name = "Show.S01E01.mkv";
    let lacked = fixture.files[1].articles[3].message_id.clone();
    for (scenario, served) in [
        (Primary::Refused, false),
        (Primary::Stalled, false),
        (Primary::ReconfiguredToRefused, false),
        (Primary::Lacks, true),
    ] {
        let healthy = LoopbackServer::start(fixture.articles()).await.unwrap();
        let backup = LoopbackServer::start(fixture.articles()).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let release = {
            let engine = engine(dir.path(), vec![server("primary", &healthy, 0)]).await;
            import(&engine, &fixture.standard_nzb(), None).await
        };
        let primary = match scenario {
            Primary::Lacks => LoopbackServer::start(fixture.without(&lacked)).await,
            _ => LoopbackServer::start(fixture.articles()).await,
        }
        .unwrap();
        if scenario == Primary::Stalled {
            for article in fixture.articles() {
                primary.stall_article(&article.message_id, usize::MAX, Duration::from_secs(10));
            }
        }
        if matches!(scenario, Primary::Refused | Primary::ReconfiguredToRefused) {
            primary.refuse().await;
        }
        let servers = vec![
            server("primary", &primary, 0),
            server("backup", &backup, 99),
        ];
        let mut config = EngineConfig::new(dir.path().to_path_buf(), servers.clone());
        config.provider_timeout = Duration::from_millis(500);
        if scenario == Primary::ReconfiguredToRefused {
            config.servers = vec![server("primary", &healthy, 0)];
        }
        let engine = Engine::new(config).await.unwrap();
        if scenario == Primary::ReconfiguredToRefused {
            engine.reconfigure(servers).unwrap();
        }
        let mut all = Vec::new();
        let read = tokio::time::timeout(
            Duration::from_secs(20),
            engine
                .open(&release.key, output(&release, name), Purpose::Play)
                .unwrap()
                .read_to_end(&mut all),
        )
        .await
        .expect("read ended");
        assert_eq!(read.is_ok(), served, "{scenario:?}: {read:?}");
        if served {
            assert!(all == fixture.data(name), "{scenario:?}");
        }
        assert_eq!(backup.body_commands(), usize::from(served), "{scenario:?}");
    }
}
