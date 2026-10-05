use super::{App, HOME_LOCK, isolate_home};
use crate::app::gateway_recommendation::{DeviceSignInEvent, SignInOrigin};
use crate::ui::dialogs::{DeviceLoginPhase, DialogType};
use crossterm::event::KeyCode;

/// An app with the ADD Provider API-key dialog open for Charm, holding a
/// half-typed key — the exact surface the Login button sits in. Call it
/// while holding `HOME_LOCK` (the caller owns the guard for the WHOLE
/// test — App::new reads setup through the redirected home).
/// (No marker or provider setup needed: the ProviderDialog pump arms never
/// touch the gateway offer state.)
fn provider_app() -> App {
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::ApiKeyInput {
        provider: "charm".to_string(),
        env_var: "CHARM_API_KEY".to_string(),
        input: "half-typed".to_string(),
        cursor_pos: 4,
        device_login: true,
    });
    app
}

/// Simulate the in-flight state `begin_device_login` produces, WITHOUT
/// spawning the real network flow (tests must not hit charm.sh). The pump
/// and key paths under test only read the dialog stack and the flag.
fn in_flight(app: &mut App) {
    app.dialog.show(DialogType::DeviceLogin {
        provider: "charm".to_string(),
        label: "Charm".to_string(),
        phase: DeviceLoginPhase::Connecting,
    });
    app.gateway_signing_in = true;
}

/// REGRESSION (review R1): the sign-in display must be PUSHED over the
/// API-key input — never a `replace()`, which wipes the whole stack and
/// destroys the half-typed key the user may still want to paste manually.
/// The real spawn runs here: on a machine with network access the flow
/// reaches charm.sh and `announce_device_transaction` opens the SYSTEM
/// BROWSER — run explicitly (`cargo test -- --ignored`).
#[ignore = "spawns the real charm.sh device flow, which opens the system browser"]
#[tokio::test]
async fn login_pushes_the_sign_in_over_the_key_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = provider_app();
    app.begin_device_login("charm");

    assert_eq!(
        app.dialog.stack.len(),
        2,
        "the sign-in display must stack ON TOP of the key input, not replace it"
    );
    match &app.dialog.stack[1].dialog_type {
        DialogType::DeviceLogin {
            provider, phase, ..
        } => {
            assert_eq!(provider, "charm");
            assert_eq!(phase, &DeviceLoginPhase::Connecting);
        }
        other => panic!("expected DeviceLogin on top, got {other:?}"),
    }
    match &app.dialog.stack[0].dialog_type {
        DialogType::ApiKeyInput { input, .. } => {
            assert_eq!(
                input, "half-typed",
                "the half-typed key must survive the push"
            );
        }
        other => panic!("expected ApiKeyInput beneath, got {other:?}"),
    }
    assert!(app.gateway_signing_in);
}

/// The sign-in is refused while another device flow is already polling:
/// no second box, no second background flow racing the shared flag.
#[tokio::test]
async fn second_login_while_signing_in_is_refused() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = provider_app();
    app.gateway_signing_in = true; // fixture: a flow is already in flight

    app.begin_device_login("charm");
    assert_eq!(
        app.dialog.stack.len(),
        1,
        "a refused login must not push a second sign-in box"
    );
    assert!(
        matches!(
            app.dialog.stack[0].dialog_type,
            DialogType::ApiKeyInput { .. }
        ),
        "the refused login must leave the key input untouched"
    );
    // NOTE: the refusal path returns BEFORE spawning, so no network task
    // was started by this test despite the fixture flag.
    app.gateway_signing_in = false;
}

