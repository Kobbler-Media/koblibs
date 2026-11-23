use md5::{Digest, Md5};

use crate::compressed::{FIXTURE_FILES, RAR4};
use crate::nzb;
use crate::post::{ARTICLE, Posted, noise, video};
use crate::repair::Protected;
use crate::support::{Rig, assert_file, completed, crc_of, extractor_on_path, finish, listing};

pub fn hash(name: &str) -> String {
    format!("{:032x}", u128::from_be_bytes(Md5::digest(name).into()))
}

#[tokio::test]
async fn obfuscated_files_with_par2_land_under_their_real_names() {
    let real = [
        ("Show.S01E01.mkv", video(5 * ARTICLE + 123, 81)),
        ("Show.S01E01.en.srt", noise(20 * 1024, 83)),
    ];
    let named = Protected::new(
        real.iter()
            .enumerate()
            .map(|(i, (name, data))| Posted::new(&format!("real{i}"), name, data.clone()))
            .collect(),
        "Show",
        &[(0, 2)],
    );
    let hidden: Vec<Posted> = real
        .iter()
        .enumerate()
        .map(|(i, (name, data))| Posted::new(&format!("obf{i}"), &hash(name), data.clone()))
        .collect();
    let index = Posted::new(
        "idx",
        &format!("{}.par2", hash("Show")),
        named.index.data.clone(),
    );
    let mut posts: Vec<&Posted> = hidden.iter().collect();
    posts.push(&index);
    let rig = Rig::new(crate::post::articles(posts.iter().copied())).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Show.S01E01", nzb::of(&posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_eq!(listing(&dir), ["Show.S01E01.en.srt", "Show.S01E01.mkv"]);
    for (name, data) in &real {
        assert_file(&dir, name, data);
    }
}

#[tokio::test]
async fn whole_volume_downloads_name_obfuscated_files_by_par2() {
    if !extractor_on_path() {
        return;
    }
    let sample = video(2 * ARTICLE + 9, 84);
    let named = Protected::new(
        vec![
            Posted::new("real0", "Release.rar", RAR4.to_vec()),
            Posted::new("real1", "Release.sample.mkv", sample.clone()),
        ],
        "Release",
        &[],
    );
    let hidden = [
        Posted::new("obf0", &hash("Release.rar"), RAR4.to_vec()),
        Posted::new("obf1", &hash("Release.sample.mkv"), sample.clone()),
    ];
    let index = Posted::new(
        "idx",
        &format!("{}.par2", hash("Release")),
        named.index.data.clone(),
    );
    let posts = [&hidden[0], &hidden[1], &index];
    let rig = Rig::new(crate::post::articles(posts)).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(&posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, "Release.sample.mkv", &sample);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
    assert!(
        !listing(&dir).iter().any(|n| n.len() == 32),
        "{:?}",
        listing(&dir)
    );
}

#[tokio::test]
async fn a_scrambled_video_named_after_a_hostile_or_slashed_release_stays_inside_its_folder() {
    for (release, file) in [
        ("a/../../escape", "a_.._.._escape.mkv"),
        ("x/../../../../escape", "x_.._.._.._.._escape.mkv"),
        ("AC/DC Live", "AC_DC Live.mkv"),
    ] {
        let data = video(3 * ARTICLE + 17, 85);
        let post = Posted::new("obf", &format!("{}.mkv", hash(release)), data.clone());
        let rig = Rig::new(post.articles.clone()).await;
        let engine = rig.engine().await;
        let mut spec = rig.spec(release, nzb::of(&[&post]), None);
        spec.id = "scrambled".to_owned();
        engine.add_job(spec).await.unwrap();

        let dir = completed(&finish(&engine, "scrambled").await);
        assert_eq!(listing(&dir), [file], "{release}");
        assert_file(&dir, file, &data);
        let folder = dir.strip_prefix(rig.dest.path()).unwrap();
        assert!(
            listing(rig.dest.path())
                .iter()
                .all(|p| std::path::Path::new(p).starts_with(folder)),
            "{release}: {:?}",
            listing(rig.dest.path())
        );
    }
}
