use super::super::{App, HOME_LOCK, isolate_home, key};
use crate::routes::add_provider::ProviderEntry;
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

/// A provider entry the keyring certainly has no key for (fake env var), so
/// the dialog flow tests never touch a real credential.
fn fake_cloud_entry() -> ProviderEntry {
    ProviderEntry {
        name: "testprovider",
        local: false,
        hint: "COSH_TEST_NONEXISTENT_KEY".to_string(),
    }
}

/// Without a saved key the plain API-key input opens (the forget/overwrite
/// picker is reserved for providers whose key is already in the keyring).
#[tokio::test]
async fn open_provider_dialog_without_key_shows_api_key_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.open_provider_dialog(&fake_cloud_entry());
    assert!(matches!(
        app.dialog.current().map(|d| &d.dialog_type),
        Some(DialogType::ApiKeyInput { .. })
    ));
}

/// Local providers keep going to the server-URL box, never the key picker.
#[tokio::test]
async fn open_provider_dialog_local_shows_url_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let entry = ProviderEntry {
        name: "ollama",
        local: true,
        hint: "http://localhost:11434".to_string(),
    };
    app.open_provider_dialog(&entry);
    assert!(matches!(
        app.dialog.current().map(|d| &d.dialog_type),
        Some(DialogType::LocalUrlInput { .. })
    ));
}

/// Navigation: arrows flip the selection, unknown keys are swallowed by the
/// sovereign modal and Esc closes the picker.
#[tokio::test]
async fn provider_key_choice_navigation_and_esc() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "testprovider".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });
    // open_provider_dialog defaults to the safe option (index 1).
    if let Some(d) = app.dialog.current_mut() {
        d.selected = 1;
    }

    app.handle_provider_key_choice_key(KeyCode::Left);
    assert_eq!(app.dialog.current().unwrap().selected, 0);
    app.handle_provider_key_choice_key(KeyCode::Right);
    assert_eq!(app.dialog.current().unwrap().selected, 1);

    // Any other key is swallowed but keeps the dialog open.
    assert!(app.handle_provider_key_choice_key(KeyCode::Char('x')));
    assert!(app.is_provider_key_choice_visible());

    assert!(app.handle_provider_key_choice_key(KeyCode::Esc));
    assert!(!app.dialog.visible(), "Esc closes the picker");
}

/// "Overwrite key" (the safe default) swaps the picker for the plain
/// API-key input carrying the same provider/env var.
#[tokio::test]
async fn provider_key_choice_overwrite_swaps_to_api_key_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "testprovider".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });
    if let Some(d) = app.dialog.current_mut() {
        d.selected = 1;
    }

    app.handle_provider_key_choice_key(KeyCode::Enter);

    assert_eq!(app.dialog.stack.len(), 1, "picker is replaced, not stacked");
    match app.dialog.current().map(|d| &d.dialog_type) {
        Some(DialogType::ApiKeyInput {
            provider, env_var, ..
        }) => {
            assert_eq!(provider, "testprovider");
            assert_eq!(env_var, "COSH_TEST_NONEXISTENT_KEY");
        }
        other => panic!("expected ApiKeyInput, got {other:?}"),
    }
}

/// "Forget key" pushes the standard destructive-action Confirm on top of the
/// picker; cancelling the Confirm (Esc) pops back to the picker untouched.
#[tokio::test]
async fn provider_key_choice_forget_gates_behind_confirm() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "testprovider".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });

    app.handle_provider_key_choice_key(KeyCode::Enter);

    assert_eq!(app.dialog.stack.len(), 2, "Confirm pushed over the picker");
    assert!(matches!(
        app.dialog.current().map(|d| &d.dialog_type),
        Some(DialogType::Confirm { .. })
    ));

    // Cancel path: the Confirm's own Esc pops back to the picker.
    assert!(app.process_key_event(key(KeyCode::Esc)).is_ok());
    assert_eq!(app.dialog.stack.len(), 1, "back to the picker");
    assert!(app.is_provider_key_choice_visible());
}

