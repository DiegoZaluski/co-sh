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
        cosh::setup::Setup::load().persisted_mode(),
        Some(Mode::Ask),
        "the cycled-to mode must be saved as the preference"
    );

    // A fresh app (new session) restores the persisted mode.
    let restored = App::new("/tmp".to_string());
    assert_eq!(
        restored.state.mode,
        Mode::Ask,
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

/// Ctrl+digit pane hotkeys must work THROUGH the /agents picker and the
/// slash menu: the modal boxes used to swallow `Ctrl+2` (their handlers
/// ignore control-modified chars, so the key was consumed with no visible
/// effect and — with the picker open — the user could not leave picker mode
/// via pane jumps either). The hotkeys are sovereign: with the split open,
/// `Ctrl+0` unfocuses (the MAIN agent) and `Ctrl+1..Ctrl+9` focus a pane,
/// while the picker's own marks stay untouched.
#[tokio::test]
async fn ctrl_digit_switches_pan_even_with_the_agents_picker_open() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["cosh".into(), "claude".into(), "kilo".into()],
    ));

    // The picker is open on top of the split (the re-open flow): digits
    // must still reach the split's panes.
    app.agents_picker.open();
    assert!(app.agents_picker.visible);

    // Ctrl+3 → the third pane (kilo, 1-based). The split FOCUSES the
    // pane; the mode never changes and the picker stays exactly as the
    // user left it.
    app.process_key_event(mod_key(KeyCode::Char('3'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(2)));
    assert!(app.agents_picker.visible, "the picker must stay open");
    assert!(
        app.agents_picker.filter.is_empty(),
        "the filter must not eat the digit"
    );
    assert_eq!(
        app.agents_picker.selected_names(),
        Vec::<String>::new(),
        "marks must not be touched by a pane hotkey"
    );
    assert!(
        app.agents_split.is_some(),
        "the split stays (mode never changes)"
    );

    // Ctrl+0 → UNFOCUSES every pane (the MAIN agent), still through the
    // open picker.
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert!(app.agents_picker.visible);

    // A digit the split does NOT own is consumed inertly: no pane moves,
    // the picker is undisturbed.
    app.process_key_event(mod_key(KeyCode::Char('7'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert!(app.agents_picker.visible);
}

/// The slash menu gate, same sovereignty contract: `Ctrl+2` while the menu
/// is filtered focuses the pane, never types into or closes the menu.
#[tokio::test]
async fn ctrl_digit_switches_pane_through_the_slash_menu() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["cosh".into(), "claude".into(), "kilo".into()],
    ));

    app.slash_menu.visible = true;
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
    assert!(app.slash_menu.visible, "the menu stays open, unfiltered");
    assert!(app.prompt_view.input.is_empty());
}

/// A PLAIN digit is a pane hotkey only while the prompt draft is empty —
/// once the user texts, digits belong to the draft again (the fallback
/// ownership rule the /agents picker regime relied on).
#[tokio::test]
async fn plain_digit_targets_the_pane_only_with_an_empty_draft() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["cosh".into(), "claude".into(), "kilo".into()],
    ));

    // Empty draft: plain '2' jumps to the second pane (1-based).
    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(1)));
    assert!(
        app.prompt_view.input.trim().is_empty(),
        "the pane jump never types"
    );

    // A second plain digit with the draft filled differentiates: it types.
    app.prompt_view.handle_paste("hel");
    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(1)));
    // The draft keeps both the paste and the typed digit.
    assert_eq!(app.prompt_view.input.trim(), "hel2");
}

/// Without the split (normal mode), digits are NEVER hotkeys: `Ctrl+2`
/// is not bound in the keymap and plain typing interjects into the draft
/// exactly as before.
#[tokio::test]
async fn digits_never_act_as_hotkeys_without_the_split() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    assert!(app.agents_split.is_none());

    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.agents_split.is_none());

    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    // A plain digit with an empty draft and no split simply types (the
    // digit lands in the prompt).
    assert!(!app.prompt_view.input.trim().is_empty());
}

