use super::{App, HOME_LOCK, isolate_home};
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

#[tokio::test]
async fn summarizer_selection_and_cancel_never_change_the_agent_model() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".into());
    app.llm_config.provider = "agent-provider".into();
    app.llm_config.model = Some("agent-model".into());
    app.llm_config.reasoning = Some("high".into());
    let original = serde_json::to_value(&app.setup.model).unwrap();
    app.summarization_model_edit = Some(None);
    app.dialog.show(DialogType::ModelList {
        models: vec![],
        current: String::new(),
        filter: String::new(),
        loading: false,
    });
    app.event_tx
        .send(cosh::harness::HarnessEvent::ModelsLoaded {
            models: vec![cosh::ModelEntry {
                provider: "summary-provider".into(),
                model: "summary-model".into(),
            }],
            current: "auto".into(),
        })
        .unwrap();
    app.poll_events();
    if let DialogType::ModelList { models, .. } = &app.dialog.current().unwrap().dialog_type {
        assert_eq!(models.len(), 1);
        assert!(models.iter().all(|entry| entry.model != "auto"));
    } else {
        panic!("expected summary model picker");
    }
    app.confirm_model_entry("auto", "");
    assert!(app.setup.routing.summarization_models.is_empty());
    app.confirm_model_entry("summary-model", "summary-provider");
    assert!(!app.dialog.visible());
    assert_eq!(
        app.setup.routing.summarization_models[0].model,
        "summary-model"
    );
    app.summarization_model_edit = Some(Some(0));
    app.dialog.show(DialogType::ModelList {
        models: vec![],
        current: String::new(),
        filter: String::new(),
        loading: false,
    });
    app.handle_model_dialog_key(KeyCode::Esc);
    assert!(app.summarization_model_edit.is_none());
    assert_eq!(app.llm_config.provider, "agent-provider");
    assert_eq!(app.llm_config.model.as_deref(), Some("agent-model"));
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));
    assert_eq!(serde_json::to_value(&app.setup.model).unwrap(), original);
    assert_eq!(
        cosh::setup::Setup::load().routing.summarization_models[0].model,
        "summary-model"
    );
}

/// Opening the model dialog with an empty cache must flag the dialog as
/// loading: the background API fetch can take a few seconds and the list
/// area shows the spinner (plus the seeded `auto` entry) until
/// `ModelsLoaded` lands.
#[tokio::test]
async fn model_dialog_opens_in_loading_state_with_empty_cache() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".into());
    app.open_model_dialog();
    match &app.dialog.current().unwrap().dialog_type {
        DialogType::ModelList {
            models, loading, ..
        } => {
            assert!(
                loading,
                "empty cache should open the dialog in loading state"
            );
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].model, "auto");
        }
        _ => panic!("expected model picker"),
    }
}

/// `ModelsLoaded` must repopulate the list and stop the spinner even when
/// the reasoning sub-dialog sits on top of the ModelList: the handler looks
/// past the top of the dialog stack instead of only checking the current
/// dialog.
#[tokio::test]
async fn models_loaded_updates_model_list_beneath_reasoning_dialog() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".into());
    app.open_model_dialog();
    // Simulate the user pressing Enter on the seeded `auto` entry while the
    // fetch is still in flight, pushing the reasoning sub-dialog on top.
    app.dialog.show(DialogType::ReasoningList {
        model: "auto".to_string(),
        provider: String::new(),
        levels: vec![
            "default".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ],
        current: String::new(),
    });
    app.event_tx
        .send(cosh::harness::HarnessEvent::ModelsLoaded {
            models: vec![cosh::ModelEntry {
                provider: "summary-provider".into(),
                model: "summary-model".into(),
            }],
            current: String::new(),
        })
        .unwrap();
    app.poll_events();
    assert!(
        matches!(
            app.dialog.current().unwrap().dialog_type,
            DialogType::ReasoningList { .. }
        ),
        "reasoning sub-dialog should still be on top"
    );
    let model_list = app
        .dialog
        .stack
        .iter()
        .find(|d| matches!(d.dialog_type, DialogType::ModelList { .. }))
        .expect("model list still in the dialog stack");
    match &model_list.dialog_type {
        DialogType::ModelList {
            models, loading, ..
        } => {
            assert!(!loading, "ModelsLoaded must stop the loading spinner");
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].model, "auto");
            assert_eq!(models[1].model, "summary-model");
        }
        _ => panic!("expected model picker beneath the reasoning dialog"),
    }
}

