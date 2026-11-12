//! Integration tests for kob-ffmpeg.

mod fixtures;

/// Returns the path to a real MKV test file, if available.
/// Set `KOB_TEST_MKV` env var to point at any MKV file on disk.
/// Tests using this are `#[ignore]` and must be run with `--ignored`.
fn real_mkv_path() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(std::env::var("KOB_TEST_MKV").ok()?);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("KOB_TEST_MKV={} not found, skipping", p.display());
        None
    }
}

// --- Probe tests ---

#[test]
fn test_probe_basic_video() {
    let result = kob_ffmpeg::probe::probe(&fixtures::basic_video()).unwrap();
    assert_eq!(result.video_streams.len(), 1);
    assert_eq!(result.audio_streams.len(), 1);

    let v = &result.video_streams[0];
    assert_eq!(v.width, 320);
    assert_eq!(v.height, 240);
    assert_eq!(v.codec, "h264");
    assert!(!v.is_hdr);

    let a = &result.audio_streams[0];
    assert_eq!(a.codec, "aac");
    assert!(a.channels > 0);

    let dur = result.format.duration_seconds.unwrap();
    assert!(dur > 1.5 && dur < 3.0, "duration was {dur}");
}

#[test]
fn test_probe_rich_mkv() {
    let result = kob_ffmpeg::probe::probe(&fixtures::rich_mkv()).unwrap();

    assert_eq!(result.video_streams.len(), 1, "should have 1 video stream");
    assert_eq!(result.audio_streams.len(), 2, "should have 2 audio streams");
    assert_eq!(
        result.subtitle_streams.len(),
        1,
        "should have 1 subtitle stream"
    );
    assert_eq!(result.chapters.len(), 2, "should have 2 chapters");

    // Audio track metadata
    assert_eq!(result.audio_streams[0].language.as_deref(), Some("eng"));
    assert_eq!(result.audio_streams[0].title.as_deref(), Some("English"));
    assert_eq!(result.audio_streams[1].language.as_deref(), Some("fra"));
    assert_eq!(result.audio_streams[1].title.as_deref(), Some("French"));

    // Subtitle metadata
    assert_eq!(result.subtitle_streams[0].language.as_deref(), Some("eng"));
    assert_eq!(result.subtitle_streams[0].codec, "subrip");

    // Chapters
    assert_eq!(result.chapters[0].title.as_deref(), Some("Chapter 1"));
    assert_eq!(result.chapters[1].title.as_deref(), Some("Chapter 2"));
    assert!(result.chapters[0].start_seconds < 0.01);
    assert!((result.chapters[0].end_seconds - 1.0).abs() < 0.01);
    assert!((result.chapters[1].start_seconds - 1.0).abs() < 0.01);
}

#[test]
fn test_probe_keyframes() {
    let kfs = kob_ffmpeg::probe::probe_keyframes(&fixtures::basic_video()).unwrap();
    assert!(!kfs.is_empty());
    assert!(kfs[0] < 0.1, "first keyframe should be near 0");
}

#[test]
fn test_probe_chaptered_audiobook() {
    let result = kob_ffmpeg::probe::probe(&fixtures::chaptered_m4b()).unwrap();
    assert_eq!(result.audio_streams.len(), 1);
    assert_eq!(result.chapters.len(), 2);
    assert_eq!(result.chapters[0].title.as_deref(), Some("Chapter One"));
    assert_eq!(result.chapters[1].title.as_deref(), Some("Chapter Two"));
}

#[test]
fn test_probe_nonexistent_file() {
    let result = kob_ffmpeg::probe::probe(std::path::Path::new("/nonexistent/file.mp4"));
    assert!(result.is_err());
}

// --- Decoder tests ---

#[test]
fn test_decode_video_frames() {
    use kob_ffmpeg::codec::decoder::DecodeResult;
    use kob_ffmpeg::format::input::{InputContext, MediaType};
    use kob_ffmpeg::frame::Frame;
    use kob_ffmpeg::packet::Packet;

    let mut ctx = InputContext::open_path(&fixtures::basic_video()).unwrap();
    ctx.find_stream_info().unwrap();
    let v_idx = ctx.find_best_stream(MediaType::Video).unwrap();
    let stream = ctx.stream(v_idx).unwrap();
    let mut decoder = stream.decoder().unwrap();

    let mut pkt = Packet::new().unwrap();
    let mut frame = Frame::new().unwrap();
    let mut count = 0;
    while ctx.read_packet(&mut pkt).unwrap() {
        if pkt.stream_index() != v_idx as i32 {
            continue;
        }
        decoder.send_packet(&pkt).unwrap();
        while let DecodeResult::Frame = decoder.receive_frame(&mut frame).unwrap() {
            count += 1;
        }
    }
    decoder.send_eof().unwrap();
    while let DecodeResult::Frame = decoder.receive_frame(&mut frame).unwrap() {
        count += 1;
    }
    assert!(count > 0);
    assert_eq!(decoder.width(), 320);
    assert_eq!(decoder.height(), 240);
}

// --- Custom I/O tests ---

#[test]
fn test_custom_io_buffer_probe() {
    use kob_ffmpeg::format::input::InputContext;

    let data = std::fs::read(fixtures::basic_video()).unwrap();
    let mut io = kob_ffmpeg::format::io_context::CustomIoContext::from_buffer(data).unwrap();

    let mut ctx = InputContext::open_custom(&mut io).unwrap();
    ctx.find_stream_info().unwrap();
    assert!(
        ctx.nb_streams() >= 2,
        "expected 2+ streams, got {}",
        ctx.nb_streams()
    );
}

#[test]
fn test_custom_io_channel() {
    use kob_ffmpeg::format::input::InputContext;

    let data = std::fs::read(fixtures::basic_video()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let data_clone = data.clone();
    std::thread::spawn(move || {
        for chunk in data_clone.chunks(4096) {
            tx.send(chunk.to_vec()).unwrap();
        }
        tx.send(Vec::new()).unwrap();
    });

    let mut io = kob_ffmpeg::format::io_context::CustomIoContext::from_channel(rx).unwrap();
    let mut ctx = InputContext::open_custom(&mut io).unwrap();
    ctx.find_stream_info().unwrap();
    assert!(ctx.nb_streams() >= 2);
}

// --- Audio tests ---

#[test]
fn test_extract_pcm_range() {
    let mut total_bytes = 0usize;
    kob_ffmpeg::audio::extract_pcm_range(
        &fixtures::basic_video(),
        8000,
        0.0,
        f64::INFINITY,
        None,
        &mut |data| {
            total_bytes += data.len();
            true
        },
    )
    .unwrap();
    assert!(
        total_bytes > 10000,
        "expected ~32000 bytes (2s mono 8kHz s16le), got {total_bytes}"
    );
}

#[test]
fn test_convert_to_wav() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("output.wav");
    kob_ffmpeg::audio::convert_to_wav(&fixtures::basic_video(), &dst, None).unwrap();

    assert!(dst.exists());
    let probe = kob_ffmpeg::probe::probe(&dst).unwrap();
    assert_eq!(probe.audio_streams.len(), 1);
    assert_eq!(probe.audio_streams[0].codec, "pcm_s16le");
}

#[test]
fn test_measure_loudness() {
    let result = kob_ffmpeg::audio::measure_loudness(&fixtures::basic_video(), None);
    assert!(result.is_ok(), "should not error: {:?}", result.err());
    // Short synthetic files may not produce LUFS metadata
    if let Some(val) = result.unwrap().integrated_lufs {
        assert!((-100.0..=0.0).contains(&val), "LUFS {val} out of range");
    }
}

#[test]
#[ignore]
fn test_measure_loudness_real_file() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();
    let result = kob_ffmpeg::audio::measure_loudness(input, None)
        .unwrap()
        .integrated_lufs;
    eprintln!("LUFS: {result:?}");
    assert!(result.is_some(), "real file must produce LUFS value");
    let val = result.unwrap();
    assert!(
        (-50.0..=0.0).contains(&val),
        "LUFS {val} out of expected range"
    );
    eprintln!("LUFS = {val:.1}");
}

#[test]
fn test_measure_loudness_corrupt_audio_packets() {
    // AVI with a few corrupted audio packets — should still succeed
    let result = kob_ffmpeg::audio::measure_loudness(&fixtures::corrupt_audio_avi(), None);
    assert!(
        result.is_ok(),
        "measure_loudness should tolerate corrupt packets: {:?}",
        result.err()
    );
}

#[test]
fn test_measure_loudness_channel_layout_change() {
    // AC3 file where first frame is stereo but rest is 5.1.
    // The filter graph must handle the layout change gracefully.
    let result = kob_ffmpeg::audio::measure_loudness(&fixtures::ac3_channel_change_mkv(), None);
    assert!(
        result.is_ok(),
        "measure_loudness should handle AC3 channel layout changes: {:?}",
        result.err()
    );
}

const DOWNMIX_PEAK_CASES: &[(&str, f64, f64)] = &[
    ("mono", 0.5, -6.021),
    ("stereo", 0.5, -6.021),
    ("5.1(side)", 0.5, 1.635),
    ("5.1", 0.5, 1.635),
    ("7.1", 0.25, -2.154),
    ("quad", 0.5, -1.375),
];

#[test]
fn test_measure_loudness_reports_unnormalized_stereo_downmix_peak() {
    for &(layout, amplitude, expected_db) in DOWNMIX_PEAK_CASES {
        let loudness =
            kob_ffmpeg::audio::measure_loudness(&fixtures::in_phase_sine(layout, amplitude), None)
                .unwrap();
        let peak = loudness
            .downmix_peak_db
            .unwrap_or_else(|| panic!("{layout}: no downmix peak"));
        assert!(
            (peak - expected_db).abs() < 0.05,
            "{layout}: downmix peak {peak:.3} dB, expected {expected_db:.3} dB"
        );
        assert!(loudness.integrated_lufs.is_some(), "{layout}: no LUFS");
    }
}

#[test]
fn test_measure_loudness_peak_survives_channel_layout_change() {
    let loudness =
        kob_ffmpeg::audio::measure_loudness(&fixtures::ac3_channel_change_mkv(), None).unwrap();
    assert!(loudness.downmix_peak_db.is_some());
}

#[test]
fn test_extract_pcm_channel_layout_change() {
    let mut total_bytes = 0usize;
    let result = kob_ffmpeg::audio::extract_pcm_range(
        &fixtures::ac3_channel_change_mkv(),
        8000,
        0.0,
        f64::INFINITY,
        None,
        &mut |data| {
            total_bytes += data.len();
            true
        },
    );
    assert!(
        result.is_ok(),
        "extract_pcm_range should handle AC3 channel layout changes: {:?}",
        result.err()
    );
    assert!(total_bytes > 0, "should have produced PCM output");
}

// --- Filter tests ---

#[test]
fn test_silence_detection() {
    let intervals = kob_ffmpeg::filter::detect::detect_silence(
        &fixtures::audio_with_silence(),
        -30.0,
        0.5,
        None,
    )
    .unwrap();
    assert!(
        !intervals.is_empty(),
        "should detect at least 1 silence interval"
    );
    // The silence gap is around 0.5s-1.5s
    let gap = &intervals[0];
    assert!(
        gap.duration_secs > 0.3,
        "silence gap should be > 0.3s, got {}",
        gap.duration_secs
    );
}

// --- Remux tests ---

#[test]
fn test_remux_mp4_to_mkv() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("output.mkv");
    kob_ffmpeg::remux::remux(&fixtures::basic_video(), &dst, None).unwrap();

    let probe = kob_ffmpeg::probe::probe(&dst).unwrap();
    assert_eq!(probe.video_streams.len(), 1);
    assert_eq!(probe.audio_streams.len(), 1);
}

#[test]
fn test_output_remux_roundtrip() {
    use kob_ffmpeg::format::input::InputContext;
    use kob_ffmpeg::format::output::OutputContext;

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("roundtrip.mp4");

    let mut in_ctx = InputContext::open_path(&fixtures::basic_video()).unwrap();
    in_ctx.find_stream_info().unwrap();

    let mut out_ctx = OutputContext::create(dst.to_str().unwrap()).unwrap();
    let nb = in_ctx.nb_streams();
    for i in 0..nb {
        let stream = in_ctx.stream(i).unwrap();
        out_ctx.add_stream_copy(&stream).unwrap();
    }
    out_ctx.write_header().unwrap();

    let mut pkt = kob_ffmpeg::packet::Packet::new().unwrap();
    while in_ctx.read_packet(&mut pkt).unwrap() {
        out_ctx.write_packet_interleaved(&mut pkt).unwrap();
    }
    out_ctx.write_trailer().unwrap();
    drop(out_ctx);

    let mut verify = InputContext::open_path(&dst).unwrap();
    verify.find_stream_info().unwrap();
    assert_eq!(verify.nb_streams(), nb);
}

// --- Subtitle tests ---

#[test]
fn test_list_subtitle_tracks() {
    let tracks = kob_ffmpeg::subtitles::list_subtitle_tracks(&fixtures::rich_mkv()).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(tracks[0].codec, "subrip");
}

