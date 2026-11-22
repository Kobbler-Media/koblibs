use kob_usenet::ReleaseKey;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::{Fixture, POSTED, bare, engine, import, server};

#[tokio::test]
async fn reencoded_nzbs_share_a_key_and_output_ordinals() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let standard = fixture.standard_nzb();
    let reencoded = fixture.nzb(&[2, 0, 1], bare, POSTED + 3600);

    let key = ReleaseKey::from_nzb(&standard).unwrap();
    assert_eq!(ReleaseKey::from_nzb(&reencoded).unwrap(), key);
    let hex = key.as_str().strip_prefix("nzb-").unwrap();
    assert!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(key.as_str().parse::<ReleaseKey>().unwrap(), key);
    assert!("nzb-../../etc".parse::<ReleaseKey>().is_err());

    let other = Fixture::season().nzb(&[0, 1], crate::support::quoted, POSTED);
    assert_ne!(ReleaseKey::from_nzb(&other).unwrap(), key);

    let mut releases = Vec::new();
    for nzb in [&standard, &reencoded] {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        releases.push(import(&engine, nzb, None).await);
    }
    assert_eq!(releases[0], releases[1]);
    assert_eq!(releases[0].key, key);
    let listed: Vec<(usize, &str, u64)> = releases[0]
        .outputs
        .iter()
        .map(|o| (o.index, o.path.as_str(), o.size))
        .collect();
    assert_eq!(
        listed,
        [
            (
                0,
                "Show.S01E01.mkv",
                fixture.data("Show.S01E01.mkv").len() as u64
            ),
            (1, "Show.S01E01.srt", 3000),
            (
                2,
                "Show.S01E02.mkv",
                fixture.data("Show.S01E02.mkv").len() as u64
            ),
        ]
    );
}
