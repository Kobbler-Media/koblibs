use kob_ffmpeg::pcm_stream::{AdtsAacEncoder, decode_to_pcm};

const RATE: i32 = 24_000;

fn tone(samples: usize) -> Vec<u8> {
    (0..samples)
        .flat_map(|i| {
            let t = i as f64 / RATE as f64;
            (((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16).to_le_bytes()
        })
        .collect()
}

#[test]
fn pcm_pushed_in_uneven_pieces_comes_back_as_aac_of_the_same_length() {
    let pcm = tone(RATE as usize * 2);
    let mut enc = AdtsAacEncoder::new(RATE, 64_000).unwrap();
    let mut frames = Vec::new();
    for piece in pcm.chunks(3001) {
        frames.extend(enc.push_s16le(piece).unwrap());
    }
    frames.extend(enc.finish().unwrap());

    assert!(frames.windows(2).all(|w| w[0].pts < w[1].pts));
    let aac: Vec<u8> = frames
        .iter()
        .flat_map(|f| f.bytes.iter().copied())
        .collect();
    assert_eq!(&aac[..2], &[0xFF, 0xF1]);

    let decoded = decode_to_pcm(aac, RATE).unwrap();
    let ms = decoded.len() / 2 * 1000 / RATE as usize;
    assert!((2000..2120).contains(&ms), "decoded {ms} ms of 2000");
}

#[test]
fn a_stream_cut_at_any_frame_still_decodes() {
    let mut enc = AdtsAacEncoder::new(RATE, 64_000).unwrap();
    let mut frames = enc.push_s16le(&tone(RATE as usize)).unwrap();
    frames.extend(enc.finish().unwrap());

    let tail: Vec<u8> = frames[10..]
        .iter()
        .flat_map(|f| f.bytes.iter().copied())
        .collect();
    let decoded = decode_to_pcm(tail, RATE).unwrap();
    assert!(!decoded.is_empty());
}

#[test]
fn a_wav_decodes_to_the_rate_asked_for() {
    let pcm = tone(RATE as usize / 2);
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&(RATE as u32).to_le_bytes());
    wav.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);

    let decoded = decode_to_pcm(wav, 48_000).unwrap();
    let ms = decoded.len() / 2 * 1000 / 48_000;
    assert!((490..=510).contains(&ms), "decoded {ms} ms of 500");
}
