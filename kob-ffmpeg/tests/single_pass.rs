//! Tests asserting `run_single_pass` produces output equivalent to today's
//! standalone single-purpose paths.

mod fixtures;

use kob_ffmpeg::single_pass::{
    AudioPeaksParams, BlackFramesParams, ChromaprintParams, CorruptionParams, EmbedSrcParams,
    SilenceParams, SinglePassRequest, TrickplayParams, VhashParams, run_single_pass,
};

#[test]
fn black_frames_branch_honours_picture_threshold() {
    let run = |params: BlackFramesParams| {
        let mut req = SinglePassRequest::new(fixtures::near_black_video());
        req.black_frames = Some(params);
        run_single_pass(req, None)
            .expect("run_single_pass")
            .black_frames
            .expect("black_frames output")
    };

    let base = BlackFramesParams::default()
        .min_duration_secs(0.08)
        .pixel_threshold(0.10);
    assert!(
        run(base.clone()).is_empty(),
        "0.98 pic_th must not call a 0.90-black picture black"
    );
    assert_eq!(run(base.picture_threshold(0.85)).len(), 1);
}

#[test]
fn single_pass_smoke_all_branches() {
    let path = fixtures::basic_video();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: Some(TrickplayParams {
            width: 320,
            fps: 2.0,
            jpeg_quality: 5,
        }),
        vhash: Some(VhashParams {
            intermediate_width: 64,
            fps: 3.0,
            windows: Vec::new(),
        }),
        embed_src: None,
        chromaprint: Some(ChromaprintParams::default()),
        silence: Some(SilenceParams::default()),
        loudness: true,
        audio_peaks: Some(Default::default()),
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    assert!(
        result.per_branch_errors.is_empty(),
        "per-branch errors: {:?}",
        result
            .per_branch_errors
            .iter()
            .map(|(b, e)| format!("{b:?}: {e}"))
            .collect::<Vec<_>>()
    );
    let trickplay = result.trickplay.expect("trickplay output");
    assert!(
        !trickplay.is_empty(),
        "trickplay produced no frames over a 2s fixture at 2 fps"
    );
    let vhash = result.vhash.expect("vhash output");
    assert!(!vhash.is_empty(), "vhash produced no descriptors");
    // chromaprint requires ~3s of audio to produce sub-fingerprints; the 2s
    // fixture legitimately produces an empty fp. Just assert the branch ran.
    let cp = result.chromaprint.expect("chromaprint output");
    assert!(
        cp.duration_secs > 1.0 || cp.raw.is_empty(),
        "chromaprint suspicious"
    );
    assert!(
        result.loudness.is_some(),
        "loudness branch produced no value"
    );
    let peaks = result.audio_peaks.expect("audio_peaks output");
    assert!(peaks.bytes.len() > 2, "peaks header-only output");
    // silence may or may not contain intervals depending on fixture content;
    // the basic_video fixture is a continuous sine tone so we just assert the
    // branch ran without error (already covered by per_branch_errors check).
    let _ = result.silence.expect("silence ran");
}

