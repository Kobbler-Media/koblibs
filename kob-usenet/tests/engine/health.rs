use kob_usenet::HealthSample;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::{Fixture, engine, import, output, server};

#[tokio::test]
async fn a_health_sample_stats_a_share_of_the_release_and_counts_what_is_gone() {
    let fixture = Fixture::season();
    let total: usize = fixture.files.iter().map(|f| f.articles.len()).sum();
    let gone = [
        fixture.files[0].articles[2].message_id.clone(),
        fixture.files[1].articles[7].message_id.clone(),
    ];
    let healthy = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let release = {
        let engine = engine(dir.path(), vec![server("a", &healthy, 0)]).await;
        import(&engine, &fixture.standard_nzb(), None).await
    };
    let articles = fixture
        .articles()
        .into_iter()
        .filter(|a| !gone.contains(&a.message_id));
    let damaged = LoopbackServer::start(articles).await.unwrap();
    let engine = engine(dir.path(), vec![server("a", &damaged, 0)]).await;

    let all = engine.sample_health(&release.key, 100, None).await.unwrap();
    assert_eq!(
        all,
        HealthSample {
            checked: total,
            missing: 2
        }
    );
    let some = engine.sample_health(&release.key, 5, None).await.unwrap();
    assert_eq!(some.checked, total.div_ceil(20));
    assert_eq!(damaged.body_commands(), 0, "a sample only STATs");

    let episode = output(&release, "Show.S01E01.mkv");
    let one = engine
        .sample_health(&release.key, 100, Some(episode))
        .await
        .unwrap();
    assert_eq!(
        one,
        HealthSample {
            checked: fixture.files[1].articles.len(),
            missing: 1
        },
        "only the asked output's articles are sampled"
    );
    assert!(matches!(
        engine.sample_health(&release.key, 100, Some(99)).await,
        Err(kob_usenet::Error::UnknownOutput { index: 99, .. })
    ));
}

#[tokio::test]
async fn a_health_sample_runs_on_a_spawned_task() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(engine(dir.path(), vec![server("a", &nntp, 0)]).await);
    let release = import(&engine, &fixture.standard_nzb(), None).await;

    let sample = tokio::spawn(async move { engine.sample_health(&release.key, 100, None).await })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(sample.missing, 0);
}