/// A fetch that finds nothing must still clear the loading flag so the
/// dialog renders the plain (auto-only) list instead of a spinner forever.
#[tokio::test]
async fn models_loaded_with_no_models_stops_the_spinner() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".into());
    app.open_model_dialog();
    app.event_tx
        .send(cosh::harness::HarnessEvent::ModelsLoaded {
            models: vec![],
            current: String::new(),
        })
        .unwrap();
    app.poll_events();
    match &app.dialog.current().unwrap().dialog_type {
        DialogType::ModelList {
            models, loading, ..
        } => {
            assert!(!loading, "an empty fetch must stop the spinner");
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].model, "auto");
        }
        _ => panic!("expected model picker"),
    }
}

#[tokio::test]
async fn manual_compaction_resolves_only_explicit_summary_models_or_the_auto_chain() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".into());
    app.llm_config.model = Some("explicit-agent".into());
    assert!(app.manual_summarization_models().is_empty());
    app.router_view.fallbacks = vec![cosh::setup::FallbackEntry {
        provider: "local".into(),
        model: "auto-choice".into(),
    }];
    app.llm_config.model = Some("auto".into());
    assert_eq!(
        app.manual_summarization_models(),
        vec![("local".into(), "auto-choice".into())]
    );
    app.setup.routing.summarization_models = vec![cosh::setup::FallbackEntry {
        provider: "chosen".into(),
        model: "summary".into(),
    }];
    assert_eq!(
        app.manual_summarization_models(),
        vec![("chosen".into(), "summary".into())]
    );
    app.llm_config.model = Some("explicit-agent".into());
    assert_eq!(
        app.manual_summarization_models(),
        vec![("chosen".into(), "summary".into())]
    );
}

fn new_cmd() -> crate::ui::slash_menu::SlashCommand {
    crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    }
}

/// The globally persisted model is a single overwritten slot loaded at
/// startup: it starts the app on that model, brand-new sessions inherit and
/// record it, returning to a session restores its own recorded selection,
/// and a later selection overwrites the slot in memory, on the current
/// session, and on disk (no history is kept).
#[tokio::test]
async fn global_model_loads_applies_and_overwrites() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // isolate_home wipes the scratch dir first, so write AFTER it runs.
    // Seed via the SAME resolution path the app uses: COSH_CONFIG_DIR now
    // points at the scratch config dir (pre-fix, this test wrote to
    // `$TEMP/.../.config/cosh` while the app read `%APPDATA%\cosh` — the
    // developer's REAL setup — so the assertion compared against live user
    // state and failed wherever it differed).
    let cfg = {
        let dir = std::env::var("COSH_CONFIG_DIR").expect("isolate_home sets COSH_CONFIG_DIR");
        std::path::PathBuf::from(dir)
    };
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("setup.json"),
        r#"{"model": {"provider": "nvidia", "model": "deepseek-ai/deepseek-v4-pro", "reasoning": "high"}}"#,
    )
    .unwrap();
    // Startup only restores a CONCRETE selection whose provider still has a
    // usable credential (the keyless restore would show a footer provider
    // that cannot authenticate). Seed the env fallback so the check passes
    // deterministically — on machines without the key in the OS keyring the
    // test must not depend on the developer's live credential state.
    // SAFETY: HOME_LOCK serializes env mutation across these tests.
    unsafe { std::env::set_var("NVIDIA_API_KEY", "test-key") };

    let mut app = App::new("/tmp".to_string());
    assert_eq!(
        app.llm_config.model.as_deref(),
        Some("deepseek-ai/deepseek-v4-pro"),
        "startup restores the globally persisted model"
    );
    assert_eq!(app.llm_config.provider, "nvidia");
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));

    // A brand-new session inherits AND records the global model.
    app.run_slash_command(&new_cmd());
    let session = app.state.current_session().unwrap();
    assert_eq!(session.provider.as_deref(), Some("nvidia"));
    assert_eq!(
        session.model.as_deref(),
        Some("deepseek-ai/deepseek-v4-pro")
    );
    assert_eq!(session.reasoning.as_deref(), Some("high"));

    // Returning to a session restores ITS recorded model, replacing whatever
    // config is currently active.
    let session = app.state.current_session_mut().unwrap();
    session.provider = Some("groq".to_string());
    session.model = Some("openai/gpt-oss-120b".to_string());
    session.reasoning = None;
    app.llm_config.model = Some("stale-model".to_string());
    app.llm_config.provider = "stale-provider".to_string();
    app.llm_config.reasoning = Some("low".to_string());
    app.restore_current_session_model();
    assert_eq!(app.llm_config.model.as_deref(), Some("openai/gpt-oss-120b"));
    assert_eq!(app.llm_config.provider, "groq");
    assert_eq!(app.llm_config.reasoning, None);

    // A later selection OVERWRITES the single slot everywhere.
    app.commit_model_selection("grok-4", "xai", Some("medium"));
    assert_eq!(app.llm_config.model.as_deref(), Some("grok-4"));
    assert_eq!(app.llm_config.provider, "xai");
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("medium"));
    let session = app.state.current_session().unwrap();
    assert_eq!(session.model.as_deref(), Some("grok-4"));
    assert_eq!(session.provider.as_deref(), Some("xai"));
    assert_eq!(session.reasoning.as_deref(), Some("medium"));

    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.join("setup.json")).unwrap()).unwrap();
    assert_eq!(
        on_disk["model"]["model"], "grok-4",
        "disk keeps the newest selection"
    );
    assert_eq!(on_disk["model"]["provider"], "xai");
    assert_eq!(on_disk["model"]["reasoning"], "medium");
    // SAFETY: restore the machine's state for the other tests (HOME_LOCK is
    // still held).
    unsafe { std::env::remove_var("NVIDIA_API_KEY") };
}

