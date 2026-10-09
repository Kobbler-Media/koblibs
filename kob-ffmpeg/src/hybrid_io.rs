//! Hybrid I/O: reads from a local file that's being progressively downloaded,
//! falling back to a persistent HTTP streaming connection for data not yet on disk.
//!
//! Behaviour mirrors FFmpeg's native HTTP handler (single persistent connection,
//! streaming reads) but transparently switches to the local file as the download
//! progresses. Once the download completes, all I/O is local — seeks are instant.
//!
//! ```ignore
//! let io = HybridIo::new(
//!     "/cache/file.mkv",                    // local file being written to
//!     "https://debrid.example.com/file.mkv", // HTTP source with Range support
//!     download_progress.clone(),             // shared progress tracker
//! );
//! let mut custom_io = CustomIoContext::new(io)?;
//! let mut ctx = InputContext::open_custom(&mut custom_io)?;
//! ```

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{debug, info, warn};

use crate::format::io_context::IoCallbacks;

/// Shared download progress tracker.
///
/// The downloader updates `bytes_downloaded` as it writes to disk.
/// The reader checks it to decide whether to read from file or HTTP.
pub struct DownloadProgress {
    /// Bytes downloaded so far.
    pub bytes_downloaded: AtomicU64,
    /// Total file size (if known).
    pub total_bytes: AtomicU64,
    /// Whether the download is complete.
    pub complete: std::sync::atomic::AtomicBool,
}

