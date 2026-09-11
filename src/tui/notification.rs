//! Native OS desktop notifications via `notify-rust`.
//!
//! When the TUI is running and the terminal window loses focus, a desktop
//! notification is sent to alert the user that the agent loop finished.
//! If the terminal IS focused, no notification fires (the user is already
//! looking at the terminal).
//!
//! The cosh logo icon is embedded in the binary at compile time and extracted
//! to a cache directory on first use, so the notification always shows the
//! cosh branding regardless of platform.

use notify_rust::Notification;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Cached path to the extracted cosh icon PNG.
static ICON_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Raw PNG bytes of the cosh logo icon (128×128), embedded at compile time.
const ICON_BYTES: &[u8] = include_bytes!("../../assets/cosh-icon.png");

/// Return the path to the cosh icon, extracting it from the binary on first
/// call.  The icon is written to an XDG-compliant cache directory
/// (`~/.cache/cosh/cosh-icon.png`) so it persists across runs.
fn icon_path() -> &'static PathBuf {
    ICON_PATH.get_or_init(|| {
        let dir = directories::BaseDirs::new()
            .map(|d| d.cache_dir().join("cosh"))
            .unwrap_or_else(|| {
                let mut p = std::env::temp_dir();
                p.push("cosh-cache");
                p
            });
        let path = dir.join("cosh-icon.png");
        // (Re)write the icon when it is missing OR when the embedded bytes
        // changed (e.g. a newer build ships a different logo). Comparing
        // bytes avoids stale cached icons from previous versions.
        let needs_write = match std::fs::read(&path) {
            Ok(existing) => existing != ICON_BYTES,
            Err(_) => true,
        };
        if needs_write {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&path, ICON_BYTES);
        }
        path
    })
}

/// Send a desktop notification that the agent loop finished.
/// No-op if `terminal_focused` is `true` (the user is already looking
/// at the terminal). Only when the terminal is NOT focused do we send
/// an OS-level notification to draw the user's attention back.
pub fn notify_loop_ended(terminal_focused: bool, title: &str, message: &str) {
    if terminal_focused {
        return;
    }

    let mut notification = Notification::new();
    notification.summary(title);
    notification.body(message);
    notification.appname("Cosh");
    notification.timeout(notify_rust::Timeout::Milliseconds(8000));

    // Attach the cosh logo icon when available.  On Linux (freedesktop) this
    // renders the PNG directly in the notification bubble; on macOS/Windows
    // the parameter is largely ignored (those platforms use the app bundle /
    // pinned-tile icon instead).
    let icon = icon_path();
    notification.icon(&icon.to_string_lossy());

    let _ = notification.show();
}

/// Convenience wrapper: send a "Done" notification.
pub fn notify_done(terminal_focused: bool) {
    notify_loop_ended(terminal_focused, "Cosh Ready", "The agent loop finished.");
}

/// Convenience wrapper: send a "Stopped" notification.
pub fn notify_stopped(terminal_focused: bool) {
    notify_loop_ended(
        terminal_focused,
        "Cosh Interrupted",
        "The agent loop was stopped.",
    );
}
