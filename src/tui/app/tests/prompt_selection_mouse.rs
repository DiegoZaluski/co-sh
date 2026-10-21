//! Integration tests for the prompt's mouse text-selection gesture, driven
//! through the REAL mouse pipeline (`handle_mouse_event`) so the press/drag/
//! release ownership handoff — not just the prompt component's byte math —
//! is covered.
//!
//! Regression being pinned: a drag anchored at the TAIL of the prompt (the
//! only way to extend a selection from the end of the text) used to lose its
//! owner the moment the pointer left the prompt box. `is_prompt_drag` was
//! re-derived per drag event from the CURRENT pointer row, so dragging below
//! the box handed the gesture to the chat's drag extraction; the release then
//! copied the CHAT region and the prompt kept only a zero-length anchor —
//! `has_selection()` false, nothing copied. Copying the tail was only
//! possible by re-anchoring at the head or mid-text.
//!
//! The assertions read `App::last_prompt_copy` (test-only record of the
//! release's copy action) — no clipboard dependency, so the tests also run on
//! headless CI where no clipboard exists.

use super::{App, HOME_LOCK, isolate_home};
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse,
    MouseEventKind as CKind,
};

fn mouse(kind: CKind, x: u16, y: u16) -> CMouse {
    CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

/// The text the release copied from the prompt selection (`None` → empty).
fn copied_text(app: &App) -> String {
    app.last_prompt_copy.clone().unwrap_or_default()
}

/// A session with one user message so the prompt drops from its landing
/// position and carries a realistic geometry, plus a prompt whose content
/// wraps (so the tail row sits well below the head row).
fn session_app() -> App {
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    if let Some(s) = app.state.current_session_mut() {
        s.messages.push(crate::types::Message {
            id: "u0".into(),
            role: crate::types::MessageRole::User,
            parts: vec![crate::types::Part::Text(crate::types::TextPart {
                text: "primeira mensagem da sessao".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
    // Wrap the draft so the prompt grows: the LAST wrapped line (the tail)
    // is what the regression drag starts on.
    app.prompt_view.input = "linha do rascunho ".repeat(12);
    app.prompt_view.cursor_pos = app.prompt_view.input.len();
    app
}

fn ctrl_up(app: &mut App) {
    let _ = app
        .process_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL))
        .unwrap();
}

/// The press→drag→release gesture for a tail selection: press on the FIRST
/// column of the prompt's LAST visible text row (the box has 5 chrome rows:
/// BASE_H + AGENT_H + CAP_H + FOOTER_H), drag downward OFF the prompt box
/// (rows below it), release there. Before the fix this handed the drag to
/// the chat mid-gesture; before the clamp it stalled on a zero-length
/// anchor the moment the pointer left the text rows.
fn tail_drag_out_of_the_box(app: &mut App) {
    let area = app.compute_prompt_area().expect("prompt area exists");
    let press_y = area.bottom().saturating_sub(5).max(area.y + 1);
    let press_x = area.x + 3; // column 0 of the text area
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), press_x, press_y))
        .unwrap();
    // Drag down, leaving the box entirely: several rows past the bottom.
    for dy in 1..=3u16 {
        let _ = app
            .handle_mouse_event(mouse(
                CKind::Drag(CBtn::Left),
                press_x,
                area.bottom() + dy,
            ))
            .unwrap();
    }
    let _ = app
        .handle_mouse_event(mouse(
            CKind::Up(CBtn::Left),
            press_x,
            area.bottom() + 3,
        ))
        .unwrap();
}

/// The selection survives a drag that leaves the prompt box and the release
/// copies the selected TAIL — the gesture that used to be stolen by the chat.
#[tokio::test]
async fn tail_drag_leaving_the_prompt_box_copies_the_selection() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    app.prompt_view.focus();
    let input = app.prompt_view.input.clone();

    tail_drag_out_of_the_box(&mut app);

    let copied = copied_text(&app);
    // A tail-anchored drag selects from its anchor to the END of the draft:
    // the copy must be a non-empty suffix of the restored prompt — the old
    // failure copied NOTHING (the chat stole the release).
    assert!(!copied.is_empty(), "the tail drag must produce a copy");
    assert!(
        input.ends_with(&copied),
        "a tail selection must copy a suffix of the draft, got {copied:?}"
    );
    // The selection was consumed by the copy: nothing lingers.
    assert!(!app.prompt_view.has_selection());
    // And the chat never opened message actions from the foreign release.
    assert!(app.session_view.pending_message_action.is_none());
}

/// Same gesture but anchored on the message pulled from the Ctrl+Up history:
/// the selection the user makes on a RESTORED draft copies exactly what was
/// restored (this is the exact reported scenario).
#[tokio::test]
async fn tail_drag_on_a_history_restored_prompt_copies_it() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    app.prompt_view.focus();

    // Pull the first user message back into the prompt with Ctrl+Up.
    ctrl_up(&mut app);
    let restored = app.prompt_view.input.clone();
    assert_eq!(
        restored, "primeira mensagem da sessao",
        "Ctrl+Up restores the session's user message"
    );

    tail_drag_out_of_the_box(&mut app);

    let copied = copied_text(&app);
    assert_eq!(
        copied, restored,
        "the copy of a restored draft must match the restored text exactly"
    );
}

/// A drag that stays INSIDE the box keeps working (no regression on the
/// ordinary path): press mid-text, drag to the end, copy.
#[tokio::test]
async fn drag_inside_the_box_still_copies() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    app.prompt_view.focus();

    let area = app.compute_prompt_area().expect("prompt area exists");
    // Press at the FIRST column of the first text row (col 0 → byte 0).
    let press_x = area.x + 3;
    let press_y = area.y + 1;
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), press_x, press_y))
        .unwrap();
    // Extend the selection to the tail row of the box.
    let tail_y = area.bottom().saturating_sub(2).max(press_y);
    for row in (press_y + 1)..=tail_y {
        let _ = app
            .handle_mouse_event(mouse(CKind::Drag(CBtn::Left), press_x + 3, row))
            .unwrap();
    }
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), press_x + 3, tail_y))
        .unwrap();

    let copied = copied_text(&app);
    assert!(!copied.is_empty(), "an in-box drag must still produce a copy");
    assert!(
        app.prompt_view.input.starts_with(&copied),
        "a head-anchored drag copies a prefix of the draft, got {copied:?}"
    );
}

/// A tail drag that leaves the box must NOT act on the chat content under
/// the release point (the old failure routed the release to the chat's drag
/// extraction; a foreign click there could also open Message actions).
#[tokio::test]
async fn tail_drag_release_outside_the_box_never_opens_message_actions() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    app.prompt_view.focus();

    tail_drag_out_of_the_box(&mut app);

    assert!(
        app.session_view.pending_message_action.is_none(),
        "a prompt-owned drag release must never land on a chat row"
    );
}
