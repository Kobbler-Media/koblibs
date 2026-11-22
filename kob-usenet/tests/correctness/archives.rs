use kob_usenet::Purpose;
use kob_usenet::vendor::testkit::Article;
use kob_usenet::vendor::yenc::crc32;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::harness::{Rig, imported, output, read_whole, refused_or_absent, served};
use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, video, wire};
use crate::rar::{self, Member};

// `7z a -mx0` of the two episodes `episode` makes: a stored 7z pack.
const PACK_7Z: &[u8] = include_bytes!("../fixtures/Show.S01.7z");

fn episode(e: u32, len: u32) -> Vec<u8> {
    let mut data: Vec<u8> = (0..len)
        .map(|i| ((i * 13 + e * 7 + i / 241) & 0xff) as u8)
        .collect();
    data[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
    data
}

#[tokio::test]
async fn each_member_of_a_stored_7z_pack_streams_exactly() {
    let pack = Posted::new("7z", "Show.S01.7z", PACK_7Z.to_vec());
    let rig = Rig::new(vec![pack.articles.clone()]).await;
    let release = rig.import(&nzb::of(&[&pack])).await.unwrap();
    served(
        &rig.engine,
        &release,
        "Show.S01E01.mkv",
        &episode(1, 30_000),
    )
    .await;
    served(
        &rig.engine,
        &release,
        "Show.S01E02.mkv",
        &episode(2, 26_000),
    )
    .await;
}

struct Pack {
    film: Vec<u8>,
    volumes: Vec<Posted>,
}

impl Pack {
    fn new() -> Self {
        let film = video(9 * ARTICLE + 321, 77);
        let set = rar::stored(
            &[Member {
                name: "Show.S01E08.mkv",
                data: &film,
            }],
            4 * ARTICLE,
        );
        let count = set.volumes.len();
        let volumes = set
            .volumes
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                Posted::new(
                    &format!("r{i}"),
                    &rar::volume_name("Show.S01E08", i, count),
                    v,
                )
            })
            .collect();
        Self { film, volumes }
    }

    fn healthy(&self) -> Vec<Article> {
        articles(&self.volumes)
    }

    fn crc_damaged(&self) -> Vec<Article> {
        let volume = &self.volumes[1];
        let part = 2;
        let mut flipped = volume.data[volume.range(part)].to_vec();
        let honest = crc32(0, &flipped);
        flipped[100] ^= 0x5A;
        let mut all = self.healthy();
        let id = volume.id(part);
        let at = all.iter().position(|a| a.message_id == id).unwrap();
        all[at].wire = wire(volume.header(part), &flipped, honest);
        all
    }

    fn posts(&self) -> Vec<&Posted> {
        self.volumes.iter().collect()
    }
}

#[tokio::test]
async fn a_damaged_article_inside_a_rar_member_is_served_from_backup_or_refused() {
    let pack = Pack::new();
    for everywhere in [false, true] {
        let backup = if everywhere {
            pack.crc_damaged()
        } else {
            pack.healthy()
        };
        let rig = Rig::new(vec![pack.crc_damaged(), backup]).await;
        let release = rig.import(&nzb::of(&pack.posts())).await;
        if everywhere {
            let Some(release) = imported(release, "damaged rar member") else {
                continue;
            };
            refused_or_absent(&rig.engine, &release, "Show.S01E08.mkv", &pack.film).await;
            for purpose in [Purpose::Cache, Purpose::Warm] {
                let index = output(&release, "Show.S01E08.mkv").unwrap();
                let mut all = Vec::new();
                let read = rig
                    .engine
                    .open(&release.key, index, purpose)
                    .unwrap()
                    .read_to_end(&mut all)
                    .await;
                assert!(read.is_err(), "{purpose:?} served a damaged member");
            }
        } else {
            let release = release.unwrap();
            served(&rig.engine, &release, "Show.S01E08.mkv", &pack.film).await;
        }
    }
}

#[tokio::test]
async fn set_end_on_a_rar_member_stops_read_ahead_at_the_end() {
    let pack = Pack::new();
    let rig = Rig::new(vec![pack.healthy()]).await;
    let release = rig.import(&nzb::of(&pack.posts())).await.unwrap();
    let index = output(&release, "Show.S01E08.mkv").unwrap();
    let before = rig.servers[0].body_commands();
    let mut reader = rig.engine.open(&release.key, index, Purpose::Play).unwrap();
    let end = 2 * ARTICLE as u64;
    reader.set_end(end);
    reader.seek(std::io::SeekFrom::Start(100)).await.unwrap();
    let mut got = vec![0; usize::try_from(end).unwrap() - 100];
    reader.read_exact(&mut got).await.unwrap();
    assert!(got == pack.film[100..usize::try_from(end).unwrap()]);
    drop(reader);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let fetched = rig.servers[0].body_commands() - before;
    assert!(
        fetched <= 4,
        "{fetched} articles fetched for two articles of member data"
    );
    read_whole(&rig.engine, &release.key, index, &pack.film)
        .await
        .unwrap();
}
