use kob_usenet::Purpose;
use kob_usenet::vendor::testkit::LoopbackServer;
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, server};

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
