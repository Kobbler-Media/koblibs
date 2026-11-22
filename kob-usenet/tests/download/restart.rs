use std::time::Duration;

use crate::durable::marked_done;
use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_engine_resumes_the_job_without_refetching_written_segments() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(30 * ARTICLE + 99, 9));
    let rig = Rig::new(post.articles.clone()).await;
    let (arrived, release) = rig.nntp.hold_article(post.id(15));
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();

    let done_at_kill = {
        let engine = rig.engine().await;
        engine.add_job(spec).await.unwrap();
        arrived.await.unwrap();
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let done = engine.job(&id).unwrap().segments_done;
            if done > 0 && marked_done(&rig) as u64 == done {
                break done;
            }
        }
    };
    drop(release);
    while rig.nntp.open_connections() > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        done_at_kill > 0,
        "some segments were written before the kill"
    );
    rig.count_bodies(&post.articles);

    let engine = rig.engine().await;
    let status = finish(&engine, &id).await;
    let dir = completed(&status);
    assert_file(&dir, &post.name, &post.data);
    assert_eq!(status.segments_total, 31);
    assert_eq!(
        rig.bodies(&post.articles) as u64,
        status.segments_total - done_at_kill,
        "only segments the first run had durably written are not fetched again"
    );
}