/// An `auto` selection is recorded as a model with an empty provider — the
/// session keeps the model but has no provider to pin, so the fallback chain
/// stays the source of truth.
#[tokio::test]
async fn auto_selection_records_model_without_provider() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());

    app.commit_model_selection("auto", "", Some("high"));
    assert_eq!(app.llm_config.model.as_deref(), Some("auto"));
    assert!(app.llm_config.provider.is_empty());

    let session = app.state.current_session().unwrap();
    assert_eq!(session.model.as_deref(), Some("auto"));
    assert_eq!(session.provider, None, "no provider to pin for auto");
    assert_eq!(session.reasoning.as_deref(), Some("high"));

    assert_eq!(app.setup.persisted_model(), Some("auto"));
    assert!(app.setup.model.provider.is_empty());
}

/// Switching back to a session with a recorded `auto` selection restores
/// `auto` AND clears any leftover provider from the previously active
/// config — a stale provider would otherwise produce a mixed
/// `model=auto` + `provider=nvidia` state.
#[tokio::test]
async fn auto_session_restore_clears_leftover_provider() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());
    {
        let session = app.state.current_session_mut().unwrap();
        session.model = Some("auto".to_string());
        session.provider = None;
        session.reasoning = Some("high".to_string());
    }
    app.llm_config.model = Some("other-model".to_string());
    app.llm_config.provider = "nvidia".to_string();
    app.llm_config.reasoning = Some("low".to_string());

    app.restore_current_session_model();

    assert_eq!(app.llm_config.model.as_deref(), Some("auto"));
    assert!(
        app.llm_config.provider.is_empty(),
        "auto must not pin a provider"
    );
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));
}

/// A session that never recorded a selection — the pre-feature legacy shape —
/// leaves the active config untouched on restore, so switching sessions no
/// longer mutates the model in cases where it never did before.
#[tokio::test]
async fn unrecorded_session_keeps_active_config_on_restore() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());
    // An active config chosen elsewhere / earlier.
    app.llm_config.model = Some("working-model".to_string());
    app.llm_config.provider = "groq".to_string();
    app.llm_config.reasoning = Some("high".to_string());
    {
        let session = app.state.current_session_mut().unwrap();
        session.provider = None;
        session.model = None;
        session.reasoning = None;
    }

    app.restore_current_session_model();

    assert_eq!(app.llm_config.model.as_deref(), Some("working-model"));
    assert_eq!(app.llm_config.provider, "groq");
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));
}

