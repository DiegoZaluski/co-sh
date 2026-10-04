use super::{App, HOME_LOCK, isolate_home};
use crate::app::gateway_recommendation::{DeviceSignInEvent, SignInOrigin};
use cosh_sdk::connector::has_api_key;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn new_cmd() -> crate::ui::slash_menu::SlashCommand {
    crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    }
}

/// The one-time-offer marker lives under the platform data-local dir — the
/// SAME path production reads (`BaseDirs` re-resolves per call, so tests
/// that redirect `$HOME` through `isolate_home` hit the scratch copy).
fn marker_path() -> std::path::PathBuf {
    let dirs = directories::BaseDirs::new().expect("home dirs resolve");
    dirs.data_local_dir().join("cosh/gateway-recommendation")
}

fn write_marker(answer: &str) {
    let path = marker_path();
    std::fs::create_dir_all(path.parent().unwrap()).expect("create marker dir");
    std::fs::write(&path, answer).expect("write marker");
}

fn read_marker() -> Option<Vec<u8>> {
    std::fs::read(marker_path()).ok()
}

/// An app pinned to a non-local provider inside an active session — the
/// state the offer path requires (`Session` route, cloud provider).
fn session_app(provider: &str) -> App {
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&new_cmd());
    app.llm_config.provider = provider.to_string();
    app
}

/// REGRESSION (bug 1 — the box reappeared after every reboot): declining
/// used to set only the in-memory latch, so the NEXT launch re-offered.
/// The decline must burn the marker WITH its answer and silence every
/// later offer path — error-driven AND pre-send.
#[tokio::test]
async fn declined_offer_burns_the_marker_and_stays_silent_after_reboot() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    assert!(
        !marker_path().exists(),
        "scratch home must start without a marker"
    );

    // First failure on a fresh install: the one-time offer fires.
    let mut app = session_app("openai");
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(
        app.free_gateway_dialog.visible,
        "the first failure of the install must offer the gateway"
    );

    // Decline (No): the answer is PERSISTED, not just latched in memory.
    app.free_gateway_dialog.selected = 1;
    app.commit_gateway_choice();
    assert_eq!(
        read_marker().as_deref(),
        Some(b"declined".as_slice()),
        "declining must burn the marker with the `declined` answer"
    );

    // "Reboot": a fresh App in the same install. The in-memory latch is
    // gone; the marker is what must keep the box down — on the error path…
    let mut rebooted = session_app("openai");
    rebooted.maybe_offer_gateway_on_error("401 unauthorized");
    assert!(
        !rebooted.free_gateway_dialog.visible,
        "a declined install must never re-offer, not even on an auth-shaped error"
    );
    // …AND on the pre-send path (`gateway_recommendation_offers`).
    assert!(
        rebooted.gateway_recommendation_offers(false).is_none(),
        "a declined install must not reopen the offer on a keyless send"
    );
}

/// The one-time guarantee must keep working when the marker was never
/// burned: the first failure offers regardless of error shape.
#[tokio::test]
async fn first_failure_without_marker_still_offers() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = session_app("openai");
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(
        app.free_gateway_dialog.visible,
        "first-install failure must surface the one-time offer"
    );
}

/// REGRESSION (bug 2 — the heuristic DELETED working credentials): an
/// auth-shaped error on the gateway provider used to wipe the keyring
/// entries (access AND refresh) before asking anyone. Heuristics must
/// never be destructive: the offer may re-open, but whatever credential
/// the machine had before the error must still be there after it. The test
/// is keyring-neutral: it snapshots `has_api_key("charm")` and asserts the
/// offer path changed NOTHING — on machines with a stored token (the
/// regression's victim) or without one (CI), before == after.
#[tokio::test]
async fn auth_error_reoffers_but_never_deletes_gateway_credentials() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // An install already using the gateway (marker = authorized).
    write_marker("authorized");
    let had_credential = has_api_key("charm");

    let mut app = session_app("charm");
    app.maybe_offer_gateway_on_error("401 unauthorized");
    assert!(
        app.free_gateway_dialog.visible,
        "a dead gateway credential is exactly what re-running the sign-in fixes"
    );
    assert_eq!(
        has_api_key("charm"),
        had_credential,
        "the error heuristic must not change stored credentials — re-running the opt-in overwrites them instead"
    );
}

/// An answered install stays quiet on transient errors — even while the
/// marker says `authorized`. Only a DEAD gateway credential (charm +
/// auth-shaped) re-opens the offer.
#[tokio::test]
async fn transient_error_after_authorization_stays_silent() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    write_marker("authorized");

    let mut app = session_app("openai");
    app.maybe_offer_gateway_on_error("request timed out after 30s");
    assert!(
        !app.free_gateway_dialog.visible,
        "transient errors must not nag an already-answered install"
    );
}

/// REGRESSION: the `Authorized` completion rewrote the live provider/model
/// unconditionally — a user who Esc'd the sign-in and picked another model
/// (or another session) had that choice silently clobbered when the
/// background flow finished. The rewrite only happens when the completion
/// actually takes over the UI (idle session + picker or parked message).
#[tokio::test]
async fn authorized_completion_without_takeover_keeps_the_live_model() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app("openai");
    app.llm_config.model = Some("user-model".into());
    app.llm_config.reasoning = Some("low".into());

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Authorized {
        origin: SignInOrigin::GatewayOffer,
        provider: "charm".to_string(),
    });
    app.pump_gateway_events();

    assert_eq!(
        app.llm_config.provider, "openai",
        "no takeover: the user's provider must survive the background completion"
    );
    assert_eq!(
        app.llm_config.model.as_deref(),
        Some("user-model"),
        "no takeover: the user's model must survive the background completion"
    );
    assert_eq!(
        app.llm_config.reasoning.as_deref(),
        Some("low"),
        "the previous model's reasoning must not leak onto the gateway config"
    );
}

