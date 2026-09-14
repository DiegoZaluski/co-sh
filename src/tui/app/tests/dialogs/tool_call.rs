use super::super::App;

/// Regression: the picker box must be wide enough to show each option's
/// full description — the inline row used to truncate at
/// "JSON written in the text,". Geometry at 80x24 with the 60-wide box:
/// dialog_x=10, list_top=12, labels start at x=13; the inline label ends
/// with 'y' at x=63 and the native one with ')' at x=58.
#[tokio::test]
async fn tool_call_dialog_render_shows_full_description_text() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut app = App::new("/tmp".to_string());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "toolcall".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);

    let theme = app.theme.clone();
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    app.dialog.render(
        &mut buf,
        Rect::new(0, 0, 80, 24),
        &theme,
        std::time::SystemTime::now(),
    );

    assert_eq!(
        buf[(63, 13)].symbol(),
        "y",
        "inline description must end with 'locally' (not truncated)"
    );
    assert_eq!(
        buf[(58, 12)].symbol(),
        ")",
        "native description must end with 'default)' (not truncated)"
    );
}

/// The tool-call picker must respond to the mouse wheel like the other
/// list dialogs (ModelList/ThemeList/ReasoningList). The wheel routes to
/// the dialog's Up/Down handler; the dialog stays open while cycling the
/// two options. Sleeps straddle the 50ms scroll debounce.
#[tokio::test]
async fn tool_call_dialog_mouse_wheel_changes_selection() {
    use crossterm::event::{KeyModifiers, MouseEvent as CMouse, MouseEventKind as CKind};
    let mut app = App::new("/tmp".to_string());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "toolcall".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(app.is_tool_call_dialog_visible());
    assert_eq!(app.dialog.current().unwrap().selected, 0);

    let wheel = |kind: CKind| CMouse {
        kind,
        column: 40,
        row: 12,
        modifiers: KeyModifiers::NONE,
    };

    // The 50ms scroll debounce starts at App creation — wait before the
    // first wheel notch so it isn't dropped.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;

    // Wheel down: native -> inline (selection 1), dialog stays open.
    assert!(app.handle_mouse_event(wheel(CKind::ScrollDown)).unwrap());
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    assert!(app.is_tool_call_dialog_visible());

    // Wheel down again: wraps back to native.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(app.handle_mouse_event(wheel(CKind::ScrollDown)).unwrap());
    assert_eq!(app.dialog.current().unwrap().selected, 0);

    // Wheel up: native -> inline.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(app.handle_mouse_event(wheel(CKind::ScrollUp)).unwrap());
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    assert!(app.is_tool_call_dialog_visible());
}

/// Clicking an option in the picker must APPLY the mode (not just close
/// the dialog like the pre-fix `_` fallback did). Row 1 = inline. Clicks
/// are processed on the Up event (the app's click dispatch gate), so the
/// test sends Down then Up.
#[tokio::test]
async fn tool_call_dialog_mouse_click_applies_mode() {
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };
    let mut app = App::new("/tmp".to_string());
    // Pin the geometry: without this, terminal::size() returns the REAL
    // console running `cargo test`, and the hardcoded click row below
    // misses the dialog rows on any terminal that is not 80x24.
    app.set_test_size(80, 24);
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "toolcall".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(app.is_tool_call_dialog_visible());

    // Geometry mirrors the ToolCallList mouse handler with the (80x24)
    // fallback terminal size: dialog_x = 20, dialog_y = 9, list_top = 12.
    // Dialog clicks are dispatched on the Up event; send Down first so
    // the app tracks a real press.
    fn click_row(app: &mut App, row: u16) {
        let evt = |kind: CKind| CMouse {
            kind,
            column: 40,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(evt(CKind::Down(CBtn::Left)))
            .unwrap();
        app.handle_mouse_event(evt(CKind::Up(CBtn::Left))).unwrap();
    }

    // Click the "inline" row (list_top + 1 = 13).
    click_row(&mut app, 13);
    assert_eq!(
        app.llm_config.tool_call_mode,
        cosh_sdk::connector::ToolCallMode::Inline,
        "clicking the inline row must apply the mode"
    );
    assert!(
        !app.is_tool_call_dialog_visible(),
        "dialog closes after applying"
    );

    // Reopen and click "native" (row 12): back to Native.
    app.run_slash_command(&cmd);
    click_row(&mut app, 12);
    assert_eq!(
        app.llm_config.tool_call_mode,
        cosh_sdk::connector::ToolCallMode::Native
    );
    assert!(!app.is_tool_call_dialog_visible());
}
