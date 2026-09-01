use super::super::{App, HOME_LOCK, isolate_home, key};
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
    });

    // Invalid: no command yet → Enter keeps the dialog open.
    assert!(app.handle_hook_input_key(key(KeyCode::Enter)));
    assert!(app.dialog.visible(), "missing command keeps the form open");

    // Fill Name, then jump to Command and fill it.
    for ch in "block rm".chars() {
        app.handle_hook_input_key(key(KeyCode::Char(ch)));
    }
    app.handle_hook_input_key(key(KeyCode::Down));
    app.handle_hook_input_key(key(KeyCode::Down));
    for ch in "exit 2".chars() {
        app.handle_hook_input_key(key(KeyCode::Char(ch)));
    }
    app.handle_hook_input_key(key(KeyCode::Enter));

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
    });
    assert!(app.handle_hook_input_key(key(KeyCode::Esc)));
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
    });

    // Derive geometry from the SAME terminal size the app will use when
    // handling the click.
    let term = app.terminal_size();
    let values = ["", "", "exit 2", ""];
    let (_, dialog_y, _, _, content_x, cols) = crate::ui::dialogs::hook_input_metrics(term, values);
    let cmd_value_row =
        crate::ui::dialogs::hook_field_geometries(dialog_y, values, cols)[2].value_y;
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
