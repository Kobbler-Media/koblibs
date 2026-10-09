//! Input format context wrapper (AVFormatContext for reading).

use std::ffi::CString;
use std::path::Path;
use std::ptr;

use crate::ffi;
use crate::packet::Packet;
use crate::{Result, ff_check};

/// Owns an `AVFormatContext` opened for reading.
/// Automatically closed on drop.
///
/// `Send` but not `Sync` — not safe for concurrent access from multiple threads.
pub struct InputContext {
    ptr: *mut ffi::AVFormatContext,
}

unsafe impl Send for InputContext {}

impl InputContext {
    /// Configure lenient demuxing flags on an input context.
    ///
    /// Called after `avformat_open_input` to tell the demuxer to discard
    /// corrupt packets and generate missing timestamps. This prevents
    /// corrupt containers from crashing the process.
    fn configure_lenient_demuxing(ptr: *mut ffi::AVFormatContext) {
        unsafe {
            let flags = ffi::kff_fmt_flags(ptr);
            ffi::kff_fmt_set_flags(
                ptr,
                flags
                    | ffi::AVFMT_FLAG_GENPTS as i32
                    | ffi::AVFMT_FLAG_DISCARD_CORRUPT as i32
                    | ffi::AVFMT_FLAG_SORT_DTS as i32,
            );
        }
    }

