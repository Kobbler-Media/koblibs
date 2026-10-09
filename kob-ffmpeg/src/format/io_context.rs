//! Custom I/O context wrappers (AVIOContext with buffer/channel/callback backends).
//!
//! These enable reading from memory buffers or channels instead of files/URLs,
//! which is critical for IPTV stream probing and debrid streaming.

use crate::Result;
use crate::ffi;

/// Owns a custom `AVIOContext` backed by a Rust callback.
///
/// The I/O context reads data by calling back into Rust code,
/// enabling zero-copy streaming from HTTP connections, channels, etc.
pub struct CustomIoContext {
    avio_ctx: *mut ffi::AVIOContext,
    /// Prevent the buffer from being freed separately — it's owned by AVIOContext.
    _buffer: *mut u8,
    /// Box<Box<dyn IoCallbacks>> — double-boxed so we have a stable thin pointer to the fat pointer.
    _opaque: Box<Box<dyn IoCallbacks>>,
}

unsafe impl Send for CustomIoContext {}

/// Trait for custom I/O callbacks.
pub trait IoCallbacks: Send {
    /// Read up to `buf.len()` bytes into `buf`. Returns the number of bytes read,
    /// or a negative AVERROR code on error. Return AVERROR_EOF on end of stream.
    fn read(&mut self, buf: &mut [u8]) -> i32;

    /// Seek to a position. Returns the new position, or a negative AVERROR code.
    /// Optional — return AVERROR(ENOSYS) if seeking is not supported.
    fn seek(&mut self, _offset: i64, _whence: i32) -> i64 {
        -1 // Not supported by default
    }
}

/// Buffer-backed I/O: reads from a `Vec<u8>` in memory.
pub struct BufferIo {
    data: Vec<u8>,
    pos: usize,
}

impl BufferIo {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, pos: 0 }
    }
}

impl IoCallbacks for BufferIo {
    fn read(&mut self, buf: &mut [u8]) -> i32 {
        let remaining = self.data.len() - self.pos;
        if remaining == 0 {
            return ffi::AVERROR_EOF;
        }
        let to_read = buf.len().min(remaining);
        buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        to_read as i32
    }

    fn seek(&mut self, offset: i64, whence: i32) -> i64 {
        let new_pos = match whence {
            0 => offset,                              // SEEK_SET
            1 => self.pos as i64 + offset,            // SEEK_CUR
            2 => self.data.len() as i64 + offset,     // SEEK_END
            0x10000 => return self.data.len() as i64, // AVSEEK_SIZE
            _ => return -1,
        };
        if new_pos < 0 || new_pos > self.data.len() as i64 {
            return -1;
        }
        self.pos = new_pos as usize;
        new_pos
    }
}

/// Channel-backed I/O: reads from a `std::sync::mpsc::Receiver<Vec<u8>>`.
pub struct ChannelIo {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    pos: usize,
}

impl ChannelIo {
    pub fn new(rx: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffer: Vec::new(),
            pos: 0,
        }
    }
}

impl IoCallbacks for ChannelIo {
    /// Read bytes from the channel into `buf`.
    ///
    /// Partial reads (returning fewer bytes than `buf.len()`) are safe here:
    /// FFmpeg's `avio_read` handles short reads correctly by issuing follow-up
    /// reads for remaining data. The TS/fMP4 demuxer also resyncs automatically
    /// on packet boundaries, so there is no alignment concern even when feeding
    /// raw transport stream data through a channel.
    fn read(&mut self, buf: &mut [u8]) -> i32 {
        let mut written = 0;

        loop {
            // Drain current buffer first
            let remaining = self.buffer.len() - self.pos;
            if remaining > 0 {
                let to_copy = (buf.len() - written).min(remaining);
                buf[written..written + to_copy]
                    .copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
                self.pos += to_copy;
                written += to_copy;
                if written >= buf.len() {
                    return written as i32;
                }
            }

            // Need more data from channel
            match self.rx.recv() {
                Ok(data) => {
                    if data.is_empty() {
                        // Empty vec signals EOF
                        break;
                    }
                    self.buffer = data;
                    self.pos = 0;
                }
                Err(_) => break, // Channel closed = EOF
            }
        }

        if written > 0 {
            written as i32
        } else {
            ffi::AVERROR_EOF
        }
    }
}

impl CustomIoContext {
    const BUFFER_SIZE: usize = 32768;

    /// Create a custom I/O context from any IoCallbacks implementation.
    pub fn new(callbacks: impl IoCallbacks + 'static) -> Result<Self> {
        let buffer = unsafe { ffi::av_malloc(Self::BUFFER_SIZE) as *mut u8 };
        if buffer.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating I/O buffer",
            ));
        }

        let opaque: Box<dyn IoCallbacks> = Box::new(callbacks);
        let opaque = Box::new(opaque); // Box<Box<dyn IoCallbacks>> — stable pointer to fat pointer
        // SAFETY: opaque.as_ref() returns a reference into the heap-allocated outer Box,
        // which is stored in _opaque and lives for the lifetime of CustomIoContext.
        let opaque_ptr = opaque.as_ref() as *const Box<dyn IoCallbacks> as *mut std::ffi::c_void;

        let avio_ctx = unsafe {
            ffi::avio_alloc_context(
                buffer,
                Self::BUFFER_SIZE as i32,
                0, // read-only
                opaque_ptr,
                Some(read_callback),
                None, // no write
                Some(seek_callback),
            )
        };

        if avio_ctx.is_null() {
            unsafe { ffi::av_free(buffer as *mut std::ffi::c_void) };
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating AVIOContext",
            ));
        }

        // Mark as seekable so av_seek_frame uses the seek callback
        // instead of doing a slow sequential scan.
        unsafe {
            (*avio_ctx).seekable = ffi::AVIO_SEEKABLE_NORMAL as i32;
        }

        Ok(Self {
            avio_ctx,
            _buffer: buffer,
            _opaque: opaque,
        })
    }

    /// Create from a memory buffer.
    pub fn from_buffer(data: Vec<u8>) -> Result<Self> {
        Self::new(BufferIo::new(data))
    }

    /// Create from a channel receiver.
    pub fn from_channel(rx: std::sync::mpsc::Receiver<Vec<u8>>) -> Result<Self> {
        Self::new(ChannelIo::new(rx))
    }

    /// Get the raw AVIOContext pointer for use with avformat_open_input.
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVIOContext {
        self.avio_ctx
    }
}

impl Drop for CustomIoContext {
    fn drop(&mut self) {
        unsafe {
            // AVIOContext frees the buffer when freed
            ffi::avio_context_free(&mut self.avio_ctx);
        }
    }
}

/// C callback for reading data from the Rust IoCallbacks.
unsafe extern "C" fn read_callback(
    opaque: *mut std::ffi::c_void,
    buf: *mut u8,
    buf_size: i32,
) -> i32 {
    if buf.is_null() || buf_size <= 0 {
        return ffi::AVERROR_EINVAL;
    }
    unsafe {
        let callbacks = &mut *(opaque as *mut Box<dyn IoCallbacks>);
        let slice = std::slice::from_raw_parts_mut(buf, buf_size as usize);
        callbacks.read(slice)
    }
}

/// C callback for seeking.
unsafe extern "C" fn seek_callback(opaque: *mut std::ffi::c_void, offset: i64, whence: i32) -> i64 {
    unsafe {
        let callbacks = &mut *(opaque as *mut Box<dyn IoCallbacks>);
        callbacks.seek(offset, whence)
    }
}
