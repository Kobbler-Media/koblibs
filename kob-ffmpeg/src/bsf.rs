//! Bitstream filter wrapper.
//!
//! Wraps FFmpeg's `AVBSFContext` for packet-level filtering without
//! re-encoding. Primary use: `aac_adtstoasc` to strip ADTS headers
//! when remuxing AAC into MP4/fMP4 containers.

use std::ffi::CString;
use std::ptr;

use crate::Result;
use crate::ff_check;
use crate::ffi;
use crate::packet::Packet;

/// Safe wrapper around `AVBSFContext`.
pub struct BitstreamFilter {
    ptr: *mut ffi::AVBSFContext,
}

unsafe impl Send for BitstreamFilter {}

impl BitstreamFilter {
    /// Create and initialize a bitstream filter.
    ///
    /// `name` is the filter name (e.g., `"aac_adtstoasc"`).
    /// `codec_params` and `time_base` come from the input stream being filtered.
    ///
    /// # Safety
    /// `codec_params` must point to a valid `AVCodecParameters`.
    pub unsafe fn new(
        name: &str,
        codec_params: *const ffi::AVCodecParameters,
        time_base: ffi::AVRational,
    ) -> Result<Self> {
        let c_name = CString::new(name).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "BSF name contains null byte")
        })?;

        let filter = unsafe { ffi::av_bsf_get_by_name(c_name.as_ptr()) };
        if filter.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_BSF_NOT_FOUND,
                format!("bitstream filter '{name}' not found"),
            ));
        }

        let mut ctx: *mut ffi::AVBSFContext = ptr::null_mut();
        unsafe {
            ff_check!(
                ffi::av_bsf_alloc(filter, &mut ctx),
                "allocating BSF context for '{name}'"
            )?;
        }

        if ctx.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "av_bsf_alloc returned null",
            ));
        }

        unsafe {
            ff_check!(
                ffi::avcodec_parameters_copy((*ctx).par_in, codec_params),
                "copying codec parameters to BSF"
            )?;
            (*ctx).time_base_in = time_base;

            ff_check!(ffi::av_bsf_init(ctx), "initializing BSF '{name}'")?;
        }

        Ok(Self { ptr: ctx })
    }

    /// Return the BSF's output codec parameters.
    ///
    /// After `av_bsf_init`, `par_out` contains the transformed codec config.
    /// For `aac_adtstoasc`, this has raw AudioSpecificConfig instead of ADTS.
    pub fn output_codec_parameters(&self) -> *const ffi::AVCodecParameters {
        unsafe { (*self.ptr).par_out }
    }

    /// Filter a packet in-place (send + receive).
    ///
    /// For 1:1 filters like `aac_adtstoasc`, this sends the packet and
    /// receives the filtered result back into the same packet.
    pub fn filter_packet(&mut self, pkt: &mut Packet) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_bsf_send_packet(self.ptr, pkt.as_mut_ptr()),
                "BSF send_packet"
            )?;
            ff_check!(
                ffi::av_bsf_receive_packet(self.ptr, pkt.as_mut_ptr()),
                "BSF receive_packet"
            )?;
        }
        Ok(())
    }
}

impl Drop for BitstreamFilter {
    fn drop(&mut self) {
        unsafe {
            ffi::av_bsf_free(&mut self.ptr);
        }
    }
}