/// A `Started` event updates ONLY the box it belongs to: a box still live
/// for its own provider moves to Authorizing; an event for a DIFFERENT
/// provider (or for a box the user already Esc'd) must not repaint
/// whatever dialog now owns the screen.
#[tokio::test]
async fn started_event_updates_only_the_live_box() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.dialog.show(DialogType::DeviceLogin {
        provider: "charm".to_string(),
        label: "Charm".to_string(),
        phase: DeviceLoginPhase::Connecting,
    });
    app.gateway_signing_in = true;

    // Matching provider: Connecting → Authorizing.
    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Started {
        origin: SignInOrigin::ProviderDialog,
        provider: "charm".to_string(),
        url: "https://charm.sh/auth".into(),
        user_code: "ABCD-1234".into(),
    });
    app.pump_gateway_events();
    match &app.dialog.stack[0].dialog_type {
        DialogType::DeviceLogin {
            phase: DeviceLoginPhase::Authorizing { url, user_code },
            ..
        } => {
            assert_eq!(url, "https://charm.sh/auth");
            assert_eq!(user_code, "ABCD-1234");
        }
        other => panic!("expected the live box to become Authorizing, got {other:?}"),
    }

    // Mismatched provider: the live charm box must stay Authorizing.
    let _ = tx.send(DeviceSignInEvent::Started {
        origin: SignInOrigin::ProviderDialog,
        provider: "copilot".to_string(),
        url: "https://github.com/login/device".into(),
        user_code: "WXYZ-5678".into(),
    });
    app.pump_gateway_events();
    assert!(matches!(
        app.dialog.stack[0].dialog_type,
        DialogType::DeviceLogin {
            phase: DeviceLoginPhase::Authorizing { .. },
            ..
        }
    ));
}

/// REGRESSION (review R1, resolve side): on success BOTH the sign-in box
/// and the API-key input beneath it pop — the credential that input was
/// opened to collect just landed in the keyring. The live provider config
/// is never touched (no takeover from the provider dialog).
#[tokio::test]
async fn authorized_pops_both_boxes_and_never_takes_over() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = provider_app();
    app.llm_config.provider = "openai".to_string();
    in_flight(&mut app);

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Authorized {
        origin: SignInOrigin::ProviderDialog,
        provider: "charm".to_string(),
    });
    app.pump_gateway_events();

    assert!(
        app.dialog.stack.is_empty(),
        "success must close the sign-in box AND the key input it served"
    );
    assert!(!app.gateway_signing_in);
    assert_eq!(
        app.llm_config.provider, "openai",
        "the provider-dialog flow must never switch the live config"
    );
}

/// REGRESSION (review R1): on failure ONLY the sign-in box pops — the user
/// lands back on the API-key input with the half-typed key intact, the
/// manual paste path preserved.
#[tokio::test]
async fn failed_returns_to_the_half_typed_key_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = provider_app();
    in_flight(&mut app);

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Failed {
        origin: SignInOrigin::ProviderDialog,
        provider: "charm".to_string(),
        error: "device flow denied".into(),
    });
    app.pump_gateway_events();

    assert_eq!(app.dialog.stack.len(), 1);
    match &app.dialog.stack[0].dialog_type {
        DialogType::ApiKeyInput { input, .. } => {
            assert_eq!(
                input, "half-typed",
                "the manual paste path must survive a failure"
            );
        }
        other => panic!("expected the key input back on top, got {other:?}"),
    }
    assert!(!app.gateway_signing_in);
}

/// Esc pops the sign-in box only — the background flow keeps polling (the
/// flag stays up) and its outcome still reports via toast, never forcing
/// the box back. Every other key is swallowed (sovereignty), Esc is the
/// only way out.
#[tokio::test]
async fn esc_pops_the_box_but_keeps_the_flow_polling() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = provider_app();
    in_flight(&mut app);

    // A non-Esc key is swallowed: the box stays, nothing pops.
    app.handle_device_login_key(KeyCode::Char('x'));
    assert_eq!(app.dialog.stack.len(), 2);
    assert!(app.gateway_signing_in);

    // Esc pops only the sign-in box.
    app.handle_device_login_key(KeyCode::Esc);
    assert_eq!(app.dialog.stack.len(), 1);
    assert!(
        matches!(
            app.dialog.stack[0].dialog_type,
            DialogType::ApiKeyInput { .. }
        ),
        "Esc must return to the key input, not an empty screen"
    );
    assert!(
        app.gateway_signing_in,
        "Esc must NOT cancel the background poll"
    );

    // The late outcome of the Esc'd flow reports without touching the
    // screen the user went back to.
    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Failed {
        origin: SignInOrigin::ProviderDialog,
        provider: "charm".to_string(),
        error: "device flow denied".into(),
    });
    app.pump_gateway_events();
    assert_eq!(app.dialog.stack.len(), 1);
    assert!(matches!(
        app.dialog.stack[0].dialog_type,
        DialogType::ApiKeyInput { .. }
    ));
    assert!(!app.gateway_signing_in);
}
