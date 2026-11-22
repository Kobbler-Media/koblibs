use crate::compressed::{FIXTURE_FILES, RAR4};
use crate::nzb;
use crate::obfuscated::hash;
use crate::post::{ARTICLE, Posted, articles, video};
use crate::rar::{self, Member};
use crate::repair::Protected;
use crate::support::{
    Rig, assert_file, completed, crc_of, extractor_on_path, finish, listing, unrar_on_path,
};

fn hidden(protected: &Protected) -> Vec<Posted> {
    protected
        .volumes
        .iter()
        .chain([&protected.index])
        .map(|p| {
            Posted::new(
                &format!("h{}", p.articles[0].message_id),
                &hash(&p.name),
                p.data.clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn par2_files_with_obfuscated_names_are_found_by_content_and_repair_the_release() {
    let release = Protected::new(
        vec![
            Posted::new("e01", "Show.S01E01.mkv", video(12 * ARTICLE + 500, 91)),
            Posted::new("e02", "Show.S01E02.mkv", video(10 * ARTICLE + 33, 92)),
        ],
        "Show",
        &[(0, 2), (2, 8)],
    );
    let par2 = hidden(&release);
    let gone = release.files[0].id(4).to_owned();
    let posts: Vec<&Posted> = release.files.iter().chain(&par2).collect();
    let rig = Rig::new(
        articles(posts.iter().copied())
            .into_iter()
            .filter(|a| a.message_id != gone)
            .collect(),
    )
    .await;
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for file in &release.files {
        assert_file(&dir, &file.name, &file.data);
    }
}

#[tokio::test]
async fn whole_volume_mode_never_downloads_obfuscated_par2_as_data() {
    if !extractor_on_path() {
        return;
    }
    let release = Protected::new(
        vec![Posted::new("rar", "Release.rar", RAR4.to_vec())],
        "Release",
        &[(0, 8)],
    );
    let par2 = hidden(&release);
    let gone = release.files[0].id(1).to_owned();
    let posts: Vec<&Posted> = release.files.iter().chain(&par2).collect();
    let rig = Rig::new(
        articles(posts.iter().copied())
            .into_iter()
            .filter(|a| a.message_id != gone)
            .collect(),
    )
    .await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(&posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
    assert!(
        !listing(&dir).iter().any(|n| n.len() == 32),
        "no PAR2 volume is kept as data: {:?}",
        listing(&dir)
    );
}

#[tokio::test]
async fn clustered_misses_inside_slices_larger_than_articles_are_not_called_hopeless() {
    let slice = 4 * ARTICLE;
    let release = Protected::sliced(
        vec![Posted::new(
            "e01",
            "Show.S01E01.mkv",
            video(24 * ARTICLE, 93),
        )],
        "Show",
        &[(0, 3)],
        slice,
    );
    let gone: Vec<String> = (4..12)
        .map(|part| release.files[0].id(part).to_owned())
        .collect();
    let rig =
        Rig::new(release.articles_without(&gone.iter().map(String::as_str).collect::<Vec<_>>()))
            .await;
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &release.files[0].name, &release.files[0].data);
}

#[tokio::test]
async fn a_member_missing_articles_switches_to_whole_volumes_before_downloading_anything_twice() {
    if !unrar_on_path() {
        return;
    }
    let member = video(30 * ARTICLE + 77, 94);
    let set = rar::stored(
        &[Member {
            name: "Movie.2020.mkv",
            data: &member,
        }],
        10 * ARTICLE,
    );
    let count = set.volumes.len();
    let volumes: Vec<Posted> = set
        .volumes
        .into_iter()
        .enumerate()
        .map(|(i, data)| Posted::new(&format!("v{i}"), &rar::volume_name("Movie", i, count), data))
        .collect();
    let release = Protected::new(volumes, "Movie", &[(0, 16)]);
    let gone = release.files[1].id(3).to_owned();
    let rig = Rig::new(release.articles_without(&[&gone])).await;
    let data: Vec<_> = release
        .files
        .iter()
        .flat_map(|f| f.articles.iter().cloned())
        .collect();
    rig.count_bodies(&data);
    let engine = rig.engine().await;
    let spec = rig.spec("Movie", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, "Movie.2020.mkv", &member);
    let fetched = rig.bodies(&data);
    assert!(
        fetched < data.len() * 3 / 2,
        "{fetched} BODY requests for {} volume articles: the member was downloaded and then every volume again",
        data.len()
    );
}
