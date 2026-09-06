use super::super::{App, HOME_LOCK, isolate_home, key, mod_key};
use crate::ui::dialogs::DialogType;
use crossterm::event::{KeyCode, KeyModifiers};

/// The hook registration box saves a valid hook into setup (in-memory)
/// and closes; an invalid one keeps the box open.
#[tokio::test]
async fn hook_input_dialog_saves_valid_hook_and_rejects_invalid() {
    // setup.save() persists to $HOME — point it at a scratch dir so the
    // test never touches the developer's real config. The lock keeps the
    // two hook tests from racing each other's environment.
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: String::new(),
        timeout: String::new(),
        field: 0,
        cursor_pos: 0,
        selection: None,
    });

    // Invalid: no command yet → Enter keeps the dialog open.
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(app.dialog.visible(), "missing command keeps the form open");

    // Fill Name, then jump to Command and fill it.
    for ch in "block rm".chars() {
        app.handle_registration_form_key(key(KeyCode::Char(ch)));
    }
    app.handle_registration_form_key(key(KeyCode::Down));
    app.handle_registration_form_key(key(KeyCode::Down));
    for ch in "exit 2".chars() {
        app.handle_registration_form_key(key(KeyCode::Char(ch)));
    }
    app.handle_registration_form_key(key(KeyCode::Enter));

    assert!(!app.dialog.visible(), "valid save closes the dialog");
    let hooks = &app.setup.hooks.events[crate::routes::settings::PRE_TOOL_USE_EVENT];
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].name, "block rm");
    assert_eq!(hooks[0].command, "exit 2");
}

#[tokio::test]
async fn hook_input_dialog_esc_discards() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });
    assert!(app.handle_registration_form_key(key(KeyCode::Esc)));
    assert!(!app.dialog.visible());
    assert!(
        !app.setup
            .hooks
            .events
            .contains_key(crate::routes::settings::PRE_TOOL_USE_EVENT),
        "esc must not persist anything"
    );
}

/// The cache-duration input accepts "1h30m"-style free-form durations:
/// Enter parses and persists the minutes; an invalid value keeps the box
/// open without touching setup.
#[tokio::test]
async fn cache_ttl_input_parses_and_persists_minutes() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::CacheTtlInput {
        setting: "anthropic_cache_ttl",
        input: String::new(),
        cursor_pos: 0,
    });

    // Typing goes through the shared single-input mechanics.
    for ch in "1h30m".chars() {
        assert!(app.handle_text_input_dialog_key(KeyCode::Char(ch)));
    }
    assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible(), "a valid duration closes the box");
    assert_eq!(app.setup.cache.anthropic_ttl_min, 90);
    assert!(
        app.setup.anthropic_cache_ttl_1h(),
        "90m maps onto the 1h TTL"
    );

    // Invalid input keeps the box open and persists nothing.
    app.dialog.show(DialogType::CacheTtlInput {
        setting: "openai_cache_retention",
        input: "abc".into(),
        cursor_pos: 3,
    });
    assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
    assert!(app.dialog.visible(), "an invalid duration stays open");
    assert_eq!(app.setup.cache.openai_retention_min, 0);

    // "default" resets the setting to 0 (the invalid "abc" is erased first).
    for _ in 0..3 {
        app.handle_text_input_dialog_key(KeyCode::Backspace);
    }
    for ch in "default".chars() {
        app.handle_text_input_dialog_key(KeyCode::Char(ch));
    }
    assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible());
    assert_eq!(app.setup.cache.openai_retention_min, 0);
    assert_eq!(app.setup.openai_cache_retention(), None);
}