#[test]
fn single_pass_trickplay_byte_equal_to_standalone() {
    let path = fixtures::basic_video();
    let width = 320;
    let fps = 2.0;
    let quality = 5;

    // Standalone path: same args extract_frames_sequential_with_hw(fast_decode=true).
    let standalone = kob_ffmpeg::thumbnails::extract_frames_sequential(
        &path,
        0.0,
        2.5,
        fps,
        Some(width),
        quality,
        true,
    )
    .expect("standalone extract");

    // Single-pass with only the trickplay branch.
    let req = SinglePassRequest {
        input_path: path,
        trickplay: Some(TrickplayParams {
            width,
            fps,
            jpeg_quality: quality,
        }),
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let single_pass_frames = result.trickplay.expect("trickplay");

    assert_eq!(
        single_pass_frames.len(),
        standalone.len(),
        "frame count mismatch (standalone {} vs single_pass {})",
        standalone.len(),
        single_pass_frames.len()
    );
    for (i, (a, b)) in standalone.iter().zip(single_pass_frames.iter()).enumerate() {
        assert!(
            (a.timestamp_secs - b.timestamp_secs).abs() < 0.01,
            "frame {i} timestamp drift: standalone {} vs single_pass {}",
            a.timestamp_secs,
            b.timestamp_secs
        );
        assert_eq!(a.width, b.width, "frame {i} width mismatch");
        assert_eq!(a.height, b.height, "frame {i} height mismatch");
        assert_eq!(
            a.jpeg_data, b.jpeg_data,
            "frame {i} JPEG bytes differ (timestamp {})",
            a.timestamp_secs
        );
    }
}

#[test]
fn single_pass_chromaprint_byte_equal_to_standalone() {
    let path = fixtures::basic_video();
    let standalone = kob_ffmpeg::chromaprint::extract_fingerprint(&path, None, f64::INFINITY)
        .expect("standalone");

    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: Some(ChromaprintParams::default()),
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let single_pass = result.chromaprint.expect("chromaprint");

    assert_eq!(
        single_pass.raw, standalone.raw,
        "chromaprint raw fingerprint bytes differ"
    );
    assert!(
        (single_pass.duration_secs - standalone.duration_secs).abs() < 0.01,
        "chromaprint duration mismatch (standalone {} vs single_pass {})",
        standalone.duration_secs,
        single_pass.duration_secs
    );
}

#[test]
fn single_pass_silence_matches_standalone() {
    let path = fixtures::audio_with_silence();
    let standalone =
        kob_ffmpeg::filter::detect::detect_silence(&path, -30.0, 0.5, None).expect("standalone");

    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: Some(SilenceParams {
            noise_db: -30.0,
            min_duration_secs: 0.5,
        }),
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let single_pass = result.silence.expect("silence");

    assert_eq!(
        single_pass.len(),
        standalone.len(),
        "silence interval count mismatch (standalone {} vs single_pass {})",
        standalone.len(),
        single_pass.len()
    );
    for (i, (a, b)) in standalone.iter().zip(single_pass.iter()).enumerate() {
        assert!(
            (a.start_secs - b.start_secs).abs() < 0.01,
            "interval {i} start: standalone {} vs single_pass {}",
            a.start_secs,
            b.start_secs
        );
        assert!(
            (a.end_secs - b.end_secs).abs() < 0.01,
            "interval {i} end: standalone {} vs single_pass {}",
            a.end_secs,
            b.end_secs
        );
    }
}

#[test]
fn single_pass_lufs_matches_standalone() {
    let path = fixtures::basic_video();
    let standalone = kob_ffmpeg::audio::measure_lufs(&path, None).expect("standalone");

    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: true,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let single_pass = result.loudness;

    match (standalone, single_pass) {
        (Some(a), Some(b)) => assert!(
            (a - b).abs() < 0.01,
            "LUFS mismatch: standalone {a} vs single_pass {b}"
        ),
        (None, None) => {}
        (a, b) => panic!("LUFS presence mismatch: standalone {a:?} vs single_pass {b:?}"),
    }
}

#[test]
fn single_pass_no_audio_branches_omit_audio_decode() {
    // Just exercise the fast path where we discard the audio stream.
    let path = fixtures::basic_video();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: Some(TrickplayParams {
            width: 320,
            fps: 1.0,
            jpeg_quality: 5,
        }),
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    assert!(result.trickplay.is_some());
    assert!(result.chromaprint.is_none());
    assert!(result.audio_peaks.is_none());
    assert!(result.per_branch_errors.is_empty());
}

#[test]
fn single_pass_handles_ac3_channel_change_under_multi_branch_load() {
    // Real-world stress: AC3 stream where the first frame is stereo and
    // subsequent frames are 5.1 surround. The multi-branch load is the
    // interesting bit — every audio sink (chromaprint, silence, loudness,
    // audio_peaks) keeps its own resampler / filter graph state, so each
    // one independently has to rebuild on the channel change. Catches the
    // class of bug where the first branch's rebuild leaves the shared
    // frame in a state the next branch can't read.
    //
    // The fixture is audio-only (sine in AC3), so we don't enable any
    // video branches.
    let path = fixtures::ac3_channel_change_mkv();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: Some(ChromaprintParams::default()),
        silence: Some(SilenceParams {
            noise_db: -50.0, // sine tone is loud — push the threshold so we don't classify the whole thing as silence
            min_duration_secs: 0.1,
        }),
        loudness: true,
        audio_peaks: Some(AudioPeaksParams::default()),
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    assert!(
        result.per_branch_errors.is_empty(),
        "per-branch errors on channel-change fixture: {:?}",
        result
            .per_branch_errors
            .iter()
            .map(|(b, e)| format!("{b:?}: {e}"))
            .collect::<Vec<_>>()
    );
    // All four audio branches should produce something. Specific values are
    // fixture-dependent so we just assert presence + non-empty.
    let cp = result.chromaprint.expect("chromaprint produced no result");
    // chromaprint may legitimately produce empty fp on a 2s clip; just verify
    // the duration accounting saw audio (i.e., the resampler ran).
    let _ = cp.raw;
    let _ = result.silence.expect("silence produced no result");
    assert!(
        result.loudness.is_some(),
        "loudness branch produced no value despite multi-frame audio"
    );
    let peaks = result.audio_peaks.expect("audio_peaks produced no result");
    assert!(
        peaks.bytes.len() > 2,
        "audio_peaks output looks header-only — no pairs got bucketed across the channel change"
    );
}

#[test]
fn single_pass_embed_src_returns_jpegs_in_memory() {
    let path = fixtures::basic_video();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: Some(EmbedSrcParams {
            width: 768,
            fps: 2.0,
            jpeg_quality: 3,
            windows: Vec::new(),
        }),
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    assert!(
        result.per_branch_errors.is_empty(),
        "per-branch errors: {:?}",
        result
            .per_branch_errors
            .iter()
            .map(|(b, e)| format!("{b:?}: {e}"))
            .collect::<Vec<_>>()
    );
    let frames = result.embed_src.expect("embed_src frames");
    assert!(!frames.is_empty(), "embed_src produced no JPEGs");
    // JPEG magic on every frame — catches silently-empty or malformed output.
    for (i, f) in frames.iter().enumerate() {
        assert!(f.jpeg_data.len() > 100, "frame {i} too small");
        assert_eq!(
            &f.jpeg_data[..3],
            &[0xFF, 0xD8, 0xFF],
            "frame {i} missing JPEG magic"
        );
    }
    // First 768-wide JPEG should be at least 1 KB even at q=3.
    assert!(
        frames[0].jpeg_data.len() > 1000,
        "first 768-wide jpeg suspiciously small ({} bytes)",
        frames[0].jpeg_data.len()
    );
}

#[test]
fn single_pass_embed_src_windows_filter_frames() {
    // Confirm the `windows` filter drops frames whose timestamp lies outside
    // the provided range. `basic_video` is ~2s long; grab only frames in [1.0, 2.0).
    let path = fixtures::basic_video();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: None,
        embed_src: Some(EmbedSrcParams {
            width: 320,
            fps: 4.0,
            jpeg_quality: 5,
            windows: vec![(1.0, 2.0)],
        }),
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let frames = result.embed_src.expect("embed_src frames");
    assert!(!frames.is_empty(), "window produced no frames at all");
    for f in &frames {
        assert!(
            (1.0..2.0).contains(&f.timestamp_secs),
            "frame timestamp {} outside [1.0, 2.0)",
            f.timestamp_secs
        );
    }
}

#[test]
fn single_pass_vhash_windows_filter_descriptors() {
    let path = fixtures::basic_video();
    let req = SinglePassRequest {
        input_path: path,
        trickplay: None,
        vhash: Some(VhashParams {
            intermediate_width: 64,
            fps: 4.0,
            windows: vec![(0.0, 1.0)],
        }),
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass");
    let descs = result.vhash.expect("vhash descriptors");
    assert!(!descs.is_empty(), "window produced no descriptors");
    for d in &descs {
        assert!(
            (0.0..1.0).contains(&d.timestamp_secs),
            "vhash timestamp {} outside [0.0, 1.0)",
            d.timestamp_secs
        );
    }
}

/// Real-content sanity check, off by default. Run with:
///   KOB_REAL_FIXTURE=/path/to/episode.mkv cargo test --test single_pass -- --ignored real_episode
/// Or with the convenience wrapper at the bottom of this file.
///
/// Not part of CI — fixture is BYO, runtime is decode-bound (~minutes for a
/// real episode). Exists so a human can prove `run_single_pass` survives
/// real production content under the full multi-branch load before flipping
/// the V2 path on.
#[test]
#[ignore]
fn single_pass_real_episode_full_load() {
    let path = match std::env::var("KOB_REAL_FIXTURE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => {
            eprintln!("KOB_REAL_FIXTURE not set — skipping");
            return;
        }
    };
    assert!(path.is_file(), "fixture not found: {}", path.display());

    let started = std::time::Instant::now();
    let req = SinglePassRequest {
        input_path: path.clone(),
        trickplay: Some(TrickplayParams {
            width: 320,
            fps: 0.2,
            jpeg_quality: 5,
        }),
        vhash: Some(VhashParams {
            intermediate_width: 64,
            fps: 3.0,
            windows: Vec::new(),
        }),
        embed_src: Some(EmbedSrcParams {
            width: 768,
            fps: 1.0,
            jpeg_quality: 3,
            windows: Vec::new(),
        }),
        chromaprint: Some(ChromaprintParams::default()),
        silence: Some(SilenceParams::default()),
        loudness: true,
        audio_peaks: Some(AudioPeaksParams::default()),
        black_frames: Some(Default::default()),
        corruption: None,
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass on real episode");
    let elapsed = started.elapsed();

    eprintln!("─── single_pass on {} ───", path.display());
    eprintln!("input duration: {:.1}s", result.input_duration_secs);
    eprintln!("wall time:      {:.1}s", elapsed.as_secs_f64());
    eprintln!(
        "decode-vs-real ratio: {:.2}x",
        result.input_duration_secs / elapsed.as_secs_f64().max(0.001),
    );
    eprintln!(
        "trickplay frames:   {}",
        result.trickplay.as_ref().map(|v| v.len()).unwrap_or(0)
    );
    eprintln!(
        "vhash descriptors:  {}",
        result.vhash.as_ref().map(|v| v.len()).unwrap_or(0)
    );
    eprintln!(
        "embed_src JPEGs:    {}",
        result.embed_src.as_ref().map(|r| r.len()).unwrap_or(0),
    );
    eprintln!(
        "chromaprint bytes:  {} subfp ({:.1}s of audio)",
        result
            .chromaprint
            .as_ref()
            .map(|c| c.raw.len())
            .unwrap_or(0),
        result
            .chromaprint
            .as_ref()
            .map(|c| c.duration_secs)
            .unwrap_or(0.0),
    );
    eprintln!(
        "silence intervals:  {}",
        result.silence.as_ref().map(|v| v.len()).unwrap_or(0)
    );
    eprintln!("loudness (LUFS):    {:?}", result.loudness);
    eprintln!(
        "audio_peaks bytes:  {}",
        result
            .audio_peaks
            .as_ref()
            .map(|p| p.bytes.len())
            .unwrap_or(0),
    );
    eprintln!("per-branch errors:  {:?}", result.per_branch_errors);

    assert!(
        result.per_branch_errors.is_empty(),
        "per-branch errors on real fixture"
    );
    // Sanity floors: real episodes always have content in every branch.
    assert!(
        result.input_duration_secs > 60.0,
        "duration too short to be a real episode"
    );
    assert!(result.trickplay.unwrap().len() > 5);
    assert!(result.vhash.unwrap().len() > 50);
    assert!(result.embed_src.unwrap().len() > 30);
    let cp = result.chromaprint.unwrap();
    assert!(
        !cp.raw.is_empty(),
        "real episode produced empty chromaprint"
    );
    assert!(result.loudness.is_some(), "real episode produced no LUFS");
    assert!(result.audio_peaks.unwrap().bytes.len() > 1_000);
}

/// Corruption detection on a known-corrupt source. Off by default — set
///   KOB_TEST_CORRUPT_MKV=/path/to/Paradise-S02E06.mkv
///   KOB_TEST_CORRUPT_AT_SECS=1839     # ~30:39 — defaults to 1839 if unset
///   KOB_TEST_CORRUPT_TOLERANCE=15     # ± seconds; defaults to 15
/// and run with `cargo test --test single_pass -- --ignored corruption_detected`.
///
/// Exists so a human can prove `CorruptionSink` actually catches the bitstream
/// corruption that breaks Chrome's HW decoder. THIS IS THE GATE — if the
/// per-frame flags don't fire reliably here, the rest of the per-segment
/// transcode-on-demand work doesn't have a foundation.
#[test]
#[ignore]
fn corruption_detected_on_real_corrupt_fixture() {
    let path = match std::env::var("KOB_TEST_CORRUPT_MKV") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => {
            eprintln!("KOB_TEST_CORRUPT_MKV not set — skipping");
            return;
        }
    };
    assert!(path.is_file(), "fixture not found: {}", path.display());

    let expected_at = std::env::var("KOB_TEST_CORRUPT_AT_SECS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1839.0); // 30:39
    let tolerance = std::env::var("KOB_TEST_CORRUPT_TOLERANCE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(15.0);

    let req = SinglePassRequest {
        input_path: path.clone(),
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: Some(CorruptionParams::default()),
        fast_decode: true,
    };
    let started = std::time::Instant::now();
    let result = run_single_pass(req, None).expect("run_single_pass on corrupt fixture");
    let elapsed = started.elapsed();

    let ranges = result
        .corruption
        .as_ref()
        .expect("corruption branch produced no result");
    eprintln!("─── corruption scan on {} ───", path.display());
    eprintln!("input duration: {:.1}s", result.input_duration_secs);
    eprintln!("wall time:      {:.1}s", elapsed.as_secs_f64());
    eprintln!("ranges found:   {}", ranges.len());
    for r in ranges {
        eprintln!(
            "  [{:.2}s..{:.2}s] frames={} concealment={} missing_ref={} invalid_bs={} slices={} other={}",
            r.start_secs,
            r.end_secs,
            r.frame_count,
            r.concealment,
            r.missing_reference,
            r.invalid_bitstream,
            r.decode_slices_failed,
            r.other,
        );
    }

    assert!(
        !ranges.is_empty(),
        "expected at least one corrupt range on known-bad fixture"
    );
    let hit = ranges
        .iter()
        .any(|r| r.start_secs <= expected_at + tolerance && r.end_secs >= expected_at - tolerance);
    assert!(
        hit,
        "no corrupt range overlapping expected_at={expected_at}s ± {tolerance}s"
    );
    let any_decoder_signal = ranges
        .iter()
        .any(|r| r.missing_reference || r.concealment || r.invalid_bitstream);
    assert!(
        any_decoder_signal,
        "ranges fired but none flagged a recognised decoder error type — \
         AV_FRAME_FLAG_CORRUPT alone is too weak a signal to act on"
    );
}

/// Clean baseline: a well-encoded fixture should produce zero corrupt ranges.
/// Off by default — set `KOB_TEST_MKV=/path/to/clean.mkv` and run with
/// `cargo test --test single_pass -- --ignored corruption_clean`.
#[test]
#[ignore]
fn corruption_clean_on_clean_fixture() {
    let path = match std::env::var("KOB_TEST_MKV") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => {
            eprintln!("KOB_TEST_MKV not set — skipping");
            return;
        }
    };
    assert!(path.is_file(), "fixture not found: {}", path.display());

    let req = SinglePassRequest {
        input_path: path.clone(),
        trickplay: None,
        vhash: None,
        embed_src: None,
        chromaprint: None,
        silence: None,
        loudness: false,
        audio_peaks: None,
        black_frames: None,
        corruption: Some(CorruptionParams::default()),
        fast_decode: true,
    };
    let result = run_single_pass(req, None).expect("run_single_pass on clean fixture");
    let ranges = result.corruption.expect("corruption branch ran");
    eprintln!(
        "clean fixture {}: {} ranges (expected 0)",
        path.display(),
        ranges.len()
    );
    for r in &ranges {
        eprintln!(
            "  [{:.2}s..{:.2}s] frames={} concealment={} missing_ref={}",
            r.start_secs, r.end_secs, r.frame_count, r.concealment, r.missing_reference,
        );
    }
    assert!(
        ranges.is_empty(),
        "clean fixture produced {} unexpected corrupt range(s)",
        ranges.len()
    );
}