/// The model dialog's Esc path reverts the live config and — critically —
/// persists NOTHING: neither the global slot nor the current session. Only
/// an explicit commit (Enter on the model list or the reasoning sub-dialog)
/// writes.
#[tokio::test]
async fn model_dialog_cancel_and_reasoning_esc_do_not_persist() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());

    // Esc in the model list: config reverts to what was captured on open.
    app.llm_config.model = Some("newer-model".to_string());
    app.llm_config.reasoning = Some("high".to_string());
    app.model_dialog_original = Some("gpt-oss-120b".to_string());
    app.reasoning_dialog_original = Some("low".to_string());
    app.dialog.show(DialogType::ModelList {
        models: vec![cosh::ModelEntry {
            provider: "groq".to_string(),
            model: "gpt-oss-120b".to_string(),
        }],
        current: "gpt-oss-120b".to_string(),
        filter: String::new(),
        loading: false,
    });
    assert!(app.handle_model_dialog_key(KeyCode::Esc));
    assert_eq!(app.llm_config.model.as_deref(), Some("gpt-oss-120b"));
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("low"));
    assert!(!app.is_model_dialog_visible());
    assert_eq!(
        app.setup.persisted_model(),
        None,
        "cancel persists nothing globally"
    );
    let session = app.state.current_session().unwrap();
    assert_eq!(session.model, None);
    assert_eq!(session.provider, None);

    // Esc in the reasoning sub-dialog returns to the model list and applies
    // nothing either.
    app.dialog.show(DialogType::ReasoningList {
        model: "deepseek-ai/deepseek-v4-pro".to_string(),
        provider: "nvidia".to_string(),
        levels: vec![
            "default".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ],
        current: String::new(),
    });
    app.dialog.current_mut().unwrap().selected = 2;
    assert!(app.handle_reasoning_dialog_key(KeyCode::Esc));
    assert!(!app.is_reasoning_dialog_visible());
    let session = app.state.current_session().unwrap();
    assert_eq!(
        session.model, None,
        "reasoning-dialog cancel persists nothing"
    );
    assert_eq!(app.setup.persisted_model(), None);
}

/// Enter in the reasoning sub-dialog commits the picked effort level through
/// the single write path: config, current session, and the global slot all
/// reflect the selection.
#[tokio::test]
async fn reasoning_dialog_enter_commits_selection() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());

    app.dialog.show(DialogType::ReasoningList {
        model: "deepseek-ai/deepseek-v4-pro".to_string(),
        provider: "nvidia".to_string(),
        levels: vec![
            "default".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ],
        current: String::new(),
    });
    app.dialog.current_mut().unwrap().selected = 3; // "high"
    assert!(app.handle_reasoning_dialog_key(KeyCode::Enter));

    assert_eq!(
        app.llm_config.model.as_deref(),
        Some("deepseek-ai/deepseek-v4-pro")
    );
    assert_eq!(app.llm_config.provider, "nvidia");
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));
    let session = app.state.current_session().unwrap();
    assert_eq!(
        session.model.as_deref(),
        Some("deepseek-ai/deepseek-v4-pro")
    );
    assert_eq!(session.provider.as_deref(), Some("nvidia"));
    assert_eq!(session.reasoning.as_deref(), Some("high"));
    assert_eq!(
        app.setup.persisted_model(),
        Some("deepseek-ai/deepseek-v4-pro")
    );
    assert_eq!(app.setup.model.reasoning.as_deref(), Some("high"));
}