/// Clicking a value row inside the hook panel focuses the field and
/// moves the insertion point (app-level path: Up-event gate included).
#[tokio::test]
async fn hook_input_click_positions_cursor() {
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });

    // Derive geometry from the SAME terminal size the app will use when
    // handling the click.
    let term = app.terminal_size();
    let values = ["", "", "exit 2", ""];
    let (_, dialog_y, _, _, content_x, cols) =
        crate::ui::dialogs::form_panel_metrics(term, &values);
    let cmd_value_row =
        crate::ui::dialogs::form_field_geometries(dialog_y, &values, cols)[2].value_y;
    use crossterm::event::{
        MouseButton as CrosstermMouseButton, MouseEvent as CrosstermMouseEvent, MouseEventKind,
    };

    let up = CrosstermMouseEvent {
        kind: MouseEventKind::Up(CrosstermMouseButton::Left),
        column: content_x + 2, // over the 'i' of "exit 2"
        row: cmd_value_row,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_event(up).expect("mouse handled");

    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput {
                field: 2,
                cursor_pos: 2,
                ..
            })
        ),
        "click must park the cursor on char index 2"
    );
}

/// Type into whichever registration form is open (hook or MCP panel).
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert!(app.handle_registration_form_key(key(KeyCode::Char(ch))));
    }
}

/// The MCP form shows name, endpoint and timeout on one panel and persists
/// on Enter; a blank timeout means the default and the server starts
/// enabled.
#[tokio::test]
async fn mcp_form_registers_server_end_to_end() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();

    type_text(&mut app, "docs");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    type_text(&mut app, "https://example.com/mcp");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));

    assert!(!app.dialog.visible(), "valid save closes the form");
    assert_eq!(app.setup.mcp.servers.len(), 1);
    let server = &app.setup.mcp.servers[0];
    assert_eq!(server.name, "docs");
    assert!(server.enabled);
    match &server.transport {
        cosh::mcp::McpTransport::Http(http) => {
            assert_eq!(http.url, "https://example.com/mcp");
            assert_eq!(http.timeout_ms, 30_000);
        }
        cosh::mcp::McpTransport::Stdio(_) => panic!("expected http transport"),
    }
}

/// An invalid endpoint keeps the panel open WITHOUT discarding the other
/// fields — the user fixes the one line instead of restarting a wizard.
#[tokio::test]
async fn mcp_form_rejects_invalid_endpoint_and_keeps_edits() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();

    type_text(&mut app, "docs");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    // Any non-blank line parses (URL → HTTP, otherwise `command args...`),
    // so clear the endpoint to force the invalid state.
    type_text(&mut app, "my-server");
    for _ in 0.."my-server".len() {
        assert!(app.handle_registration_form_key(key(KeyCode::Backspace)));
    }
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(app.dialog.visible(), "blank endpoint keeps the panel open");
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::McpForm { name, field, .. })
                if name == "docs" && *field == 1
        ),
        "the typed name and the active field survive the rejection"
    );

    // Fix just the endpoint line and save.
    type_text(&mut app, "my-server --flag");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(!app.dialog.visible(), "fixed form saves and closes");
    assert_eq!(app.setup.mcp.servers.len(), 1);
    assert_eq!(app.setup.mcp.servers[0].name, "docs");
}

/// Duplicate names and bad timeouts are rejected with the panel (and every
/// typed field) intact.
#[tokio::test]
async fn mcp_form_rejects_duplicate_name_and_bad_timeout() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.setup
        .mcp
        .servers
        .push(cosh::mcp::build_mcp_entry("docs", "https://example.com/mcp", "").unwrap());
    app.open_mcp_form();

    type_text(&mut app, "docs");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    type_text(&mut app, "https://other.example/mcp");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(app.dialog.visible(), "duplicate name stays open");
    assert_eq!(app.setup.mcp.servers.len(), 1);

    // Rename, then try a bad timeout.
    assert!(app.handle_registration_form_key(key(KeyCode::Up)));
    assert!(app.handle_registration_form_key(key(KeyCode::Up)));
    type_text(&mut app, "-other");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    type_text(&mut app, "abc");
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(app.dialog.visible(), "bad timeout stays open");
    for _ in 0..3 {
        assert!(app.handle_registration_form_key(key(KeyCode::Backspace)));
    }
    assert!(app.handle_registration_form_key(key(KeyCode::Enter)));
    assert!(
        !app.dialog.visible(),
        "blank timeout saves with the default"
    );
    assert_eq!(app.setup.mcp.servers.len(), 2);
    assert_eq!(app.setup.mcp.servers[1].name, "docs-other");
}

