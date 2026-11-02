//! Test fixture generation — each fixture is generated exactly once via OnceLock.
//!
//! Tests in this crate are split across multiple `tests/*.rs` files, each its
//! own crate. Some test crates use only a subset of these fixtures, so suppress
//! dead-code warnings at the module level.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static FIXTURE_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
static BASIC_VIDEO: OnceLock<PathBuf> = OnceLock::new();
static RICH_MKV: OnceLock<PathBuf> = OnceLock::new();
static AUDIO_SILENCE: OnceLock<PathBuf> = OnceLock::new();
static CHAPTERED_M4B: OnceLock<PathBuf> = OnceLock::new();
static CORRUPT_AUDIO_AVI: OnceLock<PathBuf> = OnceLock::new();
static AC3_CHANNEL_CHANGE: OnceLock<PathBuf> = OnceLock::new();
static INTERLACED_TS: OnceLock<PathBuf> = OnceLock::new();
static NEAR_BLACK_VIDEO: OnceLock<PathBuf> = OnceLock::new();
static MINUTE_VIDEO: OnceLock<PathBuf> = OnceLock::new();
static ASS_MKV: OnceLock<PathBuf> = OnceLock::new();

fn fixture_dir() -> &'static Path {
    FIXTURE_DIR
        .get_or_init(|| tempfile::tempdir().expect("create fixture temp dir"))
        .path()
}

fn run_ffmpeg(args: &[&str], output: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(args)
        .arg("-y")
        .arg(output)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg must be available");
    assert!(status.success(), "ffmpeg failed: {output:?}");
}

/// H.264 320x240 @ 10fps + AAC audio, 2 seconds.
pub fn basic_video() -> PathBuf {
    BASIC_VIDEO
        .get_or_init(|| {
            let path = fixture_dir().join("basic.mp4");
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=duration=2:size=320x240:rate=10",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "40",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "32k",
                    "-shortest",
                ],
                &path,
            );
            path
        })
        .clone()
}

/// H.264 160x120 @ 10fps with a keyframe every second + AAC audio, 60 seconds.
pub fn minute_video() -> PathBuf {
    MINUTE_VIDEO
        .get_or_init(|| {
            let path = fixture_dir().join("minute.mkv");
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=duration=60:size=160x120:rate=10",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=60",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "45",
                    "-g",
                    "10",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "32k",
                    "-shortest",
                ],
                &path,
            );
            path
        })
        .clone()
}

/// H.264 320x240 @ 10fps, 2 seconds, no audio. First second: black except a
/// white band covering exactly 10% of the frame. Second second: all white.
///
/// The 0.90 black-pixel ratio sits under libavfilter's 0.98 `pic_th` default
/// and over anything <= 0.85, so the fixture separates a plumbed picture
/// threshold from an ignored one. The white tail is load-bearing: `blackdetect`
/// only attaches `lavfi.black_end` to the first non-black frame, so a run that
/// reaches EOF is logged but never reported as an interval.
pub fn near_black_video() -> PathBuf {
    NEAR_BLACK_VIDEO
        .get_or_init(|| {
            let path = fixture_dir().join("near_black.mp4");
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=320x240:d=2:r=10,\
                     drawbox=x=0:y=0:w=320:h=24:color=white:t=fill,\
                     drawbox=x=0:y=0:w=320:h=240:color=white:t=fill:enable=gt(t\\,0.95)",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "0",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &path,
            );
            path
        })
        .clone()
}

/// MPEG-TS, 704x576 field-coded H.264 (field_order=tt) @ 25 fps + 48 kHz stereo
/// AAC, 4 seconds. Stands in for a 1080i25 broadcast capture.
///
/// The audio is deliberately already fltp/48000/stereo — exactly what the AAC
/// encoder asks for — so libavfilter inserts no conversion filter and the
/// buffersink link keeps the container's 1/90000 time base rather than
/// normalising it to 1/sample_rate. Real broadcast AC3 hits the same path;
/// ffmpeg's own AC3 encoder cannot stand in for it because it emits downmix
/// side data, which makes libavfilter renormalise the link and hides the bug.
pub fn interlaced_ts() -> PathBuf {
    INTERLACED_TS
        .get_or_init(|| {
            let path = fixture_dir().join("interlaced.ts");
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=duration=4:size=704x576:rate=50",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=4:sample_rate=48000",
                    "-vf",
                    "interlace",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "40",
                    "-flags",
                    "+ilme+ildct",
                    "-x264opts",
                    "tff=1",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "128k",
                    "-ac",
                    "2",
                    "-shortest",
                ],
                &path,
            );
            path
        })
        .clone()
}

