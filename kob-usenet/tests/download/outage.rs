use std::time::Duration;

use kob_usenet::{Engine, JobFailure, JobState};

use crate::durable::marked_done;
use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, failed_as, finish, waiting};

async fn queued_for_providers(engine: &kob_usenet::Engine, id: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = waiting(engine, id).await;
            assert_ne!(status.state, JobState::Failed, "{status:?}");
            let note = status.error.as_deref().unwrap_or_default();
            if status.state == JobState::Queued && note.contains("providers unavailable") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("job {id} was never requeued: {:?}", engine.job(id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_added_while_providers_are_down_waits_and_completes_once_they_return() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(6 * ARTICLE + 77, 61));
    let rig = Rig::new(post.articles.clone()).await;
    rig.nntp.refuse().await;
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    queued_for_providers(&engine, &id).await;
    rig.nntp.resume().await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &post.name, &post.data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outage_mid_download_requeues_the_job_and_it_resumes_from_its_bitmap() {
    let post = Posted::new("ep", "Show.S01E02.mkv", video(30 * ARTICLE + 5, 62));
    let rig = Rig::new(post.articles.clone()).await;
    let (arrived, release) = rig.nntp.hold_article(post.id(15));
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    let done_at_outage = {
        let engine = rig
            .engine_with(|c| c.download.straggler_after = Duration::from_secs(60))
            .await;
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
    rig.nntp.refuse().await;
    drop(release);
    while rig.nntp.open_connections() > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let engine = rig.engine().await;
    queued_for_providers(&engine, &id).await;
    rig.count_bodies(&post.articles);
    rig.nntp.resume().await.unwrap();

    let status = finish(&engine, &id).await;
    assert_file(&completed(&status), &post.name, &post.data);
    assert!(done_at_outage > 0);
    assert!(
        rig.bodies(&post.articles) as u64 == status.segments_total - done_at_outage,
        "segments written before the outage are not fetched again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn infrastructure_errors_retry_then_fail_as_infrastructure() {
    let post = Posted::new("ep", "Show.S01E03.mkv", video(3 * ARTICLE + 7, 63));
    for disk in [false, true] {
        let rig = Rig::new(post.articles.clone()).await;
        let mut config = rig.config();
        config.download.retry_attempts = 3;
        let mut spec = rig.spec("Show", nzb::of(&[&post]), None);
        if disk {
            let file = rig.dest.path().join("not-a-dir");
            std::fs::write(&file, b"").unwrap();
            spec.dest_dir = file;
        } else {
            rig.nntp.refuse().await;
        }
        let engine = Engine::new(config).await.unwrap();
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        let error = failed_as(&finish(&engine, &id).await, JobFailure::Infrastructure);
        let expected = if disk {
            "writing to disk"
        } else {
            "providers unavailable"
        };
        assert!(error.contains(expected), "{error}");
    }
}
