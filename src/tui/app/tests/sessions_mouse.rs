use super::{App, HOME_LOCK, isolate_home};
use crate::left_panel::LEFT_PANEL_WIDTH;
use crate::left_panel::Mode;
use crate::session_store::generate_session_id;
use crate::types::{Message, MessageRole, Part, TextPart};
use crate::ui::dialogs::DialogType;
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse,
    MouseEventKind as CKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn mouse(kind: CKind, x: u16, y: u16) -> CMouse {
    CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

/// Current session with several messages so chat content spans many rows,
/// plus a long prompt. The session view is rendered once so height caches
/// and `session_area` are populated (matching a live frame).
fn build_many() -> App {
    let mut app = App::new("/tmp".to_string());
    let cur = generate_session_id();
    app.state
        .add_empty_session(cur.clone(), "current".into(), 0);
    app.state.current_session_id = Some(cur);
    if let Some(s) = app.state.current_session_mut() {
        let mut msgs = Vec::new();
        for i in 0..8 {
            msgs.push(Message {
                id: format!("msg-{i}"),
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                parts: vec![Part::Text(TextPart {
                    text: format!(
                        "user msg number {i}: {}",
                        "lorem ipsum dolor sit amet ".repeat(4 + i)
                    ),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });
        }
        s.messages = msgs;
    }
    app.prompt_view.input = "y".repeat(400);
    app.prompt_view.cursor_pos = app.prompt_view.input.len();
    app.sidebar.open = true;
    app.left_panel = Mode::History;
    app.state.session_summaries.clear();
    for i in 0..40 {
        app.state
            .session_summaries
            .push(crate::session_store::SessionSummary {
                session_id: format!("history-{i}"),
                title: format!("session {i}"),
                created_at: i as u64,
                message_count: 0,
                cwd: String::new(),
                model: None,
                title_generated: false,
            });
    }
    render_session(&mut app);
    app
}

/// Current session with a single user message (no scrolling) and only a few
/// left panel sessions, mirroring a small real session.
fn build_small() -> App {
    let mut app = App::new("/tmp".to_string());
    let cur = generate_session_id();
    app.state
        .add_empty_session(cur.clone(), "current".into(), 0);
    app.state.current_session_id = Some(cur);
    if let Some(s) = app.state.current_session_mut() {
        s.messages.push(Message {
            id: "msg-u".into(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: "hello click target".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
    app.sidebar.open = true;
    app.left_panel = Mode::History;
    app.state.session_summaries.clear();
    for i in 0..3 {
        app.state
            .session_summaries
            .push(crate::session_store::SessionSummary {
                session_id: format!("history-{i}"),
                title: format!("session {i}"),
                created_at: i as u64,
                message_count: 0,
                cwd: String::new(),
                model: None,
                title_generated: false,
            });
    }
    render_session(&mut app);
    app
}

fn render_session(app: &mut App) {
    let session_area = app.session_viewport_area();
    let mut buf = Buffer::empty(Rect::new(0, 0, 140, 40));
    app.session_view.render(
        &mut buf,
        session_area,
        &app.state,
        &app.theme,
        &app.config,
        0.0,
    );
}

fn assert_no_dialog_or_focus(app: &App) {
    assert!(!app.dialog.visible(), "unexpected dialog");
    assert!(!app.prompt_view.is_focused, "prompt unexpectedly focused");
    assert!(
        app.session_view.pending_message_action.is_none(),
        "unexpected pending message action"
    );
}

#[tokio::test]
async fn sidebar_click_on_prompt_row_switches_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();
    let guard = app.state.current_session_id.clone();
    let prompt_area = app.compute_prompt_area().unwrap();

    // A row that sits inside the prompt (chat) geometry AND maps to a valid
    // left panel session (row y -> summary index scroll_offset + (y-2)).
    let y = prompt_area.y + 1;
    assert!(y >= 2, "row must reference a sidebar session");
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 5, y))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 5, y))
        .unwrap();

    let cur = app.state.current_session_id.clone();
    assert_ne!(cur, guard, "sidebar click must open a different session");
    let cur = cur.unwrap();
    assert!(
        cur.starts_with("history-"),
        "must switch to a sidebar session, got {cur}"
    );
    assert_no_dialog_or_focus(&app);
}

#[tokio::test]
async fn sidebar_tiny_drag_switches_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();
    let guard = app.state.current_session_id.clone();

    // A drag released one cell away — previously this entered the chat
    // text-region extraction path instead of switching sessions.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 5, 10))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 6, 11))
        .unwrap();

    let cur = app.state.current_session_id.clone();
    assert_ne!(cur, guard, "sidebar drag must open a different session");
    let cur = cur.unwrap();
    assert!(
        cur.starts_with("history-"),
        "must switch to a sidebar session, got {cur}"
    );
    assert_no_dialog_or_focus(&app);
}

#[tokio::test]
async fn prompt_drag_released_over_sidebar_blurs_prompt() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();
    let guard = app.state.current_session_id.clone();
    let prompt_area = app.compute_prompt_area().unwrap();

    // Drag starts inside the prompt box (focusing it), then releases over a
    // left panel session. The prompt must lose focus once the cursor leaves its
    // area — it must not stay "lit".
    let down_x = prompt_area.x + 3; // inside the prompt's text area
    let down_y = prompt_area.y + 1; // first content row of the prompt
    let mid_x = down_x + 4; // still inside the prompt (extends selection)
    let up_x = 5; // over the sidebar
    let up_y = 10; // a sidebar session row

    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), down_x, down_y))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Drag(CBtn::Left), mid_x, down_y))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), up_x, up_y))
        .unwrap();

    assert!(
        !app.prompt_view.is_focused,
        "prompt must blur after a drag released over the sidebar"
    );
    // It was a prompt text-drag (started inside the prompt), so it must not
    // switch sessions.
    assert_eq!(app.state.current_session_id, guard);
}

