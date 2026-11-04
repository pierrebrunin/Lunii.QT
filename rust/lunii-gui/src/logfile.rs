//! Persistent log file: `~/.lunii-qt/logs/lunii-qt.log`.
//!
//! One file per session, the previous ones are kept as `lunii-qt.log.1` …
//! `lunii-qt.log.4` (rotated at start-up). This module also installs the
//! `log` crate logger, so the messages of `lunii-core` that don't go through a
//! `Reporter` (database loading...) end up in the panel and in the file.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use lunii_core::Level;

const FILE_NAME: &str = "lunii-qt.log";
const KEPT_SESSIONS: usize = 5;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn log_dir() -> PathBuf {
    lunii_core::constants::cfg_dir().join("logs")
}

pub fn log_path() -> PathBuf {
    log_dir().join(FILE_NAME)
}

fn rotated(n: usize) -> PathBuf {
    log_dir().join(format!("{FILE_NAME}.{n}"))
}

/// Rotates the previous sessions, opens the log file and installs the logger.
/// Failures are not fatal: the panel keeps working without the file.
pub fn init() {
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::remove_file(rotated(KEPT_SESSIONS - 1));
        for n in (1..KEPT_SESSIONS - 1).rev() {
            let _ = std::fs::rename(rotated(n), rotated(n + 1));
        }
        let _ = std::fs::rename(log_path(), rotated(1));
        if let Ok(f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
        {
            *FILE.lock().unwrap() = Some(f);
        }
    }

    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

/// Appends a line (already prefixed with its level) to the log file.
pub fn write(line: &str) {
    let mut guard = FILE.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let now = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S%.3f");
        // a full disk must not break the application: the file is just dropped
        if writeln!(f, "{now} {line}").is_err() {
            *guard = None;
        }
    }
}

/// `log` crate facade: messages are forwarded to the UI thread (panel + file),
/// or written directly to the file when the event loop isn't running.
struct Logger;

static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // dependencies (winit, slint, ureq...) are too verbose below warnings
        metadata.level() <= Level::Warn || metadata.target().starts_with("lunii")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = record.level();
        let message = record.args().to_string();
        let fallback = format!("[{level}] {message}");
        if slint::invoke_from_event_loop(move || crate::app::log_from_facade(level, &message))
            .is_err()
        {
            write(&fallback);
        }
    }

    fn flush(&self) {
        if let Some(f) = FILE.lock().unwrap().as_mut() {
            let _ = f.flush();
        }
    }
}