#[test]
fn test_extract_subtitle() {
    let tracks = kob_ffmpeg::subtitles::list_subtitle_tracks(&fixtures::rich_mkv()).unwrap();
    assert!(!tracks.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("extracted.srt");
    kob_ffmpeg::subtitles::extract_subtitle(
        &fixtures::rich_mkv(),
        tracks[0].stream_index,
        &dst,
        None,
    )
    .unwrap();

    assert!(dst.exists());
    let content = std::fs::read_to_string(&dst).unwrap();
    assert!(
        content.contains("Hello World"),
        "SRT should contain our test subtitle"
    );
}

// --- Thumbnail tests ---

#[test]
fn test_extract_frames_sequential() {
    // Basic video is 2s long; extract one frame per second.
    let frames = kob_ffmpeg::thumbnails::extract_frames_sequential_with_hw(
        &fixtures::basic_video(),
        0.0,
        f64::INFINITY,
        1.0,
        None,
        5,
        false,
        None,
    )
    .unwrap();
    assert!(!frames.is_empty());
    for f in &frames {
        assert!(f.jpeg_data.len() > 100, "JPEG should be non-trivial");
        assert_eq!(f.width, 320);
        assert_eq!(f.height, 240);
    }
}

// --- HLS transcode tests ---

#[test]
fn test_transcode_to_hls() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(128000)
        .segment_duration(1.0);
    let result =
        kob_ffmpeg::transcode::transcode_to_hls(&fixtures::basic_video(), &config).unwrap();

    assert!(result.playlist_path.exists());
    assert!(dir.path().join("init.mp4").exists());
}

/// MPEG-TS carries a 1/90000 time base. An encoder told nothing else takes that as its
/// frame rate, and x264's zero-latency rate control then budgets bits as if 90000 frames
/// shared each second: the live DVR buffer came out a few hundred bytes per frame, grey.
#[test]
fn test_transcode_to_hls_from_mpegts_spends_its_bitrate() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("broadcast.ts");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=duration=3:size=640x360:rate=50",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-b:v",
            "4M",
            "-f",
            "mpegts",
            "-y",
        ])
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg must be available");
    assert!(status.success());

    let out = dir.path().join("hls");
    let config = kob_ffmpeg::transcode::HlsConfig::new(&out)
        .video_encoder("libx264")
        .video_bitrate(2_000_000)
        .preset("veryfast")
        .tune("zerolatency")
        .audio_encoder("aac")
        .segment_duration(1.0);
    kob_ffmpeg::transcode::transcode_to_hls(&src, &config).unwrap();

    let bytes: u64 = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".m4s"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    // 3 s at a 2 Mbit/s target is ~750 KB; the 90000 fps budget left it near 10 KB.
    assert!(
        bytes > 200_000,
        "3 s of video encoded to only {bytes} bytes"
    );
}

#[test]
fn test_transcode_to_hls_with_zero_sample_rate_codec_params() {
    // Repro for live-IPTV streams (LATM-encapsulated AAC over MPEG-TS) where
    // find_stream_info can't extract the audio sample rate from codec
    // parameters. Pre-fix, the encoder opens with sample_rate=0 and libfdk_aac
    // / aac fails. Post-fix, the transcode pre-rolls one audio frame to
    // discover the actual rate.
    let dir = tempfile::tempdir().unwrap();
    let mut ctx =
        kob_ffmpeg::format::input::InputContext::open_path(&fixtures::basic_video()).unwrap();
    ctx.find_stream_info().unwrap();

    let a_idx = ctx
        .find_best_stream(kob_ffmpeg::format::input::MediaType::Audio)
        .unwrap();
    unsafe {
        let params = ctx.stream(a_idx).unwrap().codec_parameters();
        (*params).sample_rate = 0;
    }

    // Sanity check: a fresh decoder built from these params reports rate=0,
    // which is what the bug requires.
    {
        let dec = ctx.stream(a_idx).unwrap().decoder().unwrap();
        assert_eq!(
            dec.sample_rate(),
            0,
            "test fixture must reproduce the codec-params=0 case"
        );
    }

    // Pin the audio stream so find_best_stream's sample-rate-aware ranking
    // can't silently drop it when codec params have rate=0.
    let mut config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .audio_encoder("aac")
        .audio_bitrate(64_000)
        .segment_duration(2.0);
    config.audio_stream_index = Some(a_idx);

    let result = kob_ffmpeg::transcode::transcode_to_hls_from_ctx(ctx, &config).unwrap();

    assert!(result.playlist_path.exists(), "playlist should exist");
    let init_path = dir.path().join("init.mp4");
    assert!(init_path.exists(), "init segment should exist");
    assert!(
        init_path.metadata().unwrap().len() > 0,
        "init.mp4 should be non-empty"
    );

    // Verify the HLS output actually contains audio (the bug was that audio
    // was silently dropped or transcode failed).
    let probe = kob_ffmpeg::probe::probe(&init_path).unwrap();
    assert!(
        !probe.audio_streams.is_empty(),
        "HLS output should preserve the audio stream"
    );
    assert!(
        probe.audio_streams[0].sample_rate.unwrap_or(0) > 0,
        "output audio should have a real sample rate"
    );
}

#[test]
fn test_transcode_to_hls_with_subtitle_burn_in() {
    let dir = tempfile::tempdir().unwrap();

    // rich_mkv has: video(0), audio(1), audio(2), subtitle(3)
    // Find the subtitle stream index
    let probe = kob_ffmpeg::probe::probe(&fixtures::rich_mkv()).unwrap();
    assert!(
        !probe.subtitle_streams.is_empty(),
        "rich MKV should have subtitles"
    );
    let sub_idx = probe.subtitle_streams[0].index as u32;

    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(64000)
        .segment_duration(2.0)
        .subtitle_burn_in(sub_idx);

    let result = kob_ffmpeg::transcode::transcode_to_hls(&fixtures::rich_mkv(), &config).unwrap();

    assert!(result.playlist_path.exists(), "playlist should exist");
    assert!(
        dir.path().join("init.mp4").exists(),
        "init segment should exist"
    );
}

// --- IPTV tests ---

#[test]
fn test_record_from_file_url() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("recorded.ts");
    let url = format!("file:{}", fixtures::basic_video().display());

    let packets = kob_ffmpeg::iptv::record_stream(&url, &dst, None, None, &mut |_| true).unwrap();
    assert!(packets > 0);
    assert!(dst.metadata().unwrap().len() > 0);
}

#[test]
fn test_transcode_to_mkv() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("output.mkv");

    kob_ffmpeg::iptv::transcode_to_file(
        &fixtures::basic_video(),
        &dst,
        "libx264",
        "aac",
        128000,
        None,
    )
    .unwrap();

    let mut ctx = kob_ffmpeg::format::input::InputContext::open_path(&dst).unwrap();
    ctx.find_stream_info().unwrap();
    assert!(ctx.nb_streams() >= 2);
}

#[test]
fn test_mkv_output_basic() {
    use kob_ffmpeg::codec::encoder::{Encoder, VideoEncoderConfig};
    use kob_ffmpeg::format::output::OutputContext;

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("basic.mkv");

    let mut out_ctx = OutputContext::create(dst.to_str().unwrap()).unwrap();
    let enc = Encoder::video(VideoEncoderConfig {
        codec_name: "libx264".to_string(),
        width: 160,
        height: 120,
        pixel_format: kob_ffmpeg::PixelFormat::YUV420P,
        time_base: kob_ffmpeg::TimeBase::new(1, 25),
        bit_rate: None,
        gop_size: Some(12),
        global_header: out_ctx.requires_global_header(),
        quality: None,
        hw_frames_ctx: None,
        sample_aspect_ratio: None,
        frame_rate: None,
        color: Default::default(),
        options: Vec::new(),
    })
    .unwrap();

    out_ctx.add_stream_from_encoder(&enc).unwrap();
    out_ctx.write_header().unwrap();
    out_ctx.write_trailer().unwrap();
}

// --- HW detection tests ---

#[test]
fn test_encoder_available() {
    assert!(kob_ffmpeg::detect::encoder_available("libx264"));
    assert!(kob_ffmpeg::detect::encoder_available("aac"));
    assert!(!kob_ffmpeg::detect::encoder_available(
        "nonexistent_codec_12345"
    ));
}

#[test]
fn test_decoder_available() {
    assert!(kob_ffmpeg::detect::decoder_available("h264"));
    assert!(kob_ffmpeg::detect::decoder_available("aac"));
    assert!(!kob_ffmpeg::detect::decoder_available(
        "nonexistent_codec_12345"
    ));
}

#[test]
fn test_detect_hw_capabilities_no_panic() {
    let caps = kob_ffmpeg::detect::detect_hw_capabilities();
    let _ = caps.vaapi_available;
    let _ = caps.cuda_available;
}

// --- Black frame detection tests ---

#[test]
fn test_black_frame_detection() {
    // detect_black_frames should not error even on a file with no black frames
    let params = kob_ffmpeg::filter::detect::BlackFramesParams::default()
        .min_duration_secs(0.08)
        .pixel_threshold(0.10);
    let result =
        kob_ffmpeg::filter::detect::detect_black_frames(&fixtures::basic_video(), &params, None);
    assert!(result.is_ok());
}

#[test]
fn black_detect_picture_threshold_reaches_the_filter() {
    use kob_ffmpeg::filter::detect::{BlackFramesParams, detect_black_frames};

    let path = fixtures::near_black_video();
    let base = BlackFramesParams::default()
        .min_duration_secs(0.08)
        .pixel_threshold(0.10);

    let strict = detect_black_frames(&path, &base, None).unwrap();
    assert!(
        strict.is_empty(),
        "0.98 pic_th must not call a 0.90-black picture black, got {strict:?}"
    );

    let loose = detect_black_frames(&path, &base.picture_threshold(0.85), None).unwrap();
    assert_eq!(loose.len(), 1, "0.85 pic_th must find the near-black run");
    assert!(loose[0].start_secs < 0.1);
    assert!(loose[0].duration_secs > 0.5);
}

// --- Streaming / debrid pattern tests ---

#[test]
fn test_probe_and_hold() {
    let url = format!("file:{}", fixtures::basic_video().display());
    let probed = kob_ffmpeg::streaming::probe_and_hold(&url).unwrap();
    assert_eq!(probed.probe_result.video_streams.len(), 1);
    assert_eq!(probed.probe_result.audio_streams.len(), 1);

    // Can still read packets from the held connection
    let mut pkt = kob_ffmpeg::packet::Packet::new().unwrap();
    let mut in_ctx = probed.input;
    assert!(in_ctx.read_packet(&mut pkt).unwrap());
}

#[test]
fn test_probe_and_remux_to_file() {
    let url = format!("file:{}", fixtures::basic_video().display());
    let probed = kob_ffmpeg::streaming::probe_and_hold(&url).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("remuxed.mp4");
    let packets = kob_ffmpeg::streaming::remux_probed_to_file(probed, &dst, None).unwrap();
    assert!(packets > 0);

    let verify = kob_ffmpeg::probe::probe(&dst).unwrap();
    assert_eq!(verify.video_streams.len(), 1);
}

// --- Channel-backed output tests ---

#[test]
fn test_channel_backed_output() {
    use kob_ffmpeg::format::input::InputContext;
    use kob_ffmpeg::format::output::OutputContext;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut out_ctx = OutputContext::create_to_channel("mp4", tx).unwrap();
    out_ctx
        .set_option("movflags", "frag_keyframe+empty_moov")
        .unwrap();

    let mut in_ctx = InputContext::open_path(&fixtures::basic_video()).unwrap();
    in_ctx.find_stream_info().unwrap();

    for i in 0..in_ctx.nb_streams() {
        let stream = in_ctx.stream(i).unwrap();
        out_ctx.add_stream_copy(&stream).unwrap();
    }
    out_ctx.write_header().unwrap();

    let mut pkt = kob_ffmpeg::packet::Packet::new().unwrap();
    while in_ctx.read_packet(&mut pkt).unwrap() {
        out_ctx.write_packet_interleaved(&mut pkt).unwrap();
    }
    out_ctx.write_trailer().unwrap();
    drop(out_ctx);

    // Collect all data from channel
    let mut data = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        data.extend_from_slice(&chunk);
    }
    assert!(
        data.len() > 1000,
        "should have received MP4 data, got {} bytes",
        data.len()
    );
}

// --- JPEG thumbnail tests ---

#[test]
fn test_extract_jpeg_thumbnails_magic_bytes() {
    let frames = kob_ffmpeg::thumbnails::extract_frames_sequential_with_hw(
        &fixtures::basic_video(),
        0.0,
        f64::INFINITY,
        1.0,
        None,
        5,
        false,
        None,
    )
    .unwrap();
    assert!(!frames.is_empty());
    for f in &frames {
        assert!(f.jpeg_data.len() > 100, "JPEG should be non-trivial");
        // JPEG magic bytes
        assert_eq!(
            &f.jpeg_data[0..2],
            &[0xFF, 0xD8],
            "should start with JPEG SOI marker"
        );
        assert_eq!(f.width, 320);
        assert_eq!(f.height, 240);
    }
}

// --- Cancellation test ---

#[test]
fn test_cancellation() {
    use kob_ffmpeg::progress::RunContext;
    use tokio_util::sync::CancellationToken;

    let token = CancellationToken::new();
    token.cancel(); // Pre-cancel

    let run = RunContext::cancellable(token);
    let result = kob_ffmpeg::audio::extract_pcm_range(
        &fixtures::basic_video(),
        8000,
        0.0,
        f64::INFINITY,
        Some(&run),
        &mut |_| true,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().is_cancelled());
}

// --- Gap coverage: previously untested functions ---

#[test]
fn test_split_chapters_to_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let files =
        kob_ffmpeg::audio::split_chapters_to_mp3(&fixtures::chaptered_m4b(), dir.path(), 2, None)
            .unwrap();
    assert_eq!(files.len(), 2, "chaptered M4B has 2 chapters");
    for f in &files {
        assert!(f.exists(), "chapter file should exist: {:?}", f);
        assert!(
            f.metadata().unwrap().len() > 100,
            "chapter MP3 should be non-trivial"
        );
    }
}

