use super::{App, HOME_LOCK, isolate_home};
use crate::left_panel::LEFT_PANEL_WIDTH;
use crate::left_panel::Mode;
use crate::session_store::generate_session_id;
use crate::types::{Message, MessageRole, Part, TextPart};
use crossterm::event::{
    KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
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
