use super::super::{App, HOME_LOCK, isolate_home};
use crate::routes::session::agents_split::AgentsSplitState;
use crate::types::AgentPaneRef;
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

/// An `/agents` split with ONE ACP pane (`opencode`) holding `prompts` user
/// turns in its transcript — the shape a live (or `session/load` replayed)
/// pane has.
fn app_with_acp_pane(prompts: &[&str]) -> App {
    let mut app = App::new("/tmp".to_string());
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "opencode".to_string(),
        session_id: "oc-agent-session".to_string(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("oc-agent-session".to_string());
    for prompt in prompts {
        split.echo_turn_input(0, prompt, true);
    }
    split.focused = Some(0);
    app.agents_split = Some(split);
    app
}

fn toast_message(app: &App) -> Option<(String, String)> {
    app.toast_state
        .current
        .clone()
        .map(|t| (t.title.unwrap_or_default(), t.message))
}

/// `/undo` with an ACP pane focused opens the SAME box the native path uses,
/// over the pane's transcript checkpoints — one row per user prompt, oldest
/// first, with the selection parked on the newest so Enter undoes the last
/// exchange.
#[tokio::test]
async fn acp_undo_lists_the_pane_checkpoints_newest_selected() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt", "third prompt"]);

    app.open_undo_dialog();
    let dialog = app.dialog.current().expect("the undo box is open");
    let DialogType::UndoList { versions, pane, .. } = &dialog.dialog_type else {
        panic!("expected the UndoList box, got {:?}", dialog.dialog_type);
    };
    let labels: Vec<&str> = versions.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(labels, vec!["v1", "v2", "v3"]);
    let previews: Vec<&str> = versions
        .iter()
        .map(|(_, preview)| preview.as_str())
        .collect();
    assert_eq!(
        previews,
        vec!["first prompt", "second prompt", "third prompt"]
    );
    assert_eq!(*pane, Some(0), "the list is the pane's, not the store's");
    assert_eq!(dialog.selected, 2, "Enter undoes the LAST exchange");
}

/// Enter on a checkpoint rewinds the pane exactly like Message Actions
/// Revert: the prompt and everything after it leave the transcript, the
/// prompt comes back for editing, and the agent session is abandoned (ACP
/// cannot truncate) so the next turn opens a fresh one.
#[tokio::test]
async fn acp_undo_rewinds_the_pane_to_the_picked_checkpoint() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt", "third prompt"]);

    app.open_undo_dialog();
    // Up from the newest lands on v2.
    assert!(app.handle_undo_dialog_key(KeyCode::Up));
    assert!(app.handle_undo_dialog_key(KeyCode::Enter));
    assert!(!app.is_undo_dialog_visible(), "the pick closes the box");

    let split = app.agents_split.as_ref().expect("the split survives");
    let prompts: Vec<&str> = split.panes[0]
        .activity
        .timeline
        .iter()
        .filter_map(|entry| match entry {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts,
        vec!["first prompt"],
        "the picked prompt and everything after it are gone"
    );
    assert!(
        split.panes[0].session_id.is_none(),
        "the agent session is abandoned (a fresh one opens on the next turn)"
    );
    assert!(
        split.panes[0].rewind_context.is_some(),
        "the kept prefix rides the next turn"
    );
    assert!(split.panes[0].rewound, "the replay fence is up");
    assert_eq!(app.prompt_view.input, "second prompt");
    assert!(
        toast_message(&app).is_none(),
        "a success toast would pollute the screen (the pane IS the receipt): {:?}",
        toast_message(&app)
    );
}

/// The rewind must STICK: no cold-restore replay may re-bind the abandoned
/// session and put the rolled-back turns back on screen (that is exactly what
/// made the undo look like a no-op), and the persisted reference drops the
/// session too, so a rebuild from metadata cannot resurrect it either.
#[tokio::test]
async fn a_rewound_pane_is_never_replayed_back() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt"]);
    // A real session owns the pane set, so the persistence path is exercised.
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rewind".into(), 0);
    app.state.current_session_id = Some(session_id.clone());
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![AgentPaneRef::Bound {
        agent: "opencode".into(),
        session_id: "oc-agent-session".into(),
        worktree: None,
    }]);

    app.open_undo_dialog();
    // v1 (one Up from the newest): everything the pane ever showed goes away.
    assert!(app.handle_undo_dialog_key(KeyCode::Up));
    assert!(app.handle_undo_dialog_key(KeyCode::Enter));

    let split = app.agents_split.as_ref().unwrap();
    assert!(split.panes[0].activity.timeline.is_empty());
    // A late binding (a queued fact, a rebuild) must not re-open the replay.
    app.agents_split.as_mut().unwrap().panes[0].session_id = Some("oc-agent-session".into());
    app.trigger_agents_pane_replays();
    assert!(
        !app.agents_split.as_ref().unwrap().panes[0].rehydrating,
        "the fence refuses the replay of the abandoned session"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .activity
            .timeline
            .is_empty(),
        "nothing came back into the rewound transcript"
    );
    // The persisted reference dropped the session as well.
    let persisted = app
        .state
        .current_session()
        .and_then(|s| s.agents_panes.clone())
        .unwrap();
    assert_eq!(persisted, vec![AgentPaneRef::Name("opencode".into())]);
}

