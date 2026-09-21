use super::{App, HOME_LOCK, isolate_home};
use cosh::harness::HarnessEvent;
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse,
    MouseEventKind as CKind,
};

fn mouse(kind: CKind, x: u16, y: u16) -> CMouse {
    CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

#[tokio::test]
async fn correction_failure_keeps_the_original_prompt() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "Texto original".into();
    app.prompt_view.cursor_pos = app.prompt_view.input.len();
    app.prompt_correction_active = true;
    app.prompt_correction_spinner = Some(crate::component::agent_spinner::AgentSpinner::new(
        "", &app.theme,
    ));

    app.event_tx
        .send(HarnessEvent::PromptCorrection {
            original: "Texto original".into(),
            result: Err("provider unavailable".into()),
        })
        .unwrap();
    app.poll_events();

    assert_eq!(app.prompt_view.input, "Texto original");
    assert!(!app.prompt_correction_active);
    assert!(app.prompt_correction_spinner.is_none());
}

#[tokio::test]
async fn correction_only_replaces_the_unchanged_prompt_snapshot() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "texto original".into();
    app.prompt_correction_active = true;

    app.event_tx
        .send(HarnessEvent::PromptCorrection {
            original: "texto original".into(),
            result: Ok("Texto original.".into()),
        })
        .unwrap();
    app.poll_events();
    assert_eq!(app.prompt_view.input, "Texto original.");

    // A late reply must not overwrite a draft the user edited meanwhile.
    app.prompt_view.input = "nova redação".into();
    app.prompt_correction_active = true;
    app.event_tx
        .send(HarnessEvent::PromptCorrection {
            original: "texto anterior".into(),
            result: Ok("Texto anterior.".into()),
        })
        .unwrap();
    app.poll_events();

    assert_eq!(app.prompt_view.input, "nova redação");
    assert!(!app.prompt_correction_active);
}

#[tokio::test]
async fn incomplete_right_drag_never_starts_a_correction_request() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session_store::generate_session_id();
    app.state.add_empty_session(id.clone(), "session".into(), 0);
    app.state.current_session_id = Some(id);
    app.prompt_view.input = "texto".into();
    app.prompt_view.cursor_pos = app.prompt_view.input.len();
    let prompt_area = app.compute_prompt_area().unwrap();
    let x = prompt_area.x + 3;
    let y = prompt_area.y + 1;

    app.handle_mouse_event(mouse(CKind::Down(CBtn::Right), x, y))
        .unwrap();
    // Select only "text", deliberately leaving the final "o" out.
    app.handle_mouse_event(mouse(CKind::Drag(CBtn::Right), x + 4, y))
        .unwrap();
    app.handle_mouse_event(mouse(CKind::Up(CBtn::Right), x + 4, y))
        .unwrap();

    assert_eq!(app.prompt_view.input, "texto");
    assert!(!app.prompt_correction_active);
    assert!(!app.prompt_view.correction_selection);
    assert_eq!(
        app.toast_state
            .current
            .as_ref()
            .and_then(|toast| toast.title.as_deref()),
        Some("Operation cancelled")
    );
}

#[tokio::test]
async fn esc_cancels_the_in_flight_correction_and_keeps_the_draft() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "rascunho original".into();
    app.prompt_view.cursor_pos = app.prompt_view.input.len();
    app.prompt_correction_active = true;
    app.prompt_correction_spinner = Some(crate::component::agent_spinner::AgentSpinner::new(
        "", &app.theme,
    ));

    app.process_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    // The original text is intact (a correction never mutates the draft
    // before its answer, so cancelling "restores" it), the spinner is gone
    // and a new correction can be requested right away.
    assert_eq!(app.prompt_view.input, "rascunho original");
    assert!(!app.prompt_correction_active);
    assert!(app.prompt_correction_spinner.is_none());
    assert_eq!(
        app.toast_state
            .current
            .as_ref()
            .and_then(|toast| toast.title.as_deref()),
        Some("Prompt correction")
    );

    // A late response that raced the cancel is dropped: the draft is never
    // replaced and the correction flag stays down.
    app.event_tx
        .send(HarnessEvent::PromptCorrection {
            original: "rascunho original".into(),
            result: Ok("Rascunho corrigido.".into()),
        })
        .unwrap();
    app.poll_events();

    assert_eq!(app.prompt_view.input, "rascunho original");
    assert!(!app.prompt_correction_active);
}

#[tokio::test]
async fn esc_without_a_running_correction_changes_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "rascunho".into();

    // No correction is running: the dedicated gate must be a no-op (Esc
    // falls through to its normal meanings — none of which touch the
    // draft here).
    app.process_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert_eq!(app.prompt_view.input, "rascunho");
    assert!(!app.prompt_correction_active);
}