    /// Open a media file or URL for reading.
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with_options(path, &[])
    }

    /// Open a media file or URL with protocol/format options.
    ///
    /// Options are key-value pairs passed to `avformat_open_input` via `AVDictionary`.
    /// Useful for setting protocol options like HTTP reconnect:
    /// ```ignore
    /// InputContext::open_with_options(url, &[
    ///     ("reconnect", "1"),
    ///     ("reconnect_streamed", "1"),
    ///     ("reconnect_delay_max", "30"),
    /// ])?;
    /// ```
    pub fn open_with_options(path: &str, options: &[(&str, &str)]) -> Result<Self> {
        crate::init();

        let c_path = CString::new(path).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path contains null byte")
        })?;

        // Build AVDictionary from options
        let mut dict: *mut ffi::AVDictionary = ptr::null_mut();
        for (key, value) in options {
            let c_key = CString::new(*key).map_err(|_| {
                crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "option key contains null byte")
            })?;
            let c_value = CString::new(*value).map_err(|_| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    "option value contains null byte",
                )
            })?;
            unsafe {
                ffi::av_dict_set(&mut dict, c_key.as_ptr(), c_value.as_ptr(), 0);
            }
        }

        let mut ptr: *mut ffi::AVFormatContext = ptr::null_mut();
        let ret = unsafe {
            ffi::avformat_open_input(
                &mut ptr,
                c_path.as_ptr(),
                ptr::null(),
                if dict.is_null() {
                    ptr::null_mut()
                } else {
                    &mut dict
                },
            )
        };

        // Free any unused dict entries
        if !dict.is_null() {
            unsafe { ffi::av_dict_free(&mut dict) };
        }

        ff_check!(ret, "opening input: {}", path)?;
        Self::configure_lenient_demuxing(ptr);
        Ok(Self { ptr })
    }

    /// Open with an explicit input format name and options.
    ///
    /// Used for demuxers that can't be auto-detected from the file extension,
    /// e.g. the `concat` demuxer for joining segments.
    pub fn open_with_format(
        path: &str,
        format_name: &str,
        options: &[(&str, &str)],
    ) -> Result<Self> {
        crate::init();

        let c_path = CString::new(path).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path contains null byte")
        })?;
        let c_format = CString::new(format_name).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "format name contains null byte")
        })?;

        let input_format = unsafe { ffi::av_find_input_format(c_format.as_ptr()) };
        if input_format.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!("unknown input format: {format_name}"),
            ));
        }

        let mut dict: *mut ffi::AVDictionary = ptr::null_mut();
        for (key, value) in options {
            let c_key = CString::new(*key).map_err(|_| {
                crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "option key contains null byte")
            })?;
            let c_value = CString::new(*value).map_err(|_| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    "option value contains null byte",
                )
            })?;
            unsafe {
                ffi::av_dict_set(&mut dict, c_key.as_ptr(), c_value.as_ptr(), 0);
            }
        }

        let mut ptr: *mut ffi::AVFormatContext = ptr::null_mut();
        let ret = unsafe {
            ffi::avformat_open_input(
                &mut ptr,
                c_path.as_ptr(),
                input_format,
                if dict.is_null() {
                    ptr::null_mut()
                } else {
                    &mut dict
                },
            )
        };

        if !dict.is_null() {
            unsafe { ffi::av_dict_free(&mut dict) };
        }

        ff_check!(ret, "opening input with format {}: {}", format_name, path)?;
        Self::configure_lenient_demuxing(ptr);
        Ok(Self { ptr })
    }

    /// Open a concat demuxer list file for segment concatenation.
    pub fn open_concat(list_path: &Path) -> Result<Self> {
        let s = list_path.to_str().ok_or_else(|| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path is not valid UTF-8")
        })?;
        Self::open_with_format(s, "concat", &[("safe", "0")])
    }

    /// Open from a custom I/O context (buffer, channel, or callback-backed).
    ///
    /// The `CustomIoContext` must outlive the returned `InputContext`.
    /// The caller retains ownership of the I/O context.
    pub fn open_custom(io: &mut crate::format::io_context::CustomIoContext) -> Result<Self> {
        crate::init();
        let mut ptr: *mut ffi::AVFormatContext = unsafe { ffi::avformat_alloc_context() };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating format context",
            ));
        }
        unsafe {
            ffi::kff_fmt_set_pb(ptr, io.as_mut_ptr());
            ff_check!(
                ffi::avformat_open_input(
                    &mut ptr,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                ),
                "opening input from custom I/O"
            )?;
        }
        Self::configure_lenient_demuxing(ptr);
        Ok(Self { ptr })
    }

    /// Open a media file from a filesystem path.
    pub fn open_path(path: &Path) -> Result<Self> {
        let s = path.to_str().ok_or_else(|| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path is not valid UTF-8")
        })?;
        Self::open(s)
    }

    /// Analyze the stream to populate codec and timing information.
    /// Must be called after `open()` before reading streams or metadata.
    pub fn find_stream_info(&mut self) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::avformat_find_stream_info(self.ptr, ptr::null_mut()),
                "finding stream info"
            )?;
        }
        Ok(())
    }

    /// Set the probesize (maximum bytes to read for format detection).
    pub fn set_probesize(&mut self, size: i64) {
        unsafe { ffi::kff_fmt_set_probesize(self.ptr, size) };
    }

    /// Set the maximum analyze duration in microseconds.
    pub fn set_max_analyze_duration(&mut self, duration_us: i64) {
        unsafe { ffi::kff_fmt_set_max_analyze_duration(self.ptr, duration_us) };
    }

    /// Read the next packet from the input.
    /// Returns `Ok(true)` if a packet was read, `Ok(false)` on EOF.
    pub fn read_packet(&mut self, packet: &mut Packet) -> Result<bool> {
        packet.unref();
        let ret = unsafe { ffi::av_read_frame(self.ptr, packet.as_mut_ptr()) };
        if ret == ffi::AVERROR_EOF {
            return Ok(false);
        }
        ff_check!(ret, "reading packet")?;
        Ok(true)
    }

    /// Seek to a timestamp in a specific stream.
    /// `stream_index` of -1 uses the default stream.
    /// `timestamp` is in time_base units of the specified stream (or AV_TIME_BASE if -1).
    pub fn seek(&mut self, stream_index: i32, timestamp: i64, flags: i32) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_seek_frame(self.ptr, stream_index, timestamp, flags),
                "seeking to timestamp {}",
                timestamp
            )?;
        }
        Ok(())
    }

    /// Number of streams in the input.
    pub fn nb_streams(&self) -> u32 {
        unsafe { ffi::kff_fmt_nb_streams(self.ptr) }
    }

    /// Get a stream by index. Returns None if index is out of range.
    pub fn stream(&self, index: u32) -> Option<Stream<'_>> {
        if index >= self.nb_streams() {
            return None;
        }
        unsafe {
            let streams = ffi::kff_fmt_streams(self.ptr);
            let stream_ptr = *streams.add(index as usize);
            if stream_ptr.is_null() {
                None
            } else {
                Some(Stream {
                    ptr: stream_ptr,
                    _ctx: std::marker::PhantomData,
                })
            }
        }
    }

    /// Get a mutable stream by index. Returns None if index is out of range.
    pub fn stream_mut(&mut self, index: u32) -> Option<Stream<'_>> {
        if index >= self.nb_streams() {
            return None;
        }
        unsafe {
            let streams = ffi::kff_fmt_streams(self.ptr);
            let stream_ptr = *streams.add(index as usize);
            if stream_ptr.is_null() {
                None
            } else {
                Some(Stream {
                    ptr: stream_ptr,
                    _ctx: std::marker::PhantomData,
                })
            }
        }
    }

    /// Discard all streams except the one at `keep_index`.
    ///
    /// Sets `AVDISCARD_ALL` on every other stream so the demuxer skips
    /// their packets entirely. Critical for NFS performance when only
    /// one stream type (e.g. audio) is needed from a large container.
    pub fn discard_all_except(&mut self, keep_index: u32) {
        for i in 0..self.nb_streams() {
            if i != keep_index
                && let Some(mut stream) = self.stream_mut(i)
            {
                stream.set_discard(ffi::AVDISCARD_ALL);
            }
        }
    }

    /// Iterator over all streams.
    pub fn streams(&self) -> impl Iterator<Item = Stream<'_>> {
        (0..self.nb_streams()).filter_map(move |i| self.stream(i))
    }

    /// Find the "best" stream of a given type (video, audio, subtitle).
    /// Returns the stream index, or None if no such stream exists.
    pub fn find_best_stream(&self, media_type: MediaType) -> Option<u32> {
        let ret = unsafe {
            ffi::av_find_best_stream(self.ptr, media_type.to_av(), -1, -1, ptr::null_mut(), 0)
        };
        if ret >= 0 { Some(ret as u32) } else { None }
    }

    /// Returns `Some(idx)` only when the caller-supplied index actually points
    /// at a stream of the expected codec type. Wrong-type indices are dropped
    /// with a `warn!` so the caller's `or_else(find_best_stream(...))` can
    /// recover instead of opening a video decoder for an audio encoder (or
    /// vice versa) and crashing inside `avcodec_open2`.
    ///
    /// Caller-supplied indices come from persisted DB rows (e.g. kobbler's
    /// `audio_tracks.track_index`) which can be stale or wrong; this is the
    /// boundary check.
    pub fn validate_stream_type(&self, explicit: Option<u32>, expected: MediaType) -> Option<u32> {
        let idx = explicit?;
        let actual = self.stream(idx).map(|s| s.codec_type());
        if actual == Some(expected) {
            Some(idx)
        } else {
            tracing::warn!(
                requested_index = idx,
                expected = ?expected,
                actual = ?actual,
                "explicit stream index points at wrong codec type; falling back to find_best_stream"
            );
            None
        }
    }

    /// Total duration in microseconds (AV_TIME_BASE units), or None if unknown.
    pub fn duration_us(&self) -> Option<i64> {
        let dur = unsafe { ffi::kff_fmt_duration(self.ptr) };
        if dur == ffi::AV_NOPTS_VALUE {
            None
        } else {
            Some(dur)
        }
    }

    /// Duration as fractional seconds, or None if unknown.
    pub fn duration_secs(&self) -> Option<f64> {
        self.duration_us()
            .map(|us| us as f64 / ffi::AV_TIME_BASE as f64)
    }

    /// Overall bitrate in bits/s, or None if unknown.
    pub fn bit_rate(&self) -> Option<i64> {
        let br = unsafe { ffi::kff_fmt_bit_rate(self.ptr) };
        if br > 0 { Some(br) } else { None }
    }

    /// Start time in microseconds (AV_TIME_BASE units), or None if unknown.
    pub fn start_time_us(&self) -> Option<i64> {
        let st = unsafe { ffi::kff_fmt_start_time(self.ptr) };
        if st == ffi::AV_NOPTS_VALUE {
            None
        } else {
            Some(st)
        }
    }

    /// Number of chapters.
    pub fn nb_chapters(&self) -> u32 {
        unsafe { ffi::kff_fmt_nb_chapters(self.ptr) }
    }

    /// Get a chapter by index. Returns None if index is out of range.
    pub fn chapter(&self, index: u32) -> Option<Chapter<'_>> {
        if index >= self.nb_chapters() {
            return None;
        }
        unsafe {
            let chapters = ffi::kff_fmt_chapters(self.ptr);
            let ch_ptr = *chapters.add(index as usize);
            if ch_ptr.is_null() {
                None
            } else {
                Some(Chapter {
                    ptr: ch_ptr,
                    _ctx: std::marker::PhantomData,
                })
            }
        }
    }

    /// Iterator over all chapters.
    pub fn chapters(&self) -> impl Iterator<Item = Chapter<'_>> {
        (0..self.nb_chapters()).filter_map(move |i| self.chapter(i))
    }

    /// Container-level metadata dictionary.
    pub fn metadata(&self) -> Dictionary<'_> {
        let dict = unsafe { ffi::kff_fmt_metadata(self.ptr) };
        Dictionary {
            ptr: dict,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Returns a raw pointer to the underlying AVFormatContext.
    #[inline]
    pub fn as_ptr(&self) -> *const ffi::AVFormatContext {
        self.ptr
    }

    /// Returns a mutable raw pointer to the underlying AVFormatContext.
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVFormatContext {
        self.ptr
    }
}

