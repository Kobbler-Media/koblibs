//! Peak loudness normalisation for short voice-clone reference clips.
//!
//! Why peak (and not LUFS / `loudnorm`): clone-from-clip TTS engines (chatterbox,
//! tts-breeze) reproduce the envelope of the reference. EBU R128 with dynamic-range
//! compression would smear clone character; we just want the same envelope, louder.
//! Pure Rust, no ffmpeg — see `kob-ffmpeg` for richer audio processing paths.
//!
//! Supports 16-bit PCM in canonical RIFF/WAVE files (the only format the kobbler
//! voice pipeline emits and the only format tts-breeze / chatterbox accept for
//! clones). Other bit depths are rejected loudly rather than silently mis-scaled.

use thiserror::Error;

/// Default peak target for voice-clone references. −1 dBFS leaves 1 dB of
/// headroom against rounding and downstream resampling.
pub const DEFAULT_TARGET_PEAK_DBFS: f32 = -1.0;

#[derive(Debug, Error)]
pub enum NormalizeError {
    #[error("buffer too short for a WAV file ({0} bytes)")]
    TooShort(usize),
    #[error("not a RIFF/WAVE file")]
    NotWav,
    #[error(
        "unsupported format: expected PCM (fmt 1) with bits_per_sample=16, got fmt={fmt} bps={bps}"
    )]
    UnsupportedFormat { fmt: u16, bps: u16 },
    #[error("missing 'data' chunk")]
    MissingDataChunk,
    #[error("truncated data chunk (claims {claimed} bytes, file has {available})")]
    TruncatedData { claimed: usize, available: usize },
    #[error("target_peak_dbfs must be finite and <= 0")]
    InvalidTarget,
}

/// Outcome of a normalisation run, useful for logging without re-scanning.
#[derive(Debug, Clone, Copy)]
pub struct NormalizeReport {
    /// Multiplier applied to every sample. 1.0 = no change.
    pub scale: f32,
    /// Loudest absolute sample value before normalisation (0..=32767).
    pub peak_before: u16,
    /// Loudest absolute sample value after normalisation (0..=32767).
    pub peak_after: u16,
    /// Bytes spanning the PCM data chunk (excludes header).
    pub data_bytes: usize,
}

/// Peak-normalise the 16-bit PCM data chunk of a WAV file in place.
///
/// `target_peak_dbfs` must be ≤ 0 (a dBFS value, so 0 = full scale, -1 = -1 dBFS).
/// Never produces clipping: scale factor is computed against the current peak,
/// then capped at 1.0 — clips already louder than the target are left untouched
/// (we never attenuate, that's a different intent).
pub fn peak_normalize_wav_in_place(
    bytes: &mut [u8],
    target_peak_dbfs: f32,
) -> Result<NormalizeReport, NormalizeError> {
    if !target_peak_dbfs.is_finite() || target_peak_dbfs > 0.0 {
        return Err(NormalizeError::InvalidTarget);
    }
    let (data_start, data_end, bits_per_sample) = locate_wav_data(bytes)?;
    if bits_per_sample != 16 {
        // We could fold in s24 / f32 later; voice clones don't need it.
        return Err(NormalizeError::UnsupportedFormat {
            fmt: 1,
            bps: bits_per_sample,
        });
    }
    let data_slice = &mut bytes[data_start..data_end];
    Ok(peak_normalize_s16_pcm(data_slice, target_peak_dbfs))
}

/// Peak-normalise a raw interleaved 16-bit little-endian PCM buffer in place.
/// Channel count doesn't matter — samples are scaled element-wise. The buffer
/// must be an even number of bytes (one s16 sample per pair); the trailing
/// odd byte if any is left untouched.
pub fn peak_normalize_s16_pcm(pcm: &mut [u8], target_peak_dbfs: f32) -> NormalizeReport {
    let data_bytes = pcm.len();
    let peak_before = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            let s = i16::from_le_bytes(*c);
            // i16::MIN.unsigned_abs() == 32768, which overflows i16. Cap to 32767
            // so the scale calc stays in u16 range; the practical effect is
            // identical (we never attenuate).
            s.saturating_abs().unsigned_abs()
        })
        .max()
        .unwrap_or(0);

    if peak_before == 0 {
        return NormalizeReport {
            scale: 1.0,
            peak_before: 0,
            peak_after: 0,
            data_bytes,
        };
    }

    // Target amplitude as a fraction of full-scale (i16::MAX). For -1 dBFS this
    // is 10^(-1/20) ≈ 0.8913.
    let target_amp = 10f32.powf(target_peak_dbfs / 20.0);
    let target_peak = (target_amp * i16::MAX as f32).round();
    let raw_scale = target_peak / peak_before as f32;

    // Never attenuate — that's outside the "boost without clipping" contract.
    if raw_scale <= 1.0 {
        return NormalizeReport {
            scale: 1.0,
            peak_before,
            peak_after: peak_before,
            data_bytes,
        };
    }

    let mut peak_after: u16 = 0;
    for c in pcm.as_chunks_mut::<2>().0 {
        let s = i16::from_le_bytes(*c) as f32;
        // Round then saturate. Hitting MAX/MIN is fine; it just means we hit
        // the target peak exactly.
        let scaled = (s * raw_scale)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        *c = scaled.to_le_bytes();
        let mag = scaled.saturating_abs().unsigned_abs();
        if mag > peak_after {
            peak_after = mag;
        }
    }

    NormalizeReport {
        scale: raw_scale,
        peak_before,
        peak_after,
        data_bytes,
    }
}

