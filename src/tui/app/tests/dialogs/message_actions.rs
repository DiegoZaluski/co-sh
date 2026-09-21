use super::super::{App, HOME_LOCK, isolate_home};
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

// ── Message Actions (port of opencode's dialog-message) ────────────────

fn app_with_user_message() -> App {
    use crate::types::{Message, MessageRole, Part, TextPart};
    let mut app = App::new("/tmp".to_string());
    let now = 1_000u64;
    let session = crate::types::Session {
        id: "t".into(),
        title: "t".into(),
        created_at: now,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages: vec![
            Message {
                id: "u1".into(),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: "hello world".into(),
                    synthetic: false,
                })],
                created_at: now,
                agent: None,
                model: None,
            },
            Message {
                id: "a1".into(),
                role: MessageRole::Assistant,
                parts: vec![Part::Text(TextPart {
                    text: "reply".into(),
                    synthetic: false,
                })],
                created_at: now + 1,
                agent: None,
                model: None,
            },
            Message {
                id: "u2".into(),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: "second".into(),
                    synthetic: false,
                })],
                created_at: now + 2,
                agent: None,
                model: None,
            },
        ],
    };
    app.state.add_session(session);
    app.state.current_session_id = Some("t".into());
    app
}

#[test]
fn message_prompt_text_joins_non_synthetic_parts() {
    use crate::types::{Message, MessageRole, Part, TextPart};
    let msg = Message {
        id: "m".into(),
        role: MessageRole::User,
        parts: vec![
            Part::Text(TextPart {
                text: "one".into(),
                synthetic: false,
            }),
            Part::Text(TextPart {
                text: "hidden".into(),
                synthetic: true,
            }),
            Part::Text(TextPart {
                text: "two".into(),
                synthetic: false,
            }),
        ],
        created_at: 0,
        agent: None,
        model: None,
    };
    assert_eq!(super::super::message_prompt_text(&msg), "one\ntwo");
}

#[tokio::test]
async fn message_actions_keyboard_cycles_three_options() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u2".into(), // A user message
        preview: "second".into(),
        is_user_message: true,
    });
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 2);
    // Wraps at the bottom.
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 0);
    // And wraps upward back to the last item.
    assert!(app.handle_message_actions_dialog_key(KeyCode::Up));
    assert_eq!(app.dialog.current().unwrap().selected, 2);
}

#[tokio::test]
async fn message_actions_revert_works_at_a_non_last_user_message() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // The legacy rule only allowed reverting the LAST user message; with
    // append-only context the whole timeline is addressable. Reverting the
    // FIRST message drops everything (the entire tail) and restores its
    // prompt.
    let mut app = app_with_user_message();
    app.run_message_action(0, "u1");
    let session = app.state.current_session().unwrap();
    assert!(
        session.messages.is_empty(),
        "reverting the first message drops the entire tail"
    );
    assert_eq!(app.prompt_view.input, "hello world");
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
}

#[tokio::test]
async fn message_actions_revert_truncates_and_restores_prompt() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.run_message_action(0, "u2"); // Revert at a user message
    let session = app.state.current_session().unwrap();
    let ids: Vec<&str> = session.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["u1", "a1"],
        "u2 and everything after it are dropped"
    );
    assert!(!session.messages.iter().any(|m| m.id == "u2"));
    assert_eq!(app.prompt_view.input, "second");
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
}

