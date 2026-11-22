use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use kob_usenet::JobState;

use crate::compressed::{FIXTURE_FILES, RAR4};
use crate::nzb::{self, nzb_with};
use crate::obfuscated::hash;
use crate::post::{ARTICLE, Posted, video};
use crate::rar::{self, Member};
use crate::support::{
    Rig, assert_file, completed, crc_of, extractor_on_path, failed, finish, listing, script,
    sevenzip, unrar_on_path, waiting,
};

fn packed(work: &Path, archive: &str, files: &[(&str, &[u8])], switches: &[&str]) -> Vec<u8> {
    let src = work.join(format!("{archive}.src"));
    for (name, data) in files {
        let path = src.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    let out = work.join(archive);
    let mut args = vec!["a"];
    args.extend(switches);
    args.extend([out.to_str().unwrap(), "."]);
    sevenzip(&src, &args);
    std::fs::read(out).unwrap()
}

#[derive(Debug, Clone, Copy)]
enum Broken {
    Stalls,
    DiskFull,
    NotExecutable,
    NoRoomToUnpack,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpack_failures_from_the_machine_requeue_as_infrastructure() {
    for broken in [
        Broken::Stalls,
        Broken::DiskFull,
        Broken::NotExecutable,
        Broken::NoRoomToUnpack,
    ] {
        let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
        let rig = Rig::new(rar.articles.clone()).await;
        let tools = tempfile::tempdir().unwrap();
        let tool = match broken {
            Broken::Stalls => script(tools.path(), "7z", "exec sleep 60"),
            Broken::DiskFull => script(
                tools.path(),
                "7z",
                "echo 'ERROR: E_FAIL : No space left on device' >&2; exit 2",
            ),
            Broken::NotExecutable => {
                let path = tools.path().join("7z");
                std::fs::write(&path, "not a program").unwrap();
                path
            }
            Broken::NoRoomToUnpack => "7z".into(),
        };
        if let Broken::NoRoomToUnpack = broken {
            rig.space.0.store((1 << 20) + 1000, Ordering::SeqCst);
        }
        let engine = rig
            .engine_with(|c| {
                c.download.unrar = "/nonexistent/unrar".into();
                c.download.sevenzip = tool.clone();
                c.download.unpack_stall = Duration::from_millis(300);
                c.download.retry_after = Duration::from_secs(60);
            })
            .await;
        let spec = rig.spec("Release", nzb::of(&[&rar]), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();

        let status = waiting(&engine, &id).await;
        assert_eq!(status.state, JobState::Queued, "{broken:?}: {status:?}");
        assert_eq!(status.failure, None, "{broken:?}");
    }
}

#[tokio::test]
async fn archives_in_subfolders_and_zip_members_of_a_stored_rar_are_unpacked() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let film = video(2 * ARTICLE + 3, 111);
    let notes = b"extra notes".to_vec();
    let zip = packed(
        work.path(),
        "Extras.zip",
        &[("Notes.txt", &notes)],
        &["-tzip"],
    );
    let set = rar::stored(
        &[
            Member {
                name: "Movie.mkv",
                data: &film,
            },
            Member {
                name: "Extras.zip",
                data: &zip,
            },
        ],
        100 * ARTICLE,
    );
    let stored = Posted::new("stored", "Movie.rar", set.volumes[0].clone());

    let inner = packed(work.path(), "Inner.7z", &[("Deep.txt", b"deep")], &["-t7z"]);
    let outer = packed(
        work.path(),
        "Outer.7z",
        &[("Sub/Inner.7z", &inner), ("Top.txt", b"top")],
        &["-t7z", "-mx=9"],
    );
    let nested = Posted::new("nested", "Nested.7z", outer);

    for (name, post, want) in [
        (
            "Movie",
            &stored,
            vec!["Movie.mkv".to_owned(), "Notes.txt".to_owned()],
        ),
        (
            "Nested",
            &nested,
            vec!["Sub/Deep.txt".to_owned(), "Top.txt".to_owned()],
        ),
    ] {
        let rig = Rig::new(post.articles.clone()).await;
        let engine = rig.engine().await;
        let spec = rig.spec(name, nzb::of(&[post]), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        let dir = completed(&finish(&engine, &id).await);
        assert_eq!(listing(&dir), want, "{name}");
        if name == "Movie" {
            assert_file(&dir, "Movie.mkv", &film);
            assert_file(&dir, "Notes.txt", &notes);
        }
    }
}

#[tokio::test]
async fn obfuscated_multi_volume_rar_without_par2_is_grouped_by_its_headers() {
    if !unrar_on_path() {
        return;
    }
    let film = video(25 * ARTICLE + 17, 112);
    let set = rar::stored(
        &[Member {
            name: "Movie.2026.mkv",
            data: &film,
        }],
        8 * ARTICLE,
    );
    assert!(set.volumes.len() > 2);
    let files: Vec<(String, Vec<u8>)> = set
        .volumes
        .iter()
        .enumerate()
        .map(|(i, v)| (hash(&format!("vol{i}")), v.clone()))
        .collect();
    let work = tempfile::tempdir().unwrap();
    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let outer = packed(work.path(), "Pack.7z", &borrowed, &["-t7z"]);
    let post = Posted::new("pack", "Movie.Pack.7z", outer);
    let rig = Rig::new(post.articles.clone()).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Movie", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_eq!(listing(&dir), ["Movie.2026.mkv"]);
    assert_file(&dir, "Movie.2026.mkv", &film);
}

#[tokio::test]
async fn passwords_never_reach_the_command_line_and_hostile_names_unpack_as_files() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let archive = packed(
        work.path(),
        "Locked.7z",
        &[("-rf", b"dash"), ("new\nline.txt", b"newline")],
        &["-t7z", "-psecret-pw", "-mhe=on"],
    );
    let post = Posted::new("locked", "-Locked.7z", archive);
    let rig = Rig::new(post.articles.clone()).await;
    let args = work.path().join("args");
    let tool = script(
        work.path(),
        "7z-logging",
        &format!(
            "printf '%s\\n' \"$@\" >> '{}'\nexec 7z \"$@\"",
            args.display()
        ),
    );
    let engine = rig
        .engine_with(|c| {
            c.download.unrar = "/nonexistent/unrar".into();
            c.download.sevenzip = tool.clone();
        })
        .await;
    let spec = rig.spec(
        "Locked",
        nzb_with(&[("password", "secret-pw")], &[post.listed()]),
        None,
    );
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, "-rf", b"dash");
    assert_file(&dir, "new\nline.txt", b"newline");
    let logged = std::fs::read_to_string(&args).unwrap();
    assert!(!logged.contains("secret-pw"), "{logged}");
    assert!(logged.lines().any(|l| l == "--"), "{logged}");
}

#[tokio::test]
async fn an_archive_holding_a_symbolic_link_is_refused() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("Movie.mkv"), b"movie").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", src.join("escape")).unwrap();
    sevenzip(&src, &["a", "-t7z", "-snl", "../Linked.7z", "."]);
    let post = Posted::new(
        "linked",
        "Linked.7z",
        std::fs::read(work.path().join("Linked.7z")).unwrap(),
    );
    let rig = Rig::new(post.articles.clone()).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Linked", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let error = failed(&finish(&engine, &id).await);
    assert!(error.contains("symbolic link"), "{error}");
}

#[tokio::test]
async fn each_extractor_unpacks_a_rar_on_its_own() {
    let have = |tool: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
    };
    let required = std::env::var("KOB_USENET_REQUIRE_TOOLS").is_ok_and(|v| v == "1");
    for (tool, unrar, sevenzip) in [
        ("unrar", "unrar", "/nonexistent/7z"),
        ("7z", "/nonexistent/unrar", "7z"),
    ] {
        if !have(tool) {
            assert!(!required, "{tool} is required on PATH");
            eprintln!("skipped: {tool} is not on PATH");
            continue;
        }
        let rar = Posted::new("rar", "Release.rar", RAR4.to_vec());
        let rig = Rig::new(rar.articles.clone()).await;
        let engine = rig
            .engine_with(|c| {
                c.download.unrar = unrar.into();
                c.download.sevenzip = sevenzip.into();
            })
            .await;
        let spec = rig.spec("Release", nzb::of(&[&rar]), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        let dir = completed(&finish(&engine, &id).await);
        for (name, len, crc) in FIXTURE_FILES {
            assert_eq!(crc_of(&dir, name), (len, crc), "{tool}: {name}");
        }
    }
}
