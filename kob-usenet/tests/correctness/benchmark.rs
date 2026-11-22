use kob_usenet::vendor::testkit::Article;
use md5::{Digest, Md5};

use crate::harness::{
    Rig, imported, output, paths, read_checked, read_whole, refused_or_absent, served,
};
use crate::nzb::{self, Listed};
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::rar::{self, Member};

const OBFUSCATED: &str = "9f86d081884c7d659a2feaa0c55ad015";

fn film() -> Posted {
    Posted::new("film", "Toy.Story.5.2026.mkv", video(20 * ARTICLE + 333, 1))
}

fn season(episodes: usize) -> Vec<Posted> {
    (1..=episodes)
        .flat_map(|e| {
            let name = format!("Bosch.S01E{e:02}");
            [
                Posted::new(
                    &format!("e{e}"),
                    &format!("{name}.mkv"),
                    video(3 * ARTICLE + e * 97, e as u64),
                ),
                Posted::new(
                    &format!("s{e}"),
                    &format!("{name}.srt"),
                    noise(1500 + e, 100 + e as u64),
                ),
            ]
        })
        .collect()
}

struct Volumes {
    film: Vec<u8>,
    posts: Vec<Posted>,
    spans: Vec<usize>,
}

fn rar_film() -> Volumes {
    let film = video(15 * ARTICLE + 4321, 3);
    let set = rar::stored(
        &[Member {
            name: "Father.Brown.S01E01.mkv",
            data: &film,
        }],
        4 * ARTICLE,
    );
    let spans = set.pieces[0].iter().scan(0, |at, p| {
        *at += p.len;
        Some(*at)
    });
    let spans = spans.collect();
    let count = set.volumes.len();
    let posts = set
        .volumes
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            Posted::new(
                &format!("fb{i}"),
                &rar::volume_name("Father.Brown.S01E01", i, count),
                v,
            )
        })
        .collect();
    Volumes { film, posts, spans }
}