/// Documents WHY plain arrow keys can never switch agent queues on
/// terminals that strip the ALT modifier from arrow encodings (`\x1b[D`,
/// `\x1bOD`): crossterm then reports a PLAIN Left/Right, which — while an
/// agent queue is focused — steps THAT queue's history instead. The bytes
/// are indistinguishable from a real plain arrow press. (Alt+← / Alt+→
/// queue-switching was removed: subagents are presented as ONE list, not
/// per-CLI groups — Shift+B / Shift+N remain the queue switchers.)
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

/// Issue 4 — Esc while a right-panel block is focused ONLY folds it back
/// into the list: the agent loop keeps running (stop signal untouched),
/// the focus is released and the key is CONSUMED. The old flow cleared
/// the focus but let the same Esc reach the loop-stop path — one press
/// both left the block and killed the loop.
#[tokio::test]
async fn esc_on_a_focused_panel_block_folds_back_without_stopping_the_loop() {
    use crate::routes::session::right_panel::types::{PanelFocus, RightPanelState};

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    for agent in ["kilo", "opencode"] {
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} report\n"));
    }
    // An EXPANDED subagent block is focused (the two-session default is
    // list mode — force the expanded single-window view first).
    panel.subagent_list_mode = Some(false);
    panel.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
    app.state.right_panel = panel;
    // The agent loop is mid-turn.
    app.state.status = crate::types::SessionStatus::Working;

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert_eq!(
        app.state.right_panel.panel_focus, None,
        "the Esc released the block focus"
    );
    assert_eq!(
        app.state.right_panel.subagent_list_mode,
        Some(true),
        "the expanded block folded back into the list"
    );
    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the loop must keep running — issue 4"
    );
    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Working,
        "the loop is untouched by the fold-back Esc"
    );
}

/// Issue 4 — the NEXT Esc (focus already released by the fold-back) runs
/// the normal stop semantics: with the agent loop still Working and no
/// modal box open, it interrupts the loop.
#[tokio::test]
async fn esc_after_the_fold_back_stops_the_loop_normally() {
    use crate::routes::session::right_panel::types::{PanelFocus, RightPanelState};

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    for agent in ["kilo", "opencode"] {
        panel.start_pty(format!("subagent: {agent}"), None);
        panel.complete_last_pty(format!("{agent} report\n"));
    }
    panel.subagent_list_mode = Some(false);
    panel.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
    app.state.right_panel = panel;
    app.state.status = crate::types::SessionStatus::Working;

    // First Esc: fold back, loop keeps running.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.stop_signal.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(app.state.right_panel.panel_focus, None);

    // Second Esc: focus is out of the block — the normal stop.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the SECOND Esc (focus clear) must interrupt the loop"
    );
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

/// Regression (PRIORITY RULE): with the slash menu open, ESC belongs to
/// the BOX first — it closes the menu and must NOT interrupt a running
/// agent loop. Only the SECOND Esc, with no box open anymore, flags the
/// stop signal.
#[tokio::test]
async fn esc_while_working_closes_slash_menu_first_stop_on_second_esc() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.slash_menu.visible = true;
    app.prompt_view.input = "/pl".into();

    // First Esc: closes the box, loop keeps running.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC with the slash menu open must close the box, not stop the loop"
    );
    assert!(!app.slash_menu.visible, "ESC keeps closing the slash menu");

    // Second Esc: no box open anymore — now the loop is interrupted.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the SECOND Esc (no box open) must interrupt the loop"
    );
}

/// Regression (PRIORITY RULE, the motivating bug): the model-picker box
/// opened via slash commands must own ESC while an agent loop runs — the
/// first Esc closes the box (restoring the original model) and the loop
/// keeps going; only the second Esc, with no box left on screen,
/// interrupts the loop.
#[tokio::test]
async fn esc_while_working_closes_model_dialog_first_stop_on_second_esc() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.open_model_dialog();
    assert!(app.is_model_dialog_visible());

    // First Esc: closes the box, loop keeps running.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC on the model dialog must close the box, not stop the loop"
    );
    assert!(
        !app.is_model_dialog_visible(),
        "ESC keeps closing the model dialog"
    );

    // Second Esc: no box open anymore — now the loop is interrupted.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the SECOND Esc (no box open) must interrupt the loop"
    );
}

