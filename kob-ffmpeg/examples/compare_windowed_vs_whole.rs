//! Benchmark: V1-equivalent windowed embed_src + vhash vs whole-file.
//!
//! Usage:
//!   cargo run --example compare_windowed_vs_whole --release -- /path/to/ep.mkv
//!
//! Prints elapsed wall time, frame counts, and bytes for a windowed run
//! (intro + credits windows from V1's `extract_duration_secs` math) and a
//! whole-file run. Use on a real episode to confirm the narrowing is paying
//! off before flipping media_analyze.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use kob_ffmpeg::single_pass::{
    AudioPeaksParams, BlackFramesParams, ChromaprintParams, EmbedSrcParams, SilenceParams,
    SinglePassRequest, TrickplayParams, VhashParams, run_single_pass,
};

/// Mirrors `kobbler::skip_detect::extract_duration_secs` so this example
/// doesn't need a kobbler dependency.
fn extract_duration_secs(duration: f64) -> u32 {
    if duration < 900.0 {
        480
    } else if duration < 2400.0 {
        900
    } else {
        1200
    }
}

fn build_request(path: PathBuf, windows_on: bool, duration: f64) -> SinglePassRequest {
    let (vhash_windows, embed_windows) = if windows_on {
        let intro_window = extract_duration_secs(duration) as f64;
        let vhash_credits_start = duration * 0.80;
        let embed_scan_start = (duration - 300.0).max(duration * 0.85);
        (
            vec![(0.0, intro_window), (vhash_credits_start, duration)],
            vec![(embed_scan_start, duration)],
        )
    } else {
        (Vec::new(), Vec::new())
    };
    SinglePassRequest {
        input_path: path,
        trickplay: Some(TrickplayParams {
            width: 320,
            fps: 0.2,
            jpeg_quality: 5,
        }),
        vhash: Some(VhashParams {
            intermediate_width: 64,
            fps: 3.0,
            windows: vhash_windows,
        }),
        embed_src: Some(EmbedSrcParams {
            width: 768,
            fps: 1.0,
            jpeg_quality: 3,
            windows: embed_windows,
        }),
        chromaprint: Some(ChromaprintParams::default()),
        silence: Some(SilenceParams::default()),
        loudness: true,
        audio_peaks: Some(AudioPeaksParams::default()),
        black_frames: Some(BlackFramesParams::default()),
        corruption: None,
        fast_decode: true,
    }
}

fn run(path: PathBuf, label: &str, windows_on: bool, duration: f64) {
    let req = build_request(path, windows_on, duration);
    let started = Instant::now();
    let result = run_single_pass(req, None).expect("run_single_pass");
    let elapsed = started.elapsed().as_secs_f64();

    let vhash_count = result.vhash.as_ref().map(|v| v.len()).unwrap_or(0);
    let trickplay_count = result.trickplay.as_ref().map(|v| v.len()).unwrap_or(0);
    let embed_frames = result.embed_src.as_ref();
    let embed_count = embed_frames.map(|v| v.len()).unwrap_or(0);
    let embed_bytes: usize = embed_frames
        .map(|v| v.iter().map(|f| f.jpeg_data.len()).sum())
        .unwrap_or(0);

    println!("── {label} ──────────────────────────");
    println!("  wall time:      {elapsed:.2}s");
    println!(
        "  speed vs real:  {:.2}x",
        result.input_duration_secs / elapsed.max(0.001)
    );
    println!("  trickplay:      {trickplay_count} frames");
    println!("  vhash:          {vhash_count} descriptors");
    println!(
        "  embed_src:      {embed_count} JPEGs ({:.1} MB in memory)",
        embed_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  chromaprint:    {} sub-fp ({:.1}s)",
        result
            .chromaprint
            .as_ref()
            .map(|c| c.raw.len())
            .unwrap_or(0),
        result
            .chromaprint
            .as_ref()
            .map(|c| c.duration_secs)
            .unwrap_or(0.0)
    );
    println!(
        "  silence:        {} intervals",
        result.silence.as_ref().map(|v| v.len()).unwrap_or(0)
    );
    println!(
        "  black_frames:   {} intervals",
        result.black_frames.as_ref().map(|v| v.len()).unwrap_or(0)
    );
    println!("  loudness:       {:?}", result.loudness);
    println!(
        "  audio_peaks:    {} bytes",
        result
            .audio_peaks
            .as_ref()
            .map(|p| p.bytes.len())
            .unwrap_or(0)
    );
    println!("  per-branch err: {:?}", result.per_branch_errors);
    println!();
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let positional: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    if positional.is_empty() {
        eprintln!(
            "usage: {} /path/to/episode.mkv [--whole-file-only] [--windowed-only]",
            args[0]
        );
        std::process::exit(2);
    }
    let path = PathBuf::from(positional[0]);
    let whole_only = args.iter().any(|a| a == "--whole-file-only");
    let windowed_only = args.iter().any(|a| a == "--windowed-only");

    let duration = {
        // Probe duration via a no-branch open.
        let mut req = SinglePassRequest::new(path.clone());
        req.chromaprint = Some(ChromaprintParams {
            offset_secs: Some(0.0),
            length_secs: Some(1.0),
        });
        let r = run_single_pass(req, None).expect("probe");
        // Fall through to file's stream duration when the tiny fingerprint
        // returns something smaller than the file itself.
        r.input_duration_secs.max(0.0)
    };
    println!(
        "probe reports {:.1}s of content (for window math — should match ffprobe)",
        duration
    );

    if !whole_only {
        run(path.clone(), "WINDOWED (V1-equivalent)", true, duration);
    }
    if !windowed_only {
        run(path, "WHOLE FILE (current V2 default)", false, duration);
    }
}
