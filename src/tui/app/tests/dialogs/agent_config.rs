//! The agent config box's PERSISTED model pick: a real user pick in the
//! `/agents` config box writes the per-agent record (the CLI-parity "remember
//! my last model") beside updating the pane cache. Defaults never write; a
//! persisted pick survives the session it was made in.

use super::super::{App, HOME_LOCK, isolate_home};
use crate::routes::session::agent_command_map::ConfigTarget;
use cosh_tools::subagent::events::{
    AgentConfigOption, AgentConfigValue, SubagentEvent, ToolCallStatus,
};

/// A model option as a harness advertises one: a select with two values, one
/// current.
fn model_option(current: &str) -> AgentConfigOption {
    AgentConfigOption {
        id: "model".to_string(),
        name: "Model".to_string(),
        description: None,
        category: Some("model".to_string()),
        values: vec![
            AgentConfigValue {
                value: "model-a".to_string(),
                name: "Model A".to_string(),
            },
            AgentConfigValue {
                value: "model-b".to_string(),
                name: "Model B".to_string(),
            },
        ],
        current: current.to_string(),
        boolean: None,
    }
}

fn split_app_with_model_option(current: &str) -> App {
    let mut app = App::new("/tmp".to_string());
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    // The config options ride the pane chrome event the same way a real
    // turn's report delivers them.
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option(current)],
            },
        })
        .ok();
    app.poll_events();
    app
}

/// Confirming a model pick in the config box writes the persisted per-agent
/// record: the same user action that updates the pane's cache also seeds the
/// global surface a FUTURE session's fresh pane reads. The pick rides
/// through the real confirm path (open the box, select a different value,
/// Enter) — not through a setter.
#[tokio::test]
async fn confirming_a_model_pick_persists_it_per_agent() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = split_app_with_model_option("model-a");

    // Precondition: nothing persisted yet.
    assert!(app.agents_model_picks.get("claude").is_none());

    app.open_agent_config_dialog(ConfigTarget::ById("model"));
    assert!(app.is_agent_config_dialog_visible(), "the box opened");
    // Move to the second row (Model B) and confirm — the pane's current is
    // `model-a`, so the pick is a REAL change, not a no-op re-apply.
    app.process_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyModifiers::NONE,
    ))
    .expect("down key handled");
    app.process_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ))
    .expect("enter key handled");

    // The persisted record holds the picked value.
    assert_eq!(
        app.agents_model_picks.get("claude"),
        Some("model-b"),
        "the confirm writes the per-agent persisted pick"
    );
    // The pane's cache is updated too (the turn path consumes it).
    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    let option = pane
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == "model"))
        .expect("the model option stays cached");
    assert_eq!(option.current, "model-b");
    assert!(
        pane.picked_config_ids.iter().any(|id| id == "model"),
        "the pick is recorded as USER-picked"
    );
    // The persisted record reached DISK too — the in-memory map alone would
    // pass a regression that never writes (e.g. a wrong directory composed
    // at `App::new`), and the whole point of the store is surviving the
    // session it was made in.
    let file = cosh::setup::data_dir_override()
        .join("agents")
        .join(cosh_tools::subagent::model_picks::MODEL_PICKS_FILE);
    let text = std::fs::read_to_string(&file).expect("the store file was written");
    let value: serde_json::Value = serde_json::from_str(&text).expect("whole json");
    assert_eq!(
        value,
        serde_json::json!({ "picks": { "claude": "model-b" } }),
        "the pick is on disk in the documented envelope shape"
    );
}