/// Regression (PRIORITY RULE): the queue-choice box (shown when a message
/// is typed while the agent runs) also owns ESC — the first Esc dismisses
/// it (message stays in the input, nothing queued) and the loop keeps
/// running; the second Esc interrupts the loop.
#[tokio::test]
async fn esc_while_working_closes_queue_choice_first_stop_on_second_esc() {
    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.status = crate::types::SessionStatus::Working;
    app.queue_choice_dialog.open("queued text".to_string());
    assert!(app.queue_choice_dialog.visible);

    // First Esc: closes the box, loop keeps running.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "ESC on the queue-choice dialog must close the box, not stop the loop"
    );
    assert!(
        !app.queue_choice_dialog.visible,
        "ESC keeps dismissing the queue-choice dialog"
    );

    // Second Esc: no box open anymore — now the loop is interrupted.
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the SECOND Esc (no box open) must interrupt the loop"
    );
}

/// ESC on the permission dialog used to only DENY the pending
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

/// The function-key hints the header buttons advertise must actually WORK:
/// F1 maximizes Subagent, F2 Bash, F3 TODO, and F4 brings the mixed view
/// back — the same owner switch a click on the button performs, as a
/// keyboard shortcut beside the mouse path.
#[tokio::test]
async fn function_keys_f1_to_f4_switch_the_panel_owner() {
    use crate::routes::session::right_panel::types::{RightPanelState, TodoItem};

    let _home = HOME_LOCK.lock();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    let mut panel = RightPanelState::new();
    panel.set_todos(vec![TodoItem {
        status: "pending".to_string(),
        content: "task one".to_string(),
    }]);
    panel.start_pty("ls".to_string(), None);
    panel.complete_last_pty("out\n".to_string());
    panel.start_pty("subagent: kilo".to_string(), None);
    panel.complete_last_pty("report\n".to_string());
    app.state.right_panel = panel;

    app.process_key_event(mod_key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.maximized_section,
        Some(crate::routes::session::right_panel::types::SectionKind::Subagent),
        "F1 must maximize the Subagent section"
    );

    app.process_key_event(mod_key(KeyCode::F(2), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.maximized_section,
        Some(crate::routes::session::right_panel::types::SectionKind::Bash),
        "F2 must maximize the Bash section"
    );

    app.process_key_event(mod_key(KeyCode::F(3), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.maximized_section,
        Some(crate::routes::session::right_panel::types::SectionKind::Todo),
        "F3 must maximize the TODO section"
    );

    app.process_key_event(mod_key(KeyCode::F(4), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.maximized_section, None,
        "F4 must bring the mixed view back"
    );
}

/// REGRESSION: with the agent IDLE, Esc over an expanded subagent block
/// must FOLD it into the list — not fall through to the cancel path and
/// LEAVE the session. The fold used to run without consuming the key, so
/// it only "worked" while the model was running (the Working branch
/// swallowed the fall-through); idle, the same keypress reached the
/// leave-to-Home path and exited the session the user was looking at.
#[tokio::test]
async fn esc_over_expanded_subagent_folds_and_stays_in_session_when_idle() {
    use super::super::AppMode;
    use crate::routes::session::right_panel::types::{RightPanelState, SectionKind, SectionLayout};

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.set_test_size(140, 40);
    let mut panel = RightPanelState::new();
    panel.start_pty("subagent: kilo".into(), None);
    panel.complete_last_pty("kilo report\n".into());
    // Expanded view (a click/F-key opened it): the list is NOT active, so
    // the next Esc has a fold to perform.
    panel.subagent_list_mode = Some(false);
    // A Subagent section band the cursor sits inside — section layouts are
    // normally recorded by the renderer; a synthetic band keeps this test
    // key-only.
    panel.section_layouts.push(SectionLayout {
        kind: SectionKind::Subagent,
        top: 10,
        bottom: 30,
        content_top: 11,
    });
    app.state.right_panel = panel;
    // Cursor over the subagent section inside the right panel.
    app.last_mouse_x = 140 - crate::routes::session::right_panel::RIGHT_PANEL_WIDTH;
    app.last_mouse_y = 12;

    // First Esc: folds the expanded subagent into the list AND stays in
    // the session (the agent is idle — this used to exit instead).
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state.right_panel.subagent_list_mode,
        Some(true),
        "Esc over the expanded subagent block must fold it into the list"
    );
    assert!(
        matches!(app.mode(), AppMode::Session),
        "the folding Esc must NOT leave the session while the agent is idle"
    );
    assert!(
        app.state.right_panel.panel_focus.is_none(),
        "the folding Esc must also release the panel slot focus"
    );

    // Second Esc: the list is now active (nothing to fold), so Esc keeps
    // its normal meaning — leave the session (agent still idle).
    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        !matches!(app.mode(), AppMode::Session),
        "with the list active, Esc falls through to the leave-to-Home path"
    );
}

/// `Ctrl+0` is the MAIN-agent hotkey: it unfocuses every /agents pane, so
/// the shared prompt targets the plain normal-mode loop (unchanged by the
/// split). This pins the CSI-u delivery shape
/// (`Char('0')` + CONTROL — what kitty-protocol terminals send once the
/// app negotiates `REPORT_ALL_KEYS_AS_ESCAPE_CODES`): the focus MUST
/// clear even with a draft typed, and a pane hotkey must re-focus after.
#[tokio::test]
async fn ctrl_zero_unfocuses_every_pane_to_the_main_agent() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    // Focus the second pane first (1-based), with a draft typed: the
    // Ctrl shape never consults the draft.
    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(1)));
    app.prompt_view.handle_paste("do the thing");

    // Ctrl+0 clears the focus — the draft is untouched (it now belongs to
    // the main agent's next turn).
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert_eq!(app.prompt_view.input.trim(), "do the thing");

    // A pane hotkey re-focuses from the unfocused state.
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
}

