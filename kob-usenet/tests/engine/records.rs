use std::path::{Path, PathBuf};

use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, EngineConfig, JobSpec};

use crate::support::{Fixture, server};

fn status_file(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir.join("jobs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("status").is_file())
        .expect("a job record")
        .join("status")
}

#[tokio::test]
async fn unreadable_or_unknown_version_job_records_are_moved_aside_and_the_engine_starts() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    for broken in ["garbage", "version"] {
        let data = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let config = EngineConfig::new(data.path().to_path_buf(), vec![server("a", &nntp, 0)]);
        {
            let engine = Engine::new(config.clone()).await.unwrap();
            nntp.refuse().await;
            engine
                .add_job(JobSpec {
                    id: "job".into(),
                    name: "Show".into(),
                    category: "tv".into(),
                    nzb: fixture.standard_nzb(),
                    password: None,
                    dest_dir: dest.path().to_path_buf(),
                })
                .await
                .unwrap();
        }
        nntp.resume().await.unwrap();
        let status = status_file(data.path());
        let record = status.parent().unwrap().file_name().unwrap().to_owned();
        let mut bytes = std::fs::read(&status).unwrap();
        match broken {
            "garbage" => bytes = b"\xff\xff\xff not a record".to_vec(),
            _ => bytes[0] = 99,
        }
        std::fs::write(&status, bytes).unwrap();

        let engine = Engine::new(config).await.expect(broken);
        assert!(engine.jobs().is_empty(), "{broken}");
        assert!(
            data.path()
                .join("jobs")
                .join(".quarantine")
                .join(record)
                .join("status")
                .is_file(),
            "{broken}: the record is kept aside"
        );
    }
}
