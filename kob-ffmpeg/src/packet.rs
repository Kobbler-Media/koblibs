//! AVPacket wrapper with RAII resource management.

use crate::ffi;

/// Owns an `AVPacket`. Automatically freed on drop.
///
/// `Send` but not `Sync` — an AVPacket is not thread-safe for concurrent access,
/// but can be moved between threads.
pub struct Packet {
    ptr: *mut ffi::AVPacket,
}

// SAFETY: AVPacket can be sent between threads. It's just a data buffer
// with no thread-local state. Not Sync because concurrent access is unsafe.
unsafe impl Send for Packet {}

impl Packet {
    /// Allocate a new empty packet.
    pub fn new() -> crate::Result<Self> {
        let ptr = unsafe { ffi::av_packet_alloc() };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating packet",
            ));
        }
        Ok(Self { ptr })
    }

    /// Returns a raw pointer to the underlying AVPacket.
    /// The caller must not free or store this pointer beyond the lifetime of `self`.
    #[inline]
    pub fn as_ptr(&self) -> *const ffi::AVPacket {
        self.ptr
    }

    /// Returns a mutable raw pointer to the underlying AVPacket.
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVPacket {
        self.ptr
    }

    /// Unreference the packet data, resetting it to an empty state.
    pub fn unref(&mut self) {
        unsafe { ffi::av_packet_unref(self.ptr) };
    }

    /// Presentation timestamp in time_base units.
    #[inline]
    pub fn pts(&self) -> i64 {
        unsafe { (*self.ptr).pts }
    }

    /// Decompression timestamp in time_base units.
    #[inline]
    pub fn dts(&self) -> i64 {
        unsafe { (*self.ptr).dts }
    }

    /// Stream index this packet belongs to.
    #[inline]
    pub fn stream_index(&self) -> i32 {
        unsafe { (*self.ptr).stream_index }
    }

    /// Packet flags (e.g., AV_PKT_FLAG_KEY).
    #[inline]
    pub fn flags(&self) -> i32 {
        unsafe { (*self.ptr).flags }
    }

    /// Whether this is a keyframe packet.
    #[inline]
    pub fn is_keyframe(&self) -> bool {
        self.flags() & ffi::AV_PKT_FLAG_KEY as i32 != 0
    }

    /// Size of the packet data in bytes.
    #[inline]
    pub fn size(&self) -> i32 {
        unsafe { (*self.ptr).size }
    }

    /// Duration of this packet in time_base units.
    #[inline]
    pub fn duration(&self) -> i64 {
        unsafe { (*self.ptr).duration }
    }

    /// Raw packet data as a byte slice. Returns empty slice if no data.
    pub fn data(&self) -> &[u8] {
        unsafe {
            let pkt = &*self.ptr;
            if pkt.data.is_null() || pkt.size <= 0 {
                &[]
            } else {
                std::slice::from_raw_parts(pkt.data, pkt.size as usize)
            }
        }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { ffi::av_packet_free(&mut self.ptr) };
    }
}