/// REGRESSION: the credential gate on the restore paths treated the
/// persisted `auto` selection (deliberately stored with an EMPTY provider)
/// as an orphan and refused to restore it — after a reboot, `auto` + its
/// reasoning were silently replaced by whatever the config fell back to.
/// An empty provider is a valid persisted selection, not an orphan.
#[tokio::test]
async fn persisted_auto_selection_survives_the_credential_gate() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // Boot with `auto` persisted exactly as the picker stores it (empty
    // provider, a reasoning level) — e.g. a previous run picked auto/high.
    let mut app = App::new("/tmp".to_string());
    app.setup.set_model_selection("", "auto", Some("high"));

    // The pre-send restore path (fresh session) must restore auto as-is.
    app.apply_global_model();
    assert_eq!(
        app.llm_config.model.as_deref(),
        Some("auto"),
        "the persisted auto selection must survive the restore gate"
    );
    assert!(
        app.llm_config.provider.is_empty(),
        "auto must not pin a provider"
    );
    assert_eq!(
        app.llm_config.reasoning.as_deref(),
        Some("high"),
        "auto's persisted reasoning must survive the restore gate too"
    );
}

// ── Per-pane model selection (the /agents split's cosh panes) ───────────

use crate::routes::session::agents_split::AgentsSplitState;

/// Build an app with a live split of `count` cosh (manager) panes, pane 0
/// focused. The split is the plain reflection of the picker the real flow
/// produces (`AgentPaneRef::Name` panes, no bound sessions yet).
fn app_with_cosh_panes(count: usize) -> App {
    let mut app = App::new("/tmp".into());
    // A main session exists (the split lives inside one; `/new` would tear
    // the split down, so it is created BEFORE the split attaches).
    app.run_slash_command(&new_cmd());
    let panes = (0..count)
        .map(|_| crate::types::AgentPaneRef::Name("cosh".into()))
        .collect();
    app.agents_split = Some(AgentsSplitState::new(panes));
    app
}

/// A `/models` pick on a focused cosh pane must scope to THAT pane: the
/// pane's own selection is committed while pane B, the global config, the
/// persisted global slot and the main session all stay untouched.
#[tokio::test]
async fn pane_model_pick_scopes_to_the_pane() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(2);
    app.llm_config.provider = "global-provider".into();
    app.llm_config.model = Some("global-model".into());
    app.llm_config.reasoning = Some("high".into());

    // The picker opens pane-scoped: it edits the pane, not the config.
    app.open_pane_model_dialog(0);
    assert_eq!(app.pane_model_edit, Some(0));
    assert!(app.is_model_dialog_visible());
    assert_eq!(app.llm_config.provider, "global-provider");
    assert_eq!(app.llm_config.model.as_deref(), Some("global-model"));

    // Commit through the SAME entry the global picker uses ("pane-a-model"
    // supports no reasoning, so this is the direct commit path).
    app.confirm_model_entry("pane-a-model", "pane-a-provider");
    assert!(!app.is_model_dialog_visible());
    assert_eq!(app.pane_model_edit, None);

    // Pane A runs its own selection, with the reasoning level it inherited
    // (the same "keep the previous level" rule the main path applies).
    let pane_a = &app.agents_split.as_ref().unwrap().panes[0];
    let selection = pane_a.model_selection.as_ref().expect("pane A picked");
    assert_eq!(selection.provider, "pane-a-provider");
    assert_eq!(selection.model, "pane-a-model");
    assert_eq!(selection.reasoning.as_deref(), Some("high"));

    // ...pane B never picked, and the main agent keeps the global config.
    assert!(
        app.agents_split.as_ref().unwrap().panes[1]
            .model_selection
            .is_none(),
        "pane B must not inherit pane A's pick"
    );
    assert_eq!(app.llm_config.provider, "global-provider");
    assert_eq!(app.llm_config.model.as_deref(), Some("global-model"));
    assert_eq!(app.llm_config.reasoning.as_deref(), Some("high"));
    assert_eq!(
        app.setup.persisted_model(),
        None,
        "the global slot is untouched by a pane pick"
    );
    let main_session = app.state.current_session().unwrap();
    assert_eq!(main_session.model, None, "the main session is untouched");
}