/// MKV: 1 video + 2 audio (eng/fra) + 1 subtitle (eng SRT) + 2 chapters.
pub fn rich_mkv() -> PathBuf {
    RICH_MKV
        .get_or_init(|| {
            let path = fixture_dir().join("rich.mkv");

            let srt = fixture_dir().join("test.srt");
            std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nHello World\n\n2\n00:00:01,000 --> 00:00:02,000\nTest subtitle\n").unwrap();

            let ch = fixture_dir().join("chapters.txt");
            std::fs::write(&ch, ";FFMETADATA1\n\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Chapter 1\n\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=2000\ntitle=Chapter 2\n").unwrap();

            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-f", "lavfi", "-i", "testsrc2=duration=2:size=320x240:rate=10",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                    "-f", "lavfi", "-i", "sine=frequency=880:duration=2",
                    "-i", srt.to_str().unwrap(),
                    "-i", ch.to_str().unwrap(),
                    "-map_metadata", "4",
                    "-c:v", "libx264", "-preset", "ultrafast", "-crf", "40",
                    "-c:a", "aac", "-b:a", "32k", "-c:s", "srt",
                    "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s",
                    "-metadata:s:a:0", "language=eng", "-metadata:s:a:0", "title=English",
                    "-metadata:s:a:1", "language=fra", "-metadata:s:a:1", "title=French",
                    "-metadata:s:s:0", "language=eng",
                    "-shortest", "-y",
                ])
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("ffmpeg");
            assert!(status.success(), "failed to generate rich.mkv");
            path
        })
        .clone()
}

/// MP3 with ~1s silence gap: 0.5s tone + 1s silence + 0.5s tone.
pub fn audio_with_silence() -> PathBuf {
    AUDIO_SILENCE
        .get_or_init(|| {
            let path = fixture_dir().join("silence.mp3");
            run_ffmpeg(
                &[
                    "-f", "lavfi", "-i",
                    "sine=frequency=440:duration=0.5,apad=pad_dur=1[s1];sine=frequency=880:duration=0.5[s2];[s1][s2]concat=n=2:v=0:a=1",
                    "-c:a", "libmp3lame", "-b:a", "64k",
                ],
                &path,
            );
            path
        })
        .clone()
}

/// M4B audiobook: AAC 3 seconds, 2 chapters.
pub fn chaptered_m4b() -> PathBuf {
    CHAPTERED_M4B
        .get_or_init(|| {
            let path = fixture_dir().join("book.m4b");
            let ch = fixture_dir().join("m4b_chapters.txt");
            std::fs::write(&ch, ";FFMETADATA1\n\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1500\ntitle=Chapter One\n\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1500\nEND=3000\ntitle=Chapter Two\n").unwrap();

            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=3",
                    "-i", ch.to_str().unwrap(),
                    "-map_metadata", "1",
                    "-c:a", "aac", "-b:a", "32k", "-y",
                ])
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("ffmpeg");
            assert!(status.success(), "failed to generate book.m4b");
            path
        })
        .clone()
}

/// AVI with a few corrupted audio packets — simulates old AVI files with malformed data.
pub fn corrupt_audio_avi() -> PathBuf {
    CORRUPT_AUDIO_AVI
        .get_or_init(|| {
            let path = fixture_dir().join("corrupt_audio.avi");
            // Generate a clean AVI first
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=duration=2:size=320x240:rate=10",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-c:v",
                    "mpeg4",
                    "-c:a",
                    "mp3",
                    "-b:a",
                    "64k",
                ],
                &path,
            );
            // Corrupt a few audio packets by overwriting bytes in the middle of the file.
            // AVI interleaves audio/video chunks — corrupting a few spots in the
            // middle will hit some audio packets while leaving the container intact.
            let mut data = std::fs::read(&path).expect("read generated AVI");
            let len = data.len();
            // Corrupt 3 small regions in the middle third of the file
            for offset in [len / 3, len / 3 + 512, len * 2 / 3] {
                if offset + 16 < len {
                    for b in &mut data[offset..offset + 16] {
                        *b = 0xFF;
                    }
                }
            }
            std::fs::write(&path, &data).expect("write corrupted AVI");
            path
        })
        .clone()
}