#[tokio::test]
async fn sidebar_drag_started_in_chat_keeps_selecting_text() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();
    let guard = app.state.current_session_id.clone();

    // Drag from inside the chat (x=30) into the left panel region: this must
    // NOT be routed to the left panel (the click did not start on a session).
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 30, 4))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 10, 5))
        .unwrap();

    // It is a text-selection drag; it must not switch sessions.
    assert_eq!(app.state.current_session_id, guard);
    // And no stray dialog/focus.
    assert_no_dialog_or_focus(&app);
}

#[tokio::test]
async fn sidebar_click_below_list_does_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();
    let prompt_area = app.compute_prompt_area().unwrap();

    // Row inside the prompt geometry but beyond the 3 left panel sessions.
    let y = 12u16.max(prompt_area.y + 1);
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 5, y))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 5, y))
        .unwrap();

    assert_eq!(app.state.current_session_id, guard);
    assert_no_dialog_or_focus(&app);
}

/// Exact columns of the first row's 🗑 glyph. Row geometry in the panel
/// (Rect::new(0, 0, LEFT_PANEL_WIDTH, _)): LEFT_PAD 2 + PREFIX_W 2 + the
/// title "session 0" (9 chars) + GAP 1 = trash_x 14; the glyph occupies
/// columns 14 and 15 (TRASH_W 2).
fn first_row_trash_span() -> std::ops::Range<u16> {
    14..16
}

#[tokio::test]
async fn trash_click_on_the_exact_glyph_opens_delete_confirm() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();
    let span = first_row_trash_span();

    // Down+Up on the glyph's first column must open the delete confirm —
    // and must NOT switch sessions.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), span.start, 2))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), span.start, 2))
        .unwrap();

    assert!(
        app.dialog.visible() && app.dialog.current().is_some(),
        "clicking the 🗑 glyph must open the delete confirm"
    );
    assert!(matches!(
        app.dialog.current().unwrap().dialog_type,
        crate::ui::dialogs::DialogType::Confirm { .. }
    ));
    assert_eq!(
        app.state.current_session_id, guard,
        "a delete click must not switch sessions"
    );
}

#[tokio::test]
async fn click_just_after_the_trash_glyph_does_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();
    let span = first_row_trash_span();

    // One cell past the glyph — the cleared cell / right-pad region the old
    // row-wide tests treated as delete, then as switch: it must do nothing.
    // Sweep every column from the glyph's end to the panel edge.
    for x in span.end..LEFT_PANEL_WIDTH {
        let _ = app
            .handle_mouse_event(mouse(CKind::Down(CBtn::Left), x, 2))
            .unwrap();
        let _ = app
            .handle_mouse_event(mouse(CKind::Up(CBtn::Left), x, 2))
            .unwrap();

        assert!(
            !app.dialog.visible(),
            "click at x={x} (past the glyph) must not open the delete confirm"
        );
        assert_eq!(
            app.state.current_session_id, guard,
            "click at x={x} is outside the title string: no session switch"
        );
    }
}

