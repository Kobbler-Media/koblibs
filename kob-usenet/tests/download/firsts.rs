use kob_usenet::ServerConfig;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::compressed::{FIXTURE_FILES, RAR4};
use crate::nzb;
use crate::post::{ARTICLE, Posted, noise};
use crate::support::{Rig, completed, crc_of, extractor_on_path, finish};

#[tokio::test]
async fn whole_volume_mode_takes_the_first_article_whose_part_and_total_match() {
    if !extractor_on_path() {
        return;
    }
    let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
    let decoy = Posted::new("decoy", "Other.bin", noise(3 * ARTICLE, 121));
    assert_ne!(decoy.articles.len(), rar.articles.len());
    let mut swapped = rar.articles.clone();
    swapped[0].wire = decoy.articles[0].wire.clone();
    let rig = Rig::new(swapped).await;
    let right = LoopbackServer::start(rar.articles.clone()).await.unwrap();
    let engine = rig
        .engine_with(|c| {
            let first = c.servers[0].clone();
            c.servers.push(ServerConfig {
                id: "b".into(),
                name: "server b".into(),
                port: right.addr().port(),
                priority: 1,
                ..first
            });
        })
        .await;
    let spec = rig.spec("Release", nzb::of(&[&rar]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for (name, len, crc) in FIXTURE_FILES {
        assert_eq!(crc_of(&dir, name), (len, crc), "{name}");
    }
}