/// The flip side: with a parked message, the completion DOES take over —
/// provider charm, model reset, and the stale reasoning cleared.
#[tokio::test]
async fn authorized_completion_with_parked_message_takes_over() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app("openai");
    app.llm_config.model = Some("user-model".into());
    app.llm_config.reasoning = Some("low".into());
    app.pending_gateway_message = Some("hello".into());

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Authorized {
        origin: SignInOrigin::GatewayOffer,
        provider: "charm".to_string(),
    });
    app.pump_gateway_events();

    assert_eq!(app.llm_config.provider, "charm");
    assert_eq!(app.llm_config.model, None);
    assert_eq!(
        app.llm_config.reasoning, None,
        "the old model's reasoning must not apply to the gateway default"
    );
}

/// REGRESSION (mouse/Ctrl+C stealing): the box can stay `visible` while a
/// MODAL is stacked on top — the gateway hit-tests must yield to the modal
/// instead of consuming presses aimed at it.
#[tokio::test]
async fn gateway_hit_tests_yield_to_a_stacked_modal() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app("openai");
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);

    // A point inside the drawn box: the box claims it while it is alone.
    let area = app.free_gateway_dialog_area();
    let inside = (area.x + 2, area.y + 2);
    assert!(
        app.gateway_dialog_contains(inside.0, inside.1),
        "fixture: the point must sit inside the drawn box"
    );

    // A modal on top takes over: the gateway hit-tests yield.
    app.dialog.show(crate::ui::dialogs::DialogType::Confirm {
        message: "modal on top".into(),
    });
    assert!(
        !app.gateway_dialog_contains(inside.0, inside.1),
        "a modal owns the screen; the gateway box must not intercept its presses"
    );
    assert!(
        !app.gateway_dialog_owns_drag(),
        "drag ownership yields to a stacked modal too"
    );
}

/// The offer opens TABBED (question-dialog design): one tab per offered
/// gateway, Charm first, and the active tab is what the sign-in dispatch
/// reads. Switching the active tab must retarget the dispatch.
#[tokio::test]
async fn the_offer_opens_tabbed_and_the_dispatch_follows_the_active_tab() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = session_app("openai");
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);
    assert_eq!(
        app.free_gateway_dialog.active_offer().unwrap().provider,
        "charm",
        "tab order = OFFERS order; Charm is the default tab"
    );

    // The user flips to the GitHub tab: the dispatch must follow.
    app.free_gateway_dialog
        .handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        app.free_gateway_dialog.active_offer().unwrap().provider,
        "copilot",
        "Right switches to the next gateway tab"
    );
}

/// The `Authorized` completion switches the live config to the provider the
/// event SAYS it signed in for — not hardcoded charm. GitHub's token lands
/// in the keyring and the takeover routes the parked message through copilot.
#[tokio::test]
async fn authorized_completion_switches_to_the_provider_it_signed_in() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app("openai");
    app.pending_gateway_message = Some("hello".into());

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Authorized {
        origin: SignInOrigin::GatewayOffer,
        provider: "copilot".to_string(),
    });
    app.pump_gateway_events();

    assert_eq!(
        app.llm_config.provider, "copilot",
        "the takeover must target the provider whose token just landed"
    );
    assert_eq!(app.llm_config.model, None);
}

/// A `Failed` completion from ANY provider restores the parked message,
/// hides the box and never burns the marker — the provider-neutral event
/// shape must treat copilot's failure exactly like charm's.
#[tokio::test]
async fn failed_completion_restores_the_message_and_never_burns_the_marker() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app("openai");
    app.pending_gateway_message = Some("hello".into());
    app.gateway_signing_in = true; // fixture: a flow was in flight

    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Failed {
        origin: SignInOrigin::GatewayOffer,
        provider: "copilot".to_string(),
        error: "device flow denied".into(),
    });
    app.pump_gateway_events();

    assert!(
        !app.gateway_signing_in,
        "a failed flow must release the latch"
    );
    assert!(!app.free_gateway_dialog.visible);
    assert!(
        !marker_path().exists(),
        "a failure is NOT an answer: the one-time offer must stay due"
    );
}

/// The dead-credential re-offer works for the COPILOT gateway too (the
/// `OFFERS.any` arm), and the box opens PRE-SELECTED on the copilot tab —
/// the gateway the user needs to re-run, not the first tab.
#[tokio::test]
async fn dead_copilot_credential_reoffers_on_the_copilot_tab() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    write_marker("authorized");

    let mut app = session_app("copilot");
    app.maybe_offer_gateway_on_error("401 unauthorized");
    assert!(
        app.free_gateway_dialog.visible,
        "a dead copilot credential is exactly what re-running its sign-in fixes"
    );
    assert_eq!(
        app.free_gateway_dialog.active_offer().unwrap().provider,
        "copilot",
        "the re-offer must land on the tab of the gateway that died"
    );
}
