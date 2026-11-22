use std::collections::HashMap;
use std::ops::Range;

use kob_usenet::vendor::testkit::LoopbackServer;

use crate::harness::{engine, import, output, paths, served, server};
use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::proxy::BodyCounter;
use crate::rar::{self, Member};

const TARGETS: [&str; 2] = ["Show.S01E05.mkv", "Show.S01E03E04.mkv"];

fn episodes() -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = [1, 2, 5]
        .iter()
        .map(|&e| {
            (
                format!("Show.S01E{e:02}.mkv"),
                video(6 * ARTICLE + 911 * e, e as u64),
            )
        })
        .collect();
    files.insert(
        2,
        (
            "Show.S01E03E04.mkv".to_owned(),
            video(11 * ARTICLE + 77, 34),
        ),
    );
    files.push(("Show.S01E05.srt".to_owned(), noise(2345, 55)));
    files.push(("Show.S01E06.mkv".to_owned(), video(5 * ARTICLE + 3, 6)));
    files
}

struct Fetched {
    member: u64,
    import: u64,
    read_inside: u64,
    read_outside: u64,
    import_outside: u64,
}

async fn measure(
    posts: &[Posted],
    inside: &HashMap<String, bool>,
    target: &str,
    truth: &[u8],
) -> Fetched {
    let nntp = LoopbackServer::start(articles(posts)).await.unwrap();
    let counter = BodyCounter::start(nntp.addr()).await;
    let (engine, _dir) = engine(vec![server("a", counter.addr(), 0)]).await;
    let release = import(&engine, &nzb::of(&posts.iter().collect::<Vec<_>>()))
        .await
        .unwrap();
    for name in ["Show.S01E03E04.mkv", "Show.S01E05.srt", "Show.S01E05.mkv"] {
        assert!(
            output(&release, name).is_some(),
            "{name} missing from {:?}",
            paths(&release)
        );
    }
    let imported = counter.take();
    served(&engine, &release, target, truth).await;
    let read = counter.take();
    let sizes: HashMap<&str, u64> = posts
        .iter()
        .flat_map(|p| {
            p.articles
                .iter()
                .enumerate()
                .map(|(i, a)| (a.message_id.as_str(), p.range(i).len() as u64))
        })
        .collect();
    let bytes = |counts: &HashMap<String, usize>, want: bool| -> u64 {
        counts
            .iter()
            .filter(|(id, _)| inside[id.as_str()] == want)
            .map(|(id, &n)| sizes[id.as_str()] * n as u64)
            .sum()
    };
    Fetched {
        member: truth.len() as u64,
        import: bytes(&imported, true) + bytes(&imported, false),
        import_outside: bytes(&imported, false),
        read_inside: bytes(&read, true),
        read_outside: bytes(&read, false),
    }
}

fn assert_within(shape: &str, target: &str, f: &Fetched) {
    let outside = f.import_outside + f.read_outside;
    eprintln!(
        "{shape}: member {} B, import fetched {} B ({} B outside), read fetched {} B inside and {} B outside",
        f.member, f.import, f.import_outside, f.read_inside, f.read_outside
    );
    assert!(
        outside <= f.import + f.member / 100,
        "{shape}: {outside} B outside {target} exceeds import overhead {} B + 1%",
        f.import
    );
    assert!(
        f.read_outside <= f.member / 100,
        "{shape}: reading {target} fetched {} B of other members",
        f.read_outside
    );
}

fn overlaps(article: Range<usize>, pieces: &[Range<usize>]) -> bool {
    pieces
        .iter()
        .any(|p| p.start < article.end && article.start < p.end)
}

#[tokio::test]
async fn streaming_one_episode_of_a_stored_rar_pack_fetches_only_that_member() {
    for name in TARGETS {
        stored_pack(name).await;
    }
}

async fn stored_pack(name: &str) {
    let files = episodes();
    let members: Vec<Member<'_>> = files
        .iter()
        .map(|(name, data)| Member { name, data })
        .collect();
    let set = rar::stored(&members, 9 * ARTICLE);
    let target = files.iter().position(|(n, _)| n == name).unwrap();
    let count = set.volumes.len();
    assert!(count >= 4);
    let posts: Vec<Posted> = set
        .volumes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Posted::new(
                &format!("pack{i}"),
                &rar::volume_name("Show.S01", i, count),
                v.clone(),
            )
        })
        .collect();
    let mut inside = HashMap::new();
    for (v, post) in posts.iter().enumerate() {
        let pieces: Vec<Range<usize>> = set.pieces[target]
            .iter()
            .filter(|p| p.volume == v)
            .map(|p| p.at..p.at + p.len)
            .collect();
        for (i, a) in post.articles.iter().enumerate() {
            inside.insert(a.message_id.clone(), overlaps(post.range(i), &pieces));
        }
    }
    let fetched = measure(&posts, &inside, name, &files[target].1).await;
    assert_within("stored RAR pack", name, &fetched);
}

#[tokio::test]
async fn streaming_one_episode_of_a_loose_pack_fetches_only_that_file() {
    for name in TARGETS {
        loose_pack(name).await;
    }
}

async fn loose_pack(name: &str) {
    let posts: Vec<Posted> = episodes()
        .into_iter()
        .enumerate()
        .map(|(i, (name, data))| Posted::new(&format!("loose{i}"), &name, data))
        .collect();
    let inside = posts
        .iter()
        .flat_map(|p| {
            p.articles
                .iter()
                .map(|a| (a.message_id.clone(), p.name == name))
        })
        .collect();
    let truth = posts.iter().find(|p| p.name == name).unwrap().data.clone();
    let fetched = measure(&posts, &inside, name, &truth).await;
    assert_within("loose pack", name, &fetched);
}
