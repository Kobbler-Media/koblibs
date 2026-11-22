use kob_usenet::Purpose;
use kob_usenet::vendor::nntp::Workload;
use kob_usenet::vendor::testkit::LoopbackServer;
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, output, read_at, server};

#[tokio::test]
async fn a_new_engine_on_the_same_data_dir_resumes_a_read_without_importing() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let expected = fixture.data("Show.S01E01.mkv");
    let at = 50_000;

    let (key, index) = {
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        let index = output(&release, "Show.S01E01.mkv");
        let mut reader = engine.open(&release.key, index, Purpose::Play).unwrap();
        assert_eq!(reader.len(), expected.len() as u64);
        assert_eq!(read_at(&mut reader, 0, at).await, expected[..at]);
        (release.key, index)
    };

    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let mut reader = engine.open(&key, index, Purpose::Play).unwrap();
    let mut rest = read_at(&mut reader, at as u64, 0).await;
    reader.read_to_end(&mut rest).await.unwrap();
    assert!(rest == expected[at..], "resumed bytes match");

    for stats in engine.stats() {
        for workload in [Workload::Queue, Workload::Health] {
            let w = &stats.workloads[workload as usize];
            assert_eq!(w.attempts.iter().sum::<u64>(), 0, "{workload:?}");
        }
    }
    assert_eq!(
        engine.release(&key).unwrap().outputs[index].path,
        "Show.S01E01.mkv"
    );
}