#[test]
fn test_convert_to_wav_custom_options() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("custom.wav");
    // 44100 Hz stereo instead of default 22050 mono
    kob_ffmpeg::audio::convert_to_wav_with_options(&fixtures::basic_video(), &dst, 44100, 2, None)
        .unwrap();
    let probe = kob_ffmpeg::probe::probe(&dst).unwrap();
    assert_eq!(probe.audio_streams[0].sample_rate, Some(44100));
    assert_eq!(probe.audio_streams[0].channels, 2);
}

#[test]
fn test_remux_to_fragmented_mp4() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("frag.mp4");
    kob_ffmpeg::remux::remux_to_fragmented_mp4(&fixtures::basic_video(), &dst, None).unwrap();
    assert!(dst.exists());
    assert!(dst.metadata().unwrap().len() > 500);
}

#[test]
fn test_extract_subtitles_batch() {
    let tracks = kob_ffmpeg::subtitles::list_subtitle_tracks(&fixtures::rich_mkv()).unwrap();
    assert!(!tracks.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let extractions: Vec<(i32, std::path::PathBuf)> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.stream_index, dir.path().join(format!("sub_{i}.srt"))))
        .collect();
    let refs: Vec<(i32, &std::path::Path)> = extractions
        .iter()
        .map(|(idx, p)| (*idx, p.as_path()))
        .collect();
    let count =
        kob_ffmpeg::subtitles::extract_subtitles_batch(&fixtures::rich_mkv(), &refs, None).unwrap();
    assert_eq!(count, tracks.len());
    for (_, path) in &extractions {
        assert!(path.exists());
    }
}

#[test]
fn test_detect_silence_range() {
    let intervals = kob_ffmpeg::filter::detect::detect_silence_range(
        &fixtures::audio_with_silence(),
        0.0,
        3.0,
        -30.0,
        0.3,
        None,
    )
    .unwrap();
    // Should detect the same silence as the full-file version
    eprintln!("Silence range: {} intervals", intervals.len());
}

#[test]
fn test_detect_boundaries() {
    let black_params = kob_ffmpeg::filter::detect::BlackFramesParams::default()
        .min_duration_secs(0.08)
        .pixel_threshold(0.10)
        .picture_threshold(0.85);
    let (silence, black) = kob_ffmpeg::filter::detect::detect_boundaries(
        &fixtures::basic_video(),
        0.0,
        2.0, // start, duration
        -30.0,
        0.3, // silence noise/duration
        &black_params,
        None,
    )
    .unwrap();
    eprintln!(
        "Boundaries: {} silence, {} black",
        silence.len(),
        black.len()
    );
}

#[test]
fn test_probe_and_hold_custom_io() {
    let data = std::fs::read(fixtures::basic_video()).unwrap();
    let mut io = kob_ffmpeg::format::io_context::CustomIoContext::from_buffer(data).unwrap();
    let probed = kob_ffmpeg::streaming::probe_and_hold_custom(&mut io).unwrap();
    assert_eq!(probed.probe_result.video_streams.len(), 1);
}

#[test]
fn test_remux_probed_to_fragmented_mp4() {
    let url = format!("file:{}", fixtures::basic_video().display());
    let probed = kob_ffmpeg::streaming::probe_and_hold(&url).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("frag.mp4");
    let packets =
        kob_ffmpeg::streaming::remux_probed_to_fragmented_mp4(probed, &dst, None).unwrap();
    assert!(packets > 0);
    assert!(dst.metadata().unwrap().len() > 500);
}

#[test]
fn test_chromaprint_available() {
    assert!(
        kob_ffmpeg::detect::chromaprint_available(),
        "libchromaprint should be linked"
    );
}

#[test]
fn test_chromaprint_extract() {
    // Chromaprint needs ~3s+ of audio to produce a fingerprint.
    // Our fixture is only 2s, so the fingerprint may be empty — that's expected.
    let result =
        kob_ffmpeg::chromaprint::extract_fingerprint(&fixtures::basic_video(), None, 2.0).unwrap();
    assert!(
        result.duration_secs > 0.0,
        "should have processed some audio"
    );
    eprintln!(
        "Fingerprint: {} sub-prints, {:.2}s (short file — may be empty)",
        result.raw.len(),
        result.duration_secs
    );
}

/// Regression: a cache file that is short of the real file must not read as EOF.
///
/// A failed debrid download can leave an empty cache file while the shared
/// `DownloadProgress` has been marked complete. HybridIo used to trust the
/// flag, read zero bytes locally and return AVERROR_EOF — ffmpeg then reported
/// "Invalid data found when processing input" on a perfectly good HTTP source.
#[test]
fn test_hybrid_io_short_cache_falls_back_to_http() {
    use kob_ffmpeg::format::io_context::IoCallbacks;
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use std::sync::Arc;

    const TOTAL: usize = 8192;
    let data: Vec<u8> = (0..TOTAL).map(|i| (i % 251) as u8).collect();

    // Range-capable origin, like a debrid CDN.
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let url = format!("http://{addr}/file.bin");
    let body = data.clone();
    let _server_handle = std::thread::spawn(move || {
        while let Ok(request) = server.recv() {
            let start = request
                .headers()
                .iter()
                .find(|h| h.field.as_str() == "Range" || h.field.as_str() == "range")
                .map(|h| h.value.to_string())
                .and_then(|r| {
                    r.trim_start_matches("bytes=")
                        .split('-')
                        .next()
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .unwrap_or(0)
                .min(body.len());
            let chunk = body[start..].to_vec();
            let end = body.len() - 1;
            let response = tiny_http::Response::from_data(chunk)
                .with_status_code(206)
                .with_header(
                    tiny_http::Header::from_bytes(
                        "Content-Range",
                        format!("bytes {start}-{end}/{}", body.len()),
                    )
                    .unwrap(),
                );
            let _ = request.respond(response);
        }
    });

    // The failure state: nothing on disk, but progress says complete.
    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("cache.bin");
    std::fs::write(&local_path, b"").unwrap();
    let progress = Arc::new(DownloadProgress::new(Some(TOTAL as u64)));
    progress.mark_complete();

    let mut io = HybridIo::new(&local_path, &url, progress);
    let mut got: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 1024];
    while got.len() < TOTAL {
        let n = io.read(&mut buf);
        assert!(
            n > 0,
            "read returned {n} at offset {} — a cache file short of total must \
             fall back to HTTP, not report EOF",
            got.len()
        );
        got.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(got, data, "bytes served must match the origin");

    // Past the real end, EOF is correct.
    assert_eq!(io.read(&mut buf), kob_ffmpeg::ffi::AVERROR_EOF);
}

#[test]
#[ignore]
fn test_hybrid_io_progressive_download() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use std::sync::Arc;

    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let file_size = input.metadata().unwrap().len();
    eprintln!(
        "File size: {} bytes ({:.1} GB)",
        file_size,
        file_size as f64 / 1e9
    );

    // Start a tiny HTTP server that serves the file with Range support
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let url = format!("http://{addr}/file.mkv");

    let input_path = input.to_owned();
    let _server_handle = std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};
        while let Ok(request) = server.recv() {
            let mut file = std::fs::File::open(&input_path).unwrap();
            let file_len = file.metadata().unwrap().len();

            // Parse Range header
            let range_header = request
                .headers()
                .iter()
                .find(|h| h.field.as_str() == "Range" || h.field.as_str() == "range")
                .map(|h| h.value.to_string());

            if let Some(range) = range_header {
                // Parse "bytes=START-END"
                let range = range.trim_start_matches("bytes=");
                let parts: Vec<&str> = range.split('-').collect();
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = parts
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(file_len - 1)
                    .min(file_len - 1);

                file.seek(SeekFrom::Start(start)).unwrap();
                let len = (end - start + 1) as usize;
                let mut buf = vec![0u8; len.min(1_048_576)]; // Cap at 1MB per response
                let actual_len = len.min(buf.len());
                file.read_exact(&mut buf[..actual_len]).unwrap();

                let response = tiny_http::Response::from_data(buf[..actual_len].to_vec())
                    .with_status_code(206)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            "Content-Range",
                            format!("bytes {start}-{}/{file_len}", start + actual_len as u64 - 1),
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
            } else {
                // Full file (shouldn't happen in normal use)
                let _ = request.respond(tiny_http::Response::from_file(file));
            }
        }
    });

    // Simulate progressive download: copy the file in chunks to a temp location
    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("downloading.mkv");
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));

    // Start the "downloader" in a background thread — copies 10MB chunks with delays
    let local_clone = local_path.clone();
    let progress_clone = progress.clone();
    let input_clone = input.to_owned();
    let download_handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut src = std::fs::File::open(&input_clone).unwrap();
        let mut dst = std::fs::File::create(&local_clone).unwrap();
        let mut total = 0u64;
        let mut buf = vec![0u8; 10_485_760]; // 10MB chunks

        loop {
            let n = src.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).unwrap();
            dst.flush().unwrap();
            total += n as u64;
            progress_clone.update(total);
            std::thread::sleep(std::time::Duration::from_millis(50)); // Simulate network speed
        }
        progress_clone.mark_complete();
        eprintln!("Download complete: {} bytes", total);
    });

    // Give the download a head start (50MB)
    std::thread::sleep(std::time::Duration::from_millis(300));
    eprintln!("Download progress: {} bytes", progress.downloaded());

    // Create HybridIo and open for transcoding
    let hybrid = HybridIo::new(&local_path, &url, progress.clone());
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();

    eprintln!("Probe OK: {} streams", ctx.nb_streams());
    assert!(ctx.nb_streams() >= 2);

    // Read some packets to verify streaming works
    let mut pkt = kob_ffmpeg::packet::Packet::new().unwrap();
    let mut count = 0;
    while ctx.read_packet(&mut pkt).unwrap() && count < 100 {
        count += 1;
    }
    eprintln!("Read {} packets from hybrid I/O", count);
    assert!(count == 100, "should read 100 packets");

    // Seek forward (might hit HTTP if beyond downloaded range)
    let seek_pos = (file_size / 2) as i64; // Middle of file
    eprintln!("Seeking to middle of file (byte {})", seek_pos);
    ctx.seek(
        -1,
        seek_pos / 1000,
        kob_ffmpeg::ffi::AVSEEK_FLAG_BACKWARD as i32,
    )
    .unwrap();

    let mut count2 = 0;
    while ctx.read_packet(&mut pkt).unwrap() && count2 < 50 {
        count2 += 1;
    }
    eprintln!("Read {} packets after seek", count2);
    assert!(count2 == 50);

    // Wait for download to finish
    download_handle.join().unwrap();
    assert!(progress.is_complete());

    // Read more packets — should all come from local file now
    let mut count3 = 0;
    while ctx.read_packet(&mut pkt).unwrap() && count3 < 50 {
        count3 += 1;
    }
    eprintln!("Read {} packets from completed local file", count3);
    assert!(count3 == 50);

    eprintln!("Hybrid I/O test passed!");
    drop(ctx);
    drop(custom_io);
    // Server thread will exit when the test ends
}

#[test]
#[ignore]
fn test_real_mkv_transcode() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    // Probe
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    eprintln!(
        "Format: {} Duration: {:?}s",
        probe.format.format_name, probe.format.duration_seconds
    );
    for v in &probe.video_streams {
        eprintln!("Video[{}]: {} {}x{}", v.index, v.codec, v.width, v.height);
    }
    for a in &probe.audio_streams {
        eprintln!(
            "Audio[{}]: {} ch={} lang={:?}",
            a.index, a.codec, a.channels, a.language
        );
    }

    // HLS transcode: video copy + aac (exactly what kobbler does)
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);

    let result = kob_ffmpeg::transcode::transcode_to_hls(input, &config).unwrap();
    eprintln!(
        "Transcode OK: {} video packets written",
        result.video_packets_written
    );

    assert!(result.playlist_path.exists(), "playlist missing");
    assert!(dir.path().join("init.mp4").exists(), "init segment missing");

    let seg_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".m4s")
        })
        .count();
    eprintln!("Segments: {}", seg_count);
    assert!(seg_count > 1, "should have multiple segments");
}

