use super::*;

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