/// AC3 MKV where the first frame is stereo but subsequent frames are 5.1 surround.
/// Reproduces the channel layout change bug where the decoder's probed parameters
/// don't match the actual decoded frame properties.
pub fn ac3_channel_change_mkv() -> PathBuf {
    AC3_CHANNEL_CHANGE
        .get_or_init(|| {
            let dir = fixture_dir();
            let stereo_ac3 = dir.join("ch_stereo.ac3");
            let surround_ac3 = dir.join("ch_surround.ac3");
            let mixed_ac3 = dir.join("ch_mixed.ac3");
            let path = dir.join("channel_change.mkv");

            // Generate a single stereo AC3 frame (~32ms at 48kHz)
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=0.032:sample_rate=48000",
                    "-c:a",
                    "ac3",
                    "-b:a",
                    "192k",
                    "-ac",
                    "2",
                ],
                &stereo_ac3,
            );

            // Generate 5.1 surround AC3 (2 seconds)
            run_ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2:sample_rate=48000",
                    "-c:a",
                    "ac3",
                    "-b:a",
                    "384k",
                    "-ac",
                    "6",
                ],
                &surround_ac3,
            );

            // Raw-concatenate: stereo frame(s) followed by 5.1 frames.
            // AC3 is self-describing per-frame, so the decoder reads each frame's
            // own channel mode. This produces a stream where the channel layout
            // changes after the first frame.
            let mut data = std::fs::read(&stereo_ac3).expect("read stereo AC3");
            data.extend(std::fs::read(&surround_ac3).expect("read surround AC3"));
            std::fs::write(&mixed_ac3, &data).expect("write mixed AC3");

            // Remux to MKV (stream-copy preserves the raw AC3 frames)
            let status = std::process::Command::new("ffmpeg")
                .args(["-i", mixed_ac3.to_str().unwrap(), "-c:a", "copy", "-y"])
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("ffmpeg");
            assert!(status.success(), "failed to generate channel_change.mkv");
            path
        })
        .clone()
}

/// One second of 320x240 H.264 at 25 fps, encoded with the given x264 params.
/// `fake-interlaced=1` gives the "1080i film" case: a field-coded sequence header
/// over progressive frames, whose container and SEI both say progressive.
pub fn x264_clip(file_name: &str, x264_params: &str) -> PathBuf {
    let path = fixture_dir().join(file_name);
    run_ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=duration=1:size=320x240:rate=25",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-x264-params",
            x264_params,
        ],
        &path,
    );
    path
}

/// MKV: 1 video + 1 ASS subtitle whose lines carry override tags and a hard line break.
pub fn ass_mkv() -> PathBuf {
    ASS_MKV
        .get_or_init(|| {
            let ass = fixture_dir().join("test.ass");
            std::fs::write(
                &ass,
                "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20\n\n\
                 [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                 Dialogue: 0,0:00:00.20,0:00:00.90,Default,,0,0,0,,{\\i1}Bonjour{\\i0} le monde\\Nligne deux\n\
                 Dialogue: 0,0:00:01.10,0:00:01.80,Default,,0,0,0,,Au revoir\n",
            )
            .unwrap();
            let path = fixture_dir().join("ass.mkv");
            run_ffmpeg(
                &[
                    "-f", "lavfi", "-i", "testsrc2=duration=2:size=64x64:rate=5",
                    "-i", ass.to_str().unwrap(),
                    "-c:v", "libx264", "-preset", "ultrafast", "-c:s", "ass",
                    "-map", "0:v", "-map", "1:s",
                ],
                &path,
            );
            path
        })
        .clone()
}
