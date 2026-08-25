use super::{App, HOME_LOCK, mod_key};
use crossterm::event::{KeyCode, KeyModifiers};

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
