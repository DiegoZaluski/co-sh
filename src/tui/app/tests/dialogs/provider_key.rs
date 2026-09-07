use super::super::{App, key};
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
