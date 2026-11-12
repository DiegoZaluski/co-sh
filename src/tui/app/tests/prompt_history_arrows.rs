//! Regression test for the plain ArrowUp / ArrowDown gate between prompt
//! content and prompt history.
//!
//! Old routing grabbed plain Up/Down whenever `history_index != -1` — and a
//! prompt recalled via Ctrl+Up keeps `history_index` set (only typing or
//! sending resets it). That made the internal `cursor_up` / `cursor_down`
//! movement unreachable inside a recalled draft: both arrows kept walking
//! the history instead of moving the cursor.
//!
//! Expected: the arrow is handed to the history ONLY at the matching
//! extremity of the prompt (head for Up, tail for Down — wrapping-aware,
//! so a cursor on the first/last visual line counts even on multi-line
//! drafts); everywhere else it moves the cursor.
use super::{App, HOME_LOCK, isolate_home};
use crate::types::{Message, MessageRole, Part, TextPart};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn send(app: &mut App, key: KeyEvent) {
    app.process_key_event(key).unwrap();
}

fn session_with_texts(app: &mut App, id: &str, texts: &[&str]) {
    app.state
        .add_empty_session(id.to_string(), id.to_string(), 0);
    app.state.current_session_id = Some(id.to_string());
    let s = app.state.current_session_mut().unwrap();
    for (i, text) in texts.iter().enumerate() {
        s.messages.push(Message {
            id: format!("{id}-u{i}"),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: (*text).to_string(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
}

/// A pane dispatch is recorded through `record_user_input` (the shared path
/// for the direct send, the focused-pane dispatch and the enqueue), and it
/// must be DURABLE: a pane prompt belongs to another agent's conversation and
/// never enters this transcript, so an in-memory-only list lost it the moment
/// the process exited.
#[tokio::test]
async fn pane_inputs_persist_across_a_restart() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_texts(&mut app, "s", &["main prompt"]);
    app.prompt_view.focus();

    app.record_user_input("pane prompt");

    // In memory: the walk sees the transcript's prompt and then the pane's,
    // in send order.
    assert_eq!(app.input_history(), vec!["main prompt", "pane prompt"]);

    // Durable: the reload path a restart takes (replay of the session file)
    // brings the same list back, with the transcript untouched.
    let reloaded = app
        .session_store
        .load_session("s")
        .expect("the history must be on disk");
    assert_eq!(
        reloaded.input_history,
        vec!["main prompt", "pane prompt"],
        "pane inputs must survive the process, not just the session switch"
    );
    assert_eq!(
        reloaded.messages.len(),
        1,
        "pane inputs never enter the main session's transcript"
    );
}

#[tokio::test]
async fn up_moves_cursor_inside_recalled_draft() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_texts(&mut app, "s", &["one\ntwo"]);
    app.prompt_view.focus();

    // Ctrl+Up recalls the multi-line draft; the index stays browsing.
    send(&mut app, ctrl(KeyCode::Up));
    assert_eq!(app.prompt_view.input, "one\ntwo");
    let recalled_len = app.prompt_view.cursor_pos;
    assert_eq!(recalled_len, app.prompt_view.input.len(), "cursor at tail");

    // Plain Up must NOT re-walk the history (which would replace the input):
    // the cursor is on the LAST visual line, nowhere near the head, so the
    // arrow moves the cursor within the text instead.
    send(&mut app, plain(KeyCode::Up));
    assert_eq!(
        app.prompt_view.input, "one\ntwo",
        "input must stay the recalled draft",
    );
    assert_eq!(app.prompt_view.history_index, 0, "history walk untouched");
    assert!(
        app.prompt_view.cursor_pos < recalled_len,
        "cursor must have moved within the draft (was {recalled_len}, now {})",
        app.prompt_view.cursor_pos,
    );

    // Down from the last visual line: the tail extremity IS met, so the
    // arrow resumes the history walk — which, from the only entry, returns
    // to the (empty) draft.
    send(&mut app, plain(KeyCode::Down));
    assert_eq!(app.prompt_view.input, "", "tail ArrowDown resumes the walk");
    assert_eq!(app.prompt_view.history_index, -1);
}

#[tokio::test]
async fn down_at_tail_returns_to_draft_via_history_walk() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_texts(&mut app, "s", &["older msg", "newest msg"]);
    app.prompt_view.focus();
    // Simulate a real render's text width: these single-line drafts must
    // stay on ONE visual line for the head/tail extremities to both apply.
    app.prompt_view.input_text_width.set(80);

    // Recall the newest entry, then ArrowDown at the tail must hand over to
    // the history and come back to the empty draft (never edit the draft's
    // single line in place).
    send(&mut app, ctrl(KeyCode::Up));
    assert_eq!(app.prompt_view.input, "newest msg");
    send(&mut app, plain(KeyCode::Down));
    assert_eq!(app.prompt_view.input, "", "walked back to the live draft");
    assert_eq!(app.prompt_view.history_index, -1);
}

#[tokio::test]
async fn up_at_head_continues_history_walk() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    session_with_texts(&mut app, "s", &["older msg", "newest msg"]);
    app.prompt_view.focus();
    app.prompt_view.input_text_width.set(80);

    // Empty prompt: ArrowUp is already at the head, so it goes to history.
    send(&mut app, plain(KeyCode::Up));
    assert_eq!(app.prompt_view.input, "newest msg");

    // Single-line recalled draft: the cursor is on its only visual line,
    // so BOTH extremities are met and the walk continues upward — the
    // standard shell-like behavior.
    send(&mut app, plain(KeyCode::Up));
    assert_eq!(app.prompt_view.input, "older msg");
    assert_eq!(app.prompt_view.history_index, 0);
}