/// Switching the MAIN model must not alter an existing pane: the pane keeps
/// its own committed selection (and its own reasoning), and the dispatch
/// route resolution still resolves the pane to ITS model.
#[tokio::test]
async fn main_model_change_leaves_existing_panes_alone() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(2);
    app.llm_config.provider = "global-provider".into();
    app.llm_config.model = Some("global-model".into());
    app.llm_config.reasoning = Some("high".into());
    app.open_pane_model_dialog(0);
    app.confirm_model_entry("pane-a-model", "pane-a-provider");

    // The main agent picks a new model (the global commit path).
    app.commit_model_selection("main-new-model", "main-new-provider", Some("medium"));
    assert_eq!(app.llm_config.model.as_deref(), Some("main-new-model"));
    assert_eq!(app.llm_config.provider, "main-new-provider");

    // The pane's selection survives verbatim — model AND reasoning (the
    // global pick's `medium` effort must not leak into the pane).
    let pane_a = &app.agents_split.as_ref().unwrap().panes[0];
    let selection = pane_a.model_selection.as_ref().unwrap();
    assert_eq!(selection.model, "pane-a-model");
    assert_eq!(selection.provider, "pane-a-provider");
    assert_eq!(selection.reasoning.as_deref(), Some("high"));

    // And the route the pane's next turn dispatches with is still its own.
    let route = app.manager_pane_model_selection(pane_a).unwrap();
    assert_eq!(route.model, "pane-a-model");
    assert_eq!(route.reasoning.as_deref(), Some("high"));
}

/// A pane's model pick persists in the SAME session-file markers the main
/// session uses, and the markers take precedence on a restarted pane (no
/// in-memory selection): the pane resolves back to its own model, not the
/// global config.
#[tokio::test]
async fn pane_model_persists_in_the_session_markers() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(1);
    app.llm_config.provider = "global-provider".into();
    app.llm_config.model = Some("global-model".into());

    // Bind the pane to a real cosh session (the shell a first turn mints).
    let session_id = crate::session::store::generate_session_id();
    app.session_store.save_session(&crate::types::Session {
        id: session_id.clone(),
        title: "agents manager".to_string(),
        messages: Vec::new(),
        created_at: 0,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: std::collections::HashMap::new(),
        main_agent_worktree: None,
    });
    app.agents_split.as_mut().unwrap().panes[0].session_id = Some(session_id.clone());

    app.open_pane_model_dialog(0);
    app.confirm_model_entry("pane-model", "pane-provider");
    // Auto selection keeps its own shape too: model without a provider.
    // `auto` pushes the reasoning sub-dialog (the standard effort set) —
    // pick the default level and Enter, which routes the commit to the
    // pane through the same shared path.
    app.open_pane_model_dialog(0);
    app.confirm_model_entry("auto", "");
    app.dialog.current_mut().unwrap().selected = 0;
    app.handle_reasoning_dialog_key(KeyCode::Enter);

    let persisted = app.session_store.load_session(&session_id).unwrap();
    assert_eq!(persisted.model.as_deref(), Some("auto"));
    assert_eq!(persisted.provider, None, "auto records no provider");
    assert_eq!(persisted.reasoning, None);

    // A restarted pane carries no in-memory selection: the session markers
    // take precedence over the global config, exactly like the main
    // session's markers do.
    let mut restarted = crate::routes::session::agents_split::AgentPane::new("cosh".into());
    restarted.session_id = Some(session_id);
    let route = app.manager_pane_model_selection(&restarted).unwrap();
    assert_eq!(route.model, "auto");
    assert!(route.provider.is_empty(), "auto must not pin a provider");
    // ...while a pane that never recorded anything resolves to None (the
    // global config at spawn time).
    restarted.session_id = None;
    assert!(app.manager_pane_model_selection(&restarted).is_none());
}

