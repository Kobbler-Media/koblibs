use kob_usenet::Purpose;
use kob_usenet::vendor::nntp::Workload;
use tokio::io::AsyncReadExt;

use crate::harness::Rig;
use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, video};
use crate::rar::{self, Member};

#[tokio::test]
async fn warming_an_archive_member_fetches_on_the_warm_workload() {
    let film = video(7 * ARTICLE + 99, 71);
    let set = rar::stored(
        &[Member {
            name: "Show.S01E07.mkv",
            data: &film,
        }],
        3 * ARTICLE,
    );
    let count = set.volumes.len();
    let posts: Vec<Posted> = set
        .volumes
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            Posted::new(
                &format!("w{i}"),
                &rar::volume_name("Show.S01E07", i, count),
                v,
            )
        })
        .collect();
    let posts: Vec<&Posted> = posts.iter().collect();
    let rig = Rig::new(vec![articles(posts.iter().copied())]).await;
    let release = rig.import(&nzb::of(&posts)).await.unwrap();
    let out = release
        .outputs
        .iter()
        .find(|o| o.path.ends_with(".mkv"))
        .unwrap();
    let before = rig.engine.stats()[0].workloads.clone();
    let mut all = Vec::new();
    rig.engine
        .open(&release.key, out.index, Purpose::Warm)
        .unwrap()
        .read_to_end(&mut all)
        .await
        .unwrap();
    assert!(all == film);
    let after = &rig.engine.stats()[0].workloads;
    let asked = |w: Workload| {
        after[w as usize].attempts.iter().sum::<u64>()
            - before[w as usize].attempts.iter().sum::<u64>()
    };
    assert!(asked(Workload::Warm) > 0);
    assert_eq!(asked(Workload::Health), 0);
}
