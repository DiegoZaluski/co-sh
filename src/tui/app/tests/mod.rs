use super::{App, message_prompt_text};
use crate::routes::session::right_panel::RIGHT_PANEL_WIDTH;
use crate::session::store::generate_session_id;
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

/// Serializes every test that touches the REAL system clipboard (read OR
/// write). `cargo test` runs suites on parallel threads; the Ctrl+V tests
/// (paste_burst.rs) read the clipboard after writing a fixture, and the
/// registration-form copy tests (dialogs/text_input.rs) write to it — an
/// interleaving where a foreign write lands between a fixture write and its
/// read fails both spuriously. `HOME_LOCK` does not cover the clipboard.
static CLIPBOARD_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Redirect `$HOME` — AND, crucially on Windows, the config/data dirs — to
/// a scratch dir and drop any config left there by a previous run, so
/// `setup.save()` never touches the developer's files.
///
/// `$HOME` alone is NOT enough: on Windows `directories::ProjectDirs`
/// resolves through the known-folders API (`APPDATA`), ignoring `HOME`
/// entirely. The `COSH_CONFIG_DIR` / `COSH_DATA_DIR` overrides (read by
/// `cosh::setup`) pin every consumer to the scratch dir on all platforms.
fn isolate_home() {
    let home = std::env::temp_dir().join("cosh-hook-test-home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create scratch home");
    let cfg = home.join(".config").join("cosh");
    let data = home.join(".local").join("share").join("cosh");
    std::fs::create_dir_all(&cfg).expect("create scratch config dir");
    std::fs::create_dir_all(&data).expect("create scratch data dir");
    // SAFETY: tests holding HOME_LOCK are the only threads reading these.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("COSH_CONFIG_DIR", &cfg);
        std::env::set_var("COSH_DATA_DIR", &data);
    }
}

// NOTE: `agents_click_probe` was declared here but its file never existed in
// the tree — the orphan declaration broke the bin test target entirely
// (`E0583: file not found for module`), so it was removed. Re-add it together
// with its file when the probe lands.
mod agent_loop;
mod history_panic_probe;
mod commands;
mod cost;
mod dialogs;
mod gateway_offer;
mod keys;
mod model_persistence;
mod paste_burst;
mod prompt_correction;
mod prompt_history_arrows;
mod prompt_selection_mouse;
mod prompt_undo;
mod provider_login;
mod providers;
mod queue_block_probe;
mod rag;
mod restart_toast;
mod render;
mod router_mouse;
mod sessions_mouse;
#[cfg(target_os = "linux")] // subprocess isolation uses XDG data/config roots
mod telemetry;
