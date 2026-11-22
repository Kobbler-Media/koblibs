use std::io::SeekFrom;

use kob_usenet::Purpose;
use kob_usenet::vendor::testkit::LoopbackServer;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::support::{ARTICLE, Fixture, engine, import, output, server};

#[tokio::test]
async fn a_bounded_range_fetches_nothing_past_its_last_segment() {
    let fixture = Fixture::season();
    let name = "Show.S01E01.mkv";
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let release = import(&engine, &fixture.standard_nzb(), None).await;
    let index = output(&release, name);
    let (from, last) = (2 * ARTICLE + 5, 4 * ARTICLE + 100);

    let before = nntp.body_commands();
    let mut reader = engine.open(&release.key, index, Purpose::Play).unwrap();
    reader.set_end(last as u64 + 1);
    reader.seek(SeekFrom::Start(from as u64)).await.unwrap();
    let mut range = vec![0; last + 1 - from];
    reader.read_exact(&mut range).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(range == fixture.data(name)[from..=last]);
    assert_eq!(nntp.body_commands() - before, 3, "segments 2 to 4 only");
}