#[tokio::test]
async fn click_left_of_the_title_text_does_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();

    // The left pad and the selection-prefix columns (x = 0..4) precede the
    // title: a click there is on the row but not on the string.
    for x in 0..4u16 {
        let _ = app
            .handle_mouse_event(mouse(CKind::Down(CBtn::Left), x, 2))
            .unwrap();
        let _ = app
            .handle_mouse_event(mouse(CKind::Up(CBtn::Left), x, 2))
            .unwrap();

        assert!(
            !app.dialog.visible(),
            "click at x={x} (before the title) must not open the delete confirm"
        );
        assert_eq!(
            app.state.current_session_id, guard,
            "click at x={x} is outside the title string: no session switch"
        );
    }
}

#[tokio::test]
async fn truncated_title_is_clickable_only_in_its_visible_part() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();

    // A 40-char title: the panel displays only the first max_text_w = 12
    // chars, so the clickable string is x = 4..16 — NOT the string's full
    // length. The glyph shifts to trash_x = 4 + 12 + 1 = 17 (span 17..19).
    app.state.session_summaries[0].title = "x".repeat(40);
    render_session(&mut app);

    // Last visible char of the truncated title: switches.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 15, 2))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 15, 2))
        .unwrap();
    assert_ne!(
        app.state.current_session_id, guard,
        "click on the last visible char of a truncated title must switch"
    );
    assert!(!app.dialog.visible());

    // Reset: click one char PAST the displayed truncation (x = 16, the gap
    // before the glyph). The string is 40 chars long, but only 12 are shown
    // — the click is outside what is on screen, so nothing happens.
    let mut app = build_small();
    app.state.session_summaries[0].title = "x".repeat(40);
    render_session(&mut app);
    let guard = app.state.current_session_id.clone();
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 16, 2))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 16, 2))
        .unwrap();
    assert_eq!(
        app.state.current_session_id, guard,
        "click past the displayed truncation must not switch (string len != visible width)"
    );
    assert!(
        !app.dialog.visible(),
        "click in the gap before the glyph must not open the delete confirm"
    );

    // And the glyph itself still deletes at its shifted position 17.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 17, 2))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 17, 2))
        .unwrap();
    assert!(
        app.dialog.visible() && app.dialog.current().is_some(),
        "the truncated title's glyph (shifted to x=17) must still delete"
    );
}

#[tokio::test]
async fn click_inside_the_title_text_switches_not_deletes() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    let guard = app.state.current_session_id.clone();

    // A column inside the row's title text ("session 0" spans x=4..13).
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 6, 2))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 6, 2))
        .unwrap();

    assert!(
        !app.dialog.visible(),
        "clicking the title must not open the delete confirm"
    );
    assert_ne!(
        app.state.current_session_id, guard,
        "clicking the title must switch sessions"
    );
}

#[tokio::test]
async fn chat_message_click_still_opens_message_actions() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();

    // Click the user message at the top of the chat (x inside the session
    // view, y at the first content row). Must still open Message actions.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), LEFT_PANEL_WIDTH + 4, 1))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), LEFT_PANEL_WIDTH + 4, 1))
        .unwrap();

    assert!(
        app.dialog.visible() && app.dialog.current().is_some(),
        "clicking a user message must open Message actions"
    );
    let d = app.dialog.current().unwrap();
    assert!(matches!(
        d.dialog_type,
        crate::ui::dialogs::DialogType::MessageActions { .. }
    ));
}

#[tokio::test]
async fn chat_prompt_click_still_focuses() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();
    let prompt_area = app.compute_prompt_area().unwrap();

    // Click inside the prompt box (x clear of the left panel): must focus.
    let _ = app
        .handle_mouse_event(mouse(
            CKind::Down(CBtn::Left),
            prompt_area.x + 4,
            prompt_area.y,
        ))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(
            CKind::Up(CBtn::Left),
            prompt_area.x + 4,
            prompt_area.y,
        ))
        .unwrap();

    assert!(
        app.prompt_view.is_focused,
        "clicking the prompt must focus it"
    );
}

#[tokio::test]
async fn hover_over_sidebar_does_not_highlight_chat() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_many();

    // Hovering over a left panel session at a row that (chat-side) belongs to a
    // message must not set chat hover state.
    let _ = app.handle_mouse_event(mouse(CKind::Moved, 5, 10)).unwrap();

    assert!(
        app.session_view.hovered_msg_idx.is_none(),
        "hovering the sidebar must not highlight chat content"
    );
}

