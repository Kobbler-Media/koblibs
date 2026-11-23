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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_keeps_members_nested_under_reserved_folder_names() {
    if !extractor_on_path() {
        return;
    }
    let members = [
        ("Sub/.extract-x/keep.bin", 111),
        ("Sub/.unpacked-y/held.bin", 112),
        ("Sub/Deep/.extract-z/.unpacked-w/deep.bin", 113),
    ];
    let work = tempfile::tempdir().unwrap();
    for (path, seed) in members {
        let at = work.path().join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, video(ARTICLE + 7, seed)).unwrap();
    }
    sevenzip(work.path(), &["a", "-t7z", "-mx=1", "a.7z", "Sub"]);
    std::fs::write(work.path().join("later.bin"), video(ARTICLE, 114)).unwrap();
    sevenzip(work.path(), &["a", "-t7z", "-mx=1", "b.7z", "later.bin"]);
    let read = |name: &str| std::fs::read(work.path().join(name)).unwrap();
    let (a, b) = (
        Posted::new("a", "a.7z", read("a.7z")),
        Posted::new("b", "b.7z", read("b.7z")),
    );
    let rig = Rig::new(a.articles.iter().chain(&b.articles).cloned().collect()).await;
    let mark = work.path().join("hung");
    let hanging = script(
        work.path(),
        "7z-hangs-on-b",
        &format!(
            "case \"$*\" in *b.7z*) touch '{}'; exec sleep 60;; esac\nexec 7z \"$@\"",
            mark.display()
        ),
    );
    let spec = rig.spec("Nested", nzb::of(&[&a, &b]), None);
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

    let engine = rig.engine().await;
    let dir = completed(&finish(&engine, &id).await);
    let names = listing(&dir);
    for (path, seed) in members {
        let leaf = path.rsplit('/').next().unwrap();
        let found = names
            .iter()
            .find(|n| n.ends_with(&format!("/{leaf}")))
            .unwrap_or_else(|| panic!("{leaf} was lost: {names:?}"));
        assert_eq!(
            std::fs::read(dir.join(found)).unwrap(),
            video(ARTICLE + 7, seed),
            "{found}"
        );
    }
}