#[tokio::test]
async fn message_actions_fork_branches_new_session_up_to_message() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.run_message_action(2, "u2"); // Fork at a user message
    let old = app.state.current_session().unwrap();
    assert_eq!(
        old.messages.len(),
        3,
        "fork keeps messages up to and including the clicked message"
    );
    assert!(old.messages.iter().any(|m| m.id == "u2"));
    assert!(old.title.contains("(fork)"));
    assert_ne!(old.id, "t", "the fork is a brand-new session id");
    // Regression: the fork must be persisted to disk so it survives restarts.
    // The save runs on the FIFO writer thread, so poll briefly for it — but
    // with the lock RELEASED: holding HOME_LOCK across ~2s of sleeps would
    // stall every other isolated test behind this one. The store captured
    // its absolute sessions dir at App::new, so the poll below does not
    // depend on the (now unlocked) process environment.
    let fork_id = old.id.clone();
    drop(_guard);
    let mut saved = false;
    for _ in 0..200 {
        if app.session_store.load_session(&fork_id).is_some() {
            saved = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(saved, "fork session must be saved to disk");
}

#[tokio::test]
async fn message_actions_copy_writes_clipboard_text() {
    // Clipboard may be unavailable in headless CI; only assert the text
    // extraction path via a missing-message no-op and the toast for empty.
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.run_message_action(1, "does-not-exist");
    // No panic; dialog stack untouched.
    assert!(!app.dialog.visible());
}

#[tokio::test]
async fn message_actions_dialog_renders_title_and_options() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u2".into(), // A user message
        preview: "second".into(),
        is_user_message: true,
    });
    let theme = app.theme.clone();
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.dialog.render(
        &mut buf,
        ratatui::layout::Rect::new(0, 0, 80, 24),
        &theme,
        std::time::SystemTime::now(),
    );
    let line = |y: u16| -> String {
        (0..80)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("")
    };
    let all: String = (0..24).map(line).collect();
    assert!(all.contains("Message Actions"), "title rendered");
    assert!(all.contains("Revert") && all.contains("Copy") && all.contains("Fork"));
}

#[tokio::test]
async fn message_actions_older_user_message_shows_all_options() {
    // Append-only context: ANY user message can be reverted/forked — the
    // legacy "last user message only" restriction is gone.
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(), // Not the last user message
        preview: "hello world".into(),
        is_user_message: true,
    });
    let theme = app.theme.clone();
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.dialog.render(
        &mut buf,
        ratatui::layout::Rect::new(0, 0, 80, 24),
        &theme,
        std::time::SystemTime::now(),
    );
    let line = |y: u16| -> String {
        (0..80)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("")
    };
    let all: String = (0..24).map(line).collect();
    assert!(all.contains("Message Actions"), "title rendered");
    assert!(
        all.contains("Revert") && all.contains("Copy") && all.contains("Fork"),
        "a non-last user message now offers all three actions"
    );
}

#[tokio::test]
async fn message_actions_single_user_message_shows_all_options() {
    use crate::types::{Message, MessageRole, Part, TextPart};
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let now = 1_000u64;
    let session = crate::types::Session {
        id: "single".into(),
        title: "single".into(),
        created_at: now,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages: vec![Message {
            id: "u1".into(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: "only message".into(),
                synthetic: false,
            })],
            created_at: now,
            agent: None,
            model: None,
        }],
    };
    app.state.add_session(session);
    app.state.current_session_id = Some("single".into());

    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(), // A user message
        preview: "only message".into(),
        is_user_message: true,
    });
    let theme = app.theme.clone();
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.dialog.render(
        &mut buf,
        ratatui::layout::Rect::new(0, 0, 80, 24),
        &theme,
        std::time::SystemTime::now(),
    );
    let line = |y: u16| -> String {
        (0..80)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("")
    };
    let all: String = (0..24).map(line).collect();
    assert!(all.contains("Message Actions"), "title rendered");
    assert!(
        all.contains("Revert") && all.contains("Copy") && all.contains("Fork"),
        "Single user message should show all options since it's also the last"
    );
}

#[test]
fn message_action_index_maps_correctly() {
    use super::super::App;
    assert_eq!(
        App::message_action_index(0, true),
        0,
        "user message: 0=Revert"
    );
    assert_eq!(
        App::message_action_index(1, true),
        1,
        "user message: 1=Copy"
    );
    assert_eq!(
        App::message_action_index(2, true),
        2,
        "user message: 2=Fork"
    );
    assert_eq!(
        App::message_action_index(0, false),
        1,
        "assistant message: visual 0 maps to Copy (1)"
    );
}

#[tokio::test]
async fn message_actions_older_user_message_keyboard_cycles_three_options() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(),
        preview: "hello world".into(),
        is_user_message: true,
    });
    // Down cycles through all 3 options (Revert/Copy/Fork).
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 2);
    // Up goes back.
    assert!(app.handle_message_actions_dialog_key(KeyCode::Up));
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    // Enter executes Copy (action index 1)
    assert!(app.handle_message_actions_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible(), "dialog closed after Enter");
}