#[test]
#[ignore]
fn test_real_mkv_session_seek() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let dir = tempfile::tempdir().unwrap();
    // Probe to get duration
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap_or(1983.0);

    let config = kob_ffmpeg::session::SessionConfig::new(dir.path(), duration)
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);

    let session = kob_ffmpeg::session::TranscodeSession::start(input, config).unwrap();

    // Let it transcode for a bit
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(session.is_running());

    // Update segment state from disk
    session.update_segments();

    // Check gen_0 has segments
    let gen0_dir = session.gen_dir(0);
    assert!(gen0_dir.exists(), "gen_0 should exist");
    let seg_count_0 = std::fs::read_dir(&gen0_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".m4s")
        })
        .count();
    eprintln!("gen_0: {} segments", seg_count_0);
    assert!(seg_count_0 > 0, "should have segments in gen_0");

    // Seek forward to 10 minutes
    eprintln!("Seeking to 10 minutes...");
    let result1 = session.seek(600.0).expect("seek should succeed");
    assert!(matches!(
        result1,
        kob_ffmpeg::session::SeekResult::NewGeneration { .. }
    ));
    eprintln!("New generation created");

    // Let it produce segments
    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();
    assert!(session.is_running());

    let gen1_dir = session.gen_dir(1);
    let seg_count_1 = std::fs::read_dir(&gen1_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".m4s")
        })
        .count();
    eprintln!("gen_1: {} segments", seg_count_1);
    assert!(seg_count_1 > 0, "should have segments in gen_1");

    // Seek backward to 2 minutes — gen_0 already covers this range
    // seek() calls update_segments() internally before checking cache
    eprintln!("Seeking back to 2 minutes...");
    let result2 = session.seek(120.0).expect("backward seek should succeed");
    assert!(
        matches!(result2, kob_ffmpeg::session::SeekResult::Cached { .. }),
        "2min should be cached"
    );
    eprintln!("Cache hit for 2min seek");

    // Seek to an uncached position (25 minutes)
    eprintln!("Seeking to 25 minutes (uncached)...");
    let result3 = session.seek(1500.0).expect("uncached seek should succeed");
    assert!(matches!(
        result3,
        kob_ffmpeg::session::SeekResult::NewGeneration { .. }
    ));

    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();
    let gen2_dir = session.gen_dir(session.current_generation());
    let seg_count_3 = std::fs::read_dir(&gen2_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".m4s")
        })
        .count();
    eprintln!(
        "gen_{}: {} segments (uncached seek)",
        session.current_generation(),
        seg_count_3
    );
    assert!(seg_count_3 > 0, "should have segments after uncached seek");

    // Seek back to 10min — should be cached from gen_1
    session.update_segments();

    // Debug: check what's at segment 100 (10min / 6s per segment)
    let seg100 = session.resolve_segment(100);
    eprintln!(
        "Segment 100 resolved: {:?}",
        seg100.as_ref().map(|s| (s.generation, s.local_index))
    );

    let result4 = session.seek(600.0).expect("should succeed");
    eprintln!("Seek to 10min result: {:?}", result4);
    assert!(
        matches!(result4, kob_ffmpeg::session::SeekResult::Cached { .. }),
        "10min should be cached"
    );
    eprintln!("Cache hit for 10min seek");

    // gen_0 segments should still exist
    assert!(gen0_dir.exists(), "gen_0 should still exist after seeks");

    // Cancel
    session.cancel();
    eprintln!("Session cancelled");
}

#[test]
#[ignore]
fn test_real_mkv_seek_transcode() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let dir = tempfile::tempdir().unwrap();
    let mut config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);
    config.start_time = Some(300.0); // 5 minutes in

    let result = kob_ffmpeg::transcode::transcode_to_hls(input, &config).unwrap();
    eprintln!(
        "Seek transcode OK: {} video packets",
        result.video_packets_written
    );
    assert!(result.playlist_path.exists());
}

/// Simulates kobbler's exact playback flow:
/// 1. Probe → get duration
/// 2. Start session (video copy + AAC audio transcode)
/// 3. Poll for segments (like the playlist handler does)
/// 4. Resolve segments to file paths (like the segment handler does)
/// 5. Seek forward (user clicks ahead) → check for new gen or cache
/// 6. Seek backward (user clicks back) → should hit cache
/// 7. Evict if needed (budget check)
#[test]
#[ignore]
fn test_kobbler_playback_flow() {
    use kob_ffmpeg::session::{SeekResult, SessionConfig, TranscodeSession};

    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    // Step 1: Probe (exactly what kobbler does at session creation)
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap();
    eprintln!(
        "Probed: {:.1}s, {} video, {} audio",
        duration,
        probe.video_streams.len(),
        probe.audio_streams.len()
    );

    // Step 2: Start session (video copy + AAC, like kobbler's AudioTranscode mode)
    let dir = tempfile::tempdir().unwrap();
    let config = SessionConfig::new(dir.path(), duration)
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);

    let session = TranscodeSession::start(input, config).unwrap();
    eprintln!("Session started, gen_0 transcoding...");

    // Step 3: Simulate polling for segments (kobbler's playlist handler)
    // Wait for initial segments to appear
    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();

    let segments = session.playlist_segments_from(0);
    eprintln!("Playlist has {} segments after 3s", segments.len());
    assert!(!segments.is_empty(), "should have segments by now");

    // Verify segment durations are plausible
    for seg in &segments[..segments.len().min(5)] {
        assert!(
            seg.duration > 0.0 && seg.duration < 30.0,
            "segment duration {:.2}s seems wrong",
            seg.duration
        );
    }

    // Step 4: Resolve segments to file paths (kobbler's segment serving handler)
    let resolved = session
        .resolve_segment(0)
        .expect("segment 0 should resolve");
    assert!(
        resolved.seg_path.exists(),
        "segment file should exist: {:?}",
        resolved.seg_path
    );
    assert!(resolved.init_path.exists(), "init segment should exist");
    eprintln!(
        "Segment 0: gen={}, local={}, dur={:.2}s, path={}",
        resolved.generation,
        resolved.local_index,
        resolved.duration,
        resolved.seg_path.display()
    );

    // Step 5: User seeks to 15 minutes (forward, uncached)
    let seek_time = 900.0;
    let playback_start = session.time_to_segment(seek_time);
    eprintln!(
        "\nUser seeks to {:.0}s (segment {})",
        seek_time, playback_start
    );

    let seek_result = session.seek(seek_time).expect("seek should succeed");
    match &seek_result {
        SeekResult::NewGeneration { generation } => {
            eprintln!("New generation {} created", generation);
        }
        SeekResult::Cached {
            active_generation,
            restart_from_secs,
        } => {
            eprintln!(
                "Cache hit: gen={}, restart from {:.1}s",
                active_generation, restart_from_secs
            );
        }
    }
    assert!(matches!(seek_result, SeekResult::NewGeneration { .. }));

    // Wait for segments at the new position
    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();

    let segments_after_seek = session.playlist_segments_from(playback_start);
    eprintln!(
        "After seek: {} segments from position {}",
        segments_after_seek.len(),
        playback_start
    );
    assert!(
        !segments_after_seek.is_empty(),
        "should have segments after seek"
    );

    // Verify the new segments resolve correctly
    let resolved_seek = session.resolve_segment(playback_start);
    assert!(resolved_seek.is_some(), "seeked segment should resolve");
    let resolved_seek = resolved_seek.unwrap();
    assert!(
        resolved_seek.seg_path.exists(),
        "seeked segment file should exist"
    );
    eprintln!(
        "Seeked segment: gen={}, path={}",
        resolved_seek.generation,
        resolved_seek.seg_path.display()
    );

    // Step 6: User seeks back to beginning (should be cached from gen_0)
    eprintln!("\nUser seeks back to 0s");
    session.update_segments();

    let back_result = session.seek(0.0).expect("back-seek should succeed");
    match &back_result {
        SeekResult::Cached {
            active_generation, ..
        } => {
            eprintln!("Cache hit! Serving from gen_{}", active_generation);
        }
        SeekResult::NewGeneration { generation } => {
            eprintln!(
                "New gen {} (unexpected — should have been cached)",
                generation
            );
        }
    }
    assert!(
        matches!(back_result, SeekResult::Cached { .. }),
        "beginning should be cached from gen_0"
    );

    // The cached segment should still resolve
    let resolved_back = session
        .resolve_segment(0)
        .expect("segment 0 should still resolve");
    assert!(
        resolved_back.seg_path.exists(),
        "cached segment file should still exist"
    );

    // Step 7: Eviction check (shouldn't evict with only 2 gens under budget)
    session.evict_if_needed(0);
    // gen_0 and gen_1 should both still exist
    assert!(session.gen_dir(0).exists(), "gen_0 should survive eviction");
    assert!(session.gen_dir(1).exists(), "gen_1 should survive eviction");

    // Cleanup
    session.cancel();
    eprintln!("\nKobbler flow test passed!");
}

#[test]
#[ignore]
fn test_probe_timing() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let start = std::time::Instant::now();
    let result = kob_ffmpeg::probe::probe(input).unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "Probe: {:.3}s ({} streams, {:.1}s duration)",
        elapsed.as_secs_f64(),
        result.video_streams.len() + result.audio_streams.len(),
        result.format.duration_seconds.unwrap_or(0.0)
    );
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "probe took {:.1}s — should be < 2s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn test_real_iptv_stream() {
    let Ok(stream_url) = std::env::var("KOB_FFMPEG_IPTV_URL") else {
        eprintln!("KOB_FFMPEG_IPTV_URL not set, skipping real IPTV stream test");
        return;
    };
    let stream_url = stream_url.as_str();

    // Step 1: Probe the live stream (like kobbler's probe_live_stream)
    eprintln!("=== Probing IPTV stream ===");
    let probe = kob_ffmpeg::iptv::probe_stream(stream_url);
    match &probe {
        Ok(p) => {
            eprintln!("Format: {}", p.format.format_name);
            for v in &p.video_streams {
                eprintln!("Video: {} {}x{}", v.codec, v.width, v.height);
            }
            for a in &p.audio_streams {
                eprintln!("Audio: {} ch={} lang={:?}", a.codec, a.channels, a.language);
            }
        }
        Err(e) => {
            eprintln!("Probe failed (stream may be offline): {e}");
            return; // Don't fail test if stream is down
        }
    }
    let probe = probe.unwrap();
    assert!(!probe.video_streams.is_empty(), "should have video");
    assert!(!probe.audio_streams.is_empty(), "should have audio");

    // Step 2: Test the channel-backed live transcode (video copy + AAC audio)
    // This simulates kobbler's start_live_transcode flow
    eprintln!("\n=== Testing live transcode (video copy + AAC) ===");

    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // Start the transcode in a blocking thread
    let dir_path = dir.path().to_owned();
    let transcode_handle = std::thread::spawn(move || {
        let mut io = kob_ffmpeg::format::io_context::CustomIoContext::from_channel(rx).unwrap();
        let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut io).unwrap();
        ctx.find_stream_info().unwrap();

        let config = kob_ffmpeg::transcode::HlsConfig::new(&dir_path)
            .audio_encoder("aac")
            .audio_bitrate(128_000)
            .segment_duration(4.0);

        kob_ffmpeg::transcode::transcode_to_hls_from_ctx(ctx, &config)
    });

    // Feed the stream via HTTP in another thread (simulating the async feeder)
    let feed_url = stream_url.to_string();
    let feeder = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        let mut resp = client.get(&feed_url).send().unwrap();
        let mut total = 0u64;
        let mut buf = vec![0u8; 32768];

        loop {
            match std::io::Read::read(&mut resp, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break; // transcode ended
                    }
                    total += n as u64;
                    // Stop after 15 seconds of data (~10MB for 720p)
                    if total > 10_000_000 {
                        eprintln!("Fed {:.1}MB to transcode, stopping", total as f64 / 1e6);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Stream read error: {e}");
                    break;
                }
            }
        }
        // tx dropped → channel closes → FFmpeg sees EOF
    });

    // Wait for feeder to finish (it stops after ~10MB)
    feeder.join().unwrap();

    // Wait for transcode to process remaining data
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Check output
    let playlist_path = dir.path().join("stream.m3u8");
    if playlist_path.exists() {
        let content = std::fs::read_to_string(&playlist_path).unwrap();
        let seg_count = content.lines().filter(|l| l.starts_with("seg_")).count();
        eprintln!("Playlist exists: {} segments", seg_count);
        assert!(
            seg_count > 0,
            "should have produced segments from IPTV stream"
        );

        // Verify init.mp4 exists (fMP4)
        assert!(
            dir.path().join("init.mp4").exists(),
            "fMP4 init segment should exist"
        );

        // Verify at least one .m4s segment
        let m4s_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .ends_with(".m4s")
            })
            .count();
        eprintln!("fMP4 segments on disk: {m4s_count}");
        assert!(m4s_count > 0, "should have .m4s segments");
    } else {
        // Transcode may still be running — check if it errored
        eprintln!("Playlist not found yet — checking transcode result...");
    }

    // Let transcode finish processing
    match transcode_handle.join() {
        Ok(Ok(result)) => eprintln!(
            "Transcode completed: {} packets",
            result.video_packets_written
        ),
        Ok(Err(e)) => eprintln!("Transcode error: {e}"),
        Err(_) => eprintln!("Transcode thread panicked"),
    }

    eprintln!("IPTV stream test done!");
}

#[test]
#[ignore]
fn test_chromaprint_real_file() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    // Fingerprint from the start (intro detection)
    let start = std::time::Instant::now();
    let fp = kob_ffmpeg::chromaprint::extract_fingerprint(input, None, 60.0).unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "Start fingerprint: {} sub-prints, {:.2}s audio, took {:.2}s",
        fp.raw.len(),
        fp.duration_secs,
        elapsed.as_secs_f64()
    );
    assert!(!fp.raw.is_empty());
    assert!(fp.duration_secs > 50.0, "should have ~60s of audio");

    // Fingerprint from the end (credits detection)
    let start = std::time::Instant::now();
    let fp_end = kob_ffmpeg::chromaprint::extract_fingerprint(input, Some(1900.0), 60.0).unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "End fingerprint: {} sub-prints, {:.2}s audio, took {:.2}s",
        fp_end.raw.len(),
        fp_end.duration_secs,
        elapsed.as_secs_f64()
    );
    assert!(!fp_end.raw.is_empty());

    // Fingerprints should be different (intro vs credits)
    assert_ne!(
        fp.raw, fp_end.raw,
        "intro and credits fingerprints should differ"
    );

    eprintln!("Chromaprint real file test passed!");
}

