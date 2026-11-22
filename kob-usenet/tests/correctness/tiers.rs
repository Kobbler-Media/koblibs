use kob_usenet::{ImportMode, Purpose};
use tokio::io::AsyncReadExt;

use crate::harness::Rig;
use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::rar::{self, Member};

#[tokio::test]
async fn the_priority_99_tier_gets_no_body_on_healthy_posts() {
    let loose = [
        Posted::new("t1", "Show.S01E01.mkv", video(7 * ARTICLE + 1, 91)),
        Posted::new("t2", "Show.S01E01.srt", noise(3000, 92)),
    ];
    let film = video(10 * ARTICLE + 9, 93);
    let set = rar::stored(
        &[Member {
            name: "Show.S01E02.mkv",
            data: &film,
        }],
        4 * ARTICLE,
    );
    let count = set.volumes.len();
    let volumes: Vec<Posted> = set
        .volumes
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            Posted::new(
                &format!("tv{i}"),
                &rar::volume_name("Show.S01E02", i, count),
                v,
            )
        })
        .collect();
    let posts: Vec<&Posted> = loose.iter().chain(&volumes).collect();
    let healthy = articles(posts.iter().copied());
    let rig = Rig::new(vec![healthy.clone(), healthy]).await;

    for mode in [ImportMode::Download, ImportMode::Stream] {
        let release = rig
            .engine
            .import(Some(&nzb::of(&posts)), None, "Show.S01", None, mode)
            .await
            .unwrap();
        assert_eq!(release.outputs.len(), 3);
        for out in &release.outputs {
            for purpose in [Purpose::Play, Purpose::Cache, Purpose::Warm] {
                let mut all = Vec::new();
                rig.engine
                    .open(&release.key, out.index, purpose)
                    .unwrap()
                    .read_to_end(&mut all)
                    .await
                    .unwrap();
                assert_eq!(all.len() as u64, out.size, "{}", out.path);
            }
        }
    }
    assert!(rig.servers[0].body_commands() > 0);
    assert_eq!(
        rig.servers[1].body_commands(),
        0,
        "priority 99 server got BODY"
    );
    let backup = rig
        .engine
        .stats()
        .into_iter()
        .find(|s| s.key == "p1")
        .unwrap();
    assert_eq!(backup.priority, 99);
    let asked: u64 = backup
        .workloads
        .iter()
        .flat_map(|w| w.attempts.iter())
        .sum();
    assert_eq!(asked, 0, "priority 99 server was asked {asked} times");
}