/// Confirming the forget (Enter on the Confirm) removes the key through the
/// keyring API and closes both dialogs. The fake env var has no real
/// credential behind it, so the delete itself is a no-op error on any
/// machine — but the flow must still close cleanly.
#[tokio::test]
async fn forget_confirmation_closes_confirm_and_picker() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "testprovider".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });
    app.handle_provider_key_choice_key(KeyCode::Enter);
    assert_eq!(app.dialog.stack.len(), 2);

    // Enter on the Confirm = "Yes": resolve the forget. Both dialogs close —
    // the picker is stale now (no key left), so the user lands back on the
    // AddProvider list.
    assert!(app.process_key_event(key(KeyCode::Enter)).is_ok());

    assert!(
        !app.dialog.visible(),
        "both the Confirm and the picker must close"
    );
}

/// REGRESSION: the orphaned-selection cleanup ran on BOTH match arms — a
/// FAILED keyring deletion (the key is still there!) erased the persisted
/// global model selection anyway, throwing away a perfectly valid
/// configuration. The cleanup may only run when the provider REALLY lost
/// its last credential.
#[tokio::test]
async fn failed_forget_keeps_the_persisted_model_selection() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // A concrete selection pointing at the provider whose key "forget" will
    // target — with a key still resolvable through the provider's env var
    // (under HOME_LOCK: the process env is test-global state).
    app.setup
        .set_model_selection("openai", "gpt-test-model", Some("high"));
    let env_var = "OPENAI_API_KEY";
    let had_env = std::env::var(env_var).ok();
    // SAFETY: HOME_LOCK serializes env access across the test threads.
    unsafe { std::env::set_var(env_var, "test-key-keeps-provider-alive") };
    // The in-process cache may hold a stale MISS from earlier tests.
    cosh_sdk::connector::invalidate_api_key(env_var);

    // The fake keyring entry: the delete is a no-op error on any machine,
    // so `forget_provider_api_key` takes the FAILURE branch — exactly the
    // regression's trigger.
    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "openai".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });
    app.handle_provider_key_choice_key(KeyCode::Enter);
    assert!(app.process_key_event(key(KeyCode::Enter)).is_ok());

    assert_eq!(
        app.setup.persisted_model(),
        Some("gpt-test-model"),
        "a failed keyring deletion must NOT erase the persisted selection"
    );
    assert_eq!(
        app.setup.model.provider, "openai",
        "the provider survives: its env-var key still authenticates"
    );
    assert_eq!(app.setup.model.reasoning.as_deref(), Some("high"));

    // Restore the process env exactly as found (HOME_LOCK holds).
    match had_env {
        Some(value) => unsafe { std::env::set_var(env_var, value) },
        None => unsafe { std::env::remove_var(env_var) },
    }
    cosh_sdk::connector::invalidate_api_key(env_var);
}

/// The cleanup's happy path is unchanged: when the forget REALLY leaves the
/// provider keyless, the persisted selection pointing at it is cleared so
/// the boot restore cannot resurrect a dead provider.
#[tokio::test]
async fn successful_forget_of_last_key_clears_the_persisted_selection() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // A provider whose env var does not exist on any machine: after the
    // (no-op error) keyring delete, `has_api_key` is reliably false — the
    // provider is keyless for real.
    app.setup
        .set_model_selection("testprovider", "test-model", Some("low"));

    app.dialog.show(DialogType::ProviderKeyChoice {
        provider: "testprovider".into(),
        env_var: "COSH_TEST_NONEXISTENT_KEY".into(),
    });
    app.handle_provider_key_choice_key(KeyCode::Enter);
    assert!(app.process_key_event(key(KeyCode::Enter)).is_ok());

    assert!(
        app.setup.persisted_model().is_none(),
        "a provider that truly lost its last credential must not keep a persisted selection"
    );
    assert!(
        app.setup.model.provider.is_empty(),
        "the orphaned provider reference must be cleared"
    );
}
