use kob_usenet::{JobFailure, JobStatus};

use crate::nzb::{self, nzb_with};
use crate::obfuscated::hash;
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::rar::{self, Member};
use crate::repair::Protected;
use crate::support::{
    Rig, assert_file, completed, extractor_on_path, failed, failed_as, finish, listing, sevenzip,
};

const WITH_EXECUTABLE: &[u8] = include_bytes!("../vendor/fixtures/archive/Rar.rar");
const NAMES_VISIBLE: &[u8] =
    include_bytes!("../vendor/fixtures/archive/Rar5.encrypted_filesOnly.rar");
const NAMES_HIDDEN: &[u8] =
    include_bytes!("../vendor/fixtures/archive/Rar5.encrypted_filesAndHeader.rar");
const PAYLOAD_ARTICLES: usize = 40;
const PLANNING_BODIES: usize = 6;

async fn run(posts: &[&Posted]) -> (Rig, JobStatus) {
    let all = articles(posts.iter().copied());
    let rig = Rig::new(all.clone()).await;
    rig.count_bodies(&all);
    let engine = rig.engine().await;
    let spec = rig.spec("Release", nzb::of(posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let status = finish(&engine, &id).await;
    (rig, status)
}

fn assert_refused(rig: &Rig, status: &JobStatus, named: &str) {
    let reason = failed_as(status, JobFailure::Content);
    assert!(
        reason.contains("only executable") && reason.contains(named),
        "{reason}"
    );
    assert_eq!(
        (status.bytes_done, status.segments_done),
        (0, 0),
        "{status:?}"
    );
    assert!(!rig.dest.path().join("Release").exists());
}

fn payload(seed: u64) -> Vec<u8> {
    noise(PAYLOAD_ARTICLES * ARTICLE, seed)
}

#[tokio::test]
async fn loose_executables_with_only_sidecars_are_refused_before_their_bodies() {
    for ext in [
        "exe", "msi", "bat", "cmd", "scr", "com", "lnk", "pif", "ps1", "vbs", "jar", "apk",
    ] {
        let name = format!("FLUX.2026.1080p.WEB-DL.{ext}");
        let body = if ext == "com" {
            noise(64 << 10, 1)
        } else {
            payload(1)
        };
        let fake = Posted::new("fake", &name, body);
        let info = Posted::new("nfo", "FLUX.nfo", noise(900, 2));
        let cover = Posted::new("img", "cover.jpg", noise(900, 3));
        let (rig, status) = run(&[&fake, &info, &cover]).await;
        assert_refused(&rig, &status, &name);
        let read = rig.bodies(&fake.articles);
        assert!(read <= PLANNING_BODIES, "{ext}: {read} bodies");
    }
}

#[tokio::test]
async fn an_executable_inside_a_stored_rar_is_refused_before_its_bodies() {
    let data = payload(4);
    let set = rar::stored(
        &[Member {
            name: "Movie.2026.1080p.exe",
            data: &data,
        }],
        (PAYLOAD_ARTICLES + 2) * ARTICLE,
    );
    let volumes: Vec<Posted> = set
        .volumes
        .iter()
        .enumerate()
        .map(|(i, volume)| {
            let name = rar::volume_name("Movie", i, set.volumes.len());
            Posted::new(&format!("v{i}"), &name, volume.clone())
        })
        .collect();
    let posts: Vec<&Posted> = volumes.iter().collect();
    let (rig, status) = run(&posts).await;
    assert_refused(&rig, &status, "Movie.2026.1080p.exe");
    let read: usize = volumes.iter().map(|v| rig.bodies(&v.articles)).sum();
    assert!(read <= PLANNING_BODIES, "{read} bodies");
}

#[tokio::test]
async fn members_of_compressed_archives_are_read_from_their_headers() {
    let rar = Posted::new("rar", "Release.rar", WITH_EXECUTABLE.to_vec());
    let (rig, status) = run(&[&rar]).await;
    assert_refused(&rig, &status, "test.exe");

    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("Setup.exe"), payload(5)).unwrap();
    sevenzip(work.path(), &["a", "-t7z", "-mx=1", "Fake.7z", "Setup.exe"]);
    let archive = std::fs::read(work.path().join("Fake.7z")).unwrap();
    let packed = Posted::new("sz", "Fake.7z", archive);
    let (rig, status) = run(&[&packed]).await;
    assert_refused(&rig, &status, "Setup.exe");
}

#[tokio::test]
async fn an_obfuscated_executable_named_by_par2_is_refused_before_its_bodies() {
    let data = payload(6);
    let named = Protected::new(
        vec![Posted::new("real", "Setup.exe", data.clone())],
        "Release",
        &[(0, 2)],
    );
    let hidden = Posted::new("obf", &hash("Setup.exe"), data);
    let index = Posted::new(
        "idx",
        &format!("{}.par2", hash("Release")),
        named.index.data.clone(),
    );
    let (rig, status) = run(&[&hidden, &index]).await;
    assert_refused(&rig, &status, "Setup.exe");
    let read = rig.bodies(&hidden.articles);
    assert!(read <= PLANNING_BODIES, "{read} bodies");
}

#[tokio::test]
async fn an_encrypted_archive_is_refused_only_when_its_names_are_readable() {
    let visible = Posted::new("rar", "Locked.rar", NAMES_VISIBLE.to_vec());
    let (rig, status) = run(&[&visible]).await;
    assert_refused(&rig, &status, "test.exe");

    if !extractor_on_path() {
        return;
    }
    let hidden = Posted::new("rar", "Locked.rar", NAMES_HIDDEN.to_vec());
    let (_rig, status) = run(&[&hidden]).await;
    let reason = failed(&status);
    assert!(
        reason.contains("password-protected") && !reason.contains("only executable"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_stray_executable_beside_a_real_payload_still_downloads() {
    let cases: [(&str, Vec<u8>); 4] = [
        ("Movie.2026.mkv", video(6 * ARTICLE + 11, 7)),
        ("Book.2026.epub", noise(5 * ARTICLE + 3, 8)),
        ("Album.2026.flac", noise(5 * ARTICLE + 5, 9)),
        ("Opaque.2026.zipx", noise(5 * ARTICLE + 7, 10)),
    ];
    for (name, data) in cases {
        let real = Posted::new("real", name, data);
        let stray = Posted::new("stray", "Setup.exe", noise(2 * ARTICLE, 11));
        let (_rig, status) = run(&[&real, &stray]).await;
        let dir = completed(&status);
        assert_file(&dir, name, &real.data);
        assert!(listing(&dir).contains(&name.to_owned()));
    }
}

#[tokio::test]
async fn a_release_without_any_executable_is_unaffected() {
    let book = Posted::new("book", "Book.2026.epub", noise(3 * ARTICLE, 12));
    let cover = Posted::new("img", "cover.jpg", noise(900, 13));
    let (_rig, status) = run(&[&book, &cover]).await;
    assert_file(&completed(&status), "Book.2026.epub", &book.data);

    let only_images = Posted::new("img", "scan-001.jpg", noise(3 * ARTICLE, 14));
    let (_rig, status) = run(&[&only_images]).await;
    assert_file(&completed(&status), "scan-001.jpg", &only_images.data);
}

#[tokio::test]
async fn images_or_text_that_outweigh_a_stray_program_still_download() {
    let cases: [(&str, Vec<u8>, &str, Vec<u8>); 3] = [
        (
            "Artbook/001.jpg",
            noise(6 * ARTICLE, 20),
            "Artbook/viewer.exe",
            noise(ARTICLE, 21),
        ),
        (
            "Novel.txt",
            noise(3 * ARTICLE, 22),
            "www.example.com",
            noise(120, 23),
        ),
        (
            "Scans.png",
            noise(3 * ARTICLE, 24),
            "Visit www.example.com",
            noise(80, 25),
        ),
    ];
    for (name, data, junk, extra) in cases {
        let real = Posted::new("real", name, data);
        let stray = Posted::new("stray", junk, extra);
        let (_rig, status) = run(&[&real, &stray]).await;
        assert_file(&completed(&status), name, &real.data);
    }
}

#[tokio::test]
async fn a_program_with_small_images_and_text_is_still_refused() {
    let fake = Posted::new("fake", "Movie.2026.1080p.exe", payload(30));
    let cover = Posted::new("img", "cover.jpg", noise(900, 31));
    let notes = Posted::new("txt", "readme.txt", noise(700, 32));
    let (rig, status) = run(&[&fake, &cover, &notes]).await;
    assert_refused(&rig, &status, "Movie.2026.1080p.exe");
}

#[tokio::test]
async fn a_fake_behind_an_earlier_password_is_inspected_with_the_password_that_opened_it() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("Setup.exe"), payload(40)).unwrap();
    sevenzip(
        work.path(),
        &[
            "a",
            "-t7z",
            "-mx=0",
            "-mhe=on",
            "-pright",
            "Locked.7z",
            "Setup.exe",
        ],
    );
    let packed = Posted::new(
        "sz",
        "Locked.7z",
        std::fs::read(work.path().join("Locked.7z")).unwrap(),
    );
    let rig = Rig::new(packed.articles.clone()).await;
    let engine = rig.engine().await;
    let spec = rig.spec(
        "Release",
        nzb_with(&[("password", "right")], &[packed.listed()]),
        Some("wrong"),
    );
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let status = finish(&engine, &id).await;
    let reason = failed_as(&status, JobFailure::Content);
    assert!(
        reason.contains("only executable") && reason.contains("Setup.exe"),
        "{reason}"
    );
    assert_eq!(status.bytes_done, 0, "{status:?}");
}
