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

#[tokio::test]
async fn a_quick_check_stats_a_fixed_spread_including_both_ends() {
    let fixture = Fixture::season();
    let episode = &fixture.files[1].articles;
    let (first, last) = (
        episode[0].message_id.clone(),
        episode[episode.len() - 1].message_id.clone(),
    );
    let healthy = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let release = {
        let engine = engine(dir.path(), vec![server("a", &healthy, 0)]).await;
        import(&engine, &fixture.standard_nzb(), None).await
    };
    let index = output(&release, "Show.S01E01.mkv");
    for (gone, articles, checked, missing) in [
        (first.as_str(), 2, 2, 1),
        (last.as_str(), 2, 2, 1),
        (first.as_str(), 5, 5, 1),
        (last.as_str(), 5, 5, 1),
        (last.as_str(), 1000, episode.len(), 1),
    ] {
        let kept = fixture
            .articles()
            .into_iter()
            .filter(|a| a.message_id != gone);
        let damaged = LoopbackServer::start(kept).await.unwrap();
        let engine = engine(dir.path(), vec![server("a", &damaged, 0)]).await;
        let sample = engine
            .quick_check(&release.key, articles, Some(index))
            .await
            .unwrap();
        assert_eq!(
            sample,
            HealthSample { checked, missing },
            "{articles} articles with {gone} gone"
        );
        assert_eq!(
            damaged.body_commands(),
            0,
            "a quick check never fetches bodies"
        );
        let stats = &engine.stats()[0];
        let attempts = |w: kob_usenet::vendor::nntp::Workload| {
            stats.workloads[w as usize].attempts.iter().sum::<u64>()
        };
        assert!(attempts(kob_usenet::vendor::nntp::Workload::Warm) > 0);
        assert_eq!(attempts(kob_usenet::vendor::nntp::Workload::Health), 0);
    }
}

#[tokio::test]
async fn sixty_four_articles_check_in_about_a_second_at_200ms_rtt() {
    use kob_usenet::vendor::testkit::{Impairment, post};
    let mut data = vec![7_u8; 128 * 16 * 1024];
    data[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
    let articles = post("q", "Quick.2026.mkv", &data, 16 * 1024);
    let plain = LoopbackServer::start(articles.clone()).await.unwrap();
    let slow = LoopbackServer::start_impaired(
        articles,
        Impairment {
            response_delay: std::time::Duration::from_millis(200),
            ..Impairment::default()
        },
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let nzb = crate::support::single_nzb("q", "Quick.2026.mkv", 128, 16 * 1024);
    let release = {
        let engine = engine(dir.path(), vec![server("a", &plain, 0)]).await;
        import(&engine, &nzb, None).await
    };
    let engine = engine(
        dir.path(),
        vec![kob_usenet::ServerConfig {
            connections: 16,
            ..server("a", &slow, 0)
        }],
    )
    .await;
    let started = std::time::Instant::now();
    let sample = engine.quick_check(&release.key, 64, None).await.unwrap();
    let took = started.elapsed();
    assert_eq!((sample.checked, sample.missing), (64, 0));
    assert!(
        took < std::time::Duration::from_millis(1500),
        "took {took:?}"
    );
}
