use std::time::Duration;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish, listing};

pub fn marked_done(rig: &Rig) -> usize {
    listing(rig.data.path())
        .iter()
        .filter(|path| path.ends_with("map-0"))
        .map(|path| std::fs::read(rig.data.path().join(path)).unwrap())
        .map(|map| map.iter().filter(|&&mark| mark == 1).count())
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segments_are_marked_done_on_disk_only_after_their_data_is_synced() {
    let post = Posted::new("ep", "Show.S01E03.mkv", video(30 * ARTICLE + 1, 63));
    let rig = Rig::new(post.articles.clone()).await;
    let (arrived, release) = rig.nntp.hold_article(post.id(15));
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    arrived.await.unwrap();
    while engine.job(&id).unwrap().segments_done < 10 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        marked_done(&rig),
        0,
        "no batch sync ran yet, so no segment may be marked done on disk"
    );
    drop(release);
    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &post.name, &post.data);
    assert_eq!(marked_done(&rig), 31);
}
