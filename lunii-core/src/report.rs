//! Progress / log reporting of the long operations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use log::Level;

/// Receives the feedback of long running operations.
///
/// Implementations must be thread safe: operations are expected to run in a
/// worker thread while the UI displays the progress.
pub trait Reporter: Send + Sync {
    /// A log message (replaces `signal_logger`).
    fn log(&self, level: Level, message: &str);

    /// Progress inside the current story (replaces `signal_story_progress`).
    fn story_progress(&self, _short_uuid: &str, _current: usize, _total: usize) {}

    /// Progress of the current file (replaces `signal_file_progress`).
    /// `label` is a speed indication (`"123 KB/s"`) or a status (`"⌛ MP3"`).
    fn file_progress(&self, _label: &str, _current: u64, _total: u64) {}
}

/// Reporter forwarding the messages to the `log` crate facade.
#[derive(Default)]
pub struct LogReporter;

impl Reporter for LogReporter {
    fn log(&self, level: Level, message: &str) {
        log::log!(target: "lunii", level, "{message}");
    }
}

/// Shared operation context: reporting and abort request.
#[derive(Clone)]
pub struct Context {
    reporter: Arc<dyn Reporter>,
    abort: Arc<AtomicBool>,
    /// bytes of story files written to the device (see [`Context::written`])
    written: Arc<AtomicU64>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new(Arc::new(LogReporter))
    }
}

impl Context {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self {
            reporter,
            abort: Arc::new(AtomicBool::new(false)),
            written: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn reporter(&self) -> &Arc<dyn Reporter> {
        &self.reporter
    }

    /// Handle that can be used from another thread to request an abort.
    pub fn abort_handle(&self) -> Arc<AtomicBool> {
        self.abort.clone()
    }

    pub fn request_abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    pub fn clear_abort(&self) {
        self.abort.store(false, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }

    /// Counts bytes written to the device (story files).
    pub fn add_written(&self, bytes: u64) {
        self.written.fetch_add(bytes, Ordering::Relaxed);
    }

    /// A converted file (transcoded audio, image turned into a bitmap...) of
    /// `source` bytes was written as `written` bytes: counts it as its source
    /// size, so the progress ends on the size of the imported archive.
    pub fn count_as_source(&self, source: u64, written: u64) {
        if source >= written {
            self.written.fetch_add(source - written, Ordering::Relaxed);
        } else {
            self.written.fetch_sub(written - source, Ordering::Relaxed);
        }
    }

    /// Progress of the imports since the creation of the context, in bytes of
    /// the imported archives (see [`Context::count_as_source`]), shared by its
    /// clones: lets another thread estimate the remaining time.
    pub fn progress(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    pub fn log(&self, level: Level, message: impl AsRef<str>) {
        self.reporter.log(level, message.as_ref());
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.log(Level::Error, message);
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.log(Level::Warn, message);
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.log(Level::Info, message);
    }

    pub fn debug(&self, message: impl AsRef<str>) {
        self.log(Level::Debug, message);
    }

    pub fn story_progress(&self, short_uuid: &str, current: usize, total: usize) {
        self.reporter.story_progress(short_uuid, current, total);
    }

    pub fn file_progress(&self, label: &str, current: u64, total: u64) {
        self.reporter.file_progress(label, current, total);
    }
}

/// Error returned when an operation was interrupted by [`Context::request_abort`].
#[derive(Debug, thiserror::Error)]
#[error("Aborted by user")]
pub struct Aborted;

/// `Err(Aborted)` if an abort was requested.
pub fn check_abort(ctx: &Context) -> Result<(), Aborted> {
    if ctx.is_aborted() {
        Err(Aborted)
    } else {
        Ok(())
    }
}

/// Runs `f` on each item in turn (with its index), stopping on abort. The
/// error of an item is reported, then `failed` is called and the batch goes
/// on. Returns the number of items processed without error.
pub fn batch<T>(
    ctx: &Context,
    items: &[T],
    mut f: impl FnMut(usize, &T) -> anyhow::Result<()>,
    mut failed: impl FnMut(&T),
) -> anyhow::Result<usize> {
    let mut success = 0;
    for (index, item) in items.iter().enumerate() {
        check_abort(ctx)?;
        match f(index, item) {
            Ok(()) => success += 1,
            Err(e) if e.is::<Aborted>() => return Err(e),
            Err(e) => {
                ctx.error(format!("{e:#}"));
                failed(item);
            }
        }
    }
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_goes_on_after_an_error_and_stops_on_abort() {
        let ctx = Context::default();
        let mut failed = Vec::new();
        let done = batch(
            &ctx,
            &[1, 2, 3],
            |_, &n| if n == 2 { anyhow::bail!("no") } else { Ok(()) },
            |&n| failed.push(n),
        )
        .unwrap();
        assert_eq!((done, failed), (2, vec![2]));

        let err = batch(
            &ctx,
            &[1, 2],
            |_, _| {
                ctx.request_abort();
                Ok(())
            },
            |_| {},
        )
        .unwrap_err();
        assert!(err.is::<Aborted>());
    }
}
