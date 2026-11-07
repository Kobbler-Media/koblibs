//! HEVC stream copy into fMP4 as `hvc1`.
//!
//! Browsers want `hvc1`, whose `hvcC` record must carry the VPS/SPS/PPS. A `hev1` source
//! may carry them only in-band, leaving a header-only `hvcC`; the muxer then writes an
//! empty `hvcC` box and no decoder can open the stream. Such a record is rebuilt from
//! the parameter sets in the first keyframe before the header is written.

use std::collections::VecDeque;

use crate::format::input::InputContext;
use crate::format::output::OutputContext;
use crate::packet::Packet;
use crate::{Result, ffi};

const VPS: u8 = 32;
const SPS: u8 = 33;
const PPS: u8 = 34;
const RECORD_HEADER_LEN: usize = 23;

/// Prepares a copied HEVC output stream for fMP4: fills a parameter-set-less `hvcC`
/// from the first keyframe and tags the stream `hvc1`. Packets read to find that
/// keyframe are appended to `buffered` for the caller to write.
pub(crate) fn prepare_copied_hevc(
    in_ctx: &mut InputContext,
    in_index: u32,
    out_ctx: &mut OutputContext,
    out_index: i32,
    buffered: &mut VecDeque<Packet>,
) -> Result<()> {
    let par = unsafe {
        let stream = *ffi::kff_fmt_streams(out_ctx.as_ptr()).add(out_index as usize);
        ffi::kff_stream_codecpar(stream)
    };
    if unsafe { (*par).codec_id } != ffi::AV_CODEC_ID_HEVC {
        return Ok(());
    }
    let record = unsafe { extradata(par) }.to_vec();
    if is_hvcc(&record) && !has_parameter_sets(&record) {
        let mut rebuilt = None;
        let mut pkt = Packet::new()?;
        while in_ctx.read_packet(&mut pkt)? {
            let keyframe = pkt.stream_index() == in_index as i32 && pkt.is_keyframe();
            if keyframe {
                rebuilt = with_parameter_sets(&record, pkt.data());
            }
            buffered.push_back(std::mem::replace(&mut pkt, Packet::new()?));
            if keyframe {
                break;
            }
        }
        match rebuilt {
            Some(r) => unsafe { set_extradata(par, &r) },
            None => return Ok(()),
        }
    }
    unsafe { (*par).codec_tag = ffi::mktag(b'h', b'v', b'c', b'1') };
    Ok(())
}

unsafe fn extradata<'a>(par: *const ffi::AVCodecParameters) -> &'a [u8] {
    unsafe {
        if (*par).extradata.is_null() || (*par).extradata_size <= 0 {
            return &[];
        }
        std::slice::from_raw_parts((*par).extradata, (*par).extradata_size as usize)
    }
}

unsafe fn set_extradata(par: *mut ffi::AVCodecParameters, data: &[u8]) {
    unsafe {
        let padding = ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        let buf = ffi::av_mallocz(data.len() + padding) as *mut u8;
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len());
        ffi::av_freep(&raw mut (*par).extradata as *mut std::ffi::c_void);
        (*par).extradata = buf;
        (*par).extradata_size = data.len() as i32;
    }
}

fn is_hvcc(record: &[u8]) -> bool {
    record.len() >= RECORD_HEADER_LEN && record[0] == 1
}

fn has_parameter_sets(record: &[u8]) -> bool {
    let mut seen = Vec::new();
    let mut at = RECORD_HEADER_LEN;
    for _ in 0..record[RECORD_HEADER_LEN - 1] {
        let Some(head) = record.get(at..at + 3) else {
            return false;
        };
        let nal_type = head[0] & 0x3f;
        let count = u16::from_be_bytes([head[1], head[2]]);
        at += 3;
        for _ in 0..count {
            let Some(len) = record.get(at..at + 2) else {
                return false;
            };
            at += 2 + u16::from_be_bytes([len[0], len[1]]) as usize;
        }
        if count > 0 {
            seen.push(nal_type);
        }
    }
    [VPS, SPS, PPS].iter().all(|t| seen.contains(t))
}

fn with_parameter_sets(record: &[u8], access_unit: &[u8]) -> Option<Vec<u8>> {
    let length_size = usize::from(record[21] & 0x3) + 1;
    let mut sets: [Vec<&[u8]>; 3] = Default::default();
    let mut at = 0;
    while at + length_size <= access_unit.len() {
        let len = access_unit[at..at + length_size]
            .iter()
            .fold(0usize, |n, &b| (n << 8) | usize::from(b));
        let nal = access_unit.get(at + length_size..at + length_size + len)?;
        at += length_size + len;
        let nal_type = (nal.first()? >> 1) & 0x3f;
        if let Some(slot) = [VPS, SPS, PPS].iter().position(|&t| t == nal_type) {
            sets[slot].push(nal);
        }
    }
    if sets.iter().any(Vec::is_empty) {
        return None;
    }
    let mut out = record[..RECORD_HEADER_LEN - 1].to_vec();
    out.push(3);
    for (nal_type, nals) in [VPS, SPS, PPS].into_iter().zip(&sets) {
        out.push(0x80 | nal_type);
        out.extend_from_slice(&(nals.len() as u16).to_be_bytes());
        for nal in nals {
            out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            out.extend_from_slice(nal);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(arrays: u8) -> Vec<u8> {
        let mut h = vec![1u8; RECORD_HEADER_LEN - 1];
        h[21] = 0xff;
        h.push(arrays);
        h
    }

    fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut n = vec![nal_type << 1, 1];
        n.extend_from_slice(body);
        n
    }

    fn access_unit(nals: &[Vec<u8>]) -> Vec<u8> {
        nals.iter()
            .flat_map(|n| {
                let mut framed = (n.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(n);
                framed
            })
            .collect()
    }

    #[test]
    fn a_keyframe_with_in_band_parameter_sets_fills_an_empty_record() {
        let au = access_unit(&[
            nal(VPS, b"v"),
            nal(SPS, b"s"),
            nal(PPS, b"p"),
            nal(19, b"idr"),
        ]);
        let filled = with_parameter_sets(&header(0), &au).unwrap();
        assert!(has_parameter_sets(&filled));
        assert_eq!(&filled[..22], &header(0)[..22]);
    }

    #[test]
    fn a_keyframe_missing_a_parameter_set_fills_nothing() {
        let au = access_unit(&[nal(VPS, b"v"), nal(SPS, b"s"), nal(19, b"idr")]);
        assert_eq!(with_parameter_sets(&header(0), &au), None);
    }

    #[test]
    fn a_header_only_record_has_no_parameter_sets() {
        assert!(!has_parameter_sets(&header(0)));
    }
}
