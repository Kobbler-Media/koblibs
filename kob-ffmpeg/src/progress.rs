//! Progress reporting and cancellation support for long-running operations.

use tokio_util::sync::CancellationToken;

/// Options for controlling long-running operations.
///
/// All long-running functions accept `Option<&RunContext>`. Pass `None` for
/// no progress/cancellation, or provide a context with callbacks.
#[allow(dead_code)] // on_progress used via check() — will be called from transcode progress hooks
pub struct RunContext {
    cancel: Option<CancellationToken>,
    on_progress: Option<Box<dyn Fn(Progress) + Send>>,
}

/// Progress update from a long-running operation.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Packets or frames processed so far.
    pub processed: u64,
    /// Estimated total (if known).
    pub total: Option<u64>,
    /// Current time position in seconds (if applicable).
    pub position_secs: Option<f64>,
    /// Total duration in seconds (if known).
    pub duration_secs: Option<f64>,
}

impl RunContext {
    /// Create with just a cancellation token.
    pub fn cancellable(token: CancellationToken) -> Self {
        Self {
            cancel: Some(token),
            on_progress: None,
        }
    }

    /// Create with a progress callback.
    pub fn with_progress(callback: impl Fn(Progress) + Send + 'static) -> Self {
        Self {
            cancel: None,
            on_progress: Some(Box::new(callback)),
        }
    }

    /// Create with both cancellation and progress.
    pub fn new(token: CancellationToken, callback: impl Fn(Progress) + Send + 'static) -> Self {
        Self {
            cancel: Some(token),
            on_progress: Some(Box::new(callback)),
        }
    }

    /// Check if the operation has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|t| t.is_cancelled())
    }

    /// Report progress. Returns Err if cancelled.
    #[allow(dead_code)] // will be used once transcode reports progress
    pub(crate) fn check(&self, progress: Progress) -> crate::Result<()> {
        if self.is_cancelled() {
            return Err(crate::error::FfmpegError::cancelled("operation cancelled"));
        }
        if let Some(ref cb) = self.on_progress {
            cb(progress);
        }
        Ok(())
    }

    /// Just check cancellation without progress update.
    pub(crate) fn check_cancel(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            return Err(crate::error::FfmpegError::cancelled("operation cancelled"));
        }
        Ok(())
    }
}

/// Helper to check cancellation from an Option<&RunContext>.
#[inline]
pub(crate) fn check_cancel(ctx: Option<&RunContext>) -> crate::Result<()> {
    if let Some(c) = ctx {
        c.check_cancel()?;
    }
    Ok(())
}

/// Helper to report progress from an Option<&RunContext>.
#[inline]
#[allow(dead_code)] // will be used once transcode reports progress
pub(crate) fn report_progress(ctx: Option<&RunContext>, progress: Progress) -> crate::Result<()> {
    if let Some(c) = ctx {
        c.check(progress)?;
    }
    Ok(())
}