/// Locate the PCM `data` chunk in a canonical RIFF/WAVE buffer. Returns
/// `(data_start, data_end, bits_per_sample)`. Walks the chunk list rather
/// than assuming the 44-byte layout, so files with `LIST`/`bext`/etc. between
/// `fmt ` and `data` still parse.
fn locate_wav_data(bytes: &[u8]) -> Result<(usize, usize, u16), NormalizeError> {
    if bytes.len() < 44 {
        return Err(NormalizeError::TooShort(bytes.len()));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(NormalizeError::NotWav);
    }

    let mut bits_per_sample: Option<u16> = None;
    let mut audio_format: Option<u16> = None;
    let mut cursor: usize = 12;
    while cursor + 8 <= bytes.len() {
        let id: [u8; 4] = bytes[cursor..cursor + 4].try_into().unwrap();
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let body_start = cursor + 8;
        let body_end = body_start.saturating_add(size);
        match &id {
            b"fmt " => {
                if body_end > bytes.len() || size < 16 {
                    return Err(NormalizeError::NotWav);
                }
                audio_format = Some(u16::from_le_bytes(
                    bytes[body_start..body_start + 2].try_into().unwrap(),
                ));
                bits_per_sample = Some(u16::from_le_bytes(
                    bytes[body_start + 14..body_start + 16].try_into().unwrap(),
                ));
            }
            b"data" => {
                let bps = bits_per_sample.ok_or(NormalizeError::NotWav)?;
                let fmt = audio_format.unwrap_or(0);
                if fmt != 1 {
                    return Err(NormalizeError::UnsupportedFormat { fmt, bps });
                }
                // Streaming WAVs write 0xFFFFFFFF in data-size; trust the file's
                // tail in that case. Also clamp to the buffer end defensively.
                let claimed_end = if size as u32 == u32::MAX || size == 0 {
                    bytes.len()
                } else {
                    body_end
                };
                if claimed_end > bytes.len() {
                    return Err(NormalizeError::TruncatedData {
                        claimed: size,
                        available: bytes.len() - body_start,
                    });
                }
                return Ok((body_start, claimed_end, bps));
            }
            _ => {}
        }
        // Chunks are word-aligned: pad odd sizes by 1.
        let padded = size + (size & 1);
        cursor = body_start
            .checked_add(padded)
            .ok_or(NormalizeError::NotWav)?;
    }
    Err(NormalizeError::MissingDataChunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal canonical WAV (mono, 22050 Hz, 16-bit PCM) with the
    /// supplied samples. Same layout as `kobbler::audio::clip::write_pcm_as_wav`.
    fn build_wav(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
        let bits_per_sample: u16 = 16;
        let block_align: u16 = channels * (bits_per_sample / 8);
        let byte_rate: u32 = sample_rate * block_align as u32;
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let data_len = pcm.len() as u32;
        let mut out = Vec::with_capacity(44 + pcm.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36u32 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits_per_sample.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend_from_slice(&pcm);
        out
    }

    fn data_chunk_samples(wav: &[u8]) -> Vec<i16> {
        // Walk to find the data chunk, then unpack samples.
        let (start, end, _) = locate_wav_data(wav).expect("valid wav");
        wav[start..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect()
    }

    #[test]
    fn boosts_quiet_clip_to_target_peak() {
        // 10% of full scale → expect roughly 8.91x scaling for -1 dBFS.
        let quiet: Vec<i16> = (0..1000)
            .map(|i| ((i as f32 / 1000.0 * std::f32::consts::TAU).sin() * 3000.0) as i16)
            .collect();
        let mut wav = build_wav(&quiet, 22050, 1);
        let report = peak_normalize_wav_in_place(&mut wav, DEFAULT_TARGET_PEAK_DBFS).unwrap();

        assert!(report.peak_before < 3100);
        assert!(
            report.peak_after > 28_000,
            "expected near full-scale after norm; got {}",
            report.peak_after
        );
        // -1 dBFS target peak = ~29204; allow a wide rounding margin.
        assert!(report.peak_after <= 29_300);
        assert!(
            report.scale > 8.0,
            "scale should boost ~9x; got {}",
            report.scale
        );

        // Envelope shape preserved: samples should be quiet × scale, within rounding.
        let scaled = data_chunk_samples(&wav);
        for (orig, after) in quiet.iter().zip(scaled.iter()) {
            let expected = (*orig as f32 * report.scale).round() as i32;
            let actual = *after as i32;
            assert!(
                (expected - actual).abs() <= 2,
                "envelope drift: orig={orig} after={after} expected={expected}"
            );
        }
    }

    #[test]
    fn leaves_already_loud_clip_alone() {
        // Samples near full scale: no headroom to boost.
        let loud: Vec<i16> = (0..200)
            .map(|i| if i % 2 == 0 { 30_000 } else { -30_000 })
            .collect();
        let mut wav = build_wav(&loud, 22050, 1);
        let before = data_chunk_samples(&wav);

        let report = peak_normalize_wav_in_place(&mut wav, DEFAULT_TARGET_PEAK_DBFS).unwrap();
        assert_eq!(report.scale, 1.0);
        assert_eq!(report.peak_before, report.peak_after);
        let after = data_chunk_samples(&wav);
        assert_eq!(before, after, "louder-than-target clip should be untouched");
    }

    #[test]
    fn handles_silent_clip_without_panicking() {
        let silent: Vec<i16> = vec![0; 100];
        let mut wav = build_wav(&silent, 22050, 1);
        let report = peak_normalize_wav_in_place(&mut wav, DEFAULT_TARGET_PEAK_DBFS).unwrap();
        assert_eq!(report.scale, 1.0);
        assert_eq!(report.peak_before, 0);
        assert_eq!(report.peak_after, 0);
    }

    #[test]
    fn never_clips() {
        // Mix of magnitudes; verify post-normalisation peak stays within i16 range.
        let samples: Vec<i16> = (0..2000)
            .map(|i| ((i as f32 / 200.0 * std::f32::consts::TAU).sin() * 5_000.0) as i16)
            .collect();
        let mut wav = build_wav(&samples, 22050, 1);
        peak_normalize_wav_in_place(&mut wav, DEFAULT_TARGET_PEAK_DBFS).unwrap();
        let after = data_chunk_samples(&wav);
        let target_amp = 10f32.powf(DEFAULT_TARGET_PEAK_DBFS / 20.0);
        let target_peak = (target_amp * i16::MAX as f32).round() as i32;
        for s in &after {
            assert!((*s as i32).abs() <= target_peak + 1);
        }
    }

    #[test]
    fn rejects_non_wav() {
        // ≥44 bytes so we trip the RIFF/WAVE magic check, not the size check.
        let mut not_wav = vec![b'x'; 64];
        assert!(matches!(
            peak_normalize_wav_in_place(&mut not_wav, DEFAULT_TARGET_PEAK_DBFS),
            Err(NormalizeError::NotWav)
        ));
    }

    #[test]
    fn rejects_short_buffer() {
        let mut tiny = vec![b'R', b'I', b'F', b'F'];
        assert!(matches!(
            peak_normalize_wav_in_place(&mut tiny, DEFAULT_TARGET_PEAK_DBFS),
            Err(NormalizeError::TooShort(_))
        ));
    }

    #[test]
    fn rejects_unsupported_bitdepth() {
        let mut wav = build_wav(&[0i16, 0, 0, 0], 22050, 1);
        // Manually rewrite bits-per-sample to 24
        wav[34] = 24;
        let res = peak_normalize_wav_in_place(&mut wav, DEFAULT_TARGET_PEAK_DBFS);
        assert!(matches!(
            res,
            Err(NormalizeError::UnsupportedFormat { bps: 24, .. })
        ));
    }

    #[test]
    fn rejects_invalid_target() {
        let mut wav = build_wav(&[0i16], 22050, 1);
        assert!(matches!(
            peak_normalize_wav_in_place(&mut wav, 3.0),
            Err(NormalizeError::InvalidTarget)
        ));
        assert!(matches!(
            peak_normalize_wav_in_place(&mut wav, f32::NAN),
            Err(NormalizeError::InvalidTarget)
        ));
    }

    #[test]
    fn raw_pcm_scales_in_place() {
        let mut pcm: Vec<u8> = (0..400i16)
            .map(|i| (i - 200) * 50)
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let report = peak_normalize_s16_pcm(&mut pcm, DEFAULT_TARGET_PEAK_DBFS);
        assert!(report.scale > 1.0);
        assert!(report.peak_after > 28_000);
    }

    #[test]
    fn parses_wav_with_inter_chunk_padding() {
        // Build a WAV but insert a junk LIST chunk between fmt and data.
        let samples = [5000i16; 100];
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        // placeholder — patched after build
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");

        // fmt chunk
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // channels
        out.extend_from_slice(&22050u32.to_le_bytes());
        out.extend_from_slice(&44100u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes()); // block_align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits_per_sample

        // junk LIST chunk
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&5u32.to_le_bytes()); // odd size → 1 byte pad
        out.extend_from_slice(b"INFO\0");
        out.push(0); // pad

        // data chunk
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&pcm);

        // Patch RIFF size
        let riff_size = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff_size.to_le_bytes());

        let report = peak_normalize_wav_in_place(&mut out, DEFAULT_TARGET_PEAK_DBFS).unwrap();
        assert!(report.scale > 1.0);
        assert_eq!(report.data_bytes, pcm.len());
    }
}
