use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

#[tokio::test]
async fn the_queue_pauses_below_the_free_space_floor_and_resumes_when_space_returns() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(8 * ARTICLE + 1, 51));
    let rig = Rig::new(post.articles.clone()).await;
    rig.space.0.store(1000, Ordering::SeqCst);
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(engine.paused_for_space());
    let paused = engine.job(&id).unwrap();
    assert_eq!(paused.segments_done, 0, "{paused:?}");

    rig.space.0.store(u64::MAX, Ordering::SeqCst);
    let dir = completed(&finish(&engine, &id).await);
    assert!(!engine.paused_for_space());
    assert_file(&dir, &post.name, &post.data);
}
