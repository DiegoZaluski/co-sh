use std::sync::{LazyLock, Mutex};

use arboard::Clipboard;

use crate::ui::toast::{ToastOptions, ToastState, ToastVariant};

/// Global clipboard instance, lazily initialised.
static CLIPBOARD: LazyLock<Mutex<Option<Clipboard>>> =
    LazyLock::new(|| Mutex::new(None));

/// Initialise the system clipboard. Call once at startup.
pub fn init_clipboard() {
    match Clipboard::new() {
        Ok(cb) => {
            if let Ok(mut guard) = CLIPBOARD.lock() {
                *guard = Some(cb);
            }
        }
        Err(_) => {
            // Clipboard not available – writes will be silently ignored.
        }
    }
}

/// Copy `text` to the system clipboard. Returns `true` on success.
pub fn copy_to_clipboard(text: &str) -> bool {
    if let Ok(mut guard) = CLIPBOARD.lock()
        && let Some(ref mut cb) = *guard
    {
        return cb.set_text(text.to_owned()).is_ok();
    }
    false
}

/// Check whether the clipboard is available.
pub fn clipboard_available() -> bool {
    if let Ok(guard) = CLIPBOARD.lock() {
        guard.is_some()
    } else {
        false
    }
}

/// Copy the current selection to clipboard and show a toast notification.
/// Returns `true` if text was copied.
pub fn copy_selection(
    text: &str,
    toast: &mut ToastState,
) -> bool {
    if text.is_empty() {
        return false;
    }

    let copied = copy_to_clipboard(text);
    let line_count = text.split('\n').count();
    let summary = if line_count > 1 {
        format!("{line_count} lines")
    } else {
        format!("{} chars", text.len())
    };

    let message = if copied {
        format!("Copied to clipboard ({summary})")
    } else {
        format!("Selected {summary}; clipboard write unavailable")
    };
    toast.show(ToastOptions {
        title: None,
        message,
        variant: ToastVariant::Info,
        duration_ms: 3000,
    });

    copied
}
