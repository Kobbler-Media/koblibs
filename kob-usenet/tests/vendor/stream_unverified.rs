#![allow(clippy::unwrap_used)] // Test helpers outside `#[test]` fns; panics are the assertions.

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::nntp::{
    BodyExpectation, Client, Downloads, Endpoint, Pool, PoolConfig, Provider, TlsMode, Workload,
};
use kob_usenet::vendor::stream::{Layout, Segment, SegmentReader};
use kob_usenet::vendor::testkit::{Corpus, LoopbackServer};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const SIZE: usize = 2 << 20;

#[tokio::test]
async fn only_playback_reads_a_large_article_before_it_is_verified() {
    let corpus = Corpus::generate(1025, 3, SIZE);
    let target = &corpus.articles[1];
    for workload in Workload::ALL {
        let server = LoopbackServer::start(corpus.articles.clone())
            .await
            .unwrap();
        let (_, release) = server.hold_article_after(&target.message_id, target.wire.len() / 2);
        let mut reader = SegmentReader::new(client(&server), measured(&corpus), 2)
            .with_workload(workload)
            .strict(true);
        reader.seek(SeekFrom::Start(SIZE as u64)).await.unwrap();

        let mut head = vec![0; 64 * 1024];
        let early = tokio::time::timeout(
            Duration::from_millis(if workload == Workload::Stream {
                3000
            } else {
                400
            }),
            reader.read_exact(&mut head),
        )
        .await;
        assert_eq!(early.is_ok(), workload == Workload::Stream, "{workload:?}");
        release.send(()).unwrap();
        if early.is_err() {
            reader.read_exact(&mut head).await.unwrap();
        }
        assert_eq!(head, corpus.data[SIZE..SIZE + head.len()], "{workload:?}");
    }
}

#[tokio::test]
async fn a_streamed_article_keeps_its_last_byte_and_the_range_end_until_verified() {
    let corpus = Corpus::generate(1025, 3, SIZE);
    let target = &corpus.articles[1];
    let range_end = SIZE + SIZE / 2;
    for (end, held) in [
        (corpus.data.len(), 2 * SIZE - 1),
        (range_end, range_end - 1),
    ] {
        let server = LoopbackServer::start(corpus.articles.clone())
            .await
            .unwrap();
        let (_, release) = server.hold_article_after(&target.message_id, target.wire.len() - 40);
        let mut reader = SegmentReader::new(client(&server), measured(&corpus), 2)
            .with_workload(Workload::Stream)
            .strict(true);
        reader.set_end(end as u64);
        reader.seek(SeekFrom::Start(SIZE as u64)).await.unwrap();

        let mut got = vec![0; held - SIZE];
        reader.read_exact(&mut got).await.unwrap();
        let last = tokio::time::timeout(Duration::from_millis(400), reader.read(&mut [0; 1])).await;
        assert!(last.is_err(), "the byte before {end} waits for the CRC");
        release.send(()).unwrap();
        let mut byte = [0; 1];
        reader.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte[0], corpus.data[held]);
    }
}

fn client(server: &LoopbackServer) -> Arc<Client> {
    let config = PoolConfig {
        max_connections: 4,
        pipeline_depth: 4,
        idle_timeout: Duration::from_secs(5),
        min_connections: 0,
    };
    let endpoint = Endpoint::new(
        "127.0.0.1",
        server.addr().port(),
        TlsMode::Plain,
        None,
        Duration::from_secs(5),
    )
    .unwrap();
    Arc::new(Client::new([Provider {
        pool: Pool::new(endpoint, config, Downloads::new(32)).unwrap(),
        key: "p0".to_owned(),
        name: String::new(),
        priority: 0,
        backbone: None,
    }]))
}

/// The corpus's layout with the geometry an import measures.
fn measured(corpus: &Corpus) -> Arc<Layout> {
    Arc::new(Layout::new(
        corpus
            .articles
            .iter()
            .enumerate()
            .map(|(i, a)| Segment {
                message_id: a.message_id.clone(),
                size: SIZE as u64,
                expected: BodyExpectation {
                    offset: Some((i * SIZE) as u64),
                    decoded_len: Some(SIZE as u64),
                    file_size: Some(corpus.data.len() as u64),
                    ..BodyExpectation::NONE
                },
            })
            .collect(),
    ))
}
