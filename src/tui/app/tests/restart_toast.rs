use super::{App, HOME_LOCK, isolate_home};
use crossterm::event::KeyCode;

/// Flipping the Telemetry switch in Settings persists the consent change AND
/// fires the shared restart-required toast: telemetry consent is resolved
/// once at app boot, so the user must be told the change needs a restart.
#[tokio::test]
async fn telemetry_toggle_fires_restart_toast() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.show_settings = true;

    // Jump to the Telemetry row: it is the LAST selectable row of the
    // settings list (General section, empty hook/MCP/ACP sub-lists), and
    // select_prev wraps from the first row straight to the last one.
    // select_next would wrap modulo the list length, so it cannot be
    // used to land at the end deterministically.
    let before = app.setup.telemetry;
    app.settings_view.select_prev(1, &app.setup);
    app.process_key_event(super::mod_key(KeyCode::Enter, crossterm::event::KeyModifiers::NONE))
        .unwrap();

    assert_ne!(
        app.setup.telemetry, before,
        "Enter on the Telemetry row must flip the consent flag"
    );
    let toast = app
        .toast_state
        .current
        .as_ref()
        .expect("a restart reminder toast must be shown");
    assert_eq!(
        toast.message, "Restart cosh for this to take effect.",
        "the generic restart message must be shown verbatim"
    );
    assert_eq!(toast.duration_ms, 10_000, "the toast lasts 10 seconds");
}
