use std::time::Duration;

use kob_usenet::vendor::testkit::post;
use kob_usenet::{ImportMode, Purpose};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::rig::{Loaded, media, single_nzb};

const CONNECTIONS: usize = 4;
const DEPTH: usize = 4;
const ARTICLE: usize = 64 * 1024;
const LIBRARY_ARTICLES: usize = 200;
const HELD_FROM: usize = 90;
const FILM_ARTICLES: usize = 16;

async fn settled(rig: &Loaded) {
    let mut seen = rig.nntp.body_commands();
    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let now = rig.nntp.body_commands();
        if now == seen {
            return;
        }
        seen = now;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seek_is_admitted_while_a_download_holds_every_slot() {
    let _serial = crate::rig::serial().await;
    let film = post(
        "film",
        "Film.2026.mkv",
        &media(FILM_ARTICLES * ARTICLE, 239),
        ARTICLE,
    );
    let rig = Loaded::sized(film, ARTICLE, LIBRARY_ARTICLES).await;
    let engine = rig
        .engine_configured(CONNECTIONS, |config| config.pipeline_depth = DEPTH)
        .await;
    let film = engine
        .import(
            Some(&single_nzb("film", "Film.2026.mkv", FILM_ARTICLES, ARTICLE)),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    rig.start_download(&engine).await;
    let _held: Vec<_> = (HELD_FROM..LIBRARY_ARTICLES)
        .map(|part| rig.nntp.hold_article(&format!("lib-{part}@loopback")))
        .collect();
    settled(&rig).await;

    let at = (FILM_ARTICLES as u64 / 2) * ARTICLE as u64;
    let mut reader = engine.open(&film.key, 0, Purpose::Play).unwrap();
    reader.set_end(at + ARTICLE as u64);
    reader.seek(std::io::SeekFrom::Start(at)).await.unwrap();
    let mut buf = vec![0; ARTICLE];
    tokio::time::timeout(Duration::from_secs(3), reader.read_exact(&mut buf))
        .await
        .expect("the seek waited for a slot only a finished download request could free")
        .unwrap();
    assert!(rig.nntp.peak_connections() <= CONNECTIONS);
}
