use kob_usenet::HealthSample;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::{Fixture, engine, import, server};

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

    let all = engine.sample_health(&release.key, 100).await.unwrap();
    assert_eq!(
        all,
        HealthSample {
            checked: total,
            missing: 2
        }
    );
    let some = engine.sample_health(&release.key, 5).await.unwrap();
    assert_eq!(some.checked, total.div_ceil(20));
    assert_eq!(damaged.body_commands(), 0, "a sample only STATs");
}