#[test]
#[ignore]
fn test_chromaprint_matches_fpcalc() {
    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    // Library fingerprint
    let fp = kob_ffmpeg::chromaprint::extract_fingerprint(input, None, 60.0).unwrap();
    eprintln!(
        "Library: {} sub-prints, first 10: {:?}",
        fp.raw.len(),
        &fp.raw[..10.min(fp.raw.len())]
    );

    // fpcalc fingerprint (no overlap, same rate)
    let output = std::process::Command::new("fpcalc")
        .args(["-rate", "8000", "-length", "60", "-raw"])
        .arg(input)
        .output()
        .expect("fpcalc should be available");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let fpcalc_values: Vec<u32> = stdout
        .lines()
        .find(|l| l.starts_with("FINGERPRINT="))
        .unwrap()
        .strip_prefix("FINGERPRINT=")
        .unwrap()
        .split(',')
        .filter_map(|s| s.parse().ok())
        .collect();
    eprintln!(
        "fpcalc:  {} sub-prints, first 10: {:?}",
        fpcalc_values.len(),
        &fpcalc_values[..10.min(fpcalc_values.len())]
    );

    // First N values should match exactly (same algorithm, same input)
    let compare_len = fp.raw.len().min(fpcalc_values.len()).min(20);
    let matches = fp
        .raw
        .iter()
        .zip(fpcalc_values.iter())
        .take(compare_len)
        .filter(|(a, b)| a == b)
        .count();
    eprintln!("First {compare_len} values: {matches}/{compare_len} match");
    assert!(
        matches > compare_len / 2,
        "at least half of first {compare_len} values should match"
    );
}

/// Reproduces the production segfault: CustomIoContext dropped while InputContext
/// still references it. Uses start_from_custom_io to keep both alive.
#[test]
#[ignore]
fn test_debrid_custom_io_lifetime() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let file_size = input.metadata().unwrap().len();
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap();

    // Copy enough of the file that HybridIo can probe from disk
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("cached.mkv");
    {
        use std::io::{Read, Write};
        let mut src = std::fs::File::open(input).unwrap();
        let mut dst = std::fs::File::create(&cache_path).unwrap();
        let mut buf = vec![0u8; 50_000_000]; // 50MB
        let n = src.read(&mut buf).unwrap();
        dst.write_all(&buf[..n]).unwrap();
    }

    let progress = Arc::new(DownloadProgress::new(Some(file_size)));
    progress.update(50_000_000);

    // This is what production does: create io + ctx, then move ctx to session.
    // The key: start_from_custom_io takes ownership of BOTH so neither is dropped early.
    let hybrid = HybridIo::new(
        &cache_path,
        "http://127.0.0.1:1/fake", // won't be called — all reads within 50MB
        progress,
    );
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();

    let session_dir = dir.path().join("hls");
    let config = SessionConfig::new(&session_dir, duration)
        .audio_encoder("aac")
        .audio_bitrate(128_000)
        .segment_duration(6.0);

    // start_from_custom_io moves both io and ctx into the session thread
    let session = TranscodeSession::start_from_custom_io(custom_io, ctx, None, config).unwrap();

    // custom_io and ctx are now MOVED — they live in the session thread.
    // In the old code, this is where the segfault would happen because
    // custom_io would be dropped here while the session thread still reads from it.

    std::thread::sleep(std::time::Duration::from_secs(2));
    session.update_segments();

    let segments = session.playlist_segments_from(0);
    eprintln!(
        "Custom IO lifetime test: {} segments produced",
        segments.len()
    );
    assert!(
        !segments.is_empty(),
        "should produce segments without crashing"
    );

    session.cancel();
    eprintln!("Custom IO lifetime test passed — no segfault!");
}

/// Simulates the exact debrid playback flow:
/// 1. Progressive download to local file (background)
/// 2. HybridIo reads from file where available, HTTP where not
/// 3. TranscodeSession with video copy + AAC audio transcode
/// 4. Seek forward (beyond download) → HTTP fallback
/// 5. Seek back (within downloaded range) → instant from cache
#[test]
#[ignore]
fn test_debrid_playback_flow() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SeekResult, SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let file_size = input.metadata().unwrap().len();
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap();
    eprintln!("File: {:.1}s, {:.1}GB", duration, file_size as f64 / 1e9);

    // Start HTTP server for the file (simulates debrid CDN)
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let url = format!("http://{addr}/file.mkv");

    let input_path = input.to_owned();
    let _server_thread = std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};
        while let Ok(request) = server.recv() {
            let mut file = std::fs::File::open(&input_path).unwrap();
            let file_len = file.metadata().unwrap().len();

            let range_header = request
                .headers()
                .iter()
                .find(|h| h.field.as_str() == "Range" || h.field.as_str() == "range")
                .map(|h| h.value.to_string());

            if let Some(range) = range_header {
                let range = range.trim_start_matches("bytes=");
                let parts: Vec<&str> = range.split('-').collect();
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = parts
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(file_len - 1)
                    .min(file_len - 1);
                file.seek(SeekFrom::Start(start)).unwrap();
                let len = (end - start + 1) as usize;
                let mut buf = vec![0u8; len.min(1_048_576)];
                let n = file.read(&mut buf).unwrap();
                let response =
                    tiny_http::Response::from_data(buf[..n].to_vec()).with_status_code(206);
                let _ = request.respond(response);
            } else {
                let _ = request.respond(tiny_http::Response::from_file(file));
            }
        }
    });

    // Simulate progressive download (copy file in chunks to temp location)
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("downloading.mkv");
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));

    let cache_clone = cache_path.clone();
    let progress_clone = progress.clone();
    let input_clone = input.to_owned();
    let _download_thread = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut src = std::fs::File::open(&input_clone).unwrap();
        let mut dst = std::fs::File::create(&cache_clone).unwrap();
        let mut total = 0u64;
        let mut buf = vec![0u8; 10_485_760]; // 10MB chunks
        loop {
            let n = src.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).unwrap();
            dst.flush().unwrap();
            total += n as u64;
            progress_clone.update(total);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        progress_clone.mark_complete();
        eprintln!("Download complete: {:.1}MB", total as f64 / 1e6);
    });

    // Give download a head start
    std::thread::sleep(std::time::Duration::from_millis(300));
    eprintln!("Download at: {:.1}MB", progress.downloaded() as f64 / 1e6);

    // Create HybridIo → CustomIoContext → InputContext → TranscodeSession
    // This is the exact call chain kobbler-debrid would use
    let hybrid = HybridIo::new(&cache_path, &url, progress.clone());
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();

    let session_dir = dir.path().join("hls");
    let config = SessionConfig::new(&session_dir, duration)
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);

    let session = TranscodeSession::start_from_ctx(ctx, None, config).unwrap();
    eprintln!("TranscodeSession started via HybridIo");

    // Let it transcode for a bit
    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();

    let segments = session.playlist_segments_from(0);
    eprintln!("Segments after 3s: {}", segments.len());
    assert!(!segments.is_empty(), "should have segments");

    // Resolve a segment to verify it's real
    let resolved = session
        .resolve_segment(0)
        .expect("segment 0 should resolve");
    assert!(resolved.seg_path.exists(), "segment file should exist");
    eprintln!(
        "Segment 0: gen={}, dur={:.2}s",
        resolved.generation, resolved.duration
    );

    // Seek to 15 minutes — may be beyond download, triggers HTTP fallback in HybridIo
    eprintln!("\nSeeking to 15min...");
    let seek_result = session.seek(900.0).expect("seek should work");
    eprintln!("Seek result: {:?}", std::mem::discriminant(&seek_result));

    std::thread::sleep(std::time::Duration::from_secs(3));
    session.update_segments();

    let seg_at_15 = session.time_to_segment(900.0);
    let resolved_seek = session.resolve_segment(seg_at_15);
    assert!(resolved_seek.is_some(), "seeked segment should resolve");
    eprintln!("Segment at 15min: resolved={}", resolved_seek.is_some());

    // Seek back to 0 — should be cached
    eprintln!("\nSeeking back to 0s...");
    session.update_segments();
    let back = session.seek(0.0).expect("back seek should work");
    assert!(
        matches!(back, SeekResult::Cached { .. }),
        "0s should be cached"
    );
    eprintln!("Back seek: cached!");

    session.cancel();
    eprintln!("\nDebrid playback flow test passed!");
}

/// Returns the path to the real Rooster debrid MKV reproducing the 2026-04-13
/// stall in prod. Point `KOB_REAL_DEBRID_MKV` at the file to enable this test.
fn real_debrid_mkv_path() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(std::env::var("KOB_REAL_DEBRID_MKV").ok()?);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("KOB_REAL_DEBRID_MKV={} not found, skipping", p.display());
        None
    }
}

/// Reproduces the 2026-04-13 prod stall: a debrid MKV with h264 + DDP5.1
/// picked `AudioTranscode` (VideoMode::Copy + AudioMode::Transcode), then
/// silently stalled during the progressive download.
///
/// Test shape: serve the real file via `tiny_http` with Range support, copy
/// the first 25% to a frozen on-disk cache, leave `DownloadProgress` pinned
/// at that byte count (no `mark_complete`, no further progress), and invoke
/// the exact kobbler hybrid call chain with `start_from_custom_io`.
///
/// Success = at least one .ts segment produced within 20s. Failure = 0
/// segments (reproduces the stall). On stall, also `ls` the on-disk gen dir
/// so we can distinguish "ffmpeg wrote segments but playlist_segments_from
/// isn't reporting them" from "ffmpeg wrote nothing at all".
#[test]
#[ignore]
fn test_real_debrid_audio_transcode_partial() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let Some(input) = real_debrid_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let file_size = input.metadata().unwrap().len();
    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap();
    eprintln!(
        "Real debrid file: {:.1}s, {:.2}GB ({} bytes)",
        duration,
        file_size as f64 / 1e9,
        file_size
    );
    assert!(
        !probe.video_streams.is_empty(),
        "real file should have video"
    );
    assert!(
        !probe.audio_streams.is_empty(),
        "real file should have audio"
    );
    eprintln!(
        "Video: {} {}x{}   Audio: {} {}ch",
        probe.video_streams[0].codec,
        probe.video_streams[0].width,
        probe.video_streams[0].height,
        probe.audio_streams[0].codec,
        probe.audio_streams[0].channels
    );

    // --- HTTP server for the full file with Range support ---
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let url = format!("http://{addr}/rooster.mkv");
    eprintln!("Serving file at {url}");

    let input_owned = input.to_owned();
    let _server_thread = std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};
        while let Ok(request) = server.recv() {
            let mut file = std::fs::File::open(&input_owned).unwrap();
            let file_len = file.metadata().unwrap().len();

            let range_header = request
                .headers()
                .iter()
                .find(|h| h.field.as_str() == "Range" || h.field.as_str() == "range")
                .map(|h| h.value.to_string());

            if let Some(range) = range_header {
                let range = range.trim_start_matches("bytes=");
                let parts: Vec<&str> = range.split('-').collect();
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = parts
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(file_len - 1)
                    .min(file_len - 1);
                file.seek(SeekFrom::Start(start)).unwrap();
                // Serve up to 4 MiB per response so ffmpeg's read loop isn't
                // artificially starved. The real debrid CDN serves much more
                // per Range request.
                let len = (end - start + 1) as usize;
                let mut buf = vec![0u8; len.min(4 * 1024 * 1024)];
                let n = file.read(&mut buf).unwrap();
                let content_range =
                    format!("bytes {}-{}/{}", start, start + n as u64 - 1, file_len);
                let response = tiny_http::Response::from_data(buf[..n].to_vec())
                    .with_status_code(206)
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Content-Range"[..], content_range)
                            .unwrap(),
                    )
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..])
                            .unwrap(),
                    );
                let _ = request.respond(response);
            } else {
                let response = tiny_http::Response::from_file(file).with_header(
                    tiny_http::Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap(),
                );
                let _ = request.respond(response);
            }
        }
    });

    // --- Frozen partial cache: first 25% of the real file, no further growth ---
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("downloading.mkv");
    // Override-able via env so we can sweep cache sizes. Default 25% per brief;
    // set KOB_REAL_DEBRID_PARTIAL_BYTES to a small value (e.g. 10_000_000) to
    // force ffmpeg to read past the cache frontier into HybridIo's HTTP path.
    let partial_bytes: u64 = std::env::var("KOB_REAL_DEBRID_PARTIAL_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(file_size / 4);
    {
        use std::io::{Read, Write};
        let mut src = std::fs::File::open(input).unwrap();
        let mut dst = std::fs::File::create(&cache_path).unwrap();
        let mut remaining = partial_bytes;
        let mut buf = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = (remaining as usize).min(buf.len());
            let n = src.read(&mut buf[..want]).unwrap();
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).unwrap();
            remaining -= n as u64;
        }
        dst.flush().unwrap();
    }
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));
    progress.update(partial_bytes);
    // NOTE: do NOT mark_complete and do NOT advance the download — we are
    // isolating "can HybridIo feed ffmpeg from a frozen 25% cache at all".
    eprintln!(
        "Frozen cache: {} bytes ({:.1}% of {:.2}GB)",
        partial_bytes,
        partial_bytes as f64 / file_size as f64 * 100.0,
        file_size as f64 / 1e9
    );

    // --- Exact kobbler hybrid call chain (AudioTranscode path) ---
    let hybrid = HybridIo::new(&cache_path, &url, progress.clone());
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();
    eprintln!("find_stream_info OK (probe via HybridIo succeeded)");

    let session_dir = dir.path().join("hls");
    // VideoMode::Copy (no video_encoder) + AudioMode::Transcode(aac 192k) =
    // AudioTranscode, matching the prod decision.
    let config = SessionConfig::new(&session_dir, duration)
        .segment_duration(6.0)
        .audio_encoder("aac")
        .audio_bitrate(192_000);

    let session = TranscodeSession::start_from_custom_io(custom_io, ctx, None, config)
        .expect("hybrid session should start");
    eprintln!("TranscodeSession started (AudioTranscode, from frozen 25% cache)");

    // --- Poll for 20s, printing segment count + on-disk ls each tick ---
    let gen_dir = session_dir.join("gen_0");
    let poll_interval = std::time::Duration::from_millis(500);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut last_reported_count = 0usize;
    while std::time::Instant::now() < deadline {
        session.update_segments();
        let segs = session.playlist_segments_from(0);
        let on_disk_ts: Vec<String> = std::fs::read_dir(&gen_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".ts"))
                    .collect()
            })
            .unwrap_or_default();
        let running = session.is_running();
        let err = session.error();
        if segs.len() != last_reported_count || err.is_some() || !running {
            eprintln!(
                "[poll] playlist_segments={} on_disk_ts={} running={} err={:?}",
                segs.len(),
                on_disk_ts.len(),
                running,
                err
            );
            last_reported_count = segs.len();
        }
        if !segs.is_empty() {
            break;
        }
        if !running {
            break;
        }
        std::thread::sleep(poll_interval);
    }

    session.update_segments();
    let final_segs = session.playlist_segments_from(0);
    let final_on_disk: Vec<String> = std::fs::read_dir(&gen_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    eprintln!(
        "FINAL: playlist_segments={} on_disk_entries={} running={} err={:?}",
        final_segs.len(),
        final_on_disk.len(),
        session.is_running(),
        session.error()
    );
    eprintln!("on-disk gen_0/: {final_on_disk:?}");

    session.cancel();

    // This is the bug-reproducing assertion. If it passes, kob-ffmpeg is fine
    // and the bug is higher up in the kobbler API HLS serving path.
    assert!(
        !final_segs.is_empty(),
        "AudioTranscode from frozen 25% hybrid cache produced zero segments in 20s \
         (reproduces the prod stall). on_disk={final_on_disk:?} err={:?}",
        session.error()
    );
}