/// The routing contract: `/models` on a focused cosh pane opens the
/// PANE-scoped picker; the same command with no pane focused stays global.
#[tokio::test]
async fn slash_models_on_a_focused_cosh_pane_opens_the_pane_picker() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(1);
    let models_cmd = crate::ui::slash_menu::SlashCommand {
        name: "models".into(),
        desc: String::new(),
    };

    // No focused pane: the main agent's global picker. `AgentsSplitState::
    // new` focuses pane 0 by default — the split's Alt+0 state is what
    // routes to the main agent.
    app.agents_split.as_mut().unwrap().focused = None;
    assert!(
        !crate::app::agents_dispatch::route_agent_slash_command(&mut app, &models_cmd),
        "no focused pane: the pane router declines"
    );
    // ...and the caller (run_slash_command) opens the GLOBAL picker.
    app.run_slash_command(&models_cmd);
    assert_eq!(app.pane_model_edit, None);
    assert!(app.is_model_dialog_visible());
    app.restore_model_dialog();

    // Focused cosh pane: the pane's own picker.
    app.agents_split.as_mut().unwrap().focused = Some(0);
    crate::app::agents_dispatch::route_agent_slash_command(&mut app, &models_cmd);
    assert_eq!(app.pane_model_edit, Some(0));
    assert!(app.is_model_dialog_visible());
    // And cancel (Esc) leaves nothing behind.
    app.handle_model_dialog_key(KeyCode::Esc);
    assert_eq!(app.pane_model_edit, None);
    assert!(!app.is_model_dialog_visible());
    assert_eq!(app.llm_config.model, crate::config::LlmConfig::from_env().model);
}

// ── The prompt badge must reflect the pane's OWN model ──────────────────
//
// REGRESSION (the badge half of the per-pane model feature): the commit
// landed on the pane's selection and its session markers, but the badge
// kept reading the GLOBAL config triple — a pick changed the pane's turns
// while the row kept painting the main agent's model. The badge now reads
// the pane's own serving pair (`effective_pane_prompt_info`), which the
// commit overwrites in `session_info` and the split rebuilds hydrate from
// the session markers.

/// A committed pane pick paints the prompt badge with the PANE's model the
/// moment it is committed — not the main agent's global triple.
#[tokio::test]
async fn pane_model_pick_shows_on_the_prompt_badge() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(2);
    app.llm_config.provider = "global-provider".into();
    app.llm_config.model = Some("global-model".into());

    app.open_pane_model_dialog(0);
    app.confirm_model_entry("pane-a-model", "pane-a-provider");

    // The pane's serving pair carries the pick (the badge reads it).
    let info = app
        .effective_pane_prompt_info(0)
        .expect("the pane's own badge triple");
    assert_eq!(info.model.as_deref(), Some("pane-a-model"));
    assert_eq!(info.provider.as_deref(), Some("pane-a-provider"));

    // Pane B never picked: a manager pane with no pick of its own shows NO
    // badge triple (the row falls through to the gateway triple it always
    // painted) — the global config stays untouched beneath it.
    let info_b = app.effective_pane_prompt_info(1);
    assert!(
        info_b.is_none(),
        "a manager pane with no pick shows no badge triple"
    );
}

/// A hydrated manager pane (a pick recorded on its bound session in a
/// previous run) shows its own model on the badge the moment the split
/// rebuilds — before any new turn runs.
#[tokio::test]
async fn hydrated_manager_pane_shows_the_session_model_on_the_badge() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_panes(1);
    app.llm_config.provider = "global-provider".into();
    app.llm_config.model = Some("global-model".into());

    // A bound pane whose session carries the recorded pick (the same
    // markers a first-turn shell is stamped with).
    let session_id = crate::session::store::generate_session_id();
    app.session_store.save_session(&crate::types::Session {
        id: session_id.clone(),
        title: "agents manager".to_string(),
        messages: Vec::new(),
        created_at: 0,
        title_generated: false,
        provider: Some("pane-provider".into()),
        model: Some("pane-model".into()),
        reasoning: Some("high".into()),
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: std::collections::HashMap::new(),
        main_agent_worktree: None,
    });
    app.agents_split.as_mut().unwrap().panes[0].session_id = Some(session_id);

    // The rebuild hydration resolves the markers into the pane's live
    // selection and its badge chrome.
    app.hydrate_manager_pane_model_selections();
    let info = app
        .effective_pane_prompt_info(0)
        .expect("the hydrated pane's badge triple");
    assert_eq!(info.model.as_deref(), Some("pane-model"));
    assert_eq!(info.provider.as_deref(), Some("pane-provider"));
    assert_eq!(info.reasoning.as_deref(), Some("high"));
    // The in-memory selection is in place too (the dispatch route reads it).
    let selection = app.agents_split.as_ref().unwrap().panes[0]
        .model_selection
        .as_ref()
        .expect("hydrated");
    assert_eq!(selection.model, "pane-model");
}
