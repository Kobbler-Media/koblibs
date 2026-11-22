use kob_usenet::vendor::testkit::Article;
use kob_usenet::vendor::yenc::crc32;

use crate::harness::{Rig, imported, output, read_whole, refused_or_absent, served};
use crate::nzb;
use crate::post::{ARTICLE, Posted, noise, video, wire};

#[derive(Clone, Copy, Debug)]
enum Damage {
    Missing,
    Crc,
    Truncated,
    Short,
    OtherPart,
    OtherFile,
}

#[derive(Clone, Copy, Debug)]
enum Spread {
    Primary,
    Everywhere,
}

struct Post {
    film: Posted,
    other: Posted,
}

impl Post {
    fn new() -> Self {
        Self {
            film: Posted::new("film", "Movie.2026.mkv", video(8 * ARTICLE + 500, 21)),
            other: Posted::new(
                "other",
                "Movie.2026.sample.mkv",
                noise(6 * ARTICLE + 100, 22),
            ),
        }
    }

    fn damaged(&self, part: usize, damage: Damage) -> Option<Article> {
        let film = &self.film;
        let id = film.id(part).to_owned();
        let payload = &film.data[film.range(part)];
        let header = film.header(part);
        let wire = match damage {
            Damage::Missing => return None,
            Damage::Crc => {
                let mut flipped = payload.to_vec();
                flipped[payload.len() / 2] ^= 0x55;
                wire(header, &flipped, crc32(0, payload))
            }
            Damage::Truncated => {
                let full = &film.articles[part].wire;
                let cut = full[full.len() / 2..]
                    .windows(2)
                    .position(|w| w == b"\r\n")
                    .unwrap()
                    + full.len() / 2;
                [&full[..cut + 2], b".\r\n".as_slice()].concat()
            }
            Damage::Short => {
                let short = &payload[..payload.len() - 1000];
                wire(header, short, crc32(0, short))
            }
            Damage::OtherPart => {
                let neighbour = if part + 1 < film.articles.len() {
                    part + 1
                } else {
                    part - 1
                };
                film.articles[neighbour].wire.clone()
            }
            Damage::OtherFile => self.other.articles[part.min(self.other.articles.len() - 1)]
                .wire
                .clone(),
        };
        Some(Article {
            message_id: id,
            wire,
        })
    }

    fn provider(&self, damaged: Option<(usize, Damage)>) -> Vec<Article> {
        let mut all: Vec<Article> = self.film.articles.clone();
        if let Some((part, damage)) = damaged {
            let id = self.film.id(part).to_owned();
            all.retain(|a| a.message_id != id);
            all.extend(self.damaged(part, damage));
        }
        all
    }
}

async fn case(damage: Damage, part: usize, spread: Spread) {
    let post = Post::new();
    let bad = post.provider(Some((part, damage)));
    let good = post.provider(None);
    let providers = match spread {
        Spread::Primary => vec![bad, good],
        Spread::Everywhere => vec![bad.clone(), bad],
    };
    let rig = Rig::new(providers).await;
    let film = &post.film;
    let release = rig.import(&nzb::of(&[film])).await;
    match spread {
        Spread::Primary => {
            let release =
                release.unwrap_or_else(|e| panic!("{damage:?} part {part}: import failed: {e}"));
            served(&rig.engine, &release, &film.name, &film.data).await;
        }
        Spread::Everywhere => {
            let Some(release) = imported(release, &format!("{damage:?} part {part}")) else {
                return;
            };
            refused_or_absent(&rig.engine, &release, &film.name, &film.data).await;
            if let Some(index) = output(&release, &film.name) {
                let before = part * ARTICLE;
                if before > 0 {
                    crate::harness::read_checked(
                        &rig.engine,
                        &release.key,
                        index,
                        &film.data,
                        0,
                        before,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("{damage:?} part {part}: intact start refused: {e}")
                    });
                }
                assert!(
                    read_whole(&rig.engine, &release.key, index, &film.data)
                        .await
                        .is_err(),
                    "{damage:?} part {part}"
                );
            }
        }
    }
}

macro_rules! cases {
    ($($name:ident: $damage:ident, $part:expr, $spread:ident;)*) => {
        $(
            #[tokio::test]
            async fn $name() {
                case(Damage::$damage, $part, Spread::$spread).await;
            }
        )*
    };
}

cases! {
    missing_first_on_primary_is_served_from_backup: Missing, 0, Primary;
    missing_middle_on_primary_is_served_from_backup: Missing, 4, Primary;
    missing_first_everywhere_is_refused: Missing, 0, Everywhere;
    missing_middle_everywhere_is_refused: Missing, 4, Everywhere;
    crc_mismatch_first_on_primary_is_served_from_backup: Crc, 0, Primary;
    crc_mismatch_middle_on_primary_is_served_from_backup: Crc, 4, Primary;
    crc_mismatch_first_everywhere_is_refused: Crc, 0, Everywhere;
    crc_mismatch_middle_everywhere_is_refused: Crc, 4, Everywhere;
    truncated_first_on_primary_is_served_from_backup: Truncated, 0, Primary;
    truncated_middle_on_primary_is_served_from_backup: Truncated, 4, Primary;
    truncated_first_everywhere_is_refused: Truncated, 0, Everywhere;
    truncated_middle_everywhere_is_refused: Truncated, 4, Everywhere;
    short_yenc_first_on_primary_is_served_from_backup: Short, 0, Primary;
    short_yenc_middle_on_primary_is_served_from_backup: Short, 4, Primary;
    short_yenc_first_everywhere_is_refused: Short, 0, Everywhere;
    short_yenc_middle_everywhere_is_refused: Short, 4, Everywhere;
    other_part_first_on_primary_is_served_from_backup: OtherPart, 0, Primary;
    other_part_middle_on_primary_is_served_from_backup: OtherPart, 4, Primary;
    other_part_first_everywhere_is_refused: OtherPart, 0, Everywhere;
    other_part_middle_everywhere_is_refused: OtherPart, 4, Everywhere;
    issue_1915_other_file_first_on_primary_is_served_from_backup: OtherFile, 0, Primary;
    other_file_middle_on_primary_is_served_from_backup: OtherFile, 4, Primary;
    issue_1915_other_file_first_everywhere_is_refused: OtherFile, 0, Everywhere;
    other_file_middle_everywhere_is_refused: OtherFile, 4, Everywhere;
}