/// Legacy terminals have no byte encoding for `Ctrl+0` — it arrives as a
/// plain `0`. The plain-digit fallback must still unfocus while the draft
/// is empty (picker closed, no modal): that is the ONLY shape those
/// terminals can deliver.
#[tokio::test]
async fn plain_zero_unfocuses_with_an_empty_draft() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    assert!(!app.agents_picker.visible);

    // Plain '0' with an empty draft unfocuses (fresh splits start on the
    // first pane).
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert!(
        app.prompt_view.input.trim().is_empty(),
        "the unfocus never types"
    );

    // Plain '1' re-focuses the first pane (1-based).
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
}

/// A plain `0` with a non-empty draft is TYPING, not a hotkey: the focus
/// must not move and the digit lands in the prompt.
#[tokio::test]
async fn plain_zero_types_when_the_draft_is_not_empty() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    app.prompt_view.handle_paste("hel");
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
    assert_eq!(app.prompt_view.input.trim(), "hel0");
}

/// `Ctrl+0` stays sovereign through the open /agents picker (like every
/// other pane hotkey): it unfocuses without touching the picker's marks.
#[tokio::test]
async fn ctrl_zero_unfocuses_through_the_open_picker() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["cosh".into(), "claude".into(), "kilo".into()],
    ));
    app.agents_picker.open();
    assert!(app.agents_picker.visible);

    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert!(app.agents_picker.visible, "the picker must stay open");
    assert!(
        app.agents_picker.filter.is_empty(),
        "the filter must not eat the digit"
    );
}

/// ESC with NO pane focused PARKS the split — even while the main loop is
/// Working (issue 3): leaving to watch/delegate in the normal view must
/// never interrupt the main loop's turn, and the main loop is interrupted
/// from the normal view exactly as before (its stop signal stays UNTOUCHED
/// by the split's Esc).
#[tokio::test]
async fn esc_with_main_focused_parks_and_never_stops_the_loop() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    app.state.status = crate::types::SessionStatus::Working;

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "the split's Esc must NEVER flag the main loop's stop — issue 3"
    );
    assert!(
        app.agents_split.is_none(),
        "Esc parks the split regardless of focus or the main loop's state"
    );
    assert!(
        app.parked_agents_split.is_some(),
        "the pane set parks (nothing is destroyed)"
    );
    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Working,
        "the main loop keeps running behind the park"
    );
}

/// ESC with NO pane focused and everything idle parks the split, exactly
/// like the idle focused-pane path.
#[tokio::test]
async fn esc_with_main_focused_and_idle_parks_the_split() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.state.status, crate::types::SessionStatus::Idle);

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.agents_split.is_none(),
        "idle ESC with the main agent focused leaves the mode"
    );
    assert!(
        app.parked_agents_split.is_some(),
        "the pane set parks (nothing is destroyed)"
    );
}

