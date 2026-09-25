use super::{App, HOME_LOCK, isolate_home, mod_key};
use crossterm::event::{KeyCode, KeyModifiers};

/// Tab (ToggleMode) persists the cycled-to mode as the user's preference:
/// a NEW App instance starts in that mode instead of the Build default.
/// A legacy config without a mode selection keeps starting in Build.
#[tokio::test]
async fn toggled_mode_persists_and_restores_for_new_sessions() {
    use cosh::harness::Mode;

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    assert_eq!(app.state.mode, Mode::Build);

    // One Tab: Build → Ask, and the selection is persisted.
    app.process_key_event(mod_key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.state.mode, Mode::Ask);
    assert_eq!(
        crate::util::setup::Setup::load().persisted_mode(),
        Some(Mode::Ask),
        "the cycled-to mode must be saved as the preference"
    );

    // A fresh app (new session) restores the persisted mode.
    let restored = App::new("/tmp".to_string());
    assert_eq!(
        restored.state.mode, Mode::Ask,
        "a new session must start in the last used mode"
    );

    // A legacy setup.json without a mode selection starts in Build.
    let dir = std::env::var("COSH_CONFIG_DIR").expect("HOME tests isolate the config dir");
    std::fs::write(
        std::path::Path::new(&dir).join("setup.json"),
        r#"{"appearance": {}}"#,
    )
    .unwrap();
    let legacy = App::new("/tmp".to_string());
    assert_eq!(legacy.state.mode, Mode::Build);
}

/// Alt+← / Alt+→ (NextAgent/PrevAgent keymap bindings) must switch the
/// right-panel focus across agent queues — the ONLY way to reach a
/// queue whose windows are hidden for lack of space.
#[tokio::test]
async fn alt_arrows_switch_agent_queue_through_keymap() {
    use crate::routes::session::right_panel::types::{PanelFocus, RightPanelState};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    for agent in ["kilo", "opencode"] {
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} report\n"));
    }
    panel.panel_focus = Some(PanelFocus::Agent("opencode".to_string()));
    app.state.right_panel = panel;

    // Alt+← → previous queue (kilo).
    app.process_key_event(mod_key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("kilo".to_string())),
        "Alt+Left must cycle to the previous agent queue"
    );

    // Alt+→ → back to opencode.
    app.process_key_event(mod_key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("opencode".to_string()))
    );

    // Terminals that attach extra modifier bits to Alt+arrows must
    // still cycle (the exact-match keymap lookup misses those).
    let extra = KeyModifiers::ALT | KeyModifiers::SHIFT;
    app.process_key_event(mod_key(KeyCode::Left, extra))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("kilo".to_string())),
        "Alt+Left with stray modifiers must still cycle queues"
    );
}

/// Documents WHY Alt+arrows can never switch queues on terminals that
/// strip the ALT modifier from arrow encodings (`\x1b[D`, `\x1bOD`):
/// crossterm then reports a PLAIN Left/Right, which — while an agent
/// queue is focused — steps THAT queue's history instead. The bytes are
/// indistinguishable from a real plain arrow press.
#[tokio::test]
async fn plain_left_arrow_with_agent_focus_steps_queue_not_switch() {
    use crate::routes::session::right_panel::types::{PanelFocus, RightPanelState};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    for agent in ["kilo", "opencode"] {
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} v1\n"));
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} v2\n"));
    }
    panel.panel_focus = Some(PanelFocus::Agent("opencode".to_string()));
    app.state.right_panel = panel;

    // Plain Left (modifiers NONE): steps opencode's queue back — the
    // focused queue's history navigation, NOT a queue switch.
    app.process_key_event(mod_key(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("opencode".to_string())),
        "plain Left must not change the focused queue"
    );
    assert_eq!(
        app.state.right_panel.queue_nav_index("opencode"),
        Some(0),
        "plain Left must step the queue history"
    );
}

