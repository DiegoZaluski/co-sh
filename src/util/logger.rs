//! File-based logger for debugging the agent loop and tool dispatch.
//!
//! Writes structured log entries to `<tmpdir>/cosh/log/stdout.log` with
//! timestamps — the same directory that receives the MCP stdio servers'
//! stderr (`mcp_server_<label>_<ts>.log`), so everything observable about a
//! broken MCP connection lives in one place.
//!
//! # Build behaviour
//!
//! - **Debug builds**: cosh workspace `log::debug!()` calls are routed to the
//!   file logger. Dependency debug chatter is filtered out. The shared file
//!   is truncated at 16 MiB so a long session cannot fill the disk.
//! - **Release builds**: `log::debug!()` and `log::trace!()` are stripped at
//!   compile time by Cargo's default `release` profiles. Only `info!()`,
//!   `warn!()`, and `error!()` calls survive — the logger caps at `Info`, so
//!   the MCP connection lifecycle (dialing, settled, per-server failures)
//!   stays visible even outside a debug build.

use log::{Level, Log, Metadata, Record};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

struct FileLogger {
    file: Mutex<std::fs::File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
            || (metadata.level() == Level::Debug
                && matches!(
                    metadata.target().split("::").next(),
                    Some("cosh" | "cosh_sdk" | "cosh_tui" | "cosh_tools" | "cosh_recall")
                ))
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Ok(mut f) = self.file.lock() {
            // The shared debug log can otherwise grow for the entire lifetime
            // of an interactive session. Truncation also reclaims an oversized
            // log left by a previous run without reading it into memory.
            if f.metadata().is_ok_and(|m| m.len() >= MAX_LOG_BYTES) {
                let _ = f.set_len(0);
            }
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
/// - **Debug builds**: sets the max log level to `Debug` — workspace debug
///   calls and all `info!()`, `warn!()`, and `error!()` calls are captured.
/// - **Release builds**: sets the max log level to `Info` — info, warnings
///   and errors are recorded (the MCP lifecycle logs are Info).
///
/// # Panics
///
/// Panics if the log directory/file cannot be created (e.g. permission
/// denied) or if a logger has already been registered. Entry points that
/// must never abort on a logging failure (the standalone uninstaller) use
/// [`try_init`] instead.
pub fn init(scope: &str) {
    if let Err(reason) = try_init(scope) {
        panic!("{reason}");
    }
}

/// Non-panicking variant of [`init`] for entry points that must continue
/// running even when logging cannot be set up (e.g. `cosh-uninstall`, whose
/// documented invariant is that nothing panics before the cleanup). Returns
/// `Err(reason)` when the log directory/file cannot be created or a logger
/// has already been registered; logging simply stays off in that case.
pub fn try_init(scope: &str) -> Result<(), String> {
    let log_dir = crate::harness::truncate::scratch_log_dir().join("log");
    std::fs::create_dir_all(&log_dir)
        .map_err(|err| format!("cannot create log directory {}: {err}", log_dir.display()))?;
    let log_path = log_dir.join("stdout.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;

    let logger = FileLogger {
        file: Mutex::new(file),
    };

    log::set_boxed_logger(Box::new(logger))
        .map_err(|_| "logger already initialized".to_string())?;

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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_debug_is_filtered() {
        let path = std::env::temp_dir().join(format!("cosh-logger-test-{}", std::process::id()));
        let logger = FileLogger {
            file: Mutex::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap(),
            ),
        };
        let own = Metadata::builder()
            .level(Level::Debug)
            .target("cosh::app")
            .build();
        let dependency = Metadata::builder()
            .level(Level::Debug)
            .target("keyring::entry")
            .build();
        assert!(logger.enabled(&own));
        assert!(!logger.enabled(&dependency));
        drop(logger);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn oversized_log_is_truncated_before_write() {
        let path =
            std::env::temp_dir().join(format!("cosh-logger-cap-test-{}", std::process::id()));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_LOG_BYTES).unwrap();
        let logger = FileLogger {
            file: Mutex::new(file),
        };
        let record = Record::builder()
            .args(format_args!("bounded"))
            .level(Level::Info)
            .target("cosh")
            .build();
        logger.log(&record);
        assert!(std::fs::metadata(&path).unwrap().len() < 100);
        drop(logger);
        let _ = std::fs::remove_file(path);
    }
}