/// `Alt+0..Alt+9` are exact aliases of the `Ctrl` pane hotkeys — the
/// reliable form on terminals where Ctrl+digits have no byte encoding or
/// are eaten by the emulator. `Alt+0` unfocuses (the MAIN agent),
/// `Alt+1..` focus the numbered pane (1-based), never consulting the
/// draft, the picker or the modals.
#[tokio::test]
async fn alt_digits_mirror_the_ctrl_pane_hotkeys() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    // Alt+2 focuses the second pane even with a draft typed.
    app.prompt_view.handle_paste("do the thing");
    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(1)));
    assert_eq!(app.prompt_view.input.trim(), "do the thing");

    // Alt+0 unfocuses to the main agent; the draft is untouched.
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    assert_eq!(app.prompt_view.input.trim(), "do the thing");

    // Alt+1 re-focuses the first pane from the unfocused state.
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
}

/// The Alt hotkeys stay sovereign through the open /agents picker: the
/// picker's marks and filter are untouched.
#[tokio::test]
async fn alt_digit_focuses_through_the_open_picker() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["cosh".into(), "claude".into(), "kilo".into()],
    ));
    app.agents_picker.open();
    assert!(app.agents_picker.visible);

    app.process_key_event(mod_key(KeyCode::Char('3'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(2)));
    assert!(app.agents_picker.visible, "the picker must stay open");
    assert!(
        app.agents_picker.filter.is_empty(),
        "the filter must not eat the digit"
    );
    assert_eq!(
        app.agents_picker.selected_names(),
        Vec::<String>::new(),
        "marks must not be touched by a pane hotkey"
    );
}

/// `Ctrl+Alt+digit` is the AltGr character signature on Windows consoles,
/// never a hotkey: neither the Ctrl shape (ALT excluded) nor the Alt
/// shape (CONTROL excluded) may fire — the focus stays put.
#[tokio::test]
async fn ctrl_alt_digit_is_not_a_pane_hotkey() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    app.process_key_event(mod_key(
        KeyCode::Char('2'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        app.agents_split.as_ref().map(|s| s.focused),
        Some(Some(0)),
        "the AltGr shape must leave the focus alone"
    );
    app.process_key_event(mod_key(
        KeyCode::Char('0'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        app.agents_split.as_ref().map(|s| s.focused),
        Some(Some(0)),
        "the AltGr shape must never unfocus either"
    );
}

/// The Alt alias is EXACT: `Alt+Shift+digit` (what shift-reporting
/// terminals emit) must not jump panes — only a bare Alt+digit is a
/// hotkey, so the combination falls through to normal handling with the
/// focus untouched.
#[tokio::test]
async fn alt_shift_digit_is_not_a_pane_hotkey() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    app.process_key_event(mod_key(
        KeyCode::Char('2'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.agents_split.as_ref().map(|s| s.focused),
        Some(Some(0)),
        "Alt+Shift+digit must leave the focus alone"
    );
}

/// Issue 3 — Enter with a SPLIT PANE focused dispatches to that pane even
/// while the MAIN agent is Working: delegating to an idle pane mid-run
/// must work, and the main loop keeps streaming untouched. The pane's
/// dispatch path is exactly the normal one (the running flag flips, the
/// echo lands).
#[tokio::test]
async fn enter_dispatches_to_a_focused_pane_while_the_main_loop_is_working() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(Some(0)));
    // The MAIN loop is mid-turn.
    app.state.status = crate::types::SessionStatus::Working;
    app.prompt_view.input = "help the agent".into();

    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.agents_split.as_ref().unwrap().panes[0].running,
        "the focused pane's turn started while the main loop runs"
    );
    assert!(
        !app.queue_choice_dialog.visible,
        "a pane-targeted Enter must NOT open the queue-choice dialog"
    );
    assert!(
        app.prompt_view.input.is_empty(),
        "the dispatched message left the prompt"
    );
    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Working,
        "the main loop's status is untouched"
    );
}