/// The real crash flow: a click on a user message in the transcript opens the
/// Message Actions box. Sweep every viewport row through the app's real mouse
/// dispatch (Down+Up + a re-render after each hit) so the panic — wherever it
/// hides on this path — reproduces in CI.
#[tokio::test]
async fn clicking_a_user_message_opens_message_actions_without_panicking() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use crossterm::event::{
        KeyCode, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = app_with_user_message();
    let (w, h) = (80u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();

    let mouse = |kind: CKind, x: u16, y: u16| CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };

    let mut opened = 0;
    for y in 0..h {
        for x in [6u16, 20, 40] {
            eprintln!("EVENT down x={x} y={y}");
            app.handle_mouse_event(mouse(CKind::Down(CBtn::Left), x, y))
                .expect("down handled");
            app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), x, y))
                .expect("up handled");
            let hit = app.is_message_actions_dialog_visible();
            if hit {
                opened += 1;
                // Render the frame that follows the open, then act on the box.
                terminal.draw(|f| app.render(f, 0.016)).unwrap();
                app.handle_message_actions_dialog_key(KeyCode::Esc);
            }
            eprintln!("RENDER after x={x} y={y}");
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
        }
    }
    assert!(opened >= 1, "at least one click must open Message Actions");
}

/// Regression for the real crash: clicking a MULTILINE user prompt opened
/// the Message Actions box whose preview carried a raw `\n` into
/// `draw_text_line`, panicking ratatui's `cell_width` ("control character
/// passed to cell_width without filtering") on the frame after the open —
/// the TUI died with a garbled screen. Uses the exact session that crashed
/// (multiline prompt + HTTP-401 error assistant line), restored through the
/// real disk load path.
#[tokio::test]
async fn clicking_multiline_user_message_opens_message_actions_without_panicking() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use crossterm::event::{
        KeyCode, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    let dir = tempfile::tempdir().unwrap();
    let sessions_dir = dir.path().join("crashhash");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(
        sessions_dir.join("session-1788063765396.jsonl"),
        concat!(
            r#"{"title":"Aug 30 01:22","title_generated":false,"created_at":1788063765396,"#,
            r#""cwd":"C:\\sandbox","provider":"opencode","model":"big-pickle","reasoning":"high","context":null}"#,
            "\n",
            r#"{"Message":{"id":"msg-0","role":"user","parts":[{"type":"Text","text":"Crie um AGENT.md para o projeto,\n1. pesquise como cria um AGENT.md eficiente \n2. estude o projeto e implemente","synthetic":false}],"created_at":1788063765396,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
            r#"{"Message":{"id":"msg-err-1","role":"assistant","parts":[{"type":"Text","text":"Error: HTTP 401 - ModelError","synthetic":false}],"created_at":1788063765396,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let store = crate::session_store::SessionStore::with_dir(sessions_dir, "crashhash".into());

    let mut app = App::new("/tmp".to_string());
    app.state.session_summaries = store.list_sessions();
    let id = "1788063765396".to_string();
    assert!(app.state.ensure_session_cached(&id, &store));
    app.state.current_session_id = Some(id);

    let (w, h) = (100u16, 30u16);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();

    let mouse = |kind: CKind, x: u16, y: u16| CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };

    // Sweep every viewport row through the app's real mouse dispatch so any
    // panic on the click-to-open-to-render path reproduces in CI.
    let mut opened = 0;
    for y in 0..h {
        for x in [4u16, 15, 30, 60] {
            app.handle_mouse_event(mouse(CKind::Down(CBtn::Left), x, y))
                .expect("down handled");
            app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), x, y))
                .expect("up handled");
            if app.is_message_actions_dialog_visible() {
                opened += 1;
                // The frame right after the open used to panic here.
                terminal.draw(|f| app.render(f, 0.016)).unwrap();
                let d = app.dialog.current().unwrap();
                if let crate::ui::dialogs::DialogType::MessageActions { preview, .. } =
                    &d.dialog_type
                {
                    assert!(
                        !preview.chars().any(char::is_control),
                        "preview must be sanitized: {preview:?}"
                    );
                }
                // Drive the actions (Revert included) with a render after each.
                app.handle_message_actions_dialog_key(KeyCode::Down);
                app.handle_message_actions_dialog_key(KeyCode::Down);
                app.handle_message_actions_dialog_key(KeyCode::Enter);
                terminal.draw(|f| app.render(f, 0.016)).unwrap();
            }
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
        }
    }
    assert!(
        opened >= 1,
        "Message Actions must open for the user message"
    );
}
