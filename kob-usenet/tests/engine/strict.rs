use kob_usenet::Purpose;
use kob_usenet::vendor::testkit::LoopbackServer;
use tokio::io::AsyncReadExt;

use crate::support::{ARTICLE, Fixture, engine, import, output, server};

#[tokio::test]
async fn every_purpose_refuses_a_gap_instead_of_serving_filler() {
    let fixture = Fixture::season();
    let name = "Show.S01E01.mkv";
    let missing = fixture.files[1].articles[5].message_id.clone();
    let nntp = LoopbackServer::start(fixture.without(&missing))
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let release = import(&engine, &fixture.standard_nzb(), None).await;
    let index = output(&release, name);

    for purpose in [Purpose::Play, Purpose::Cache, Purpose::Warm] {
        let mut reader = engine.open(&release.key, index, purpose).unwrap();
        let mut read = Vec::new();
        let result = reader.read_to_end(&mut read).await;
        assert!(result.is_err(), "{purpose:?} read {} bytes", read.len());
        assert!(read.len() <= 5 * ARTICLE, "{purpose:?}");
        assert!(read == fixture.data(name)[..read.len()], "{purpose:?}");
    }
}