/// Issue 3 — Enter with NO pane focused (the MAIN agent target) while
/// Working keeps the EXISTING queue behavior: the queue-choice dialog
/// opens, nothing is dispatched, no turn starts.
#[tokio::test]
async fn enter_with_main_target_while_working_still_queues() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().map(|s| s.focused), Some(None));
    app.state.status = crate::types::SessionStatus::Working;
    app.prompt_view.input = "follow up".into();

    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.queue_choice_dialog.visible,
        "the main-target Enter keeps its queue-choice behavior"
    );
    assert!(
        !app.agents_split.as_ref().unwrap().panes[0].running
            && !app.agents_split.as_ref().unwrap().panes[1].running,
        "no pane received the message"
    );
    assert!(
        !app.prompt_view.input.is_empty(),
        "the draft stays until the queue choice resolves"
    );
}

/// Issue 3 — the `/agents` command opens the split (or re-attaches the
/// parked one) EVEN WHILE the main loop streams: the old Working refusal
/// is gone. A parked set re-attaches verbatim; a fresh entry opens the
/// picker pre-marked. The main loop's state is untouched either way.
#[tokio::test]
async fn agents_command_reattaches_the_parked_split_while_working() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    // A previously-parked split: the pane set persists in the metadata.
    app.state.current_session_mut().unwrap().agents_panes =
        Some(vec![crate::types::AgentPaneRef::Name("kilo".into())]);
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["kilo".into()],
    ));
    app.park_agents_split();
    assert!(app.parked_agents_split.is_some() && app.agents_split.is_none());
    // The MAIN loop is mid-turn.
    app.state.status = crate::types::SessionStatus::Working;

    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "agents".into(),
        desc: String::new(),
    });

    assert!(
        app.agents_split.is_some(),
        "the parked split re-attaches while the main loop runs"
    );
    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Working,
        "the main loop keeps running behind the re-attach"
    );
}

/// Issue 5 — Ctrl+X with a split pane focused force-closes THAT pane's
/// stuck turn: the pane goes idle (it accepts prompts again), the stop
/// signal is stored (a live turn winds down through its normal Done path)
/// and the timeline seals with the `[turn …]` cancellation marker. This is
/// the only keyboard path out of a silently stalled pane — split Esc is
/// park-only (issue 3), so a `running` pane with no activity used to be
/// unrecoverable.
#[tokio::test]
async fn ctrl_x_force_closes_a_stuck_focused_pane_turn() {
    use crate::routes::session::right_panel::types::SubagentTimelineEntry;

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .unwrap();
    // The focused pane's turn starts (the normal dispatch path), then
    // stalls: running with no further events.
    app.prompt_view.input = "long task".into();
    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.agents_split.as_ref().unwrap().panes[0].running);

    app.process_key_event(mod_key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();

    let split = app.agents_split.as_ref().unwrap();
    assert!(
        !split.panes[0].running,
        "the stuck pane's turn is force-closed"
    );
    assert!(
        split.panes[0].stop_signal.is_none(),
        "the pane accepts a new dispatch (signal cleared)"
    );
    assert!(
        !split.panes[1].running,
        "the sibling pane is untouched"
    );
    assert!(
        split.panes[0].activity.timeline.iter().any(|e| matches!(
            e,
            SubagentTimelineEntry::Message { text }
                if text.contains("[turn cancelled] interrupted by the user (Ctrl+X)")
        )),
        "the timeline seals with the cancellation marker"
    );
    // The MAIN agent's stop signal is NEVER touched (only the pane's).
    assert!(!app.stop_signal.load(std::sync::atomic::Ordering::Relaxed));
}