#[tokio::test]
async fn leaving_chat_content_to_sidebar_clears_focus_highlight() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();

    // Hover the single user message at the top of the chat: the row tint
    // (the "focus coloring") turns on.
    let _ = app.handle_mouse_event(mouse(CKind::Moved, 30, 1)).unwrap();
    assert!(
        app.session_view.hovered_msg_idx.is_some(),
        "hovering the user message must light its highlight"
    );

    // Move sideways out of the chat content, over the left panel: the highlight
    // must turn off, not stay lit.
    let _ = app.handle_mouse_event(mouse(CKind::Moved, 5, 10)).unwrap();

    assert!(
        app.session_view.hovered_msg_idx.is_none(),
        "moving over the sidebar must clear the chat focus highlight"
    );
}

/// The `delete all` footer button: pinned geometry (terminal 140x40 →
/// sidebar rows 0..40, footer row 39, centered button columns 6..16).
#[tokio::test]
async fn delete_all_button_click_opens_the_confirm() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    app.set_test_size(140, 40);

    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 10, 39))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 10, 39))
        .unwrap();

    assert!(
        app.dialog.visible(),
        "the click must open the Confirm dialog"
    );
    let dialog = app.dialog.current().expect("confirm dialog");
    let DialogType::Confirm { message } = &dialog.dialog_type else {
        panic!("expected a Confirm dialog");
    };
    assert_eq!(
        message, "Delete ALL sessions",
        "the confirm must name the cwd scope, not a single session"
    );
    // Default must be "No" — a destructive bulk action never defaults to yes.
    assert_eq!(dialog.selected, 1, "the confirm must default to No");
}

/// Confirming the delete-all dialog removes every cwd session from the
/// sidebar (the store is cwd-scoped by construction, so disk-wise the same
/// set is tombstoned).
#[tokio::test]
async fn delete_all_confirm_removes_every_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    app.set_test_size(140, 40);

    // Click the button, then confirm with the keyboard: select Yes, Enter.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 10, 39))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 10, 39))
        .unwrap();
    assert!(app.dialog.visible());
    if let Some(d) = app.dialog.current_mut() {
        d.selected = 0;
    }
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.state.session_summaries.is_empty(),
        "every cwd session must be removed from the sidebar"
    );
    assert!(
        !app.dialog.visible(),
        "the confirm must pop after resolving"
    );
    // The viewed session is the unsaved in-memory one (not a persisted
    // summary), so it stays — exactly the single-delete semantics.
    assert!(app.state.current_session_id.is_some());
}

/// The default "No" keeps every session: the confirm is a real gate.
#[tokio::test]
async fn delete_all_cancel_keeps_every_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    app.set_test_size(140, 40);
    let before = app.state.session_summaries.len();

    // Open the confirm (default No) and dismiss it with Esc.
    let _ = app
        .handle_mouse_event(mouse(CKind::Down(CBtn::Left), 10, 39))
        .unwrap();
    let _ = app
        .handle_mouse_event(mouse(CKind::Up(CBtn::Left), 10, 39))
        .unwrap();
    assert!(app.dialog.visible());
    app.process_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(!app.dialog.visible());
    assert_eq!(
        app.state.session_summaries.len(),
        before,
        "cancelling must not delete anything"
    );
}

/// The footer row outside the button span is dead: no dialog, no selection
/// change — the reserved row is not a list row and the button is exact-span.
/// The centered button occupies 6..16 of the 22-wide panel, so x=0 is a dead
/// in-panel column; x=23 additionally exercises the outside-the-panel guard.
#[tokio::test]
async fn delete_all_footer_row_outside_the_button_does_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = build_small();
    app.set_test_size(140, 40);
    let selected = app.sidebar.selection.selected_index;

    for (x, why) in [(0u16, "in-panel dead column"), (23, "outside the panel")] {
        let _ = app
            .handle_mouse_event(mouse(CKind::Down(CBtn::Left), x, 39))
            .unwrap();
        let _ = app
            .handle_mouse_event(mouse(CKind::Up(CBtn::Left), x, 39))
            .unwrap();
        assert!(
            !app.dialog.visible(),
            "click at x={x} ({why}) must not open the confirm"
        );
        assert_eq!(
            app.sidebar.selection.selected_index, selected,
            "the footer row is not a list row: no selection change at x={x}"
        );
    }
}
