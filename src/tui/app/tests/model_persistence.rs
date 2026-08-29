use super::{App, HOME_LOCK, isolate_home};
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

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
    // isolate_home wipes <scratch>/.config first, so write AFTER it runs.
    let cfg = std::env::temp_dir()
        .join("cosh-hook-test-home")
        .join(".config/cosh");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("setup.json"),
        r#"{"model": {"provider": "nvidia", "model": "deepseek-ai/deepseek-v4-pro", "reasoning": "high"}}"#,
    )
    .unwrap();

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

/// The free-gateway reroute (opt into Zen after a keyless failure): the
/// routed model becomes the session's model so switching back restores a
/// working config, and the write is SESSION-scoped — the global slot is not
/// overwritten, so brand-new sessions still start on the user's selection.
#[tokio::test]
async fn gateway_reroute_records_free_model_on_session_only() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());
    let session = app.state.current_session().unwrap();
    let session_id = session.id.clone();

    // The current selection failed (no key) and the user opts into the free
    // gateway (option 0). No parked message, so no agent loop is replayed.
    app.free_gateway_dialog.selected = 0;
    app.commit_gateway_choice();

    assert_eq!(app.llm_config.provider, cosh_sdk::connector::ZEN_PROVIDER);
    assert_eq!(app.llm_config.model.as_deref(), Some("hy3-free"));
    assert_eq!(app.llm_config.reasoning, None);
    let session = app.state.current_session().unwrap();
    assert_eq!(session.id, session_id);
    assert_eq!(
        session.provider.as_deref(),
        Some(cosh_sdk::connector::ZEN_PROVIDER)
    );
    assert_eq!(session.model.as_deref(), Some("hy3-free"));
    assert_eq!(session.reasoning, None);

    // The opt-in persists (setup.json), but the GLOBAL model slot is NOT
    // overwritten by the reroute.
    assert_eq!(app.setup.persisted_model(), None);
    assert_eq!(app.setup.zen_public_opt_in(), Some(true));
}