/// A completed call and a non-model pick never write the record: ONLY a real
/// user model pick touches the store (defaults and unrelated options stay
/// out — the file's whole point is a faithful "what the user chose").
#[tokio::test]
async fn non_model_picks_and_defaults_never_persist() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = split_app_with_model_option("model-a");

    // A tool-call completion event (the panes' ordinary traffic) writes
    // nothing.
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ToolCall {
                id: "c1".into(),
                title: "find_grep".into(),
                kind: cosh_tools::subagent::events::ToolKind::Search,
                status: ToolCallStatus::Completed,
                raw_input: None,
                tool: Some("find_grep".into()),
            },
        })
        .ok();
    app.poll_events();
    assert!(app.agents_model_picks.get("claude").is_none());

    // A reasoning pick does not touch the model record: only a model pick
    // writes the store.
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option("model-a"),
                    AgentConfigOption {
                        id: "reasoning".to_string(),
                        name: "Reasoning".to_string(),
                        description: None,
                        category: Some("thought_level".to_string()),
                        values: vec![AgentConfigValue {
                            value: "high".to_string(),
                            name: "High".to_string(),
                        }],
                        current: "high".to_string(),
                        boolean: None,
                    }],
            },
        })
        .ok();
    app.poll_events();
    app.open_agent_config_dialog(ConfigTarget::ById("reasoning"));
    // The reasoning box opened (one value is still selectable).
    assert!(app.is_agent_config_dialog_visible(), "the box opened");
    // The current is already selected; Enter re-applies it — a reasoning
    // pick, never a model write.
    app.process_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ))
    .expect("enter key handled");
    assert!(
        app.agents_model_picks.get("claude").is_none(),
        "a reasoning pick must not create a model record"
    );
    assert!(!app.is_agent_config_dialog_visible());
}

/// The /models dot's seed: the persisted pick rides OVER the harness default
/// (which a real `session/new` reports as a NON-empty current — guarding on
/// emptiness would skip the seed and the dot would lie), a persisted value
/// the option no longer offers leaves the report alone, and a SIBLING
/// pane's own pick outranks the global record — the same precedence
/// `pane_turn_picks` applies at dispatch.
#[tokio::test]
async fn config_options_report_seeds_the_persisted_dot() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = split_app_with_model_option("model-a");

    // No pick persisted: the report passes through untouched.
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option("model-a")],
            },
        })
        .ok();
    app.poll_events();
    let option = app.agents_split.as_ref().unwrap().panes[0]
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == "model"))
        .expect("cached");
    assert_eq!(option.current, "model-a", "no record, no seed");

    // A persisted pick rides over the harness default — even though the
    // default is a NON-empty current.
    app.agents_model_picks
        .set("claude", "model-b")
        .expect("persist");
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option("model-a")],
            },
        })
        .ok();
    app.poll_events();
    let option = app.agents_split.as_ref().unwrap().panes[0]
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == "model"))
        .expect("cached");
    assert_eq!(
        option.current, "model-b",
        "the persisted pick seeds over the harness default"
    );

    // A persisted value the option no longer offers: the harness default
    // stays (a stale dot would lie about the serving model).
    app.agents_model_picks
        .set("claude", "model-gone")
        .expect("persist");
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option("model-a")],
            },
        })
        .ok();
    app.poll_events();
    let option = app.agents_split.as_ref().unwrap().panes[0]
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == "model"))
        .expect("cached");
    assert_eq!(
        option.current, "model-a",
        "an unoffered persisted value never seeds"
    );

    // The pane's OWN pick outranks the global record: a sibling pane's
    // confirm rewrites the store, but THIS pane picked fresher and dispatch
    // serves ITS pick — the dot must show what the turn will really serve.
    app.agents_model_picks
        .set("claude", "model-a")
        .expect("persist");
    let pane = &mut app.agents_split.as_mut().unwrap().panes[0];
    pane.picked_config_ids.push("model".to_string());
    if let Some(o) = pane
        .config_options
        .as_mut()
        .and_then(|opts| opts.iter_mut().find(|o| o.id == "model"))
    {
        o.current = "model-b".to_string();
    }
    app.event_tx
        .send(cosh::harness::HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            event: SubagentEvent::ConfigOptions {
                options: vec![model_option("model-a")],
            },
        })
        .ok();
    app.poll_events();
    let option = app.agents_split.as_ref().unwrap().panes[0]
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == "model"))
        .expect("cached");
    assert_eq!(
        option.current, "model-b",
        "the pane's own pick outranks the persisted record"
    );
}