/// Esc discards the whole form without persisting anything.
#[tokio::test]
async fn mcp_form_esc_discards() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();
    type_text(&mut app, "docs");
    assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    type_text(&mut app, "https://example.com/mcp");
    assert!(app.handle_registration_form_key(key(KeyCode::Esc)));
    assert!(!app.dialog.visible());
    assert!(
        app.setup.mcp.servers.is_empty(),
        "esc must not persist anything"
    );
}

/// Ctrl+key event for driving the form's word ops in tests.
fn ctrl(code: KeyCode) -> crossterm::event::KeyEvent {
    mod_key(code, KeyModifiers::CONTROL)
}

/// Mouse helpers driving the form's press/drag/release selection flow.
fn form_mouse(
    kind: crossterm::event::MouseEventKind,
    x: u16,
    y: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

/// Value-row geometry of a hook form field, derived from the SAME
/// terminal size the app uses when handling the mouse.
fn hook_field_row(app: &App, values: &[&str], field: usize) -> (u16, u16) {
    let term = app.terminal_size();
    let (_, dialog_y, _, _, content_x, cols) = crate::ui::dialogs::form_panel_metrics(term, values);
    let value_y = crate::ui::dialogs::form_field_geometries(dialog_y, values, cols)[field].value_y;
    (content_x, value_y)
}

/// Press-drag-release over "exit 2" selects "xit": the release copies the
/// RANGE (not the whole field) through the shared clipboard helper and
/// clears the highlight.
#[tokio::test]
async fn registration_form_drag_selects_range_and_release_copies() {
    use crossterm::event::{MouseButton as MBtn, MouseEventKind as MKind};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });

    let values = ["", "", "exit 2", ""];
    let (content_x, value_y) = hook_field_row(&app, &values, 2);
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 1, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 4, value_y))
        .expect("drag handled");
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { selection, .. })
                if matches!(
                    selection.as_ref(),
                    Some(sel) if sel.field() == 2 && sel.range_for(2, 6) == Some((1, 4))
                )
        ),
        "drag anchors at char 1 and extends to char 4"
    );

    // Release over the panel: auto-copy consumes the range, clears the
    // highlight, keeps the panel open.
    app.handle_mouse_event(form_mouse(MKind::Up(MBtn::Left), content_x + 4, value_y))
        .expect("release handled");
    let toast = app
        .toast_state
        .current
        .as_ref()
        .expect("release shows the copy toast");
    assert!(
        toast.message.contains("3 chars"),
        "the RANGE (xit) is copied, not the field: {:?}",
        toast.message
    );
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { selection, .. }) if selection.is_none()
        ),
        "release clears the highlight"
    );
    assert!(app.dialog.visible(), "copy keeps the panel open");
}

/// Typing and Backspace replace the selected range (standard editor
/// behavior); the caret lands where the range started.
#[tokio::test]
async fn registration_form_typing_replaces_selected_range() {
    use crossterm::event::{MouseButton as MBtn, MouseEventKind as MKind};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_hook_form(crate::routes::settings::PRE_TOOL_USE_EVENT, None);
    type_text(&mut app, "hello world");

    let values = ["hello world", "", "", ""];
    let (content_x, value_y) = hook_field_row(&app, &values, 0);
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 6, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 11, value_y))
        .expect("drag handled");

    // Typing replaces "world".
    assert!(app.handle_registration_form_key(key(KeyCode::Char('X'))));
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput {
                name,
                cursor_pos,
                selection,
                ..
            }) if name == "hello X" && *cursor_pos == 7 && selection.is_none()
        ),
        "typing swaps the range and parks the caret after it"
    );

    // Backspace over a fresh selection deletes just the range.
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 6, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 7, value_y))
        .expect("drag handled");
    assert!(app.handle_registration_form_key(key(KeyCode::Backspace)));
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { name, .. }) if name == "hello "
        ),
        "Backspace deletes only the selected range"
    );
}