/// Shift+B / Shift+N switch agent queues while the right panel owns the
/// keyboard — the encoding survives terminals that strip Alt from
/// arrows (crossterm derives SHIFT from the uppercase letter).
#[tokio::test]
async fn shift_b_n_switch_agent_queue_when_panel_focused() {
    use crate::routes::session::right_panel::types::{PanelFocus, RightPanelState};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    for agent in ["kilo", "opencode"] {
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} report\n"));
    }
    panel.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
    app.state.right_panel = panel;

    // Shift+N → next queue (alphabetical wrap): kilo → opencode.
    app.process_key_event(mod_key(KeyCode::Char('N'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("opencode".to_string())),
        "Shift+N must switch to the next agent queue"
    );

    // Shift+B → back to kilo.
    app.process_key_event(mod_key(KeyCode::Char('B'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.state.right_panel.panel_focus,
        Some(PanelFocus::Agent("kilo".to_string())),
        "Shift+B must switch to the previous agent queue"
    );
}

/// Without a focused panel slot, Shift+B / Shift+N are ordinary
/// uppercase letters: they type into the prompt and never cycle queues.
#[tokio::test]
async fn shift_b_n_type_normally_without_panel_focus() {
    use crate::routes::session::right_panel::types::RightPanelState;

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    panel.start_pty("subagent: kilo".to_string(), None);
    panel.complete_last_pty("report\n".to_string());
    app.state.right_panel = panel;

    app.process_key_event(mod_key(KeyCode::Char('B'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.prompt_view.input, "B", "Shift+B must type when idle");
    app.process_key_event(mod_key(KeyCode::Char('N'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.prompt_view.input, "BN", "Shift+N must type when idle");
    assert_eq!(app.state.right_panel.panel_focus, None);
}

/// Ctrl+P toggles the right panel: hidden even with content and a wide
/// terminal, shown again on a second press, and a panel slot that held
/// keyboard focus is released when the panel is hidden.
#[tokio::test]
async fn ctrl_p_toggles_right_panel_visibility() {
    use crate::routes::session::right_panel::{should_show_right_panel, types::RightPanelState};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    panel.start_pty("echo hi".to_string(), None);
    panel.complete_last_pty("hi\n".to_string());
    panel.panel_focus = Some(crate::routes::session::right_panel::types::PanelFocus::Bash);
    app.state.right_panel = panel;
    assert!(should_show_right_panel(120, &app.state.right_panel));

    app.process_key_event(mod_key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.state.right_panel.user_hidden);
    assert!(
        !should_show_right_panel(120, &app.state.right_panel),
        "Ctrl+P must hide the panel even with content and a wide terminal"
    );
    assert_eq!(
        app.state.right_panel.panel_focus, None,
        "hiding the panel must release a focused slot"
    );

    app.process_key_event(mod_key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!app.state.right_panel.user_hidden);
    assert!(should_show_right_panel(120, &app.state.right_panel));
}

/// Regression: `is_in_right_panel` is also visibility-aware — after Ctrl+P
/// the panel's old screen band belongs to the chat, so scroll/click events
/// there must never be routed to the (hidden) panel.
#[tokio::test]
async fn hidden_right_panel_band_belongs_to_the_chat() {
    use crate::routes::session::right_panel::{RIGHT_PANEL_WIDTH, types::RightPanelState};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    panel.start_pty("echo hi".to_string(), None);
    panel.complete_last_pty("hi\n".to_string());
    app.state.right_panel = panel;

    let width = app.terminal_size().width;
    let band_x = width.saturating_sub(RIGHT_PANEL_WIDTH);
    if width >= 100 {
        assert!(
            app.is_in_right_panel(band_x),
            "visible panel must claim its band"
        );
    }

    app.process_key_event(mod_key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        !app.is_in_right_panel(band_x),
        "hidden panel must NOT claim its old band: scroll would be swallowed"
    );
}

/// Regression: ESC must interrupt the agent loop no matter what
/// incidental UI state is active. The gates below `process_key_event`'s
/// confirm-dialog sovereignty used to consume ESC before the
/// Interrupt/Cancel handlers ran, leaving the loop running. The guard at
/// the top of `process_key_event` now flags the shared stop signal first,
/// then ESC still performs its normal local action.
#[tokio::test]
async fn esc_while_working_sets_stop_signal_despite_prompt_selection() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;

    app.prompt_view.input = "abc".into();
    app.prompt_view.sel_start = Some(0);
    app.prompt_view.sel_end = Some(2);
    assert!(app.prompt_view.has_selection());

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC with a prompt selection must still interrupt the loop"
    );
    assert!(
        !app.prompt_view.has_selection(),
        "ESC keeps its local action: clearing the selection"
    );
}

/// Regression: ESC while Working must flag the stop signal even when the
/// left panel has focus (the old gate only unfocused the left panel).
#[tokio::test]
async fn esc_while_working_sets_stop_signal_despite_sidebar_focus() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.sidebar_focused = true;

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC with sidebar focus must still interrupt the loop"
    );
    assert!(!app.sidebar_focused, "ESC keeps unfocusing the sidebar");
}

/// Regression: ESC while Working must flag the stop signal even when the
/// slash menu is open (the old gate only cleared the prompt and closed
/// the menu).
#[tokio::test]
async fn esc_while_working_sets_stop_signal_despite_slash_menu() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.slash_menu.visible = true;
    app.prompt_view.input = "/pl".into();

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC with the slash menu open must still interrupt the loop"
    );
    assert!(!app.slash_menu.visible, "ESC keeps closing the slash menu");
}

/// Regression: ESC on the permission dialog used to only DENY the pending
/// permission — the loop kept running (the model would just try something
/// else). ESC must now interrupt the loop as well.
#[tokio::test]
async fn esc_while_working_sets_stop_signal_on_permission_dialog() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.permission_dialog.visible = true;

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC on the permission dialog must interrupt the loop"
    );
    assert!(
        !app.permission_dialog.visible,
        "ESC keeps denying/closing the permission dialog"
    );
}

/// ESC must NOT flag the stop signal when no agent loop is running —
/// closing overlays while idle must stay side-effect-free.
#[tokio::test]
async fn esc_while_idle_does_not_set_stop_signal() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.slash_menu.visible = true;

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC while idle must not touch the stop signal"
    );
    assert!(!app.slash_menu.visible, "ESC keeps closing the slash menu");
}

/// The quit confirm keeps modal sovereignty: while it is visible, ESC
/// drives the dialog and must NOT reach the interrupt guard, even if the
/// loop is still marked Working.
#[tokio::test]
async fn esc_with_confirm_dialog_visible_does_not_set_stop_signal() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.process_key_event(KeyEvent::new(K::Char('/'), KeyModifiers::NONE))
        .unwrap();
    // Blur the prompt: focused, Ctrl+C clears the draft instead of quitting.
    app.prompt_view.blur();
    app.process_key_event(KeyEvent::new(K::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.is_confirm_dialog_visible());

    app.process_key_event(KeyEvent::new(K::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "confirm-dialog ESC must not set the stop signal (sovereignty)"
    );
    assert!(!app.is_confirm_dialog_visible());
}