impl Drop for InputContext {
    fn drop(&mut self) {
        unsafe { ffi::avformat_close_input(&mut self.ptr) };
    }
}

// --- Stream ---

/// Borrowed reference to an AVStream within an InputContext.
pub struct Stream<'a> {
    ptr: *mut ffi::AVStream,
    _ctx: std::marker::PhantomData<&'a InputContext>,
}

impl Stream<'_> {
    /// Stream index within the format context.
    pub fn index(&self) -> i32 {
        unsafe { ffi::kff_stream_index(self.ptr) }
    }

    /// Codec parameters for this stream.
    pub fn codec_parameters(&self) -> *mut ffi::AVCodecParameters {
        unsafe { ffi::kff_stream_codecpar(self.ptr) }
    }

    /// Stream time base (rational number).
    pub fn time_base(&self) -> ffi::AVRational {
        unsafe { ffi::kff_stream_time_base(self.ptr) }
    }

    /// Stream duration in time_base units, or None if unknown.
    pub fn duration(&self) -> Option<i64> {
        let d = unsafe { ffi::kff_stream_duration(self.ptr) };
        if d == ffi::AV_NOPTS_VALUE {
            None
        } else {
            Some(d)
        }
    }

    /// Duration as fractional seconds, or None if unknown.
    pub fn duration_secs(&self) -> Option<f64> {
        let tb = self.time_base();
        self.duration()
            .map(|d| d as f64 * tb.num as f64 / tb.den as f64)
    }

    /// Number of frames, or None if unknown.
    pub fn nb_frames(&self) -> Option<i64> {
        let n = unsafe { ffi::kff_stream_nb_frames(self.ptr) };
        if n > 0 { Some(n) } else { None }
    }

    /// Stream disposition flags.
    pub fn disposition(&self) -> i32 {
        unsafe { ffi::kff_stream_disposition(self.ptr) }
    }

    /// Stream-level metadata dictionary.
    pub fn metadata(&self) -> Dictionary<'_> {
        let dict = unsafe { ffi::kff_stream_metadata(self.ptr) };
        Dictionary {
            ptr: dict,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Average frame rate (video streams).
    pub fn avg_frame_rate(&self) -> ffi::AVRational {
        unsafe { ffi::kff_stream_avg_frame_rate(self.ptr) }
    }

    /// Real base frame rate (video streams).
    pub fn r_frame_rate(&self) -> ffi::AVRational {
        unsafe { ffi::kff_stream_r_frame_rate(self.ptr) }
    }

    /// Sample aspect ratio.
    pub fn sample_aspect_ratio(&self) -> ffi::AVRational {
        unsafe { ffi::kff_stream_sample_aspect_ratio(self.ptr) }
    }

    /// Codec type (video, audio, subtitle, etc.).
    pub fn codec_type(&self) -> MediaType {
        let params = self.codec_parameters();
        let ct = unsafe { (*params).codec_type };
        MediaType::from_av(ct)
    }

    /// Codec ID.
    pub fn codec_id(&self) -> u32 {
        let params = self.codec_parameters();
        unsafe { (*params).codec_id }
    }

    /// Create a decoder for this stream.
    /// This is the safe wrapper — no `unsafe` needed at the call site.
    pub fn decoder(&self) -> crate::Result<crate::codec::decoder::Decoder> {
        unsafe { crate::codec::decoder::Decoder::from_codec_parameters(self.codec_parameters()) }
    }

    /// Create a hardware-accelerated decoder for this stream.
    /// The hardware device context is attached before the codec is opened,
    /// allowing the decoder to negotiate hardware pixel formats.
    pub fn decoder_hw(
        &self,
        hw_device: &crate::hw::device::HwDeviceContext,
    ) -> crate::Result<crate::codec::decoder::Decoder> {
        unsafe {
            crate::codec::decoder::Decoder::from_codec_parameters_hw(
                self.codec_parameters(),
                hw_device,
            )
        }
    }

    /// Decoder with a caller-specified `thread_count`. Pass `0` to let ffmpeg
    /// auto-pick frame/slice threads (big batch-decode speedup); pass `1` for
    /// single-thread latency behaviour. See `Decoder::from_codec_parameters_with_threads`.
    pub fn decoder_with_threads(
        &self,
        thread_count: i32,
    ) -> crate::Result<crate::codec::decoder::Decoder> {
        unsafe {
            crate::codec::decoder::Decoder::from_codec_parameters_with_threads(
                self.codec_parameters(),
                thread_count,
            )
        }
    }

    /// Hardware-accelerated decoder with a caller-specified `thread_count`.
    /// See `decoder_with_threads`.
    pub fn decoder_hw_with_threads(
        &self,
        hw_device: &crate::hw::device::HwDeviceContext,
        thread_count: i32,
    ) -> crate::Result<crate::codec::decoder::Decoder> {
        unsafe {
            crate::codec::decoder::Decoder::from_codec_parameters_hw_with_threads(
                self.codec_parameters(),
                hw_device,
                thread_count,
            )
        }
    }

    /// Stream time base as a Rust-native TimeBase.
    pub fn time_base_native(&self) -> crate::types::TimeBase {
        crate::types::TimeBase::from_av(self.time_base())
    }

    /// Pixel format (video streams).
    pub fn pixel_format(&self) -> crate::types::PixelFormat {
        let params = self.codec_parameters();
        crate::types::PixelFormat::from_raw(unsafe { (*params).format })
    }

    /// Sample format (audio streams).
    pub fn sample_format(&self) -> crate::types::SampleFormat {
        crate::types::SampleFormat::from_raw(unsafe { (*self.codec_parameters()).format })
    }

    /// Channel layout (audio streams).
    pub fn channel_layout(&self) -> crate::types::ChannelLayout {
        crate::types::ChannelLayout::from_raw(unsafe { (*self.codec_parameters()).ch_layout })
    }

    /// Set the discard flag on this stream.
    ///
    /// `AVDISCARD_ALL` tells the demuxer to skip this stream entirely —
    /// packets are never read from the file, which is critical for NFS
    /// performance when only one stream type is needed.
    pub fn set_discard(&mut self, discard: ffi::AVDiscard) {
        unsafe { ffi::kff_stream_set_discard(self.ptr, discard) };
    }

    /// Raw pointer to the underlying AVStream.
    pub fn as_ptr(&self) -> *const ffi::AVStream {
        self.ptr
    }
}

