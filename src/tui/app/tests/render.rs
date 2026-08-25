use super::{App, format_tokens};

#[test]
fn format_tokens_small_values_have_no_separator() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(9), "9");
    assert_eq!(format_tokens(999), "999");
}

#[test]
fn format_tokens_groups_thousands() {
    assert_eq!(format_tokens(1_000), "1,000");
    assert_eq!(format_tokens(9_612), "9,612");
    assert_eq!(format_tokens(100_000), "100,000");
    assert_eq!(format_tokens(1_000_000), "1,000,000");
    assert_eq!(format_tokens(1_234_567), "1,234,567");
    assert_eq!(format_tokens(12_345_678), "12,345,678");
}

#[test]
fn format_tokens_handles_large_values() {
    assert_eq!(format_tokens(123_456), "123,456");
    assert_eq!(format_tokens(9_876_543_210), "9,876,543,210");
    assert_eq!(format_tokens(usize::MAX), "18,446,744,073,709,551,615");
}

/// The session chat area must shrink by the right-panel width whenever the
/// panel is visible — and the mouse dispatch shares this exact helper with
/// `render`. A wider mouse area re-wraps every message and shifts `prefix_y`,
/// making tool-box expand/collapse clicks land on the wrong row (the bug was
/// "boxes can't be expanded while the agent loop is active").
#[tokio::test]
async fn session_main_area_matches_render_width_with_right_panel() {
    use ratatui::layout::Rect;

    use super::RIGHT_PANEL_WIDTH;
    use crate::routes::session::right_panel::types::RightPanelState;

    let mut app = App::new("/tmp".to_string());
    let id = super::generate_session_id();
    app.state.add_empty_session(id.clone(), "t".into(), 0);
    app.state.current_session_id = Some(id);
    app.state.right_panel = RightPanelState::new();
    app.state.right_panel.start_pty("echo hi".into(), None);

    let area = Rect::new(0, 0, 140, 30);
    let sa = app.session_main_area(area);
    assert_eq!(sa.main.width, 140 - RIGHT_PANEL_WIDTH);
    assert_eq!(sa.right_panel_w, RIGHT_PANEL_WIDTH);

    app.sidebar.open = true;
    let sa = app.session_main_area(area);
    assert_eq!(
        sa.main.width,
        140 - super::SIDEBAR_WIDTH - RIGHT_PANEL_WIDTH,
        "open sidebar + right panel must both be subtracted"
    );
}

/// Hidden panel (narrow terminal or no content) must not shrink the area.
#[tokio::test]
async fn session_main_area_ignores_hidden_right_panel() {
    use ratatui::layout::Rect;

    use crate::routes::session::right_panel::types::RightPanelState;

    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.right_panel = RightPanelState::new();

    // Narrow terminal → panel hidden even if there were content.
    app.state.right_panel.start_pty("echo hi".into(), None);
    let sa = app.session_main_area(Rect::new(0, 0, 90, 30));
    assert_eq!(sa.right_panel_w, 0);
    assert_eq!(sa.main.width, 90);

    // Wide terminal but no todos/pty content → panel hidden.
    let mut app2 = App::new("/tmp".to_string());
    app2.state.add_empty_session("t".into(), "t".into(), 0);
    app2.state.current_session_id = Some("t".into());
    let sa2 = app2.session_main_area(Rect::new(0, 0, 140, 30));
    assert_eq!(sa2.right_panel_w, 0);
    assert_eq!(sa2.main.width, 140);
}