/// First Esc drops the highlight (prompt convention); the panel only
/// discards on the next Esc.
#[tokio::test]
async fn registration_form_esc_clears_selection_before_discarding() {
    use crossterm::event::{MouseButton as MBtn, MouseEventKind as MKind};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });

    let values = ["", "", "exit 2", ""];
    let (content_x, value_y) = hook_field_row(&app, &values, 2);
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 1, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 4, value_y))
        .expect("drag handled");

    assert!(app.handle_registration_form_key(key(KeyCode::Esc)));
    assert!(app.dialog.visible(), "first Esc only drops the highlight");
    assert!(matches!(
        app.dialog.current().map(|d| &d.dialog_type),
        Some(DialogType::HookInput { selection, .. }) if selection.is_none()
    ));
    assert!(app.handle_registration_form_key(key(KeyCode::Esc)));
    assert!(!app.dialog.visible(), "second Esc discards the panel");
}

/// Ctrl+C copies the selected range (not the whole field) and keeps the
/// panel open.
#[tokio::test]
async fn registration_form_ctrl_c_copies_selected_range() {
    use crossterm::event::{MouseButton as MBtn, MouseEventKind as MKind};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });

    let values = ["", "", "exit 2", ""];
    let (content_x, value_y) = hook_field_row(&app, &values, 2);
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 1, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 4, value_y))
        .expect("drag handled");

    assert!(app.copy_registration_form_field());
    let toast = app
        .toast_state
        .current
        .as_ref()
        .expect("Ctrl+C shows the copy toast");
    assert!(
        toast.message.contains("3 chars"),
        "the RANGE (xit) is copied, not the field: {:?}",
        toast.message
    );
    assert!(app.is_registration_form_open());
}

/// The highlight paints exactly over the selected cells: covered cells
/// wear swapped text/background colors, neighbors keep the panel style.
#[tokio::test]
async fn registration_form_selection_highlight_paints_selected_cells() {
    use crossterm::event::{MouseButton as MBtn, MouseEventKind as MKind};
    use ratatui::{Terminal, backend::TestBackend};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::HookInput {
        event: crate::routes::settings::PRE_TOOL_USE_EVENT,
        editing_index: None,
        name: String::new(),
        matcher: String::new(),
        command: "exit 2".into(),
        timeout: String::new(),
        field: 2,
        cursor_pos: 6,
        selection: None,
    });

    let values = ["", "", "exit 2", ""];
    let (content_x, value_y) = hook_field_row(&app, &values, 2);
    app.handle_mouse_event(form_mouse(MKind::Down(MBtn::Left), content_x + 1, value_y))
        .expect("press handled");
    app.handle_mouse_event(form_mouse(MKind::Drag(MBtn::Left), content_x + 4, value_y))
        .expect("drag handled");

    let term = app.terminal_size();
    let mut terminal = Terminal::new(TestBackend::new(term.width, term.height)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let theme = app.theme.clone();

    // Chars 1..4 ("xit") highlighted; char 0 ('e') untouched.
    for (dx, expected) in [(1u16, true), (2, true), (3, true), (0, false), (4, false)] {
        let bg = buf
            .cell((content_x + dx, value_y))
            .map(|c| c.bg)
            .expect("panel cell exists");
        assert_eq!(
            bg,
            if expected {
                crate::theme::rgba_color(theme.text)
            } else {
                crate::theme::rgba_color(theme.background_panel)
            },
            "cell at +{dx} highlight mismatch"
        );
    }
}

/// Bracketed paste lands in the active field at the cursor with newlines
/// stripped (fields are single-line); with no form open it is a no-op that
/// leaves the background prompt untouched.
#[tokio::test]
async fn registration_form_paste_inserts_at_cursor_and_strips_newlines() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();

    type_text(&mut app, "ab");
    assert!(app.handle_registration_form_key(key(KeyCode::Left)));
    app.paste_registration_form("X\nY\rZ");
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::McpForm {
                name,
                cursor_pos,
                ..
            }) if name == "aXYZb" && *cursor_pos == 4
        ),
        "paste inserts at the cursor with newlines stripped"
    );

    // No form open: paste must not leak into the background prompt.
    app.dialog.pop();
    let before = app.prompt_view.input.clone();
    app.paste_registration_form("nope");
    assert!(!app.dialog.visible());
    assert_eq!(app.prompt_view.input, before);
}