impl DownloadProgress {
    pub fn new(total_bytes: Option<u64>) -> Self {
        Self {
            bytes_downloaded: AtomicU64::new(0),
            total_bytes: AtomicU64::new(total_bytes.unwrap_or(0)),
            complete: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn update(&self, bytes: u64) {
        self.bytes_downloaded.store(bytes, Ordering::Release);
    }

    pub fn mark_complete(&self) {
        self.complete.store(true, Ordering::Release);
        info!(
            target: "kob_ffmpeg::hybrid_io",
            downloaded = self.bytes_downloaded.load(Ordering::Acquire),
            total = self.total_bytes.load(Ordering::Acquire),
            "DownloadProgress: marked complete"
        );
    }

    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }

    pub fn downloaded(&self) -> u64 {
        self.bytes_downloaded.load(Ordering::Acquire)
    }

    pub fn total(&self) -> u64 {
        self.total_bytes.load(Ordering::Acquire)
    }

    /// Record a total size learned after construction, e.g. from a
    /// `Content-Range` header. Never overwrites a size we were given up front —
    /// the caller's `filesize` is authoritative when it has one.
    pub fn set_total_if_unknown(&self, total: u64) {
        if total > 0 {
            let _ =
                self.total_bytes
                    .compare_exchange(0, total, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}

/// A persistent streaming HTTP connection from a given byte offset.
/// Reads sequentially — same behaviour as FFmpeg's native HTTP handler.
/// Dropped and reopened on seek (one reconnect, not per-read).
struct HttpStream {
    /// Current byte position in the stream.
    pos: u64,
    /// Streaming response body.
    reader: Box<dyn Read + Send>,
}

/// Hybrid I/O reader: local file + persistent HTTP streaming fallback.
pub struct HybridIo {
    local_path: PathBuf,
    http_url: String,
    progress: Arc<DownloadProgress>,
    /// Current logical read position.
    position: u64,
    /// Open local file handle (lazily opened, survives seeks).
    file: Option<std::fs::File>,
    /// HTTP client for opening streaming connections.
    http: reqwest::blocking::Client,
    /// Persistent streaming connection for data beyond the download frontier.
    /// Opened on first HTTP read, reused for sequential reads, dropped on seek.
    stream: Option<HttpStream>,
}

impl HybridIo {
    pub fn new(
        local_path: impl Into<PathBuf>,
        http_url: impl Into<String>,
        progress: Arc<DownloadProgress>,
    ) -> Self {
        Self {
            local_path: local_path.into(),
            http_url: http_url.into(),
            progress,
            position: 0,
            file: None,
            http: reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .tcp_user_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to create HTTP client"),
            stream: None,
        }
    }

    /// Try to open the local file. Returns false if it doesn't exist yet.
    fn try_open_file(&mut self) -> bool {
        if self.file.is_some() {
            return true;
        }
        match std::fs::File::open(&self.local_path) {
            Ok(f) => {
                self.file = Some(f);
                true
            }
            Err(_) => false,
        }
    }

    /// Read from the local file at the current position.
    /// Returns None if the file isn't available yet.
    fn read_from_file(&mut self, buf: &mut [u8]) -> Option<std::io::Result<usize>> {
        if !self.try_open_file() {
            return None;
        }
        let file = self.file.as_mut().unwrap();
        Some((|| {
            file.seek(SeekFrom::Start(self.position))?;
            file.read(buf)
        })())
    }

    /// Open a persistent streaming connection from `pos` to end of file.
    /// Same as FFmpeg's HTTP handler: one `Range: bytes=N-` GET request,
    /// then stream the response body.
    fn open_stream(&mut self, pos: u64) -> std::io::Result<()> {
        let range = format!("bytes={pos}-");
        let t0 = std::time::Instant::now();
        info!(target: "kob_ffmpeg::hybrid_io", pos, url = %self.http_url, "HybridIo: opening HTTP stream");
        let resp = self
            .http
            .get(&self.http_url)
            .header("Range", &range)
            .send()
            .map_err(|e| {
                warn!(target: "kob_ffmpeg::hybrid_io", pos, elapsed_ms = t0.elapsed().as_millis() as u64, error = %e, "HybridIo: HTTP stream open failed");
                std::io::Error::other(e)
            })?;

        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            warn!(target: "kob_ffmpeg::hybrid_io", pos, %status, "HybridIo: HTTP non-2xx on stream open");
            return Err(std::io::Error::other(format!(
                "HTTP {status} opening stream at byte {pos}"
            )));
        }
        // Debrid sources don't always report a filesize up front. Without a
        // total we can't answer AVSEEK_SIZE, and a short cache file is
        // indistinguishable from a real EOF — so learn it from the response.
        if self.progress.total() == 0
            && let Some(total) = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.trim().parse::<u64>().ok())
        {
            self.progress.set_total_if_unknown(total);
            info!(target: "kob_ffmpeg::hybrid_io", total, "HybridIo: learned total size from Content-Range");
        }

        info!(
            target: "kob_ffmpeg::hybrid_io",
            pos,
            %status,
            elapsed_ms = t0.elapsed().as_millis() as u64,
            "HybridIo: HTTP stream opened"
        );

        self.stream = Some(HttpStream {
            pos,
            reader: Box::new(resp),
        });
        Ok(())
    }

    /// Whether a zero-byte local read means we are genuinely at the end of the
    /// file, rather than at the end of a cache file that is short of the real
    /// thing (failed download, interrupted write, truncated cache). When the
    /// total size is unknown we have nothing to compare against, so a local
    /// EOF is taken at face value.
    fn local_eof_is_real(&self) -> bool {
        let total = self.progress.total();
        total == 0 || self.position >= total
    }

    /// Read from the persistent HTTP stream. Opens a new connection if needed
    /// (first read, or after a seek invalidated the old one).
    fn read_from_stream(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // If stream exists but position doesn't match, drop it
        if let Some(ref s) = self.stream
            && s.pos != self.position
        {
            self.stream = None;
        }

        // Open if needed
        if self.stream.is_none() {
            self.open_stream(self.position)?;
        }

        let stream = self.stream.as_mut().unwrap();
        match stream.reader.read(buf) {
            Ok(n) => {
                stream.pos += n as u64;
                Ok(n)
            }
            Err(e) => {
                // Connection broke — drop stream so next read reopens
                self.stream = None;
                Err(e)
            }
        }
    }
}

impl IoCallbacks for HybridIo {
    fn read(&mut self, buf: &mut [u8]) -> i32 {
        let t0 = std::time::Instant::now();
        let req_pos = self.position;
        let req_len = buf.len();
        let downloaded = self.progress.downloaded();
        let is_complete = self.progress.is_complete();

        // If the data we need is on disk, read from the local file.
        // This covers: download complete, or position within downloaded range.
        let file_has_data = is_complete || self.position < downloaded;

        if file_has_data {
            // Clamp read to what's actually downloaded (unless complete)
            let read_len = if is_complete || self.position + buf.len() as u64 <= downloaded {
                buf.len()
            } else {
                // Partial: only read what's on disk
                (downloaded - self.position) as usize
            };

            if read_len > 0
                && let Some(result) = self.read_from_file(&mut buf[..read_len])
            {
                match result {
                    Ok(n) if n > 0 => {
                        self.position += n as u64;
                        debug!(
                            target: "kob_ffmpeg::hybrid_io",
                            pos = req_pos,
                            req_len,
                            downloaded,
                            source = "local",
                            ret = n,
                            clamped = (read_len != buf.len()),
                            elapsed_us = t0.elapsed().as_micros() as u64,
                            "HybridIo read (local)"
                        );
                        return n as i32;
                    }
                    Ok(_) if self.local_eof_is_real() => {
                        info!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded, source = "local", ret = 0, "HybridIo read EOF (local)");
                        return crate::ffi::AVERROR_EOF;
                    }
                    Err(ref e) if self.local_eof_is_real() => {
                        warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, error = %e, "HybridIo local read error -> EOF");
                        return crate::ffi::AVERROR_EOF;
                    }
                    // The cache file is short of the real file — the download
                    // failed, stalled, or was marked complete when it wasn't.
                    // Serve the rest over HTTP instead of truncating the input.
                    Ok(_) => {
                        warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded, total = self.progress.total(), "HybridIo: local file short of total, falling through to HTTP");
                    }
                    Err(e) => {
                        warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, error = %e, "HybridIo local read error, falling through to HTTP");
                    }
                }
            } else {
                // File not available yet — fall through to HTTP
                info!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded, "HybridIo: file not available despite downloaded>pos, falling through to HTTP");
            }
        }

        // Position is beyond downloaded range (or file not available).
        // If close to the download frontier, wait briefly for it to catch up.
        // Bail early if download has stalled (no progress for 1s).
        if !is_complete && downloaded > 0 && self.position < downloaded + 1_048_576 {
            let t_wait = std::time::Instant::now();
            let mut last_dl = downloaded;
            let mut stall_count = 0u32;
            let mut iterations = 0u32;
            for _ in 0..20 {
                iterations += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
                let new_dl = self.progress.downloaded();
                if self.position + buf.len() as u64 <= new_dl || self.progress.is_complete() {
                    if let Some(result) = self.read_from_file(buf) {
                        match result {
                            Ok(n) if n > 0 => {
                                self.position += n as u64;
                                debug!(
                                    target: "kob_ffmpeg::hybrid_io",
                                    pos = req_pos,
                                    req_len,
                                    downloaded = new_dl,
                                    waited_ms = t_wait.elapsed().as_millis() as u64,
                                    source = "local_after_wait",
                                    ret = n,
                                    "HybridIo read (local after wait)"
                                );
                                return n as i32;
                            }
                            Ok(_) if self.local_eof_is_real() => {
                                info!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded = new_dl, waited_ms = t_wait.elapsed().as_millis() as u64, source = "local_after_wait", ret = 0, "HybridIo read EOF (local after wait)");
                                return crate::ffi::AVERROR_EOF;
                            }
                            Err(ref e) if self.local_eof_is_real() => {
                                warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, error = %e, "HybridIo local read after wait error -> EOF");
                                return crate::ffi::AVERROR_EOF;
                            }
                            // Short/unreadable cache file — serve over HTTP.
                            Ok(_) => {
                                warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded = new_dl, total = self.progress.total(), "HybridIo: local file short of total after wait, falling through to HTTP");
                            }
                            Err(e) => {
                                warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, error = %e, "HybridIo local read after wait error, falling through to HTTP");
                            }
                        }
                    }
                    break;
                }
                if new_dl == last_dl {
                    stall_count += 1;
                    if stall_count >= 10 {
                        warn!(
                            target: "kob_ffmpeg::hybrid_io",
                            pos = req_pos,
                            req_len,
                            downloaded = new_dl,
                            waited_ms = t_wait.elapsed().as_millis() as u64,
                            iterations,
                            "HybridIo: download stalled for 1s while waiting; falling through to HTTP"
                        );
                        break;
                    }
                } else {
                    stall_count = 0;
                    last_dl = new_dl;
                }
            }
            if iterations == 20 {
                info!(
                    target: "kob_ffmpeg::hybrid_io",
                    pos = req_pos,
                    req_len,
                    downloaded = self.progress.downloaded(),
                    waited_ms = t_wait.elapsed().as_millis() as u64,
                    "HybridIo: exhausted 2s wait for download to reach position; falling through to HTTP"
                );
            }
        }

        // Use persistent streaming HTTP connection (same as FFmpeg native).
        // Retry with fresh connections on transient failures (timeouts, resets).
        for attempt in 0..3u32 {
            let t_http = std::time::Instant::now();
            match self.read_from_stream(buf) {
                Ok(0) => {
                    info!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded, attempt, source = "http", ret = 0, "HybridIo read EOF (http)");
                    return crate::ffi::AVERROR_EOF;
                }
                Ok(n) => {
                    self.position += n as u64;
                    debug!(
                        target: "kob_ffmpeg::hybrid_io",
                        pos = req_pos,
                        req_len,
                        downloaded,
                        attempt,
                        source = "http",
                        ret = n,
                        elapsed_ms = t_http.elapsed().as_millis() as u64,
                        total_ms = t0.elapsed().as_millis() as u64,
                        "HybridIo read (http)"
                    );
                    return n as i32;
                }
                Err(e) => {
                    warn!(
                        target: "kob_ffmpeg::hybrid_io",
                        pos = req_pos,
                        req_len,
                        downloaded,
                        attempt,
                        elapsed_ms = t_http.elapsed().as_millis() as u64,
                        error = %e,
                        "HybridIo HTTP read failed"
                    );
                    if attempt < 2 {
                        let delay = std::time::Duration::from_secs(1 << attempt);
                        std::thread::sleep(delay);
                        // stream was dropped in read_from_stream, next call opens fresh
                    }
                }
            }
        }
        warn!(target: "kob_ffmpeg::hybrid_io", pos = req_pos, req_len, downloaded, "HybridIo: 3 HTTP attempts failed -> EOF");
        crate::ffi::AVERROR_EOF
    }

    fn seek(&mut self, offset: i64, whence: i32) -> i64 {
        let total = self.progress.total();
        let old_pos = self.position;

        let new_pos = match whence {
            0 => offset as u64,                          // SEEK_SET
            1 => (self.position as i64 + offset) as u64, // SEEK_CUR
            2 => {
                if total == 0 {
                    return -1;
                }
                (total as i64 + offset) as u64
            }
            0x10000 => {
                // AVSEEK_SIZE — return total file size
                if total > 0 {
                    return total as i64;
                }
                return -1;
            }
            _ => return -1,
        };

        // Drop HTTP stream if seeking to a non-sequential position.
        // Next read will either hit the local file or open a new stream.
        let mut dropped_stream = false;
        if let Some(ref s) = self.stream
            && new_pos != s.pos
        {
            self.stream = None;
            dropped_stream = true;
        }

        self.position = new_pos;
        info!(
            target: "kob_ffmpeg::hybrid_io",
            whence,
            old_pos,
            new_pos,
            total,
            downloaded = self.progress.downloaded(),
            dropped_stream,
            "HybridIo seek"
        );
        new_pos as i64
    }
}
