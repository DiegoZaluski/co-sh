//! Integration tests for the prompt edit history — the Ctrl+Z / Ctrl+Y
//! feature — driven through the REAL key pipeline (`process_key_event`), so
//! the tests cover the handler gates, the recording paths and the restore
//! application end to end.

use super::{App, HOME_LOCK, isolate_home};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn session_app() -> App {
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.process_key_event(key(KeyCode::Char(ch))).unwrap();
    }
}

/// Ctrl+Z removes the whole coalesced typing burst (the "last sentence"
/// convention); Ctrl+Y brings it back.
#[tokio::test]
async fn ctrl_z_removes_the_last_typed_burst_and_ctrl_y_restores_it() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "olá mundo");
    assert_eq!(app.prompt_view.input, "olá mundo");

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "");

    app.process_key_event(ctrl(KeyCode::Char('y'))).unwrap();
    assert_eq!(app.prompt_view.input, "olá mundo");
    // The cursor survived the round trip.
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
}

/// Delete and type bursts are separate groups: one Ctrl+Z undoes only the
/// backspace run that followed the typing.
#[tokio::test]
async fn undo_after_backspaces_restores_the_deleted_text_only() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "primeira");
    app.process_key_event(key(KeyCode::Backspace)).unwrap();
    app.process_key_event(key(KeyCode::Backspace)).unwrap();
    assert_eq!(app.prompt_view.input, "primei");

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "primeira");
}

/// The prompt corrector's rewrite is ONE atomic history group: the first
/// Ctrl+Z after it restores the exact pre-correction draft, and Ctrl+Y
/// reapplies the correction.
#[tokio::test]
async fn ctrl_z_after_a_correction_restores_the_original_prompt() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "texto originnal com erros");
    // The event handler applies a successful correction through
    // `replace_with_correction` — the same path the production code uses.
    app.prompt_view
        .replace_with_correction("Texto original, com erros corrigidos.".into());
    assert_eq!(
        app.prompt_view.input,
        "Texto original, com erros corrigidos."
    );

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "texto originnal com erros");

    app.process_key_event(ctrl(KeyCode::Char('y'))).unwrap();
    assert_eq!(
        app.prompt_view.input,
        "Texto original, com erros corrigidos."
    );
}

/// Editing the corrected text first undoes the EDITS; the next Ctrl+Z
/// crosses the correction boundary back to the original draft.
#[tokio::test]
async fn undo_first_reverts_edits_then_crosses_the_correction_boundary() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "rascunho");
    app.prompt_view
        .replace_with_correction("Rascunho corrigido.".into());
    type_text(&mut app, " Ok");
    assert_eq!(app.prompt_view.input, "Rascunho corrigido. Ok");

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "Rascunho corrigido.");

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "rascunho");
}

/// Sending the message ends the draft context: the edit history is dropped,
/// so Ctrl+Z cannot resurface a stale slash-command state afterwards.
#[tokio::test]
async fn sending_the_message_clears_the_edit_history() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "mensagem");
    app.prompt_view.send_message();
    assert_eq!(app.prompt_view.input, "");

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "");
}

/// A fresh history makes Ctrl+Z a harmless no-op that types nothing.
#[tokio::test]
async fn ctrl_z_on_an_empty_prompt_is_a_noop() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "");
    app.process_key_event(ctrl(KeyCode::Char('y'))).unwrap();
    assert_eq!(app.prompt_view.input, "");
}

/// M1 regression: with the slash menu open (draft starts with `/`), the
/// menu's Char handler runs BEFORE the Session-mode one — Ctrl+Z must undo
/// there too, never type a literal `z` into the filtered command.
#[tokio::test]
async fn ctrl_z_works_while_the_slash_menu_is_open() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "/he");
    assert!(
        app.slash_menu.visible,
        "typing a slash command opens the menu"
    );

    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "");
    // The undo resync closed the menu (empty input has no command to show).
    assert!(!app.slash_menu.visible);

    app.process_key_event(ctrl(KeyCode::Char('y'))).unwrap();
    assert_eq!(app.prompt_view.input, "/he");
}

/// A new edit after an undo discards the redo branch (standard behavior).
#[tokio::test]
async fn typing_after_undo_discards_the_redo_branch() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();

    type_text(&mut app, "abc");
    app.process_key_event(ctrl(KeyCode::Char('z'))).unwrap();
    assert_eq!(app.prompt_view.input, "");

    type_text(&mut app, "x");
    assert_eq!(app.prompt_view.input, "x");

    app.process_key_event(ctrl(KeyCode::Char('y'))).unwrap();
    // Noop redo must NOT have restored "abc".
    assert_eq!(app.prompt_view.input, "x");
}
