use std::time::Duration;

use crate::durable::marked_done;
use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish, listing};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jobs_sharing_a_name_stage_apart_and_removing_one_leaves_the_other_whole() {
    let one = Posted::new("one", "Show.S01E01.mkv", video(20 * ARTICLE + 7, 1));
    let two = Posted::new("two", "Show.S01E01.mkv", video(20 * ARTICLE + 7, 2));
    let rig = Rig::new(one.articles.iter().chain(&two.articles).cloned().collect()).await;
    let (one_arrived, _one_held) = rig.nntp.hold_article(one.id(3));
    let (two_arrived, two_held) = rig.nntp.hold_article(two.id(3));
    let engine = rig.engine().await;
    for (id, post) in [("one", &one), ("two", &two)] {
        let mut spec = rig.spec("Show", nzb::of(&[post]), None);
        spec.id = id.to_owned();
        engine.add_job(spec).await.unwrap();
    }
    one_arrived.await.unwrap();
    two_arrived.await.unwrap();
    while engine.job("two").unwrap().segments_done < 10 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.remove_job("one", false).await.unwrap();
    drop(two_held);

    let dir = completed(&finish(&engine, "two").await);
    assert_file(&dir, &two.name, &two.data);
    assert_eq!(
        listing(&rig.dest.path().join(".incomplete")),
        Vec::<String>::new(),
        "a removed job leaves no staging behind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_jobs_sharing_a_name_both_complete_byte_exact() {
    let one = Posted::new("one", "Show.S01E01.mkv", video(20 * ARTICLE + 7, 1));
    let two = Posted::new("two", "Show.S01E01.mkv", video(20 * ARTICLE + 7, 2));
    let rig = Rig::new(one.articles.iter().chain(&two.articles).cloned().collect()).await;
    let engine = rig.engine().await;
    for (id, post) in [("one", &one), ("two", &two)] {
        let mut spec = rig.spec("Show", nzb::of(&[post]), None);
        spec.id = id.to_owned();
        engine.add_job(spec).await.unwrap();
    }
    for (id, post) in [("one", &one), ("two", &two)] {
        let dir = completed(&finish(&engine, id).await);
        assert_file(&dir, &post.name, &post.data);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_staging_is_downloaded_again_instead_of_trusting_the_segment_map() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(30 * ARTICLE + 99, 9));
    let rig = Rig::new(post.articles.clone()).await;
    let (arrived, release) = rig.nntp.hold_article(post.id(15));
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    let staging = {
        let engine = rig.engine().await;
        let staging = engine.add_job(spec).await.unwrap().path;
        arrived.await.unwrap();
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let done = engine.job(&id).unwrap().segments_done;
            if done > 0 && marked_done(&rig) as u64 == done {
                break staging;
            }
        }
    };
    drop(release);
    while rig.nntp.open_connections() > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Dropping an engine aborts its job without waiting, so a blocking write can still be
    // removing its own temporary file during this walk.
    while staging.exists() {
        match std::fs::remove_dir_all(&staging) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("{e}"),
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }

    let engine = rig.engine().await;
    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &post.name, &post.data);
}
