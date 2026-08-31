use super::super::App;

fn click(app: &mut App, column: u16, row: u16) {
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };
    let evt = |kind: CKind| CMouse {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_event(evt(CKind::Down(CBtn::Left)))
        .unwrap();
    app.handle_mouse_event(evt(CKind::Up(CBtn::Left))).unwrap();
}

/// Regression: clicking "Yes" on the Ctrl+C "Quit cosh?" confirm while the
/// slash menu is open must quit the app. At 80x24 the confirm box is centered
/// at dialog_x=25, dialog_y=8; the options row is dialog_y+4=12 and "Yes"
/// spans x=35..38 (opts_x = 25 + (30-9)/2 = 35).
#[tokio::test]
async fn confirm_quit_yes_click_quits_even_with_slash_menu_open() {
    let mut app = App::new("/tmp".to_string());

    // Slash menu open, as when the user typed "/" into the prompt.
    app.slash_menu.visible = true;

    // Show the quit confirm exactly like the Ctrl+C handler does.
    app.pending_delete_session_id = None;
    app.dialog.show(crate::ui::dialogs::DialogType::Confirm {
        message: "Quit cosh?".into(),
    });
    if let Some(d) = app.dialog.current_mut() {
        d.selected = 1; // default is "No" (see the Ctrl+C handler)
    }

    click(&mut app, 36, 12);

    assert!(
        app.should_quit,
        "clicking Yes on the quit confirm must set should_quit"
    );
    assert!(
        !app.dialog.visible(),
        "confirm dialog closes after the click"
    );
}

/// Same click, but in a real Session with the prompt holding "/" so the
/// slash menu is live — mirrors the exact user flow (type "/", Ctrl+C,
/// click Yes).
#[tokio::test]
async fn confirm_quit_yes_click_in_session_with_live_slash_menu() {
    let mut app = App::new("/tmp".to_string());

    // Enter Session mode via /new, then type "/" to open the slash menu.
    let new_cmd = crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    };
    app.run_slash_command(&new_cmd);
    assert!(matches!(app.mode(), crate::app::AppMode::Session));

    app.prompt_view.input = "/".into();
    app.slash_menu.update(&app.prompt_view.input);
    assert!(app.slash_menu.visible, "slash menu is open with '/'");

    // Ctrl+C: show the quit confirm (mirrors the key handler).
    app.pending_delete_session_id = None;
    app.dialog.show(crate::ui::dialogs::DialogType::Confirm {
        message: "Quit cosh?".into(),
    });
    if let Some(d) = app.dialog.current_mut() {
        d.selected = 1;
    }

    click(&mut app, 36, 12);

    assert!(
        app.should_quit,
        "clicking Yes on the quit confirm must set should_quit"
    );
    assert!(
        !app.dialog.visible(),
        "confirm dialog closes after the click"
    );
}

/// Full real-input flow: type "/" through the key handler, press Ctrl+C
/// through the key handler, then click "Yes". No direct state manipulation.
#[tokio::test]
async fn confirm_quit_full_real_input_flow_with_slash_menu() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};

    let mut app = App::new("/tmp".to_string());

    let key = |code: K, mods: KeyModifiers| KeyEvent::new(code, mods);

    // Enter Session mode via /new, then open the slash menu with "/".
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.process_key_event(key(K::Char('/'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.slash_menu.visible, "slash menu open after typing '/'");

    // Ctrl+C opens the quit confirm (default selection = "No").
    app.process_key_event(key(K::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        app.is_confirm_dialog_visible(),
        "Ctrl+C must show the quit confirm even with the menu open"
    );

    click(&mut app, 36, 12);

    assert!(
        app.should_quit,
        "clicking Yes on the quit confirm must quit the app"
    );
}

/// THE regression: with the slash menu open, pressing Enter on the quit
/// confirm used to be STOLEN by the menu (it ran the selected command
/// instead of confirming). The modal must be sovereign: Enter decides.
#[tokio::test]
async fn confirm_quit_sovereign_enter_beats_slash_menu() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};

    let mut app = App::new("/tmp".to_string());

    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.process_key_event(KeyEvent::new(K::Char('/'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.slash_menu.visible);

    app.process_key_event(KeyEvent::new(K::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.is_confirm_dialog_visible());

    // The dialog's default selection is "No" — move to "Yes", then Enter.
    app.process_key_event(KeyEvent::new(K::Left, KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(K::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.should_quit,
        "Enter on the quit confirm must quit even with the slash menu open"
    );
    assert!(
        app.slash_menu.visible,
        "the menu itself is not closed by the modal — it is just inert"
    );
}

/// Sovereign modal: Esc cancels the quit confirm and nothing else reacts —
/// in particular the slash menu must not swallow Esc or run anything.
#[tokio::test]
async fn confirm_quit_sovereign_esc_cancels_without_menu_reaction() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};

    let mut app = App::new("/tmp".to_string());

    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.process_key_event(KeyEvent::new(K::Char('/'), KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(K::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.is_confirm_dialog_visible());

    app.process_key_event(KeyEvent::new(K::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(!app.is_confirm_dialog_visible(), "Esc cancels the confirm");
    assert!(!app.should_quit, "Esc must NOT quit");
    assert!(app.slash_menu.visible, "the menu stays as it was");
}

/// Sovereign modal: while the quit confirm is up, ordinary keys are
/// swallowed — they must not reach the prompt input or the menu filter.
#[tokio::test]
async fn confirm_quit_swallows_other_keys() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};

    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.process_key_event(KeyEvent::new(K::Char('/'), KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(K::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    let input_before = app.prompt_view.input.clone();
    let query_before = app.slash_menu.query.clone();

    app.process_key_event(KeyEvent::new(K::Char('x'), KeyModifiers::NONE))
        .unwrap();

    assert_eq!(
        app.prompt_view.input, input_before,
        "the prompt must not receive keys while the modal decides"
    );
    assert_eq!(
        app.slash_menu.query, query_before,
        "the menu filter must not receive keys while the modal decides"
    );
    assert!(app.is_confirm_dialog_visible());
}