/// Ctrl+W deletes the word before the cursor (prompt/question-dialog
/// convention); any other Ctrl+letter is consumed but ignored so it never
/// leaks a bare letter into the field. The numeric timeout keeps word ops
/// off, so Ctrl+W is a harmless no-op there.
#[tokio::test]
async fn registration_form_ctrl_w_deletes_word_and_other_ctrl_is_ignored() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_hook_form(crate::routes::settings::PRE_TOOL_USE_EVENT, None);

    type_text(&mut app, "hello world");
    assert!(app.handle_registration_form_key(ctrl(KeyCode::Char('w'))));
    assert!(app.handle_registration_form_key(ctrl(KeyCode::Char('a'))));
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { name, .. }) if name == "hello "
        ),
        "Ctrl+W eats one word, Ctrl+A is swallowed"
    );

    // Down through matcher and command to the timeout (last field).
    for _ in 0..3 {
        assert!(app.handle_registration_form_key(key(KeyCode::Down)));
    }
    type_text(&mut app, "30");
    assert!(app.handle_registration_form_key(ctrl(KeyCode::Char('w'))));
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { timeout, .. }) if timeout == "30"
        ),
        "word ops stay off on the numeric timeout"
    );
}

/// While the panel owns the keyboard, Ctrl+C copies the active field
/// through the shared selection helper instead of opening the quit
/// confirm — even when the field is empty (still consumed, nothing to
/// copy).
#[tokio::test]
async fn registration_form_ctrl_c_copies_active_field_instead_of_quitting() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();
    type_text(&mut app, "docs");

    assert!(app.copy_registration_form_field());
    assert!(app.is_registration_form_open());
    assert!(
        !matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::Confirm { .. })
        ),
        "copy must not push the quit confirm"
    );

    // Full dispatch path: Ctrl+C reaches the copy branch, not quit.
    app.process_key_event(ctrl(KeyCode::Char('c'))).unwrap();
    assert!(app.is_registration_form_open());
    assert!(
        !matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::Confirm { .. })
        ),
        "dispatched Ctrl+C must not quit either"
    );

    // Empty field: still consumed, still no quit confirm.
    for _ in 0.."docs".len() {
        assert!(app.handle_registration_form_key(key(KeyCode::Backspace)));
    }
    app.process_key_event(ctrl(KeyCode::Char('c'))).unwrap();
    assert!(app.is_registration_form_open());
}

/// Regression: the copy toast must paint ABOVE the modal registration
/// panel. The toast used to render before the dialog, so the panel buried
/// it — Ctrl+C looked completely dead (zero feedback) even when the
/// clipboard write itself succeeded.
#[tokio::test]
async fn registration_form_copy_toast_paints_above_the_panel() {
    use ratatui::{Terminal, backend::TestBackend};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_mcp_form();
    type_text(&mut app, "docs");

    // The real copy path. CI has no clipboard backend, so this shows the
    // "... clipboard write unavailable" toast — same paint order as the
    // success toast.
    assert!(app.copy_registration_form_field());

    // Toast message row with no title: y=3, text starts at x=toast_x+2.
    // Cover a wide and a compact terminal: the centered panel must never
    // bury the top-right toast at either size.
    for (w, h) in [(100u16, 30u16), (80u16, 24u16)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let toast_x = w.saturating_sub(60u16.min(w.saturating_sub(6)) + 2);
        let row: String = (toast_x + 2..w).map(|x| buf[(x, 3)].symbol()).collect();
        assert!(
            row.contains("clipboard"),
            "copy toast must stay visible above the panel at {w}x{h}, got: {row:?}"
        );
    }
}
