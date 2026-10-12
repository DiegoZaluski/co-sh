use super::*;
use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

fn summary(id: &str, title: &str) -> SessionSummary {
    SessionSummary {
        session_id: id.to_string(),
        title: title.to_string(),
        created_at: 0,
        message_count: 1,
        cwd: ".".to_string(),
        model: None,
        title_generated: false,
    }
}

#[test]
fn backspace_requests_delete_of_the_selected_session() {
    let mut view = SessionsView::new();
    view.open = true;
    let summaries = [summary("s1", "first"), summary("s2", "second")];

    // Selection defaults to the first row.
    assert_eq!(
        view.handle_key(KeyCode::Backspace, &summaries),
        SessionsAction::RequestDelete("s1".to_string())
    );

    // Move to the second row — backspace targets it.
    view.select_next(summaries.len());
    assert_eq!(
        view.handle_key(KeyCode::Backspace, &summaries),
        SessionsAction::RequestDelete("s2".to_string())
    );
}

#[test]
fn backspace_is_inert_when_the_list_is_closed() {
    let mut view = SessionsView::new();
    view.open = false;
    let summaries = [summary("s1", "first")];
    assert_eq!(
        view.handle_key(KeyCode::Backspace, &summaries),
        SessionsAction::None
    );
}

#[test]
fn backspace_on_an_empty_list_is_none() {
    let mut view = SessionsView::new();
    view.open = true;
    assert_eq!(
        view.handle_key(KeyCode::Backspace, &[]),
        SessionsAction::None
    );
}

fn hover(x: u16, y: u16) -> MouseEvent {
    MouseEvent::new(
        MouseEventType::Move,
        MouseButton::Left,
        x,
        y,
        MouseModifiers::none(),
    )
}

#[test]
fn hover_moves_the_shared_selection_without_switching() {
    let mut view = SessionsView::new();
    view.open = true;
    let summaries = [
        summary("s1", "first"),
        summary("s2", "second"),
        summary("s3", "third"),
    ];
    // Panel rows 0..20: header 0..2, items from row 2.
    let area = Rect::new(0, 0, 22, 20);

    // Hovering the third row moves the keyboard-shared selection there —
    // and reports a hit, but performs no action by itself.
    assert!(view.follow_hover(&hover(5, 4), area, &summaries, &SessionStatus::Idle));
    assert_eq!(view.selection.selected_index, 2);

    // Hovering the header misses and keeps the current selection.
    assert!(!view.follow_hover(&hover(5, 0), area, &summaries, &SessionStatus::Idle));
    assert_eq!(view.selection.selected_index, 2);

    // Past the last row misses too (row 5 would be index 3: no session).
    assert!(!view.follow_hover(&hover(5, 5), area, &summaries, &SessionStatus::Idle));
    assert_eq!(view.selection.selected_index, 2);
}

#[test]
fn hover_is_inert_when_closed_or_busy() {
    let area = Rect::new(0, 0, 22, 20);
    let summaries = [summary("s1", "first"), summary("s2", "second")];

    let mut closed = SessionsView::new();
    closed.open = false;
    assert!(!closed.follow_hover(&hover(5, 3), area, &summaries, &SessionStatus::Idle));
    assert_eq!(closed.selection.selected_index, 0);

    // While the agent works the selection stays put, mirroring the click
    // path's idle gate.
    let mut busy = SessionsView::new();
    busy.open = true;
    assert!(!busy.follow_hover(&hover(5, 3), area, &summaries, &SessionStatus::Working));
    assert_eq!(busy.selection.selected_index, 0);
}