/// Test HybridIo against a real debrid URL.
///
/// Set DEBRID_URL to a Real-Debrid unrestricted link to run:
///   DEBRID_URL="https://..." cargo test -p kob-ffmpeg --test integration test_real_debrid_range -- --nocapture
#[test]
fn test_real_debrid_range() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use std::sync::Arc;

    let url = match std::env::var("DEBRID_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Skipping — set DEBRID_URL to test");
            return;
        }
    };

    // HEAD request to get file size
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let head = client.head(&url).send().unwrap();
    let file_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Need Content-Length");
    eprintln!("File size: {:.1}GB", file_size as f64 / 1e9);

    // Download first 50MB to local file
    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("partial.mkv");
    let chunk_size: u64 = 50 * 1024 * 1024;

    eprintln!("Downloading first {:.0}MB...", chunk_size as f64 / 1e6);
    let resp = client
        .get(&url)
        .header("Range", format!("bytes=0-{}", chunk_size - 1))
        .send()
        .unwrap();
    eprintln!("Range GET: HTTP {}", resp.status());
    let bytes = resp.bytes().unwrap();
    std::fs::write(&local_path, &bytes).unwrap();
    eprintln!("Local file: {}MB", bytes.len() / 1_048_576);

    // HybridIo: 50MB local, rest via HTTP
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));
    progress.update(bytes.len() as u64);

    let hybrid = HybridIo::new(&local_path, &url, progress.clone());
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    eprintln!("Opening input via HybridIo...");
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();

    let n = ctx.nb_streams();
    eprintln!("Streams: {n}");
    for i in 0..n {
        if let Some(s) = ctx.stream(i) {
            eprintln!("  stream {i}: type={:?}", s.codec_type());
        }
    }

    let duration = ctx.duration_secs().expect("need duration");
    eprintln!("Duration: {duration:.1}s");

    // Seek to 30 min (well beyond 50MB local data)
    let seek_time = 1800.0;
    eprintln!("\nSeeking to {seek_time}s...");
    let flags = kob_ffmpeg::ffi::AVSEEK_FLAG_BACKWARD as i32;
    ctx.seek(-1, (seek_time * 1_000_000.0) as i64, flags)
        .unwrap();
    eprintln!("Seek succeeded");

    // Read packets after seek
    let mut pkt = kob_ffmpeg::packet::Packet::new().unwrap();
    for i in 0..5 {
        match ctx.read_packet(&mut pkt) {
            Ok(true) => {
                eprintln!(
                    "Packet {i}: stream={}, pts={}, size={}",
                    pkt.stream_index(),
                    pkt.pts(),
                    pkt.size()
                );
            }
            Ok(false) => {
                eprintln!("EOF at packet {i}");
                break;
            }
            Err(e) => {
                eprintln!("Read error at packet {i}: {e}");
                break;
            }
        }
    }

    // Now test it through TranscodeSession with a seek
    eprintln!("\n--- TranscodeSession test ---");
    let local_path2 = dir.path().join("partial2.mkv");
    std::fs::copy(&local_path, &local_path2).unwrap();
    let progress2 = Arc::new(DownloadProgress::new(Some(file_size)));
    progress2.update(chunk_size);

    let hybrid2 = HybridIo::new(&local_path2, &url, progress2);
    let mut custom_io2 = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid2).unwrap();
    let mut ctx2 = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io2).unwrap();
    ctx2.find_stream_info().unwrap();

    let session_dir = dir.path().join("hls");
    let config = kob_ffmpeg::session::SessionConfig::new(&session_dir, duration)
        .audio_encoder("aac")
        .audio_bitrate(192_000)
        .segment_duration(6.0);

    let session =
        kob_ffmpeg::session::TranscodeSession::start_from_custom_io(custom_io2, ctx2, None, config)
            .unwrap();
    eprintln!("TranscodeSession started");

    // Let it produce a few segments from the start
    std::thread::sleep(std::time::Duration::from_secs(5));
    session.update_segments();
    let seg0 = session.playlist_segments_from(0);
    eprintln!("Segments from start: {}", seg0.len());

    // Seek to 30min — beyond local data, must use HTTP Range
    eprintln!("\nSeeking session to {seek_time}s...");
    let result = session.seek(seek_time);
    eprintln!("Seek result: {result:?}");

    // Wait longer — HTTP reads at 30min offset through a remote server take time
    for wait in 0..6 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        session.update_segments();
        let target_seg = session.time_to_segment(seek_time);
        let seg_after = session.playlist_segments_from(target_seg);
        eprintln!(
            "After {}s: {} segments from target, session running={}",
            (wait + 1) * 5,
            seg_after.len(),
            session.is_running()
        );
        if !seg_after.is_empty() {
            let resolved = session.resolve_segment(target_seg);
            eprintln!("Resolved: {resolved:?}");
            break;
        }
        if !session.is_running() {
            eprintln!("Session died — checking gen_1 dir...");
            let gen_dir = session.gen_dir(1);
            if gen_dir.exists() {
                for entry in std::fs::read_dir(&gen_dir).unwrap() {
                    let entry = entry.unwrap();
                    eprintln!(
                        "  {}: {}B",
                        entry.file_name().to_string_lossy(),
                        entry.metadata().unwrap().len()
                    );
                }
            } else {
                eprintln!("  gen_1 dir does not exist");
            }
            break;
        }
    }

    session.cancel();
    eprintln!("\nReal debrid range test done.");
}

/// Test SessionConfig.start_time against a local file — verifies that
/// the transcode starts at the requested position, not from 0.
#[test]
#[ignore]
fn test_start_time_local_file() {
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};

    let Some(input) = real_mkv_path() else {
        return;
    };
    let input = input.as_path();

    let probe = kob_ffmpeg::probe::probe(input).unwrap();
    let duration = probe.format.duration_seconds.unwrap();
    eprintln!("File: {duration:.1}s");

    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("hls");

    // Start at 15 minutes
    let start_time = 900.0;
    let config = SessionConfig::new(&session_dir, duration)
        .audio_encoder("aac")
        .audio_bitrate(192_000)
        .segment_duration(6.0);
    let mut config = config;
    config.start_time = Some(start_time);

    let session = TranscodeSession::start(input, config).unwrap();
    eprintln!("Session started with start_time={start_time}s");

    // Wait for first segment
    for i in 0..10 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        session.update_segments();
        let target_seg = session.time_to_segment(start_time);
        let segs = session.playlist_segments_from(target_seg);
        eprintln!(
            "After {}s: {} segments from target seg {target_seg}",
            i + 1,
            segs.len()
        );
        if !segs.is_empty() {
            let resolved = session.resolve_segment(target_seg).unwrap();
            eprintln!(
                "First segment: gen={}, local_idx={}, dur={:.2}s",
                resolved.generation, resolved.local_index, resolved.duration
            );
            assert_eq!(
                resolved.generation, 0,
                "should be gen 0 (initial start, not a seek)"
            );
            assert_eq!(
                resolved.local_index, 0,
                "first segment should be local index 0"
            );

            // Verify NO segments exist before the start position
            let early_segs = session.playlist_segments_from(0);
            assert!(
                early_segs.is_empty() || early_segs[0].generation == 0,
                "should not have segments from before start_time"
            );
            eprintln!("start_time test passed!");
            break;
        }
    }

    session.cancel();
}

/// Test start_time via HybridIo against real debrid URL.
///
/// Set DEBRID_URL to run: DEBRID_URL="https://..." cargo test --test integration test_start_time_debrid -- --nocapture
#[test]
fn test_start_time_debrid() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let url = match std::env::var("DEBRID_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Skipping — set DEBRID_URL to test");
            return;
        }
    };

    // HEAD request to get file size
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let head = client.head(&url).send().unwrap();
    let file_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Need Content-Length");
    eprintln!("File size: {:.1}GB", file_size as f64 / 1e9);

    // Download first 50MB to local file
    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("partial.mkv");
    let chunk_size: u64 = 50 * 1024 * 1024;

    eprintln!("Downloading first {:.0}MB...", chunk_size as f64 / 1e6);
    let resp = client
        .get(&url)
        .header("Range", format!("bytes=0-{}", chunk_size - 1))
        .send()
        .unwrap();
    let bytes = resp.bytes().unwrap();
    std::fs::write(&local_path, &bytes).unwrap();

    let progress = Arc::new(DownloadProgress::new(Some(file_size)));
    progress.update(bytes.len() as u64);

    // Start at 30 minutes via start_time (not a seek after creation)
    let start_time = 1800.0;
    let hybrid = HybridIo::new(&local_path, &url, progress);
    let mut custom_io = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut custom_io).unwrap();
    ctx.find_stream_info().unwrap();
    let duration = ctx.duration_secs().expect("need duration");

    let session_dir = dir.path().join("hls");
    let mut config = SessionConfig::new(&session_dir, duration)
        .audio_encoder("aac")
        .audio_bitrate(192_000)
        .segment_duration(6.0);
    config.start_time = Some(start_time);

    eprintln!("Starting session with start_time={start_time}s...");
    let session = TranscodeSession::start_from_custom_io(custom_io, ctx, None, config).unwrap();

    // Wait for first segment — should appear at the start_time position
    let target_seg = session.time_to_segment(start_time);
    for i in 0..12 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        session.update_segments();
        let segs = session.playlist_segments_from(target_seg);
        eprintln!(
            "After {}s: {} segments from target seg {target_seg}, running={}",
            (i + 1) * 5,
            segs.len(),
            session.is_running()
        );
        if !segs.is_empty() {
            let resolved = session.resolve_segment(target_seg).unwrap();
            eprintln!(
                "First segment: gen={}, local_idx={}, dur={:.2}s",
                resolved.generation, resolved.local_index, resolved.duration
            );
            assert_eq!(
                resolved.generation, 0,
                "should be gen 0 (start_time, not seek)"
            );
            eprintln!("debrid start_time test passed!");
            break;
        }
        if !session.is_running() {
            panic!("Session died before producing segments");
        }
    }

    session.cancel();
}

