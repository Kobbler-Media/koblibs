use std::time::{Duration, Instant};

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

const STALL: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_straggler_article_is_reissued_so_the_tail_does_not_wait_on_it() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(20 * ARTICLE + 5, 61));
    let rig = Rig::new(post.articles.clone()).await;
    rig.nntp.stall_article(post.id(18), 1, STALL);
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();

    let started = Instant::now();
    engine.add_job(spec).await.unwrap();
    let dir = completed(&finish(&engine, &id).await);
    let took = started.elapsed();

    assert_file(&dir, &post.name, &post.data);
    assert_eq!(
        rig.nntp.stalled(post.id(18)),
        1,
        "the straggler was requested"
    );
    assert!(
        took < STALL / 3,
        "the tail waited on the straggler: {took:?}"
    );
}
