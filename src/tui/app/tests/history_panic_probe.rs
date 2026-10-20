//! Regression tests for user-triggerable panics found while auditing the
//! session UI. Each test drives the REAL key/dispatch pipeline and asserts
//! the behavior survives (or that the guard catches it) instead of panicking.
//!
//! 1. Prompt history navigation (Ctrl+Up / Ctrl+Down) across a session
//!    switch: `history_index` is only reset by typing/sending, never by a
//!    session switch, so a positive index carried from a long session can
//!    point past a shorter session's user messages. Every path — including
//!    the worst case — must land on a clamped message or a clear, never
//!    panic.
use super::{App, HOME_LOCK, isolate_home};
use crate::types::{Message, MessageRole, Part, TextPart};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// Build a session with `n` user messages (assistant replies interleaved)
/// and make it the current session.
fn session_with_user_messages(app: &mut App, id: &str, n: usize, tag: &str) {
    app.state.add_empty_session(id.to_string(), tag.to_string(), 0);
    app.state.current_session_id = Some(id.to_string());
    {
        let s = app.state.current_session_mut().unwrap();
        for i in 0..n {
            s.messages.push(Message {
                id: format!("{tag}-u{i}"),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: format!("{tag} user message {i}"),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });
            s.messages.push(Message {
                id: format!("{tag}-a{i}"),
                role: MessageRole::Assistant,
                parts: vec![Part::Text(TextPart {
                    text: format!("{tag} answer {i}"),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });
        }
    }
}

/// Ctrl+Up browses to older messages; walk the whole history of a long
/// session so `history_index` is riding high when the switch happens.
fn browse_to_top(app: &mut App, count: usize) {
    for _ in 0..count {
        app.process_key_event(ctrl(KeyCode::Up)).unwrap();
    }
}

#[tokio::test]
async fn history_up_after_session_switch_never_panics() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    // Long session: 10 user messages.
    session_with_user_messages(&mut app, "long", 10, "long");
    // Short session: only 1 user message.
    session_with_user_messages(&mut app, "short", 1, "short");
    // A third stopgap session (no messages) guards the walk at both ends.
    app.state.add_empty_session("stopgap".into(), "stopgap".into(), 0);

    app.state.current_session_id = Some("long".into());
    app.prompt_view.focus();

    // Climb PART-way: 3 presses set history_index = 9, then 8, then 7 —
    // a high index that the short session's 1-message history cannot
    // satisfy. (Browsing to the top would leave the index at 0, which is
    // in-bounds everywhere and would mask the bug.)
    browse_to_top(&mut app, 3);
    assert_eq!(app.prompt_view.input, "long user message 7");
    assert_eq!(app.prompt_view.history_index, 7);

    // Switch to the short session WITHOUT touching the prompt (the real
    // switch path resets hovered_msg_idx and the panel but NOT
    // history_index).
    app.switch_session_locked("short");
    app.prompt_view.focus();
    assert_eq!(app.prompt_view.history_index, 7, "index carried across the switch");

    // The carried index (7) now points past the short session's single
    // user message. Ctrl+Up must clamp or no-op — not panic.
    app.process_key_event(ctrl(KeyCode::Up)).unwrap();
    assert_eq!(
        app.prompt_view.input, "short user message 0",
        "restored draft must come from the short session's history",
    );
}

#[tokio::test]
async fn history_down_after_session_switch_never_panics() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_user_messages(&mut app, "a", 6, "a");
    session_with_user_messages(&mut app, "b", 2, "b");

    app.state.current_session_id = Some("a".into());
    app.prompt_view.focus();
    browse_to_top(&mut app, 8); // history_index = 5 in a 6-message session

    app.switch_session_locked("b");
    app.prompt_view.focus();

    // Walk down past the end of the short session's history repeatedly —
    // each press must stay in bounds or no-op.
    for _ in 0..8 {
        app.process_key_event(ctrl(KeyCode::Down)).unwrap();
    }
    // Walking all the way down must end at the cleared prompt (index -1).
    assert_eq!(app.prompt_view.input, "", "history_down at the end clears");
}

#[tokio::test]
async fn history_up_on_empty_session_never_panics() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_user_messages(&mut app, "full", 4, "full");

    app.state.current_session_id = Some("full".into());
    app.prompt_view.focus();
    browse_to_top(&mut app, 6); // history_index = 3

    // Switch to a session with NO user messages at all.
    app.state.add_empty_session("empty".into(), "empty".into(), 0);
    app.switch_session_locked("empty");
    app.prompt_view.focus();

    // Both directions from a carried index into an empty history.
    app.process_key_event(ctrl(KeyCode::Up)).unwrap();
    app.process_key_event(ctrl(KeyCode::Down)).unwrap();
    // Survived with no panic.
}
