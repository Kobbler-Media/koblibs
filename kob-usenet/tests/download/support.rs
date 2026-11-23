use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::post::Posted;
use kob_usenet::vendor::testkit::{Article, LoopbackServer};
use kob_usenet::{
    DownloadConfig, Engine, EngineConfig, FreeSpace, IpPreference, JobFailure, JobSpec, JobState,
    JobStatus, ServerConfig,
};

const FINISH_DEADLINE: Duration = Duration::from_secs(30);

pub struct Space(pub AtomicU64);

impl FreeSpace for Space {
    fn available(&self, _: &Path) -> io::Result<u64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

pub struct Rig {
    pub nntp: LoopbackServer,
    pub data: tempfile::TempDir,
    pub dest: tempfile::TempDir,
    pub space: Arc<Space>,
}

impl Rig {
    pub async fn new(articles: Vec<Article>) -> Self {
        Self::on(LoopbackServer::start(articles).await.unwrap())
    }

    pub fn on(nntp: LoopbackServer) -> Self {
        Self {
            nntp,
            data: tempfile::tempdir().unwrap(),
            dest: tempfile::tempdir().unwrap(),
            space: Arc::new(Space(AtomicU64::new(u64::MAX))),
        }
    }

    pub fn config(&self) -> EngineConfig {
        let mut config = EngineConfig::new(self.data.path().to_path_buf(), vec![self.server()]);
        config.download = DownloadConfig {
            free_space: Arc::clone(&self.space) as Arc<dyn FreeSpace>,
            free_space_floor: 1 << 20,
            straggler_after: Duration::from_millis(200),
            retry_after: Duration::from_millis(200),
            retry_max: Duration::from_secs(1),
            ..DownloadConfig::default()
        };
        config
    }

    pub async fn engine(&self) -> Engine {
        Engine::new(self.config()).await.unwrap()
    }

    pub async fn engine_with(&self, change: impl FnOnce(&mut EngineConfig)) -> Engine {
        let mut config = self.config();
        change(&mut config);
        Engine::new(config).await.unwrap()
    }

    pub fn record(&self, name: &str) -> PathBuf {
        let found = listing(self.data.path())
            .into_iter()
            .find(|p| p.starts_with("jobs/") && p.ends_with(&format!("/{name}")))
            .unwrap_or_else(|| panic!("no job {name} in {:?}", listing(self.data.path())));
        self.data.path().join(found)
    }

    fn server(&self) -> ServerConfig {
        ServerConfig {
            id: "a".to_owned(),
            name: "server a".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: self.nntp.addr().port(),
            tls: false,
            verify_cert: true,
            ip_preference: IpPreference::Any,
            connections: 4,
            priority: 0,
            backbone: None,
            retention_days: None,
            expires_on: None,
            quota_bytes: None,
            username: "user".to_owned(),
            password: "secret".to_owned(),
        }
    }

    pub fn spec(&self, name: &str, nzb: Vec<u8>, password: Option<&str>) -> JobSpec {
        JobSpec {
            id: format!("job-{name}"),
            name: name.to_owned(),
            category: "kobload".to_owned(),
            nzb,
            password: password.map(str::to_owned),
            dest_dir: self.dest.path().to_path_buf(),
        }
    }

    pub fn count_bodies(&self, articles: &[Article]) {
        for article in articles {
            self.nntp
                .stall_article(&article.message_id, usize::MAX, Duration::ZERO);
        }
    }

    pub fn bodies(&self, articles: &[Article]) -> usize {
        articles
            .iter()
            .map(|a| self.nntp.stalled(&a.message_id))
            .sum()
    }
}

pub async fn finish(engine: &Engine, id: &str) -> JobStatus {
    tokio::time::timeout(FINISH_DEADLINE, async {
        loop {
            let status = engine.job(id).expect("job is listed");
            if matches!(status.state, JobState::Completed | JobState::Failed) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("job {id} did not finish: {:?}", engine.job(id)))
}

pub async fn waiting(engine: &Engine, id: &str) -> JobStatus {
    tokio::time::timeout(FINISH_DEADLINE, async {
        loop {
            let status = engine.job(id).expect("job is listed");
            if status.state.is_finished() || status.error.is_some() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("job {id} never waited: {:?}", engine.job(id)))
}

pub fn completed(status: &JobStatus) -> PathBuf {
    assert_eq!(
        status.state,
        JobState::Completed,
        "job failed: {:?}",
        status.error
    );
    status.path.clone()
}

pub fn failed(status: &JobStatus) -> String {
    failed_as(status, JobFailure::Content)
}

pub fn failed_as(status: &JobStatus, kind: JobFailure) -> String {
    assert_eq!(status.state, JobState::Failed, "{status:?}");
    assert_eq!(status.failure, Some(kind), "{status:?}");
    status.error.clone().expect("a failed job says why")
}

pub fn assert_file(dir: &Path, name: &str, truth: &[u8]) {
    let got = std::fs::read(dir.join(name))
        .unwrap_or_else(|e| panic!("{name} in {}: {e}: {:?}", dir.display(), listing(dir)));
    assert_eq!(got.len(), truth.len(), "{name} size");
    assert!(got == truth, "{name} bytes differ");
}

pub fn listing(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    walk(dir, dir, &mut names);
    names.sort();
    names
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            out.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub fn sevenzip(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("7z")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

fn tools_on_path(tools: &[&str]) -> bool {
    if std::env::var("KOB_USENET_REQUIRE_TOOLS").is_ok_and(|v| v == "1") {
        for tool in ["unrar", "7z"] {
            assert!(on_path(tool), "{tool} is required on PATH");
        }
    }
    let missing: Vec<&str> = tools.iter().copied().filter(|t| !on_path(t)).collect();
    if !missing.is_empty() {
        eprintln!("skipped: {missing:?} not on PATH");
    }
    missing.is_empty()
}

pub fn extractor_on_path() -> bool {
    tools_on_path(&["7z"])
}

// 7-Zip refuses the CRCs of the synthetic split RAR5 volumes these tests build; production
// unpacks RAR with unrar, so tests of multi-volume RAR unpacking need it.
pub fn unrar_on_path() -> bool {
    tools_on_path(&["unrar"])
}

pub fn par2_set(files: &[&Posted], base: &str, slice: usize, redundancy: u32) -> Vec<Posted> {
    let dir = tempfile::tempdir().unwrap();
    for file in files {
        std::fs::write(dir.path().join(&file.name), &file.data).unwrap();
    }
    let status = std::process::Command::new("par2")
        .current_dir(dir.path())
        .args(["create", "-q", "-q", "-n2"])
        .arg(format!("-r{redundancy}"))
        .arg(format!("-s{slice}"))
        .arg(format!("{base}.par2"))
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
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            Posted::new(
                &format!("{base}{i}"),
                name,
                std::fs::read(dir.path().join(name)).unwrap(),
            )
        })
        .collect()
}

pub async fn run_job(rig: &Rig, engine: &Engine, name: &str, posts: &[&Posted]) -> JobStatus {
    let spec = rig.spec(name, crate::nzb::of(posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    finish(engine, &id).await
}

pub fn articles_without(posts: &[&Posted], gone: &[&str]) -> Vec<Article> {
    posts
        .iter()
        .flat_map(|p| p.articles.iter().cloned())
        .filter(|a| !gone.contains(&a.message_id.as_str()))
        .collect()
}

pub fn crc_of(dir: &Path, name: &str) -> (usize, u32) {
    let data =
        std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}: {:?}", listing(dir)));
    (data.len(), kob_usenet::vendor::yenc::crc32(0, &data))
}
