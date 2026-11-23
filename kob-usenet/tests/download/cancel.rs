use std::time::{Duration, Instant};

use crate::post::{ARTICLE, Posted, noise, video};
use crate::support::{Rig, articles_without, assert_file, completed, finish, par2_set};

const SLICE: usize = 4096;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_job_stops_its_repair_and_only_then_frees_its_slot() {
    let big = Posted::new("big", "Big.mkv", noise(768 * ARTICLE, 61));
    let sets = par2_set(&[&big], "B", SLICE, 30);
    let next = Posted::new("next", "Next.mkv", video(2 * ARTICLE, 62));
    let gone: Vec<String> = (0..128).map(|i| big.id(i * 6).to_owned()).collect();
    let gone: Vec<&str> = gone.iter().map(String::as_str).collect();
    let mut posts = vec![&big];
    posts.extend(&sets);
    let mut articles = articles_without(&posts, &gone);
    articles.extend(next.articles.iter().cloned());
    let rig = Rig::new(articles).await;
    let engine = rig.engine_with(|c| c.download.active_jobs = 1).await;
    let spec = rig.spec("Big", crate::nzb::of(&posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let started = Instant::now();
    while engine.repairs_running() == 0 {
        assert!(started.elapsed() < Duration::from_secs(60), "no repair ran");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    engine.remove_job(&id, true).await.unwrap();
    let removed = Instant::now();
    while engine.repairs_running() > 0 {
        assert!(
            removed.elapsed() < Duration::from_secs(2),
            "the repair kept running after its job was removed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let spec = rig.spec("Next", crate::nzb::of(&[&next]), None);
    let next_id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let dir = completed(&finish(&engine, &next_id).await);
    assert_file(&dir, &next.name, &next.data);
}