#[tokio::test]
async fn plain_video_reads_whole_and_at_every_seek() {
    let film = film();
    let rig = Rig::new(vec![film.articles.clone()]).await;
    let release = rig.import(&nzb::of(&[&film])).await.unwrap();
    served(&rig.engine, &release, &film.name, &film.data).await;
    let index = output(&release, &film.name).unwrap();
    for (at, len) in [
        (ARTICLE - 10, 20),
        (7 * ARTICLE + 5, 3 * ARTICLE),
        (film.data.len() - 100, 100),
    ] {
        read_checked(&rig.engine, &release.key, index, &film.data, at, len)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn damaged_partial_serves_around_the_hole_and_refuses_it() {
    let film = film();
    let hole = 7;
    let rig = Rig::new(vec![
        film.articles
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != hole)
            .map(|(_, a)| a.clone())
            .collect(),
    ])
    .await;
    let release = rig.import(&nzb::of(&[&film])).await.unwrap();
    let index = output(&release, &film.name).unwrap();
    let (engine, key, data) = (&rig.engine, &release.key, &film.data);
    read_checked(engine, key, index, data, 0, hole * ARTICLE)
        .await
        .unwrap();
    let after = (hole + 1) * ARTICLE;
    read_checked(engine, key, index, data, after, data.len() - after)
        .await
        .unwrap();
    assert!(
        read_checked(engine, key, index, data, hole * ARTICLE + 100, 200)
            .await
            .is_err(),
        "a read inside the missing article must fail"
    );
    assert!(
        read_whole(engine, key, index, data).await.is_err(),
        "a read across the missing article must fail"
    );
}

#[tokio::test]
async fn obfuscated_names_without_extensions_are_served_by_content() {
    let film = film();
    let hidden = Posted::new("obf", OBFUSCATED, film.data.clone());
    let rig = Rig::new(vec![hidden.articles.clone()]).await;
    let release = rig.import(&nzb::of(&[&hidden])).await.unwrap();
    let video = release
        .outputs
        .iter()
        .find(|o| o.size == film.data.len() as u64)
        .unwrap_or_else(|| panic!("no output of the film's size in {:?}", paths(&release)));
    read_whole(&rig.engine, &release.key, video.index, &film.data)
        .await
        .unwrap();
}

#[tokio::test]
async fn rar_stored_member_spanning_volumes_reads_whole_and_across_each_boundary() {
    let set = rar_film();
    assert!(set.posts.len() >= 4);
    let rig = Rig::new(vec![articles(&set.posts)]).await;
    let release = rig
        .import(&nzb::of(&set.posts.iter().collect::<Vec<_>>()))
        .await
        .unwrap();
    let name = "Father.Brown.S01E01.mkv";
    served(&rig.engine, &release, name, &set.film).await;
    let index = output(&release, name).unwrap();
    for &boundary in &set.spans[..set.spans.len() - 1] {
        read_checked(
            &rig.engine,
            &release.key,
            index,
            &set.film,
            boundary - 50,
            100,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn season_pack_lists_every_file_at_its_size_and_serves_them() {
    let posts = season(10);
    let rig = Rig::new(vec![articles(&posts)]).await;
    let release = rig
        .import(&nzb::of(&posts.iter().collect::<Vec<_>>()))
        .await
        .unwrap();
    assert_eq!(release.outputs.len(), posts.len(), "{:?}", paths(&release));
    for post in &posts {
        let index = output(&release, &post.name)
            .unwrap_or_else(|| panic!("{} missing from {:?}", post.name, paths(&release)));
        assert_eq!(
            release.outputs[index].size,
            post.data.len() as u64,
            "{}",
            post.name
        );
        read_checked(&rig.engine, &release.key, index, &post.data, 0, 1024)
            .await
            .unwrap();
    }
    served(&rig.engine, &release, &posts[8].name, &posts[8].data).await;
}

#[tokio::test]
async fn dead_post_is_refused() {
    let posts = season(2);
    let dead: Vec<Article> = articles(&posts)
        .into_iter()
        .map(|a| Article {
            message_id: dead_id(&a.message_id),
            wire: a.wire,
        })
        .collect();
    let rig = Rig::new(vec![articles(&posts)]).await;
    let mut next = dead.iter();
    let listed: Vec<Listed<'_>> = posts
        .iter()
        .map(|p| Listed {
            subject: format!("\"{}\" yEnc (1/{})", p.name, p.articles.len()),
            segments: (1..=p.articles.len())
                .map(|n| (n, next.next().unwrap()))
                .collect(),
        })
        .collect();
    let Some(release) = imported(rig.import(&nzb::nzb(&listed)).await, "dead post") else {
        return;
    };
    for post in &posts {
        refused_or_absent(&rig.engine, &release, &post.name, &post.data).await;
    }
}

#[tokio::test]
async fn incomplete_archive_set_is_refused_not_padded() {
    let set = rar_film();
    let rig = Rig::new(vec![articles(&set.posts)]).await;
    let kept: Vec<&Posted> = set
        .posts
        .iter()
        .filter(|p| !p.name.ends_with(".part03.rar"))
        .collect();
    assert_eq!(kept.len(), set.posts.len() - 1);
    let Some(release) = imported(rig.import(&nzb::of(&kept)).await, "incomplete set") else {
        return;
    };
    refused_or_absent(&rig.engine, &release, "Father.Brown.S01E01.mkv", &set.film).await;
}

#[tokio::test]
async fn damaged_severe_is_refused() {
    let film = film();
    let rig = Rig::new(vec![film.articles.clone()]).await;
    let mut listed = film.listed();
    listed.segments.retain(|(n, _)| (n - 1) % 3 == 0);
    let Some(release) = imported(rig.import(&nzb::nzb(&[listed])).await, "damaged severe") else {
        return;
    };
    refused_or_absent(&rig.engine, &release, &film.name, &film.data).await;
    if let Some(index) = output(&release, &film.name) {
        for part in [1, 2, 4, 5] {
            let at = part * ARTICLE + 10;
            assert!(
                read_checked(&rig.engine, &release.key, index, &film.data, at, 100)
                    .await
                    .is_err(),
                "article {part} is not in the NZB, so a read inside it must fail"
            );
        }
    }
}

fn dead_id(id: &str) -> String {
    format!(
        "{:032x}@dead.invalid",
        u128::from_be_bytes(Md5::digest(id).into())
    )
}