/// The fence must come DOWN once a real session binds again — otherwise the
/// pane would be refused every future rehydration for the rest of the session
/// (the flag had no other clearing site). The test reproduces the terminal
/// `AgentsPaneDone` order: the handler seeds the live resume id FIRST and only
/// then calls the binding helper, so a clear behind the dedupe never runs.
#[tokio::test]
async fn a_real_binding_lifts_the_rewind_fence() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt"]);

    app.open_undo_dialog();
    assert!(app.handle_undo_dialog_key(KeyCode::Up));
    assert!(app.handle_undo_dialog_key(KeyCode::Enter));
    assert!(app.agents_split.as_ref().unwrap().panes[0].rewound);

    // The seeded id of the NEXT turn, already on the pane (as the Done
    // handler leaves it) before the binding helper runs.
    app.agents_split.as_mut().unwrap().panes[0].session_id = Some("oc-new-session".into());
    app.bind_agents_pane_session(0, "oc-new-session".into());

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(
        !pane.rewound,
        "a bound session lifts the fence (a stuck fence blocks every replay)"
    );
    assert_eq!(pane.session_id.as_deref(), Some("oc-new-session"));
}

/// The truncation must reach the SCREEN: the pane paints its transcript
/// from `activity.timeline`, so a removed answer cannot still be drawn.
#[tokio::test]
async fn acp_undo_removes_the_dropped_turn_from_the_painted_pane() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt"]);
    let split = app.agents_split.as_mut().unwrap();
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "second answer".into(),
        },
    );

    app.open_undo_dialog();
    assert!(app.handle_undo_dialog_key(KeyCode::Enter));

    let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::routes::session::agents_split::render(
        &mut buf,
        area,
        app.agents_split.as_mut().unwrap(),
        &theme,
        0.0,
    );
    let painted: String = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    assert!(
        !painted.contains("second answer"),
        "the reverted answer still paints:\n{painted}"
    );
}

/// The manager (cosh) pane keeps the NATIVE store-backed list: its
/// conversation is a real cosh session, so a focused manager pane must never
/// be routed to the ACP checkpoint path.
#[tokio::test]
async fn undo_on_a_manager_pane_uses_the_store_backed_list() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Name("cosh".to_string())]);
    split.echo_turn_input(0, "manager prompt", true);
    split.focused = Some(0);
    app.agents_split = Some(split);
    app.state.current_session_id = Some("main".to_string());

    app.open_undo_dialog();
    assert!(!app.is_undo_dialog_visible(), "no revert points, no box");
    assert!(
        toast_message(&app).is_some_and(|(_, message)| message == "No revert points yet."),
        "the native refusal, not the ACP one: {:?}",
        toast_message(&app)
    );
}

/// A pane mid-turn cannot rewind: the refusal is a toast, and nothing about
/// the transcript or the session binding moves.
#[tokio::test]
async fn acp_undo_refuses_while_the_pane_is_running() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_acp_pane(&["first prompt", "second prompt"]);
    app.agents_split.as_mut().unwrap().panes[0].running = true;

    app.open_undo_dialog();
    assert!(!app.is_undo_dialog_visible());
    assert!(
        toast_message(&app)
            .is_some_and(|(title, message)| title == "Undo" && message.contains("working")),
        "{:?}",
        toast_message(&app)
    );
    let split = app.agents_split.as_ref().unwrap();
    assert_eq!(split.panes[0].activity.timeline.len(), 2);
    assert_eq!(
        split.panes[0].session_id.as_deref(),
        Some("oc-agent-session")
    );
}

/// `/undo` must be DISCOVERABLE in an ACP pane's menu (its source is the
/// harness's own command list, which never carries cosh commands).
#[test]
fn the_acp_pane_menu_lists_undo() {
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Name("opencode".to_string())]);
    split.focused = Some(0);
    let (entries, _) = split.slash_source().expect("the pane serves a list");
    let undo = entries
        .iter()
        .find(|entry| entry.name == "undo")
        .expect("the ACP pane lists /undo");
    assert_eq!(undo.desc, crate::ui::slash_menu::undo_command_entry().desc);
}

/// A rewind abandons the agent session but NOT the worktree pin: a restart
/// rebuilds the pane from metadata, and a bare-name ref would move it back to
/// the session cwd (the isolated work silently going to the main tree).
#[tokio::test]
async fn a_rewound_pane_keeps_its_worktree_pin_on_disk() {
    use cosh::git::worktree::{AssignedBy, PaneWorktree};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let pin = PaneWorktree {
        path: "/tmp/cosh-worktrees/fix-undo".into(),
        branch: "fix-undo".to_string(),
        name: "fix-undo".to_string(),
        assigned_by: AssignedBy::User,
    };
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rewind".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![AgentPaneRef::Bound {
        agent: "opencode".into(),
        session_id: "oc-agent-session".into(),
        worktree: Some(pin.clone()),
    }]);
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "opencode".into(),
        session_id: "oc-agent-session".into(),
        worktree: Some(pin.clone()),
    }]);
    split.panes[0].session_id = Some("oc-agent-session".into());
    split.panes[0].worktree = Some(pin.clone());
    split.echo_turn_input(0, "first prompt", true);
    split.echo_turn_input(0, "second prompt", true);
    split.focused = Some(0);
    app.agents_split = Some(split);

    app.open_undo_dialog();
    assert!(app.handle_undo_dialog_key(KeyCode::Up));
    assert!(app.handle_undo_dialog_key(KeyCode::Enter));

    let persisted = app
        .state
        .current_session()
        .and_then(|s| s.agents_panes.clone())
        .expect("the pane set is still persisted");
    assert_eq!(
        persisted,
        vec![AgentPaneRef::Unbound {
            agent: "opencode".into(),
            worktree: pin.clone(),
        }],
        "the session is gone, the worktree pin is not"
    );
    // A rebuild from that ref restores the isolation and no session.
    let rebuilt = crate::routes::session::agents_split::AgentPane::from_ref(&persisted[0]);
    assert!(rebuilt.session_id.is_none());
    assert_eq!(
        rebuilt.worktree.map(|w| w.branch),
        Some("fix-undo".to_string())
    );
}
