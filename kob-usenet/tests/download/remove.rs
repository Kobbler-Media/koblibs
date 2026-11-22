use std::time::Duration;

use kob_usenet::{Error, JobState};

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, completed, finish};

#[tokio::test]
async fn removing_jobs_cancels_them_and_deletes_their_files_on_request() {
    let done = Posted::new("done", "Done.mkv", video(3 * ARTICLE, 71));
    let held = Posted::new("held", "Held.mkv", video(3 * ARTICLE, 72));
    let rig = Rig::new(
        done.articles
            .iter()
            .chain(&held.articles)
            .cloned()
            .collect(),
    )
    .await;
    let (arrived, _release) = rig.nntp.hold_article(held.id(1));
    let engine = rig.engine().await;
    let first = rig.spec("Done", nzb::of(&[&done]), None);
    let second = rig.spec("Held", nzb::of(&[&held]), None);
    let (first_id, second_id) = (first.id.clone(), second.id.clone());
    engine.add_job(first.clone()).await.unwrap();
    engine.add_job(second).await.unwrap();
    assert!(matches!(
        engine.add_job(first).await,
        Err(Error::DuplicateJob(id)) if id == first_id
    ));

    let dir = completed(&finish(&engine, &first_id).await);
    arrived.await.unwrap();
    assert_eq!(engine.job(&second_id).unwrap().state, JobState::Downloading);

    engine.remove_job(&second_id, true).await.unwrap();
    engine.remove_job(&first_id, false).await.unwrap();
    assert!(engine.jobs().is_empty());
    assert!(dir.join("Done.mkv").exists(), "kept without delete_files");
    assert!(!rig.dest.path().join(".incomplete").join("Held").exists());
    assert!(matches!(
        engine.remove_job(&first_id, false).await,
        Err(Error::UnknownJob(_))
    ));

    drop(engine);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rig.engine().await.jobs().is_empty(), "removal is persisted");
}
