use std::path::Path;
use std::time::Duration;

use kob_usenet::{JobFailure, JobState, JobStatus};

use crate::jobs::{Rig, waiting};
use crate::nzb;
use crate::post::{ARTICLE, Posted, noise};

const DAMAGED: &str = "damaged or the password is wrong";
const UNPACK_STALL: Duration = Duration::from_millis(1500);

async fn unpacking(case: &str) -> JobStatus {
    let tool = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chatty-tool");
    let rar = Posted::new(case, &format!("{case}.rar"), archive());
    let rig = Rig::new(rar.articles.clone()).await;
    let engine = rig
        .engine_with(|c| {
            c.download.unrar = tool;
            c.download.unpack_stall = UNPACK_STALL;
            c.download.retry_after = Duration::from_secs(60);
        })
        .await;
    let spec = rig.spec(case, nzb::of(&[&rar]), Some("secret"));
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    waiting(&engine, &id).await
}

fn archive() -> Vec<u8> {
    let mut data = include_bytes!("../vendor/fixtures/archive/Rar.rar").to_vec();
    data.extend(noise(ARTICLE, 7));
    data
}

fn assert_refused(case: &str, status: &JobStatus, expected: &str) {
    assert_eq!(status.state, JobState::Failed, "{case}: {status:?}");
    assert_eq!(status.failure, Some(JobFailure::Content), "{case}");
    let error = status.error.as_deref().unwrap_or_default();
    assert!(error.contains(expected), "{case}: {error}");
}

#[tokio::test]
async fn content_failures_are_classified_when_streams_interleave_mid_marker() {
    for (case, expected) in [
        ("damaged-interleaved", DAMAGED),
        ("damaged-on-stdout", DAMAGED),
        ("error-before-flood", DAMAGED),
        ("wrong-password-split", "Wrong archive password"),
        ("opaque-split", "opaque failure"),
    ] {
        assert_refused(case, &unpacking(case).await, expected);
    }
}

#[tokio::test]
async fn machine_failures_are_retried_when_streams_interleave_mid_marker() {
    for (case, expected) in [
        ("no-space-split", "ran out of disk space"),
        (
            "machine-failure",
            "hit a write, memory or file creation error",
        ),
    ] {
        let status = unpacking(case).await;
        assert_eq!(status.state, JobState::Queued, "{case}: {status:?}");
        assert_eq!(status.failure, None, "{case}");
        let error = status.error.as_deref().unwrap_or_default();
        assert!(error.contains(expected), "{case}: {error}");
    }
}

#[tokio::test]
async fn output_on_either_stream_keeps_the_unpack_watchdog_quiet() {
    let (stdout, stderr) = tokio::join!(unpacking("alive-stdout"), unpacking("alive-stderr"));
    assert_refused("alive-stdout", &stdout, "opaque failure");
    assert_refused("alive-stderr", &stderr, "opaque failure");
}
