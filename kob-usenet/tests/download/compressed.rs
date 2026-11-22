use crate::nzb;
use crate::post::Posted;
use crate::repair::Protected;
use crate::support::{Rig, completed, crc_of, extractor_on_path, finish, listing};

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
