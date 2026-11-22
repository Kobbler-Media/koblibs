use crate::nzb;
use crate::post::Posted;
use crate::post::{ARTICLE, video};
use crate::rar::{self, Member};
use crate::repair::Protected;
use crate::support::{Rig, assert_file, completed, crc_of, extractor_on_path, finish, listing};

pub const RAR4: &[u8] = include_bytes!("../vendor/fixtures/archive/Rar.rar");

pub const FIXTURE_FILES: [(&str, usize, u32); 2] = [
    ("exe/test.exe", 45_056, 0xCFB1_09C8),
    ("jpg/test.jpg", 40_372, 0x0888_14E3),
];

#[tokio::test]
async fn a_compressed_rar_is_downloaded_whole_and_unpacked_by_the_external_tool() {
    if !extractor_on_path() {
        return;
    }
    let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
    let rig = Rig::new(rar.articles.clone()).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(&[&rar]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
    assert!(
        !listing(&dir).iter().any(|n| n.ends_with(".rar")),
        "volumes are removed after unpacking: {:?}",
        listing(&dir)
    );
}

#[tokio::test]
async fn a_damaged_compressed_rar_is_repaired_from_par2_before_it_is_unpacked() {
    if !extractor_on_path() {
        return;
    }
    let release = Protected::new(
        vec![Posted::new("rar", "Release.rar", RAR4.to_vec())],
        "Release",
        &[(0, 8)],
    );
    let gone = release.files[0].id(1).to_owned();
    let rig = Rig::new(release.articles_without(&[&gone])).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
}

#[tokio::test]
async fn a_volume_whose_first_article_is_gone_everywhere_is_rebuilt_from_par2() {
    if !extractor_on_path() {
        return;
    }
    let release = Protected::new(
        vec![Posted::new("rar", "Release.rar", RAR4.to_vec())],
        "Release",
        &[(0, 8)],
    );
    let gone = release.files[0].id(0).to_owned();
    let rig = Rig::new(release.articles_without(&[&gone])).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
}

#[tokio::test]
async fn an_archive_inside_an_unpacked_archive_is_unpacked_too() {
    if !extractor_on_path() {
        return;
    }
    let film = video(3 * ARTICLE + 11, 81);
    let inner = rar::stored(
        &[Member {
            name: "Movie.2026.mkv",
            data: &film,
        }],
        100 * ARTICLE,
    );
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("Movie.2026.rar"), &inner.volumes[0]).unwrap();
    let packed = std::process::Command::new("7z")
        .args(["a", "-t7z", "-mx=9", "outer.7z", "Movie.2026.rar"])
        .current_dir(work.path())
        .output()
        .unwrap();
    assert!(packed.status.success(), "{packed:?}");
    let outer = std::fs::read(work.path().join("outer.7z")).unwrap();
    let post = Posted::new("outer", "Movie.Pack.7z", outer);
    let rig = Rig::new(post.articles.clone()).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Movie", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, "Movie.2026.mkv", &film);
    assert_eq!(listing(&dir), ["Movie.2026.mkv"]);
}
