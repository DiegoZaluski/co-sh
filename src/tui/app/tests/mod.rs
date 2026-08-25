use super::render::format_tokens;
use super::{App, SIDEBAR_WIDTH, message_prompt_text};
use crate::routes::session::right_panel::RIGHT_PANEL_WIDTH;
use crate::session_store::generate_session_id;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Plain key event (no modifiers) for driving text inputs in tests.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Key event with a single modifier (e.g. Alt+arrow).
fn mod_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

/// Serializes tests that redirect `$HOME`: `Setup` persists through
/// `dirs`, which reads the process-wide environment.
static HOME_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Redirect `$HOME` to a scratch dir and drop any config left there by a
/// previous run, so `setup.save()` never touches the developer's files.
fn isolate_home() {
    let home = std::env::temp_dir().join("cosh-hook-test-home");
    let _ = std::fs::remove_dir_all(home.join(".config"));
    std::fs::create_dir_all(&home).expect("create scratch home");
    // SAFETY: tests holding HOME_LOCK are the only threads reading it.
    unsafe { std::env::set_var("HOME", &home) };
}

mod agent_loop;
mod commands;
mod dialogs;
mod keys;
mod providers;
mod rag;
mod render;
