use kob_usenet::Purpose;
use kob_usenet::vendor::testkit::LoopbackServer;
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, output, server};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfiguring_never_holds_more_connections_than_the_account_allows() {
    let fixture = Fixture::season();
    for change in ["rename", "password", "connections"] {
        let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        let read = |path| {
            let mut reader = engine
                .open(&release.key, output(&release, path), Purpose::Play)
                .unwrap();
            async move {
                let mut all = Vec::new();
                reader.read_to_end(&mut all).await.unwrap();
                all
            }
        };
        assert!(read("Show.S01E01.mkv").await == fixture.data("Show.S01E01.mkv"));
        assert!(nntp.peak_connections() <= 4, "{change}");
        let mut changed = server("a", &nntp, 0);
        match change {
            "rename" => changed.name = "renamed".into(),
            "password" => changed.password = "rotated".into(),
            _ => changed.connections = 3,
        }
        engine.reconfigure(vec![changed]).unwrap();
        assert!(read("Show.S01E02.mkv").await == fixture.data("Show.S01E02.mkv"));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            nntp.peak_connections() <= 4,
            "{change}: {} over the account cap",
            nntp.peak_connections()
        );
        let name = if change == "rename" {
            "renamed"
        } else {
            "server a"
        };
        assert_eq!(engine.stats()[0].name, name);
    }
}
