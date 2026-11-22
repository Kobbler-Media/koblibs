use std::time::Duration;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::rar::{self, Member};
use crate::support::{
    Rig, assert_file, completed, extractor_on_path, finish, listing, script, sevenzip,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_during_unpack_finishes_unpacking_without_zero_filled_volumes() {
    if !extractor_on_path() {
        return;
    }
    let film = video(3 * ARTICLE + 11, 101);
    let inner = rar::stored(
        &[Member {
            name: "Movie.2026.mkv",
            data: &film,
        }],
        100 * ARTICLE,
    );
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("Movie.2026.rar"), &inner.volumes[0]).unwrap();
    sevenzip(
        work.path(),
        &["a", "-t7z", "-mx=9", "outer.7z", "Movie.2026.rar"],
    );
    let post = Posted::new(
        "outer",
        "Movie.Pack.7z",
        std::fs::read(work.path().join("outer.7z")).unwrap(),
    );
    let rig = Rig::new(post.articles.clone()).await;
    let mark = work.path().join("hung");
    let hanging = script(
        work.path(),
        "7z-hangs-on-rar",
        &format!(
            "case \"$*\" in *.rar*) 7z \"$@\"; touch '{}'; exec sleep 60;; esac\nexec 7z \"$@\"",
            mark.display()
        ),
    );
    let spec = rig.spec("Movie", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    {
        let engine = rig
            .engine_with(|c| {
                c.download.unrar = "/nonexistent/unrar".into();
                c.download.sevenzip = hanging.clone();
            })
            .await;
        engine.add_job(spec).await.unwrap();
        while !mark.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    rig.count_bodies(&post.articles);

    let engine = rig.engine().await;
    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, "Movie.2026.mkv", &film);
    assert_eq!(listing(&dir), ["Movie.2026.mkv"]);
    assert_eq!(rig.bodies(&post.articles), 0, "nothing is downloaded again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_after_the_final_move_reports_the_moved_folder() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(6 * ARTICLE + 5, 102));
    let rig = Rig::new(post.articles.clone()).await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    let dir = {
        let engine = rig.engine().await;
        engine.add_job(spec).await.unwrap();
        let queued = std::fs::read(rig.record("status")).unwrap();
        let dir = completed(&finish(&engine, &id).await);
        std::fs::write(rig.record("status"), queued).unwrap();
        dir
    };
    rig.count_bodies(&post.articles);

    let engine = rig.engine().await;
    let status = finish(&engine, &id).await;
    assert_eq!(completed(&status), dir);
    assert_file(&dir, &post.name, &post.data);
    assert_eq!(listing(rig.dest.path()), [format!("Show/{}", post.name)]);
    assert_eq!(rig.bodies(&post.articles), 0, "nothing is downloaded again");
}
