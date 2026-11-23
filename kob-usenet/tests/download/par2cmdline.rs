use std::process::Command;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

const SLICE: usize = ARTICLE / 2 / 4 * 4;

struct Case {
    gone: &'static [(usize, usize)],
    memory: usize,
}

const CASES: &[Case] = &[
    Case {
        gone: &[(0, 4)],
        memory: 256 << 20,
    },
    Case {
        gone: &[(0, 0)],
        memory: 256 << 20,
    },
    Case {
        gone: &[(0, 11), (1, 9)],
        memory: 256 << 20,
    },
    Case {
        gone: &[(0, 2), (0, 3), (1, 0), (1, 5)],
        memory: 256 << 20,
    },
    Case {
        gone: &[(0, 2), (0, 3), (1, 0), (1, 5)],
        memory: 3 * SLICE,
    },
    Case {
        gone: &[(0, 1), (0, 6), (1, 2), (1, 7), (1, 9)],
        memory: SLICE + 4096,
    },
];

fn par2_on_path() -> bool {
    let found = Command::new("par2").arg("--version").output().is_ok();
    let required = std::env::var("KOB_USENET_REQUIRE_TOOLS").is_ok_and(|v| v == "1");
    assert!(found || !required, "par2 is required on PATH");
    found
}

fn created_by_par2cmdline(files: &[Posted]) -> Vec<Posted> {
    let dir = tempfile::tempdir().unwrap();
    for file in files {
        std::fs::write(dir.path().join(&file.name), &file.data).unwrap();
    }
    let status = Command::new("par2")
        .current_dir(dir.path())
        .args(["create", "-q", "-q", "-r45", "-n4"])
        .arg(format!("-s{SLICE}"))
        .arg("Show.par2")
        .args(files.iter().map(|f| &f.name))
        .status()
        .unwrap();
    assert!(status.success());
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".par2"))
        .collect();
    names.sort();
    assert!(names.len() > 1, "{names:?}");
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let data = std::fs::read(dir.path().join(name)).unwrap();
            Posted::new(&format!("par{i}"), name, data)
        })
        .collect()
}

#[tokio::test]
async fn a_set_made_by_par2cmdline_repairs_to_the_exact_bytes() {
    if !par2_on_path() {
        eprintln!("skipped: par2 is not on PATH");
        return;
    }
    for (n, case) in CASES.iter().enumerate() {
        let seed = 40 + n as u64;
        let files = vec![
            Posted::new("e01", "Show.S01E01.mkv", video(12 * ARTICLE + 500, seed)),
            Posted::new(
                "e02",
                "Show.S01E02.mkv",
                video(10 * ARTICLE + 33, seed + 20),
            ),
        ];
        let par2 = created_by_par2cmdline(&files);
        let gone: Vec<String> = case
            .gone
            .iter()
            .map(|&(file, article)| files[file].id(article).to_owned())
            .collect();
        let posts: Vec<&Posted> = files.iter().chain(&par2).collect();
        let articles = posts
            .iter()
            .flat_map(|p| p.articles.iter().cloned())
            .filter(|a| !gone.contains(&a.message_id))
            .collect();
        let rig = Rig::new(articles).await;
        let memory = case.memory;
        let engine = rig
            .engine_with(|c| c.download.repair_memory_bytes = memory)
            .await;
        let spec = rig.spec("Show", nzb::of(&posts), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        let dir = completed(&finish(&engine, &id).await);
        for file in &files {
            assert_file(&dir, &file.name, &file.data);
        }
    }
}
