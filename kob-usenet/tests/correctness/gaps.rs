use kob_usenet::vendor::testkit::Article;
use kob_usenet::{JobState, JobStatus};

use crate::jobs::{Rig, assert_file, completed, crc_of, extractor_on_path, finish};
use crate::nzb::{self, Listed};
use crate::par2::{self, Set};
use crate::post::{ARTICLE, Posted, articles, noise};

const RAR4: &[u8] = include_bytes!("../fixtures/Rar.no-exe.rar");
const MEMBERS: [(&str, usize, u32); 2] = [
    ("exe/test.bin", 45_056, 0xCFB1_09C8),
    ("jpg/test.jpg", 40_372, 0x0888_14E3),
];

fn unlisting(post: &Posted, number: usize) -> Listed<'_> {
    let mut listed = post.listed();
    listed.segments.retain(|&(n, _)| n != number);
    listed
}

#[tokio::test]
async fn a_compressed_rar_whose_nzb_skips_a_segment_entry_is_repaired_from_par2() {
    if !extractor_on_path() {
        return;
    }
    let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
    assert!(rar.articles.len() > 2);
    let set = Set::new(&[(rar.name.as_str(), rar.data.as_slice())]);
    let index = Posted::new("idx", "Release.par2", set.index());
    let volume = Posted::new("vol", &par2::volume_name("Release", 0, 8), set.volume(0, 8));
    for unlisted in [2, 1] {
        let rig = Rig::new(articles([&rar, &index, &volume])).await;
        let engine = rig.engine().await;
        let listing = nzb::nzb(&[unlisting(&rar, unlisted), index.listed(), volume.listed()]);
        let spec = rig.spec("Release", listing, None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        let status = finish(&engine, &id).await;
        let dir = completed(&status);
        for (name, len, crc) in MEMBERS {
            assert_eq!(
                crc_of(&dir, name),
                (len, crc),
                "segment {unlisted} unlisted: {name}"
            );
        }
    }
}

struct Mismatch {
    rar: Posted,
    index: Posted,
    volume: Posted,
    phantom: Article,
}

impl Mismatch {
    fn new(index_name: &str, volume_name: &str) -> Self {
        let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
        let set = Set::new(&[(rar.name.as_str(), rar.data.as_slice())]);
        Self {
            index: Posted::new("idx", index_name, set.index()),
            volume: Posted::new("vol", volume_name, set.volume(0, 8)),
            phantom: Article {
                message_id: "phantom@loopback".to_owned(),
                wire: vec![b'x'; ARTICLE],
            },
            rar,
        }
    }

    fn with_phantom<'a>(&'a self, post: &'a Posted) -> Listed<'a> {
        let mut listed = post.listed();
        listed
            .segments
            .push((post.articles.len() + 1, &self.phantom));
        listed
    }

    fn without_last(&self) -> Listed<'_> {
        unlisting(&self.rar, self.rar.articles.len())
    }
}

async fn download(served: Vec<Article>, listing: Vec<u8>) -> (Rig, JobStatus) {
    let rig = Rig::new(served).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Release", listing, None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let status = finish(&engine, &id).await;
    (rig, status)
}

fn unpacked(status: &JobStatus, case: &str) {
    let dir = completed(status);
    for (name, len, crc) in MEMBERS {
        assert_eq!(crc_of(&dir, name), (len, crc), "{case}: {name}");
    }
}

#[tokio::test]
async fn a_volume_listed_with_another_segment_count_is_taken_once_par2_proves_it() {
    if !extractor_on_path() {
        return;
    }
    let m = Mismatch::new("Release.par2", &par2::volume_name("Release", 0, 8));
    let all = || articles([&m.rar, &m.index, &m.volume]);
    for (case, listed) in [
        ("last entry unlisted", m.without_last()),
        ("a listed entry past the post", m.with_phantom(&m.rar)),
    ] {
        let listing = nzb::nzb(&[listed, m.index.listed(), m.volume.listed()]);
        let (_rig, status) = download(all(), listing).await;
        unpacked(&status, case);
    }
}

#[tokio::test]
async fn an_obfuscated_par2_volume_listed_with_another_segment_count_is_still_found() {
    if !extractor_on_path() {
        return;
    }
    let m = Mismatch::new("Release.par2", "0123456789abcdef0123456789abcdef");
    let gone = m.rar.id(1).to_owned();
    let served: Vec<_> = articles([&m.rar, &m.index, &m.volume])
        .into_iter()
        .filter(|a| a.message_id != gone)
        .collect();
    let listing = nzb::nzb(&[m.rar.listed(), m.index.listed(), m.with_phantom(&m.volume)]);
    let (_rig, status) = download(served, listing).await;
    unpacked(&status, "obfuscated volume");
}

#[tokio::test]
async fn another_files_part_one_is_never_taken_for_a_volume_par2_does_not_describe() {
    if !extractor_on_path() {
        return;
    }
    let m = Mismatch::new("Release.par2", &par2::volume_name("Release", 0, 8));
    let extra = Posted::new("extra", "Extra.rar", RAR4.to_vec());
    let decoy = Posted::new("decoy", "Other.bin", noise(3 * ARTICLE, 121));
    assert_ne!(decoy.articles.len(), extra.articles.len());
    let mut served = articles([&m.rar, &extra, &m.index, &m.volume]);
    let first = served
        .iter_mut()
        .find(|a| a.message_id == extra.id(0))
        .unwrap();
    first.wire = decoy.articles[0].wire.clone();
    let listing = nzb::nzb(&[
        m.rar.listed(),
        extra.listed(),
        m.index.listed(),
        m.volume.listed(),
    ]);
    let (_rig, status) = download(served, listing).await;
    assert_eq!(status.state, JobState::Failed, "{status:?}");
    let reason = status.error.as_deref().unwrap_or_default();
    assert!(reason.contains("Extra.rar"), "{reason}");
}

#[tokio::test]
async fn another_files_part_one_under_a_described_volume_is_rebuilt_from_par2() {
    if !extractor_on_path() {
        return;
    }
    let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
    let extra = Posted::new("extra", "Extra.bin", noise(4 * ARTICLE, 77));
    let decoy = Posted::new("decoy", "Other.bin", noise(3 * ARTICLE, 121));
    let set = Set::new(&[
        (rar.name.as_str(), rar.data.as_slice()),
        (extra.name.as_str(), extra.data.as_slice()),
    ]);
    let index = Posted::new("idx", "Release.par2", set.index());
    let volume = Posted::new("vol", &par2::volume_name("Release", 0, 8), set.volume(0, 8));
    let mut served = articles([&rar, &extra, &index, &volume]);
    let first = served
        .iter_mut()
        .find(|a| a.message_id == extra.id(0))
        .unwrap();
    first.wire = decoy.articles[0].wire.clone();
    let listing = nzb::of(&[&rar, &extra, &index, &volume]);
    let (_rig, status) = download(served, listing).await;
    unpacked(&status, "decoy over a described volume");
    assert_file(&status.path, "Extra.bin", &extra.data);
}