// --- Chapter ---

/// Borrowed reference to an AVChapter within an InputContext.
pub struct Chapter<'a> {
    ptr: *mut ffi::AVChapter,
    _ctx: std::marker::PhantomData<&'a InputContext>,
}

impl Chapter<'_> {
    /// Chapter ID.
    pub fn id(&self) -> i64 {
        unsafe { ffi::kff_chapter_id(self.ptr) }
    }

    /// Chapter time base.
    pub fn time_base(&self) -> ffi::AVRational {
        unsafe { ffi::kff_chapter_time_base(self.ptr) }
    }

    /// Start time in time_base units.
    pub fn start(&self) -> i64 {
        unsafe { ffi::kff_chapter_start(self.ptr) }
    }

    /// End time in time_base units.
    pub fn end(&self) -> i64 {
        unsafe { ffi::kff_chapter_end(self.ptr) }
    }

    /// Start time in seconds.
    pub fn start_secs(&self) -> f64 {
        let tb = self.time_base();
        self.start() as f64 * tb.num as f64 / tb.den as f64
    }

    /// End time in seconds.
    pub fn end_secs(&self) -> f64 {
        let tb = self.time_base();
        self.end() as f64 * tb.num as f64 / tb.den as f64
    }

    /// Chapter metadata dictionary.
    pub fn metadata(&self) -> Dictionary<'_> {
        let dict = unsafe { ffi::kff_chapter_metadata(self.ptr) };
        Dictionary {
            ptr: dict,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Chapter title from metadata, if present.
    pub fn title(&self) -> Option<String> {
        self.metadata().get("title")
    }
}

