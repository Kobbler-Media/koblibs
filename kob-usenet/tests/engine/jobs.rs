use std::time::Duration;

use kob_usenet::ImportMode;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::{Fixture, POSTED, engine, quoted, server};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_never_waits_behind_another_imports_stalled_reply() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let (arrived, release) = nntp.hold_article(&fixture.files[1].articles[0].message_id);
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let (held, free) = (
        fixture.nzb(&[1], quoted, POSTED),
        fixture.nzb(&[0], quoted, POSTED),
    );

    let stalled = engine.import(Some(&held), None, "held", None, ImportMode::Stream);
    let other = async {
        arrived.await.unwrap();
        let other = tokio::time::timeout(
            Duration::from_secs(3),
            engine.import(Some(&free), None, "free", None, ImportMode::Stream),
        )
        .await;
        release.send(()).unwrap();
        other
    };
    let (stalled, other) = tokio::join!(stalled, other);
    other
        .expect("the other import finished while the first one's reply was held")
        .unwrap();
    stalled.unwrap();
}
