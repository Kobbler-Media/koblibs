use std::time::Duration;

use kob_usenet::ImportMode;
use kob_usenet::vendor::testkit::{LoopbackServer, post};

use crate::support::{ARTICLE, Fixture, POSTED, Posted, engine, quoted, server};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_never_waits_behind_another_imports_stalled_reply() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let (arrived, release) = nntp.hold_article(&fixture.files[1].articles[0].message_id);
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let (held, free) = (
        fixture.nzb(&[1], quoted, POSTED),
        fixture.nzb(&[0], quoted, POSTED),
    );

    let stalled = engine.import(Some(&held), None, "held", None, ImportMode::Stream);
    let other = async {
        arrived.await.unwrap();
        let other = tokio::time::timeout(
            Duration::from_secs(3),
            engine.import(Some(&free), None, "free", None, ImportMode::Stream),
        )
        .await;
        release.send(()).unwrap();
        other
    };
    let (stalled, other) = tokio::join!(stalled, other);
    other
        .expect("the other import finished while the first one's reply was held")
        .unwrap();
    stalled.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_play_import_never_queues_behind_prewarm_imports() {
    let fixture = Fixture::season();
    let warming: Vec<Posted> = (0..6)
        .map(|i| {
            let mut data = vec![i as u8; 2 * ARTICLE];
            data[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
            let name: &'static str = Box::leak(format!("Warm.{i}.mkv").into_boxed_str());
            Posted {
                name,
                articles: post(&format!("warm{i}"), name, &data, ARTICLE),
                data,
            }
        })
        .collect();
    let mut articles = fixture.articles();
    articles.extend(warming.iter().flat_map(|w| w.articles.clone()));
    let nntp = LoopbackServer::start(articles).await.unwrap();
    let held: Vec<_> = warming
        .iter()
        .map(|w| nntp.hold_article(&w.articles[0].message_id))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let prewarm = Fixture { files: warming };
    let nzbs: Vec<Vec<u8>> = (0..prewarm.files.len())
        .map(|i| prewarm.nzb(&[i], quoted, POSTED))
        .collect();

    let warm = futures_util::future::join_all(
        nzbs.iter()
            .map(|nzb| engine.import(Some(nzb), None, "warm", None, ImportMode::Prewarm)),
    );
    let play = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let played = tokio::time::timeout(
            Duration::from_secs(3),
            engine.import(
                Some(&fixture.standard_nzb()),
                None,
                "play",
                None,
                ImportMode::Stream,
            ),
        )
        .await;
        for (_, release) in held {
            let _ = release.send(());
        }
        played
    };
    let (warmed, played) = tokio::join!(warm, play);
    played
        .expect("the play import finished while prewarm imports were held")
        .unwrap();
    assert!(warmed.iter().all(Result::is_ok));
}
