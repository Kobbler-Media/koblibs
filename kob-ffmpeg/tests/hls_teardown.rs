//! A live tune is torn down by cancelling its segmenter and deleting the segment directory
//! at the same time, so the segmenter can find its directory gone mid-write. Whatever it
//! finds, it has to return rather than take the process down.

mod fixtures;

use kob_ffmpeg::MediaType;
use kob_ffmpeg::format::input::InputContext;
use kob_ffmpeg::format::io_context::CustomIoContext;
use kob_ffmpeg::format::output::OutputContext;
use kob_ffmpeg::packet::Packet;
use kob_ffmpeg::progress::RunContext;
use kob_ffmpeg::transcode::{HlsConfig, HlsWindow, transcode_to_hls_from_ctx};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn minute_ts() -> PathBuf {
    let path = std::env::temp_dir().join(format!("kob-ffmpeg-teardown-{}.ts", std::process::id()));
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(fixtures::minute_video())
        .args(["-c", "copy", "-f", "mpegts"])
        .arg(&path)
        .status()
        .expect("ffmpeg must be available");
    assert!(status.success());
    path
}

fn has_segment(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".m4s"))
        })
        .unwrap_or(false)
}

/// Feeds the stream the way a tuner does: a chunk at a time, at a pace the segmenter keeps
/// up with, so it is still writing when the directory goes.
fn segment_live(ts: &Path, config: HlsConfig) -> std::thread::JoinHandle<()> {
    let bytes = std::fs::read(ts).unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for chunk in bytes.chunks(4096) {
            if tx.send(chunk.to_vec()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    });
    std::thread::spawn(move || {
        let mut io = CustomIoContext::from_channel(rx).unwrap();
        let mut ctx = InputContext::open_custom(&mut io).unwrap();
        ctx.find_stream_info().unwrap();
        let _ = transcode_to_hls_from_ctx(ctx, &config);
    })
}

#[test]
fn a_live_segmenter_whose_directory_is_deleted_under_it_returns() {
    let ts = minute_ts();
    for (what, window, cancel) in [
        (
            "sliding, cancelled",
            HlsWindow::Sliding { segments: 6 },
            true,
        ),
        ("sliding", HlsWindow::Sliding { segments: 6 }, false),
        ("dvr, cancelled", HlsWindow::Dvr, true),
        ("dvr", HlsWindow::Dvr, false),
    ] {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("buffer");
        std::fs::create_dir(&dir).unwrap();
        let token = CancellationToken::new();
        let config = HlsConfig::new(&dir)
            .video_encoder("libx264")
            .segment_duration(0.5)
            .hls_window(window)
            .run_context(RunContext::cancellable(token.clone()));

        let run = segment_live(&ts, config);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !has_segment(&dir) {
            assert!(
                Instant::now() < deadline,
                "{what}: no segment within a minute"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !run.is_finished(),
            "{what}: finished before its directory went"
        );
        if cancel {
            token.cancel();
        }
        while !run.is_finished() {
            let _ = std::fs::remove_dir_all(&dir);
            std::thread::sleep(Duration::from_millis(10));
        }
        run.join()
            .unwrap_or_else(|_| panic!("{what}: the segmenter panicked"));
    }
    let _ = std::fs::remove_file(ts);
}

/// A muxer whose write failed is left in whatever state the failure left it, and FFmpeg's
/// HLS muxer has crashed writing a trailer from there. The directory comes back empty before
/// the drop, so a trailer that was written anyway shows up in it.
#[test]
fn a_muxer_that_failed_a_write_is_dropped_without_writing_its_trailer() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("buffer");
    std::fs::create_dir(&dir).unwrap();
    let mut input = InputContext::open(fixtures::minute_video().to_str().unwrap()).unwrap();
    let video = input.find_best_stream(MediaType::Video).unwrap();
    let in_tb = input.stream(video).unwrap().time_base();
    let mut out =
        OutputContext::create_with_format(dir.join("stream.m3u8").to_str().unwrap(), "hls")
            .unwrap();
    out.set_option("hls_time", "0.5").unwrap();
    let out_idx = out.add_stream_copy(&input.stream(video).unwrap()).unwrap();
    out.write_header().unwrap();

    std::fs::remove_dir_all(&dir).unwrap();
    let mut packet = Packet::new().unwrap();
    let mut failed = false;
    while input.read_packet(&mut packet).unwrap() {
        if packet.stream_index() == video as i32
            && out
                .write_packet_rescaled(&mut packet, out_idx, in_tb)
                .is_err()
        {
            failed = true;
            break;
        }
    }
    assert!(failed, "no write failed with the directory gone");

    std::fs::create_dir(&dir).unwrap();
    drop(out);
    let written: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        written.is_empty(),
        "the trailer was written after a failed write: {written:?}"
    );
}