// --- Dictionary ---

/// Read-only view of an AVDictionary.
pub struct Dictionary<'a> {
    ptr: *mut ffi::AVDictionary,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl Dictionary<'_> {
    /// Get a value by key. Returns None if the key doesn't exist.
    pub fn get(&self, key: &str) -> Option<String> {
        if self.ptr.is_null() {
            return None;
        }
        let c_key = CString::new(key).ok()?;
        let entry = unsafe { ffi::av_dict_get(self.ptr, c_key.as_ptr(), ptr::null(), 0) };
        if entry.is_null() {
            return None;
        }
        unsafe {
            let value = (*entry).value;
            if value.is_null() {
                None
            } else {
                Some(
                    std::ffi::CStr::from_ptr(value)
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        }
    }

    /// Iterate over all key-value pairs.
    pub fn iter(&self) -> DictionaryIter<'_> {
        DictionaryIter {
            dict: self.ptr,
            prev: ptr::null(),
            _lifetime: std::marker::PhantomData,
        }
    }
}

/// Iterator over AVDictionary entries.
pub struct DictionaryIter<'a> {
    dict: *mut ffi::AVDictionary,
    prev: *const ffi::AVDictionaryEntry,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Iterator for DictionaryIter<'a> {
    type Item = (String, String);

    fn next(&mut self) -> Option<Self::Item> {
        if self.dict.is_null() {
            return None;
        }
        let entry = unsafe {
            ffi::av_dict_get(
                self.dict,
                c"".as_ptr(),
                self.prev,
                ffi::AV_DICT_IGNORE_SUFFIX as i32,
            )
        };
        if entry.is_null() {
            return None;
        }
        self.prev = entry;
        unsafe {
            let key = std::ffi::CStr::from_ptr((*entry).key)
                .to_string_lossy()
                .into_owned();
            let value = std::ffi::CStr::from_ptr((*entry).value)
                .to_string_lossy()
                .into_owned();
            Some((key, value))
        }
    }
}

// --- MediaType ---

/// FFmpeg media type enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

impl MediaType {
    pub fn from_av(av: ffi::AVMediaType) -> Self {
        match av {
            ffi::AVMEDIA_TYPE_VIDEO => Self::Video,
            ffi::AVMEDIA_TYPE_AUDIO => Self::Audio,
            ffi::AVMEDIA_TYPE_SUBTITLE => Self::Subtitle,
            ffi::AVMEDIA_TYPE_DATA => Self::Data,
            ffi::AVMEDIA_TYPE_ATTACHMENT => Self::Attachment,
            _ => Self::Unknown,
        }
    }

    pub fn to_av(self) -> ffi::AVMediaType {
        match self {
            Self::Video => ffi::AVMEDIA_TYPE_VIDEO,
            Self::Audio => ffi::AVMEDIA_TYPE_AUDIO,
            Self::Subtitle => ffi::AVMEDIA_TYPE_SUBTITLE,
            Self::Data => ffi::AVMEDIA_TYPE_DATA,
            Self::Attachment => ffi::AVMEDIA_TYPE_ATTACHMENT,
            Self::Unknown => ffi::AVMEDIA_TYPE_UNKNOWN,
        }
    }
}
