use md5::{Digest, Md5};

use crate::harness::{Rig, paths, served};
use crate::nzb;
use crate::par2;
use crate::post::{ARTICLE, Posted, articles, noise, video};

fn hash(name: &str) -> String {
    format!("{:032x}", u128::from_be_bytes(Md5::digest(name).into()))
}

#[tokio::test]
async fn par2_names_obfuscated_outputs_before_the_first_read() {
    let real: Vec<(&str, Vec<u8>)> = vec![
        ("Show.S01E01.mkv", video(5 * ARTICLE + 123, 81)),
        ("Show.S01E02.mkv", video(4 * ARTICLE + 456, 82)),
        ("Show.S01E01.en.srt", noise(20 * 1024, 83)),
    ];
    let hidden: Vec<Posted> = real
        .iter()
        .enumerate()
        .map(|(i, (name, data))| Posted::new(&format!("obf{i}"), &hash(name), data.clone()))
        .collect();
    let index = par2::index(
        &real
            .iter()
            .map(|(n, d)| (*n, d.as_slice()))
            .collect::<Vec<_>>(),
    );
    let set = hash("Show.S01");
    let par2 = Posted::new("par2", &format!("{set}.par2"), index);
    let mut posts: Vec<&Posted> = hidden.iter().collect();
    posts.push(&par2);
    let rig = Rig::new(vec![articles(posts.iter().copied())]).await;
    let release = rig.import(&nzb::of(&posts)).await.unwrap();
    let bodies = rig.servers[0].body_commands();

    let mut names = paths(&release);
    names.sort_unstable();
    assert_eq!(
        names,
        ["Show.S01E01.en.srt", "Show.S01E01.mkv", "Show.S01E02.mkv"]
    );
    assert_eq!(rig.engine.release(&release.key).unwrap(), release);
    assert_eq!(
        rig.servers[0].body_commands(),
        bodies,
        "naming must not need a read"
    );

    for (name, data) in &real {
        served(&rig.engine, &release, name, data).await;
    }
}