/// Issue 5 — Ctrl+X only interrupts when a pane turn is actually stuck:
/// an idle focused pane is a no-op (nothing to close, no marker appended),
/// and with NO pane focused (the MAIN agent) the key is inert — the main
/// loop is interrupted by plain Esc in the normal view, never by Ctrl+X.
#[tokio::test]
async fn ctrl_x_is_inert_without_a_running_focused_pane() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec!["claude".into(), "kilo".into()],
    ));

    // Focused but IDLE pane: no-op.
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .unwrap();
    app.process_key_event(mod_key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!app.agents_split.as_ref().unwrap().panes[0].running);
    assert!(
        app.agents_split
            .as_ref()
            .unwrap()
            .panes[0]
            .activity
            .timeline
            .iter()
            .all(|e| !matches!(
                e,
                crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text }
                    if text.contains("[turn")
            )),
        "no cancellation marker on an idle pane"
    );

    // Unfocused (MAIN agent target) with a stuck-looking main loop: Ctrl+X
    // does NOT touch the main loop's stop signal.
    app.process_key_event(mod_key(KeyCode::Char('0'), KeyModifiers::CONTROL))
        .unwrap();
    app.state.status = crate::types::SessionStatus::Working;
    app.process_key_event(mod_key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "Ctrl+X never interrupts the MAIN agent — plain Esc does"
    );
    assert!(
        app.agents_split.is_some(),
        "the split stays open (Ctrl+X is not a park/leave key)"
    );
}

/// Backspace on a focused pane (prompt blurred, the click contract) HIDES
/// it — the pane leaves the visible set but is NOT deleted; Ctrl+Z restores
/// the most recently hidden pane at its old slot and takes the focus. The
/// prompt keeps its Backspace/Ctrl+Z whenever it holds the focus.
#[tokio::test]
async fn backspace_hides_the_focused_pane_and_ctrl_z_restores_it() {
    use crate::ui::agents_picker::AgentPick;

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.open_agents_split(vec![
        AgentPick {
            name: "claude".into(),
            instances: 1,
        },
        AgentPick {
            name: "kilo".into(),
            instances: 1,
        },
        AgentPick {
            name: "cosh".into(),
            instances: 1,
        },
    ]);
    // Selecting a pane blurs the prompt (a pane click does this); simulate it.
    app.prompt_view.blur();
    app.process_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.agents_split.as_ref().and_then(|s| s.focused), Some(1));

    // Backspace hides the focused pane.
    app.process_key_event(mod_key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(
            session.agents_panes.as_ref().unwrap().len(),
            2,
            "the pane is hidden, not deleted"
        );
        assert_eq!(session.agents_hidden.len(), 1);
        assert_eq!(session.agents_hidden[0].pane.agent(), "kilo");
        assert_eq!(session.agents_hidden[0].index, 1);
    }
    assert_eq!(app.agents_split.as_ref().unwrap().panes.len(), 2);

    // Ctrl+Z restores it at its old index and focuses it.
    app.process_key_event(mod_key(KeyCode::Char('z'), KeyModifiers::CONTROL))
        .unwrap();
    let session = app.state.current_session().unwrap();
    assert_eq!(session.agents_panes.as_ref().unwrap().len(), 3);
    assert!(session.agents_hidden.is_empty());
    let split = app.agents_split.as_ref().unwrap();
    assert_eq!(split.panes[1].agent, "kilo", "restored at its old slot");
    assert_eq!(split.focused, Some(1), "the restored pane takes the focus");
}

/// The prompt keeps its own Backspace/Ctrl+Z: with the prompt FOCUSED the
/// keys edit the draft and never hide/restore panes.
#[tokio::test]
async fn backspace_and_ctrl_z_edit_the_prompt_when_it_is_focused() {
    use crate::ui::agents_picker::AgentPick;

    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.open_agents_split(vec![
        AgentPick {
            name: "claude".into(),
            instances: 1,
        },
        AgentPick {
            name: "kilo".into(),
            instances: 1,
        },
    ]);
    // The split focuses the prompt; seed a draft.
    app.prompt_view.focus();
    app.prompt_view.handle_paste("a");
    assert_eq!(app.prompt_view.input, "a");

    // Backspace edits the draft — no pane is hidden.
    app.process_key_event(mod_key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert!(app.prompt_view.input.is_empty());
    assert!(
        app.state
            .current_session()
            .unwrap()
            .agents_hidden
            .is_empty()
    );

    // Ctrl+Z is the prompt undo (restores the deleted char), not a pane
    // restore: the visible set is unchanged.
    app.process_key_event(mod_key(KeyCode::Char('z'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.prompt_view.input, "a");
    assert_eq!(
        app.state
            .current_session()
            .unwrap()
            .agents_panes
            .as_ref()
            .unwrap()
            .len(),
        2
    );
}
