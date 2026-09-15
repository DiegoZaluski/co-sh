//! Native OS desktop notifications via `notify-rust`.
//!
//! When the TUI is running and the terminal window loses focus, a desktop
//! notification is sent to alert the user of a gate that needs their
//! attention: the agent loop finished, or the harness is waiting on a
//! confirmation (tool-permission approval / question dialog).
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

/// Send a desktop notification.
/// No-op if `terminal_focused` is `true` (the user is already looking
/// at the terminal). Only when the terminal is NOT focused do we send
/// an OS-level notification to draw the user's attention back.
///
/// Every convenience wrapper below funnels through this single
/// focus-gated send path, so the focusing rule, icon, appname and
/// timeout stay consistent across all notification kinds.
pub fn notify(terminal_focused: bool, title: &str, message: &str) {
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

/// Send a desktop notification that the agent loop finished.
pub fn notify_loop_ended(terminal_focused: bool, title: &str, message: &str) {
    notify(terminal_focused, title, message);
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

/// Truncate a tool description for a notification body. Desktop
/// notifications render only a couple of lines, and long tool
/// descriptions would be truncated arbitrarily by the OS anyway.
fn truncate_description(description: &str) -> String {
    const MAX_DESCRIPTION: usize = 120;
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION {
        let truncated: String = description.chars().take(MAX_DESCRIPTION).collect();
        format!("{truncated}…")
    } else {
        description.to_string()
    }
}

/// Convenience wrapper: send a notification that the harness is waiting
/// for a tool-permission approval (the Allow / Allow Once / Deny dialog).
/// The tool name leads the body so the user knows what is being asked
/// without switching to the terminal first.
pub fn notify_permission(terminal_focused: bool, tool: &str, description: &str) {
    notify(
        terminal_focused,
        "Cosh Needs Approval",
        &format!(
            "Permission requested for `{tool}`.\n{}",
            truncate_description(description)
        ),
    );
}

/// Convenience wrapper: send a notification that the agent asked the user
/// one or more questions (the question dialog is blocking the loop).
pub fn notify_question(terminal_focused: bool) {
    notify(
        terminal_focused,
        "Cosh Question",
        "The agent is waiting for your answer.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_description_keeps_short_text_untouched() {
        assert_eq!(
            truncate_description("  Run a shell command  "),
            "Run a shell command"
        );
    }

    #[test]
    fn truncate_description_caps_long_text_at_120_chars_with_ellipsis() {
        let long = "x".repeat(200);
        let truncated = truncate_description(&long);
        assert_eq!(truncated.chars().count(), 121); // 120 chars + '…'
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_description_counts_chars_not_bytes() {
        // 'é' is 2 bytes in UTF-8; 121 chars = 242 bytes, above the
        // byte-level read of the cap but exactly one char over it.
        let accents = "é".repeat(121);
        let truncated = truncate_description(&accents);
        assert_eq!(truncated.chars().count(), 121);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_description_keeps_exactly_at_cap_intact() {
        let exact = "y".repeat(120);
        assert_eq!(truncate_description(&exact), exact);
    }
}
