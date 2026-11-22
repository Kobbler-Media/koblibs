use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::repair::Protected;
use crate::support::{Rig, failed, finish};

#[tokio::test]
async fn damage_beyond_par2_capacity_fails_from_the_stat_probe_before_the_bulk_is_fetched() {
    let release = Protected::new(
        vec![
            Posted::new("e01", "Show.S01E01.mkv", video(12 * ARTICLE + 500, 41)),
            Posted::new("e02", "Show.S01E02.mkv", video(12 * ARTICLE + 7, 42)),
        ],
        "Show",
        &[(0, 2)],
    );
    let gone: Vec<String> = release
        .files
        .iter()
        .flat_map(|f| (2..10).map(|part| f.id(part).to_owned()))
        .collect();
    let rig =
        Rig::new(release.articles_without(&gone.iter().map(String::as_str).collect::<Vec<_>>()))
            .await;
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let error = failed(&finish(&engine, &id).await);
    assert!(error.contains("hopeless"), "{error}");
    let data_articles: usize = release.files.iter().map(|f| f.articles.len()).sum();
    assert!(
        rig.nntp.body_commands() < data_articles / 2,
        "{} BODY requests: the probe must fail the job before the download",
        rig.nntp.body_commands()
    );
    assert!(!rig.dest.path().join("Show").exists());
}
