//! File-based logger for debugging the agent loop and tool dispatch.
//!
//! Writes structured log entries to `<tmpdir>/cosh/log/stdout.log` with
//! timestamps — the same directory that receives the MCP stdio servers'
//! stderr (`mcp_server_<label>_<ts>.log`), so everything observable about a
//! broken MCP connection lives in one place.
//!
//! # Build behaviour
//!
//! - **Debug builds**: `log::debug!()` and `log::trace!()` are compiled in and
//!   routed to the file logger. All crates in the workspace that use `log` will
//!   have their output captured here — just call `init()` once at startup.
//! - **Release builds**: `log::debug!()` and `log::trace!()` are stripped at
//!   compile time by Cargo's default `release` profiles. Only `info!()`,
//!   `warn!()`, and `error!()` calls survive — the logger caps at `Info`, so
//!   the MCP connection lifecycle (dialing, settled, per-server failures)
//!   stays visible even outside a debug build.

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
/// Writes to `<tmpdir>/cosh/log/stdout.log` in append mode (creating the
/// directory and the file as needed). The path comes from
/// [`std::env::temp_dir`] — the std-native, cross-platform temp location —
/// so every entry point funnels into the same stream the MCP server logs
/// share. `scope` identifies the caller (e.g. `"tui_main"`) and is stamped
/// into the session's first line, so appends from different entry points
/// stay attributable.
///
/// - **Debug builds**: sets the max log level to `Debug` — all `debug!()`,
///   `info!()`, `warn!()`, and `error!()` calls are captured.
/// - **Release builds**: sets the max log level to `Info` — info, warnings
///   and errors are recorded (the MCP lifecycle logs are Info).
///
/// # Panics
///
/// Panics if the log directory/file cannot be created (e.g. permission
/// denied) or if a logger has already been registered.
pub fn init(scope: &str) {
    let log_dir = crate::harness::truncate::scratch_log_dir().join("log");
    if let Err(err) = std::fs::create_dir_all(&log_dir) {
        panic!(
            "cannot create log directory {}: {err}",
            log_dir.display()
        );
    }
    let log_path = log_dir.join("stdout.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", log_path.display()));

    let logger = FileLogger {
        file: Mutex::new(file),
    };

    log::set_boxed_logger(Box::new(logger)).expect("logger already initialized");

    #[cfg(debug_assertions)]
    log::set_max_level(log::LevelFilter::Debug);

    #[cfg(not(debug_assertions))]
    log::set_max_level(log::LevelFilter::Info);

    // Session banner: with multiple entry points appending to one file,
    // this is what makes each run's block attributable (and shows the
    // process started at all — the #1 question when "nothing happens").
    log::info!(
        "──── cosh logger init (scope={scope}, pid={}) ────",
        std::process::id()
    );
}
