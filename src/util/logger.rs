//! File-based logger for debugging the agent loop and tool dispatch.
//!
//! Writes structured log entries to `/tmp/cosh_debug.log` with timestamps.
//!
//! # Build behaviour
//!
//! - **Debug builds**: `log::debug!()` and `log::trace!()` are compiled in and
//!   routed to the file logger. All crates in the workspace that use `log` will
//!   have their output captured here — just call `init()` once at startup.
//! - **Release builds**: `log::debug!()` and `log::trace!()` are stripped at
//!   compile time by Cargo's default `release` profiles. Only `warn!()`,
//!   `error!()`, and `info!()` calls survive — but the logger itself caps at
//!   `Warn` level, so only warnings and errors are written.

use log::{Level, Log, Metadata, Record};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

struct FileLogger {
    file: Mutex<std::fs::File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(
                f,
                "[{}] [{}] {}",
                chrono::Local::now().format("%H:%M:%S%.3f"),
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// Initialize the debug file logger.
///
/// Writes to `/tmp/cosh_debug.log` in append mode (creates the file if it
/// does not exist).
///
/// - **Debug builds**: sets the max log level to `Debug` — all `debug!()`,
///   `info!()`, `warn!()`, and `error!()` calls are captured.
/// - **Release builds**: sets the max log level to `Warn` — only warnings and
///   errors are recorded.
///
/// # Panics
///
/// Panics if the log file cannot be opened (e.g. permission denied) or if
/// a logger has already been registered.
pub fn init() {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cosh_debug.log")
        .unwrap_or_else(|e| panic!("cannot open /tmp/cosh_debug.log: {e}"));

    let logger = FileLogger {
        file: Mutex::new(file),
    };

    log::set_boxed_logger(Box::new(logger))
        .expect("logger already initialized");

    #[cfg(debug_assertions)]
    log::set_max_level(log::LevelFilter::Debug);

    #[cfg(not(debug_assertions))]
    log::set_max_level(log::LevelFilter::Warn);
}
