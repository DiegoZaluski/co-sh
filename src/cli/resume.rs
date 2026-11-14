//! Startup session resolution for `-c` / `--continue` and `-s` / `--session`.
//!
//! The store is per directory (`{data}/sessions/{cwd-hash}/`), so "continue"
//! means "the most recently active session of the directory cosh was started
//! in" — the same isolation the TUI's session list has. Resolution happens
//! before the terminal is initialized, which is why the messages are plain
//! stderr lines instead of toasts.

use cosh::session::store::SessionStore;

use crate::cli::LaunchOptions;

/// The session id to open on startup, if the command line asked for one.
///
/// `Ok(None)` means "start normally" (no flag, or `-c` with nothing to
/// continue — a warning is printed and Home is the right place to land).
/// `Err` is reserved for an explicit `-s <id>` that cannot be honoured: the
/// user named a session, so silently starting elsewhere would be a lie.
pub fn resolve(options: &LaunchOptions) -> Result<Option<String>, String> {
    let store = SessionStore::new();
    let cwd = working_directory();

    if let Some(id) = &options.session_id {
        return match store.load_session(id) {
            Some(session) => Ok(Some(session.id)),
            None => Err(format!(
                "no session '{id}' in {cwd}\n  Sessions are stored per directory; run `cosh -c` to continue the last one here, or press Ctrl+S in cosh to browse."
            )),
        };
    }

    if !options.continue_last {
        return Ok(None);
    }

    match store.last_active_session() {
        Some(summary) => Ok(Some(summary.session_id)),
        None => {
            eprintln!(
                "cosh: nothing to continue in {cwd} yet — starting at home.\n  Sessions are stored per directory, so `-c` only resumes sessions started here."
            );
            Ok(None)
        }
    }
}

fn working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "?".to_string())
}
