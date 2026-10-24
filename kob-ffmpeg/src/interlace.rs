use crate::ffi;

/// # Safety
/// `params` must be a valid (or null) `AVCodecParameters` pointer.
pub unsafe fn codec_parameters_interlaced(params: *const ffi::AVCodecParameters) -> bool {
    if params.is_null() {
        return false;
    }
    let p = unsafe { &*params };
    if !matches!(
        p.field_order,
        ffi::AV_FIELD_PROGRESSIVE | ffi::AV_FIELD_UNKNOWN
    ) {
        return true;
    }
    if p.codec_id != ffi::AV_CODEC_ID_H264 || p.extradata.is_null() || p.extradata_size <= 0 {
        return false;
    }
    let extradata = unsafe { std::slice::from_raw_parts(p.extradata, p.extradata_size as usize) };
    h264_field_coded(extradata).unwrap_or(false)
}

// Films carried in a 1080i service, and x264's fake-interlaced, code progressive frames
// under a field-capable SPS: field_order reads progressive, yet some hardware decoders
// (Pixel 10's c2.google.avc.decoder) emit nothing for any SPS with frame_mbs_only_flag=0.
fn h264_field_coded(extradata: &[u8]) -> Option<bool> {
    let sps = first_sps(extradata)?;
    frame_mbs_only_flag(&unescape(sps)).map(|frame_mbs_only| !frame_mbs_only)
}

fn first_sps(extradata: &[u8]) -> Option<&[u8]> {
    if extradata.first() == Some(&1) {
        let count = *extradata.get(5)? & 0x1f;
        if count == 0 {
            return None;
        }
        let len = u16::from_be_bytes([*extradata.get(6)?, *extradata.get(7)?]) as usize;
        return extradata.get(8..8 + len);
    }
    annex_b_nals(extradata).find(|nal| nal.first().is_some_and(|h| h & 0x1f == 7))
}

fn annex_b_nals(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let starts: Vec<usize> = (0..data.len().saturating_sub(2))
        .filter(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
        .map(|i| i + 3)
        .collect();
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| s - 3)
        .chain([data.len()])
        .collect();
    starts.into_iter().zip(ends).map(move |(s, e)| &data[s..e])
}

fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn frame_mbs_only_flag(sps: &[u8]) -> Option<bool> {
    let mut r = BitReader {
        data: sps.get(1..)?,
        pos: 0,
    };
    let profile_idc = r.bits(8)?;
    r.bits(16)?;
    r.ue()?;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.bit()? {
                    r.skip_scaling_list(if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?;
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?;
    r.bit()?;
    r.ue()?;
    r.ue()?;
    r.bit()
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<bool> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = byte >> (7 - self.pos % 8) & 1 == 1;
        self.pos += 1;
        Some(bit)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |acc, _| Some(acc << 1 | self.bit()? as u32))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut leading = 0;
        while !self.bit()? {
            leading += 1;
            if leading > 31 {
                return None;
            }
        }
        Some((1 << leading) - 1 + self.bits(leading)?)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }

    fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
        let mut last = 8i32;
        let mut next = 8i32;
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()? + 256) % 256;
            }
            last = if next == 0 { last } else { next };
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct BitWriter {
        bits: Vec<bool>,
    }

    impl BitWriter {
        fn bits(&mut self, n: u32, v: u32) -> &mut Self {
            self.bits.extend((0..n).rev().map(|i| v >> i & 1 == 1));
            self
        }
        fn ue(&mut self, v: u32) -> &mut Self {
            let len = 32 - (v + 1).leading_zeros();
            self.bits(len - 1, 0).bits(len, v + 1)
        }
        fn se(&mut self, v: i32) -> &mut Self {
            self.ue(if v > 0 {
                2 * v as u32 - 1
            } else {
                (-2 * v) as u32
            })
        }
        fn bytes(&self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|c| {
                    c.iter()
                        .enumerate()
                        .fold(0, |b, (i, &x)| b | (x as u8) << (7 - i))
                })
                .collect()
        }
    }

    // High 4:4:4 with every scaling list present and POC type 1: the SPS branches x264 never writes.
    fn high444_sps(frame_mbs_only: bool) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(8, 0x67).bits(8, 244).bits(16, 0).ue(0);
        w.ue(3).bits(1, 0).ue(2).ue(2).bits(1, 0).bits(1, 1);
        for i in 0..12 {
            w.bits(1, 1);
            for _ in 0..if i < 6 { 16 } else { 64 } {
                w.se(-3);
            }
        }
        w.ue(4).ue(1).bits(1, 0).se(-2).se(5).ue(2).se(7).se(-1);
        w.ue(4)
            .bits(1, 0)
            .ue(119)
            .ue(67)
            .bits(1, frame_mbs_only as u32)
            .bits(1, 1);
        w.bytes()
    }

    #[test]
    fn reads_frame_mbs_only_past_scaling_lists_and_poc_type_1() {
        for frame_mbs_only in [true, false] {
            let nal = high444_sps(frame_mbs_only);
            let mut annex_b = vec![0, 0, 0, 1];
            annex_b.extend(&nal);
            let mut avcc = vec![1, 244, 0, 40, 0xff, 0xe1];
            avcc.extend((nal.len() as u16).to_be_bytes());
            avcc.extend(&nal);
            for extradata in [annex_b, avcc] {
                assert_eq!(h264_field_coded(&extradata), Some(!frame_mbs_only));
            }
        }
    }

    #[test]
    fn strips_emulation_prevention_bytes() {
        assert_eq!(
            unescape(&[0x67, 0, 0, 3, 1, 0, 0, 3, 0]),
            vec![0x67, 0, 0, 1, 0, 0, 0]
        );
    }
}