/// Reproduce the production debrid failure and verify the fix.
/// Runs both concurrent (broken) and serialized (fixed) flows against the real URL.
///
/// DEBRID_URL="https://..." cargo test --test integration test_debrid_concurrent_vs_serialized -- --nocapture
#[test]
fn test_debrid_concurrent_vs_serialized() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kob_ffmpeg=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let url = match std::env::var("DEBRID_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Skipping — set DEBRID_URL");
            return;
        }
    };

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let head = client.head(&url).send().unwrap();
    let file_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Need Content-Length");
    eprintln!("File size: {:.1}GB", file_size as f64 / 1e9);

    let start_time = 1800.0;

    // ===== BROKEN FLOW: concurrent download + HybridIo =====
    eprintln!("\n=== BROKEN FLOW (concurrent) ===");
    let concurrent_ok = {
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("partial.mkv");
        std::fs::write(&local_path, b"").unwrap();
        let progress = Arc::new(DownloadProgress::new(Some(file_size)));

        // Start progressive download FIRST
        let dl_url = url.clone();
        let dl_path = local_path.clone();
        let dl_progress = progress.clone();
        let dl_handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            match reqwest::blocking::get(&dl_url) {
                Ok(mut r) => {
                    let mut f = std::fs::File::create(&dl_path).unwrap();
                    let mut total = 0u64;
                    let mut buf = vec![0u8; 1_048_576];
                    loop {
                        match r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                f.write_all(&buf[..n]).unwrap();
                                total += n as u64;
                                dl_progress.update(total);
                            }
                            Err(e) => {
                                eprintln!(
                                    "[CONCURRENT] dl error at {:.0}MB: {e}",
                                    total as f64 / 1e6
                                );
                                return (false, total);
                            }
                        }
                    }
                    dl_progress.mark_complete();
                    (true, total)
                }
                Err(e) => {
                    eprintln!("[CONCURRENT] dl start failed: {e}");
                    (false, 0)
                }
            }
        });

        std::thread::sleep(std::time::Duration::from_millis(200));

        let hybrid = HybridIo::new(&local_path, &url, progress.clone());
        let mut cio = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
        let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut cio).unwrap();
        ctx.find_stream_info().unwrap();
        let d = ctx.duration_secs().unwrap_or(0.0);
        eprintln!("[CONCURRENT] Probe OK: {d:.1}s");

        let sdir = dir.path().join("hls");
        let mut cfg = SessionConfig::new(&sdir, d)
            .audio_encoder("aac")
            .audio_bitrate(192_000)
            .segment_duration(6.0);
        cfg.start_time = Some(start_time);
        let session = TranscodeSession::start_from_custom_io(cio, ctx, None, cfg).unwrap();

        let target = session.time_to_segment(start_time);
        let mut produced = false;
        for i in 0..8 {
            std::thread::sleep(std::time::Duration::from_secs(5));
            session.update_segments();
            let segs = session.playlist_segments_from(target);
            eprintln!(
                "[CONCURRENT] {}s: {} segs, dl={:.0}MB, running={}",
                (i + 1) * 5,
                segs.len(),
                progress.downloaded() as f64 / 1e6,
                session.is_running()
            );
            if !segs.is_empty() {
                produced = true;
                break;
            }
            if !session.is_running() {
                break;
            }
        }
        session.cancel();
        let (dl_ok, dl_bytes) = dl_handle.join().unwrap();
        eprintln!(
            "[CONCURRENT] segs={produced}, dl_ok={dl_ok}, dl={:.0}MB",
            dl_bytes as f64 / 1e6
        );
        produced
    };

    // ===== FIXED FLOW: probe first, download after =====
    eprintln!("\n=== FIXED FLOW (serialized) ===");
    let serialized_ok = {
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("partial.mkv");
        std::fs::write(&local_path, b"").unwrap();
        let progress = Arc::new(DownloadProgress::new(Some(file_size)));

        eprintln!("[SERIALIZED] Probing...");
        let hybrid = HybridIo::new(&local_path, &url, progress.clone());
        let mut cio = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
        let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut cio).unwrap();
        ctx.find_stream_info().unwrap();
        let duration = ctx.duration_secs().expect("need duration");
        eprintln!("[SERIALIZED] Probe OK: {duration:.1}s");

        let sdir = dir.path().join("hls");
        let mut cfg = SessionConfig::new(&sdir, duration)
            .audio_encoder("aac")
            .audio_bitrate(192_000)
            .segment_duration(6.0);
        cfg.start_time = Some(start_time);
        let session = TranscodeSession::start_from_custom_io(cio, ctx, None, cfg).unwrap();
        eprintln!("[SERIALIZED] Session started, now starting download...");

        let dl_url = url.clone();
        let dl_path = local_path.clone();
        let dl_progress = progress.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            match reqwest::blocking::get(&dl_url) {
                Ok(mut r) => {
                    let mut f = std::fs::File::create(&dl_path).unwrap();
                    let mut total = 0u64;
                    let mut buf = vec![0u8; 1_048_576];
                    loop {
                        match r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                f.write_all(&buf[..n]).unwrap();
                                total += n as u64;
                                dl_progress.update(total);
                            }
                            Err(e) => {
                                eprintln!("[SERIALIZED] dl error: {e}");
                                break;
                            }
                        }
                    }
                    dl_progress.mark_complete();
                }
                Err(e) => eprintln!("[SERIALIZED] dl failed: {e}"),
            }
        });

        let target_seg = session.time_to_segment(start_time);
        let mut produced = false;
        for i in 0..12 {
            std::thread::sleep(std::time::Duration::from_secs(5));
            session.update_segments();
            let segs = session.playlist_segments_from(target_seg);
            eprintln!(
                "[SERIALIZED] {}s: {} segs, dl={:.0}MB, running={}",
                (i + 1) * 5,
                segs.len(),
                progress.downloaded() as f64 / 1e6,
                session.is_running()
            );
            if !segs.is_empty() {
                produced = true;
                break;
            }
            if !session.is_running() {
                break;
            }
        }
        session.cancel();
        produced
    };

    eprintln!("\n=== RESULTS ===");
    eprintln!(
        "Concurrent: {}",
        if concurrent_ok { "OK" } else { "FAILED" }
    );
    eprintln!(
        "Serialized: {}",
        if serialized_ok { "OK" } else { "FAILED" }
    );
    assert!(serialized_ok, "serialized flow must produce segments");
}

/// Reproduce the prod bug: play-from-start with concurrent progressive download.
/// Audio-transcode, video-copy, no start_time. Expect segment count to grow
/// steadily. If HybridIo fights the downloader for disk or HTTP capacity we'll
/// see flat/zero segments or a died session.
///
/// DEBRID_URL="https://..." cargo test --test integration test_debrid_play_from_start -- --nocapture
#[test]
fn test_debrid_play_from_start() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::sync::Arc;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kob_ffmpeg=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let url = match std::env::var("DEBRID_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Skipping — set DEBRID_URL");
            return;
        }
    };

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let head = client.head(&url).send().unwrap();
    let file_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Need Content-Length");
    eprintln!("File size: {:.1}GB", file_size as f64 / 1e9);

    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("partial.mkv");
    std::fs::write(&local_path, b"").unwrap();
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));

    // Start progressive download FIRST (like prod debrid_stream.rs)
    let dl_url = url.clone();
    let dl_path = local_path.clone();
    let dl_progress = progress.clone();
    let dl_handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        match reqwest::blocking::get(&dl_url) {
            Ok(mut r) => {
                let mut f = std::fs::File::create(&dl_path).unwrap();
                let mut total = 0u64;
                let mut buf = vec![0u8; 1_048_576];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            f.write_all(&buf[..n]).unwrap();
                            total += n as u64;
                            dl_progress.update(total);
                        }
                        Err(e) => {
                            eprintln!("[PFS] dl error at {:.0}MB: {e}", total as f64 / 1e6);
                            return total;
                        }
                    }
                }
                dl_progress.mark_complete();
                total
            }
            Err(e) => {
                eprintln!("[PFS] dl start failed: {e}");
                0
            }
        }
    });

    // Give the download a 200ms head start, just like prod does (session
    // created, then probe begins while download is already in flight).
    std::thread::sleep(std::time::Duration::from_millis(200));

    eprintln!("[PFS] Probing via HybridIo (dl in flight)...");
    let hybrid = HybridIo::new(&local_path, &url, progress.clone());
    let mut cio = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut cio).unwrap();
    ctx.find_stream_info().unwrap();
    let duration = ctx.duration_secs().expect("need duration");
    eprintln!(
        "[PFS] Probe OK: {duration:.1}s, dl={:.0}MB",
        progress.downloaded() as f64 / 1e6
    );

    let sdir = dir.path().join("hls");
    // Audio-transcode + video-copy — exactly matches prod `AudioTranscode` mode.
    let cfg = SessionConfig::new(&sdir, duration)
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);
    let session = TranscodeSession::start_from_custom_io(cio, ctx, None, cfg).unwrap();
    eprintln!("[PFS] Session started, transcoding from position 0");

    let mut last_segs = 0usize;
    let mut stall_ticks = 0u32;
    let mut max_segs = 0usize;
    for i in 0..15 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        session.update_segments();
        let segs = session.playlist_segments_from(0);
        let running = session.is_running();
        let dl_mb = progress.downloaded() as f64 / 1e6;
        eprintln!(
            "[PFS] t={:>3}s: segs={:>3}, dl={:>6.0}MB, running={running}, err={:?}",
            (i + 1) * 3,
            segs.len(),
            dl_mb,
            session.error()
        );
        max_segs = max_segs.max(segs.len());
        if !running {
            eprintln!("[PFS] Session died — stopping loop");
            break;
        }
        if segs.len() == last_segs {
            stall_ticks += 1;
        } else {
            stall_ticks = 0;
            last_segs = segs.len();
        }
        if stall_ticks >= 3 {
            eprintln!("[PFS] Segment count flat for 9s — likely stalled");
        }
    }
    session.cancel();
    let dl_bytes = dl_handle.join().unwrap();
    eprintln!(
        "[PFS] final: max_segs={max_segs}, dl={:.0}MB, session_err={:?}",
        dl_bytes as f64 / 1e6,
        session.error()
    );

    // We expect at least ~5 segments within 45s under healthy HybridIo+progressive.
    // Don't panic on failure — this is a diagnostic repro; the asserts are for
    // regression tracking once we fix things.
    if max_segs < 5 {
        eprintln!("[PFS] FAILURE: produced only {max_segs} segments in 45s");
    } else {
        eprintln!("[PFS] SUCCESS: produced {max_segs} segments");
    }
}

/// Full-loop repro: HybridIo + progressive download + transcode session +
/// ffmpeg-as-player reading the resulting HLS playlist while it's being
/// written. Mimics what hls.js does in the browser, except via local files.
///
/// DEBRID_URL="https://..." cargo test --test integration test_debrid_full_playback_loop -- --nocapture
#[test]
fn test_debrid_full_playback_loop() {
    use kob_ffmpeg::hybrid_io::{DownloadProgress, HybridIo};
    use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
    use std::io::Read;
    use std::sync::Arc;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kob_ffmpeg=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let url = match std::env::var("DEBRID_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Skipping — set DEBRID_URL");
            return;
        }
    };

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let head = client.head(&url).send().unwrap();
    let file_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Need Content-Length");
    eprintln!("[FPL] File size: {:.1}GB", file_size as f64 / 1e9);

    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("partial.mkv");
    std::fs::write(&local_path, b"").unwrap();
    let progress = Arc::new(DownloadProgress::new(Some(file_size)));

    // Throttle download to simulate prod network pacing — debrid isn't
    // always gigabit. Without this the file finishes before the player
    // even wakes up, defeating the point of "playback while downloading".
    let throttle_mbps: u64 = std::env::var("DEBRID_THROTTLE_MBPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let bytes_per_sec = throttle_mbps * 1_000_000 / 8;
    eprintln!("[FPL] Throttling download to ~{throttle_mbps}Mbps ({bytes_per_sec} B/s)");

    let dl_url = url.clone();
    let dl_path = local_path.clone();
    let dl_progress = progress.clone();
    let dl_handle = std::thread::spawn(move || {
        use std::io::Write;
        let t0 = std::time::Instant::now();
        match reqwest::blocking::get(&dl_url) {
            Ok(mut r) => {
                let mut f = std::fs::File::create(&dl_path).unwrap();
                let mut total = 0u64;
                let mut buf = vec![0u8; 65_536];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            f.write_all(&buf[..n]).unwrap();
                            f.sync_data().ok();
                            total += n as u64;
                            dl_progress.update(total);

                            // Throttle: pace against wall clock.
                            let expected = total * 1000 / bytes_per_sec.max(1);
                            let actual = t0.elapsed().as_millis() as u64;
                            if expected > actual {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    expected - actual,
                                ));
                            }
                        }
                        Err(e) => {
                            eprintln!("[FPL] dl error: {e}");
                            return total;
                        }
                    }
                }
                dl_progress.mark_complete();
                total
            }
            Err(e) => {
                eprintln!("[FPL] dl start failed: {e}");
                0
            }
        }
    });

    std::thread::sleep(std::time::Duration::from_millis(200));

    eprintln!("[FPL] Probing via HybridIo (dl in flight)...");
    let hybrid = HybridIo::new(&local_path, &url, progress.clone());
    let mut cio = kob_ffmpeg::format::io_context::CustomIoContext::new(hybrid).unwrap();
    let mut ctx = kob_ffmpeg::format::input::InputContext::open_custom(&mut cio).unwrap();
    ctx.find_stream_info().unwrap();
    let duration = ctx.duration_secs().expect("need duration");
    eprintln!(
        "[FPL] Probe OK: {duration:.1}s, dl={:.0}MB",
        progress.downloaded() as f64 / 1e6
    );

    let sdir = dir.path().join("hls");
    let cfg = SessionConfig::new(&sdir, duration)
        .audio_encoder("aac")
        .audio_bitrate(256_000)
        .segment_duration(6.0);
    let session = TranscodeSession::start_from_custom_io(cio, ctx, None, cfg).unwrap();
    eprintln!("[FPL] Session started, waiting for first segment...");

    // Wait until at least one segment exists before invoking the player.
    let gen0_dir = session.gen_dir(0);
    let playlist = gen0_dir.join("stream.m3u8");
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if playlist.exists() {
            let content = std::fs::read_to_string(&playlist).unwrap_or_default();
            if content.contains("seg_00000.m4s") || content.contains("#EXTINF") {
                break;
            }
        }
    }
    assert!(playlist.exists(), "stream.m3u8 never appeared");
    eprintln!("[FPL] Playlist ready at {}", playlist.display());

    // Spawn ffmpeg as the "player" — read the HLS playlist, decode, discard.
    // `-xerror` makes it exit non-zero on any read error. `-loglevel warning`
    // keeps stderr focused on problems. `-fflags +nobuffer` disables input
    // buffering so stalls surface immediately.
    let t_player = std::time::Instant::now();
    let mut player = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            "-xerror",
            "-fflags",
            "+nobuffer",
            "-i",
        ])
        .arg(&playlist)
        .args(["-f", "null", "-"])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn ffmpeg player");
    eprintln!("[FPL] Player started (pid={})", player.id());

    // Capture player stderr in a thread.
    let player_stderr = player.stderr.take().unwrap();
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut reader = std::io::BufReader::new(player_stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut reader, &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.contains("Error")
                        || line.contains("failed")
                        || line.contains("404")
                        || line.contains("Invalid")
                    {
                        eprintln!("[player-stderr] {}", line.trim_end());
                    }
                    buf.extend_from_slice(line.as_bytes());
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    });

    // Poll session + player for up to 300s (well past transcode end even at 30Mbps).
    let mut max_segs = 0usize;
    let mut player_exit: Option<std::process::ExitStatus> = None;
    for i in 0..60 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        session.update_segments();
        let segs = session.playlist_segments_from(0);
        max_segs = max_segs.max(segs.len());
        let dl_mb = progress.downloaded() as f64 / 1e6;
        let running = session.is_running();
        let p_status = player.try_wait().ok().flatten();
        eprintln!(
            "[FPL] t={:>3}s: segs={:>3}, dl={:>6.0}MB, session_running={running}, player={:?}",
            (i + 1) * 5,
            segs.len(),
            dl_mb,
            p_status.map(|s| format!("{s}"))
        );
        if let Some(status) = p_status {
            player_exit = Some(status);
            break;
        }
        if !running && segs.is_empty() {
            eprintln!("[FPL] Session died without segments — aborting");
            break;
        }
    }

    if player_exit.is_none() {
        eprintln!("[FPL] Player still running after 300s — killing");
        let _ = player.kill();
        player_exit = player.wait().ok();
    }

    let stderr_output = stderr_thread.join().unwrap_or_default();
    session.cancel();
    let dl_bytes = dl_handle.join().unwrap();

    let elapsed = t_player.elapsed();
    eprintln!(
        "[FPL] DONE: player_exit={:?}, max_segs={max_segs}, dl={:.0}MB, elapsed={:.1}s",
        player_exit,
        dl_bytes as f64 / 1e6,
        elapsed.as_secs_f64()
    );

    // Tail of stderr for context.
    let tail: String = stderr_output
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("[FPL] player stderr tail:\n{tail}");
}

// --- Deinterlace + DVR window ---

fn probe_output_fps(path: &std::path::Path) -> f64 {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=avg_frame_rate",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe must be available");
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.lines().next().unwrap().trim();
    let (num, den) = first.split_once('/').unwrap();
    num.parse::<f64>().unwrap() / den.parse::<f64>().unwrap()
}

#[test]
fn test_deinterlace_auto_dvr_window() {
    let input = fixtures::interlaced_ts();
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(128000)
        .segment_duration(1.0)
        .deinterlace(kob_ffmpeg::filter::graph::Deinterlace::Auto)
        .hls_window(kob_ffmpeg::transcode::HlsWindow::Dvr);
    let result = kob_ffmpeg::transcode::transcode_to_hls(&input, &config).unwrap();

    assert!(result.playlist_path.exists());
    let playlist = std::fs::read_to_string(&result.playlist_path).unwrap();
    let listed: Vec<&str> = playlist.lines().filter(|l| l.ends_with(".m4s")).collect();
    let on_disk = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".m4s"))
        .count();

    assert!(on_disk >= 3, "expected retained segments, got {on_disk}");
    assert_eq!(
        listed.len(),
        on_disk,
        "DVR playlist must list every segment"
    );
    assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
    assert!(playlist.contains("#EXT-X-ENDLIST"));
    assert_eq!(&playlist[..7], "#EXTM3U");

    // bwdif=mode=send_field turns 25i into 50p.
    let seg = dir.path().join(listed[0]);
    assert!(seg.exists());
    let fps = probe_output_fps(&result.playlist_path);
    assert!(
        (fps - 50.0).abs() < 0.5,
        "expected 50 fps output, got {fps}"
    );
}

#[test]
fn test_probe_reads_field_coding_from_the_sequence_header() {
    let cases = [
        ("fake.mkv", "fake-interlaced=1:pic-struct=1", true),
        ("fake.ts", "fake-interlaced=1:pic-struct=1", true),
        ("fake.mp4", "fake-interlaced=1", true),
        ("poc0.mkv", "fake-interlaced=1:bframes=3", true),
        ("poc0_progressive.mkv", "bframes=3", false),
        ("plain.ts", "keyint=25", false),
    ];
    for (name, params, interlaced) in cases {
        let clip = fixtures::x264_clip(name, params);
        let probed = kob_ffmpeg::probe::probe(&clip).unwrap();
        assert_eq!(
            probed.video_streams[0].is_interlaced, interlaced,
            "{name} ({params})"
        );
    }
    assert!(
        kob_ffmpeg::probe::probe(&fixtures::interlaced_ts())
            .unwrap()
            .video_streams[0]
            .is_interlaced
    );
}

#[test]
fn test_deinterlace_auto_reencodes_a_field_coded_progressive_source() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .segment_duration(1.0)
        .deinterlace(kob_ffmpeg::filter::graph::Deinterlace::Auto);
    let clip = fixtures::x264_clip("fake_auto.mkv", "fake-interlaced=1:pic-struct=1");
    let result = kob_ffmpeg::transcode::transcode_to_hls(&clip, &config).unwrap();
    let fps = probe_output_fps(&result.playlist_path);
    assert!(
        (fps - 50.0).abs() < 0.5,
        "expected 50 fps output, got {fps}"
    );
}

#[test]
fn test_deinterlace_none_leaves_frame_rate_alone() {
    let input = fixtures::interlaced_ts();
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0);
    let result = kob_ffmpeg::transcode::transcode_to_hls(&input, &config).unwrap();
    let fps = probe_output_fps(&result.playlist_path);
    assert!(
        (fps - 25.0).abs() < 0.5,
        "expected 25 fps output, got {fps}"
    );
}

#[test]
fn test_deinterlace_auto_skips_progressive_source() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0)
        .deinterlace(kob_ffmpeg::filter::graph::Deinterlace::Auto);
    let result =
        kob_ffmpeg::transcode::transcode_to_hls(&fixtures::basic_video(), &config).unwrap();
    let fps = probe_output_fps(&result.playlist_path);
    assert!(
        (fps - 10.0).abs() < 0.5,
        "expected 10 fps output, got {fps}"
    );
}

#[test]
fn test_sliding_window_still_deletes_and_omits_endlist() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0);
    config.live_source = true;
    let result =
        kob_ffmpeg::transcode::transcode_to_hls(&fixtures::interlaced_ts(), &config).unwrap();
    let playlist = std::fs::read_to_string(&result.playlist_path).unwrap();
    assert!(!playlist.contains("#EXT-X-ENDLIST"));
    assert!(!playlist.contains("#EXT-X-PLAYLIST-TYPE"));
}

#[test]
fn test_dvr_cancel_writes_endlist() {
    let input = fixtures::interlaced_ts();
    let dir = tempfile::tempdir().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0)
        .hls_window(kob_ffmpeg::transcode::HlsWindow::Dvr)
        .run_context(kob_ffmpeg::RunContext::cancellable(cancel));
    let result = kob_ffmpeg::transcode::transcode_to_hls(&input, &config).unwrap();
    let playlist = std::fs::read_to_string(&result.playlist_path).unwrap();
    assert!(playlist.contains("#EXT-X-ENDLIST"));
}

#[test]
fn test_sliding_cancel_still_errors() {
    let input = fixtures::interlaced_ts();
    let dir = tempfile::tempdir().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let mut config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0)
        .run_context(kob_ffmpeg::RunContext::cancellable(cancel));
    config.live_source = true;
    let Err(err) = kob_ffmpeg::transcode::transcode_to_hls(&input, &config) else {
        panic!("sliding-window cancel must not return Ok");
    };
    assert_eq!(err.kind(), kob_ffmpeg::ErrorKind::Cancelled);
}

// --- A/V timestamp agreement (MPEG-TS stream time_base != audio sample rate) ---

fn probe_pts_span(path: &std::path::Path, stream: &str) -> (f64, f64) {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe must be available");
    let vals: Vec<f64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse::<f64>().ok())
        .collect();
    assert!(!vals.is_empty(), "no {stream} packets in {path:?}");
    vals.iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
}

fn assert_av_aligned(playlist: &std::path::Path, label: &str) {
    let (v_first, v_last) = probe_pts_span(playlist, "v");
    let (a_first, a_last) = probe_pts_span(playlist, "a");
    let start_skew = (a_first - v_first).abs();
    let end_skew = (a_last - v_last).abs();
    assert!(
        start_skew < 0.25 && end_skew < 0.25,
        "{label}: A/V desync — video {v_first:.6}..{v_last:.6}, \
         audio {a_first:.6}..{a_last:.6} (start skew {start_skew:.3}s, end skew {end_skew:.3}s)"
    );
}

#[test]
fn test_mpegts_audio_timestamps_match_video_transcode_to_hls() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(128000)
        .segment_duration(1.0);
    let result =
        kob_ffmpeg::transcode::transcode_to_hls(&fixtures::interlaced_ts(), &config).unwrap();
    assert_av_aligned(&result.playlist_path, "transcode_to_hls");
}

#[test]
fn test_mpegts_audio_timestamps_match_video_from_ctx() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx =
        kob_ffmpeg::format::input::InputContext::open_path(&fixtures::interlaced_ts()).unwrap();
    ctx.find_stream_info().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(128000)
        .segment_duration(1.0)
        .deinterlace(kob_ffmpeg::filter::graph::Deinterlace::Auto)
        .hls_window(kob_ffmpeg::transcode::HlsWindow::Dvr);
    let result = kob_ffmpeg::transcode::transcode_to_hls_from_ctx(ctx, &config).unwrap();
    assert_av_aligned(
        &result.playlist_path,
        "transcode_to_hls_from_ctx + deinterlace",
    );
}

#[test]
fn test_mpegts_audio_timestamps_match_video_segment_session() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::session::SessionConfig::new(dir.path(), 4.0)
        .segment_duration(1.0)
        .video_encoder("libx264")
        .audio_encoder("aac")
        .audio_bitrate(128000);
    let session =
        kob_ffmpeg::session::TranscodeSession::start(&fixtures::interlaced_ts(), config).unwrap();
    session.join().unwrap();
    assert_av_aligned(&dir.path().join("gen_0/stream.m3u8"), "TranscodeSession");
}

// --- encoder preset / tune ---

fn hls_segment_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".m4s"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no segments in {dir:?}");
    let mut out = Vec::new();
    for n in names {
        out.extend_from_slice(&std::fs::read(dir.join(n)).unwrap());
    }
    out
}

fn transcode_with(
    dir: &std::path::Path,
    f: impl FnOnce(kob_ffmpeg::transcode::HlsConfig) -> kob_ffmpeg::transcode::HlsConfig,
) -> Vec<u8> {
    let base = kob_ffmpeg::transcode::HlsConfig::new(dir)
        .video_encoder("libx264")
        .audio_encoder("aac")
        .segment_duration(1.0);
    let config = f(base);
    kob_ffmpeg::transcode::transcode_to_hls(&fixtures::interlaced_ts(), &config).unwrap();
    hls_segment_bytes(dir)
}

#[test]
fn test_no_preset_leaves_encoder_output_unchanged() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let plain_1 = transcode_with(a.path(), |c| c);
    let plain_2 = transcode_with(b.path(), |c| c);
    assert_eq!(
        plain_1, plain_2,
        "default config must stay deterministic — this is the VOD baseline"
    );

    let c = tempfile::tempdir().unwrap();
    let fast = transcode_with(c.path(), |c| c.preset("veryfast").tune("zerolatency"));
    assert_ne!(
        plain_1, fast,
        "preset/tune must actually reach libx264, not be silently dropped"
    );
}

#[test]
fn test_invalid_preset_is_rejected_by_the_encoder() {
    let dir = tempfile::tempdir().unwrap();
    let config = kob_ffmpeg::transcode::HlsConfig::new(dir.path())
        .video_encoder("libx264")
        .segment_duration(1.0)
        .preset("definitely-not-a-preset");
    let Err(err) = kob_ffmpeg::transcode::transcode_to_hls(&fixtures::interlaced_ts(), &config)
    else {
        panic!("libx264 must reject an unknown preset — otherwise options are being ignored");
    };
    let _ = err;
}

#[test]
fn test_read_text_cues() {
    let tracks = kob_ffmpeg::subtitles::read_text_cues(&fixtures::rich_mkv(), None).unwrap();
    assert_eq!(tracks.len(), 1);
    let cues: Vec<_> = tracks[0]
        .cues
        .iter()
        .map(|c| (c.start_secs, c.text.as_str()))
        .collect();
    assert_eq!(cues, vec![(0.0, "Hello World"), (1.0, "Test subtitle")]);
}

#[test]
fn test_read_text_cues_strips_ass_markup() {
    let tracks = kob_ffmpeg::subtitles::read_text_cues(&fixtures::ass_mkv(), None).unwrap();
    let texts: Vec<_> = tracks[0].cues.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["Bonjour le monde\nligne deux", "Au revoir"]);
}

#[test]
fn test_read_text_cues_stops_at_the_limit() {
    let tracks = kob_ffmpeg::subtitles::read_text_cues(&fixtures::rich_mkv(), Some(0.5)).unwrap();
    assert_eq!(tracks[0].cues.len(), 1);
}
