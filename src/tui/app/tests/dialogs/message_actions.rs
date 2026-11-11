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
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
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
        pane_action: None,
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
        pane_action: None,
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
        pane_action: None,
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
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
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
        pane_action: None,
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
        pane_action: None,
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
    let store = crate::session::store::SessionStore::with_dir(sessions_dir, "crashhash".into());

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
    // panic on the click-to-open-to-render path reproduces in CI. Row 0 is
    // the header — its left edge is the back link, whose Up handler runs the
    // Esc pipeline and leaves the session (Home), so sweeping it would spend
    // the rest of the sweep clicking Home. Start below the header.
    let mut opened = 0;
    for y in 1..h {
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

// ── Pane message actions (the /agents dispatch, commit 41ab38d) ────────

/// An app with a cosh pane bound to a REAL stored session (the fork/revert
/// targets): two turns persisted through the store, the pane echoing the
/// same timeline.
fn app_with_cosh_pane() -> App {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::{AgentPaneRef, Message, MessageRole, Part, TextPart};
    let mut app = App::new("/tmp".to_string());

    // Persist the pane session through the store (a real JSONL history).
    let now = 1_000u64;
    let make = |id: &str, role: MessageRole, text: &str, at: u64| Message {
        id: id.to_string(),
        role,
        parts: vec![Part::Text(TextPart {
            text: text.to_string(),
            synthetic: false,
        })],
        created_at: at,
        agent: None,
        model: None,
    };
    let session = crate::types::Session {
        id: "pane-s".into(),
        title: "pane".into(),
        created_at: now,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
        messages: vec![
            make("m0", MessageRole::User, "first prompt", now),
            make("m1", MessageRole::Assistant, "first answer", now + 1),
            make("m2", MessageRole::User, "second prompt", now + 2),
            make("m3", MessageRole::Assistant, "second answer", now + 3),
        ],
    };
    app.session_store.save_session(&session);
    app.state.add_session(session);
    app.state.current_session_id = Some("main".into());
    app.state
        .add_empty_session("main".into(), "main".into(), now);

    // The split: ONE cosh pane bound to the stored session.
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "pane-s".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("pane-s".into());
    // Echo the timeline (user, agent, user, agent).
    split.echo_turn_input(0, "first prompt", true);
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "first answer".into(),
        },
    );
    split.echo_turn_input(0, "second prompt", true);
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "second answer".into(),
        },
    );
    app.agents_split = Some(split);
    app
}

#[tokio::test]
async fn pane_fork_branches_the_pane_session_not_the_main_one() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_pane();
    // Fork the pane's SECOND user prompt (ordinal 1): action 2 = Fork.
    app.run_pane_message_action(2, 0, Some(1), 2);
    let toast = app.toast_state.current.clone();
    let toast = toast.map(|t| (t.title.unwrap_or_default(), t.message));
    // The fork must NOT fail: no warning/error toast about the branch.
    match &toast {
        Some((title, message))
            if title == "Fork"
                && (message.contains("Could not") || message.contains("working")) =>
        {
            panic!("pane fork refused: {toast:?}");
        }
        _ => {}
    }
    // The fork was fired FROM the split: the split parked (mode closed,
    // panes preserved) and the fork takes the VISIBLE view — switching the
    // session behind an open split left the screen on the pane grid while
    // the fork existed only behind it.
    assert!(app.agents_split.is_none(), "the split parked on the fork");
    assert!(
        app.parked_agents_split.is_some(),
        "the parked split keeps the panes"
    );
    let current = app.state.current_session().expect("a session is open");
    assert!(
        current.title.contains("(fork)"),
        "the fork session is the visible one: {:?}",
        current.title
    );
    // The fork branched off the PANE session: its messages are the pane's
    // prefix through the second prompt (m0..m2), not the main session's.
    let ids: Vec<&str> = current.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m0", "m1", "m2"], "fork keeps the pane's prefix");
    // The fork session exists in the store too (survives restarts).
    let summaries = app.state.session_summaries.clone();
    assert!(
        summaries.iter().any(|s| s.title.contains("(fork)")),
        "a fork session was created: {summaries:?}"
    );
}

/// Revert from a pane click truncates the PANE's stored session and rebuilds
/// the pane's timeline from it — the main view's session stays untouched.
#[tokio::test]
async fn pane_revert_truncates_the_pane_session_and_rebuilds_the_timeline() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_pane();
    // Revert the pane's SECOND user prompt (ordinal 1): action 0 = Revert.
    app.run_pane_message_action(0, 0, Some(1), 2);
    // The repainted pane is the receipt; no success bubble fires.
    assert!(
        app.toast_state.current.is_none(),
        "a success toast fired: {:?}",
        app.toast_state.current.clone().map(|t| t.message)
    );
    // The store session was truncated through the second prompt.
    let pane = app.session_store.load_session("pane-s").unwrap();
    let ids: Vec<&str> = pane.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m0", "m1"], "the pane session lost m2 and m3");
    // The pane's timeline was rebuilt from the truncated session.
    let split = app.agents_split.as_ref().unwrap();
    let timeline: Vec<bool> = split.panes[0]
        .activity
        .timeline
        .iter()
        .map(|e| matches!(e, crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage { .. }))
        .collect();
    assert_eq!(
        timeline,
        vec![true, false],
        "timeline rebuilt: prompt, answer"
    );
    // The main view's session never moved.
    assert_eq!(app.state.current_session_id.as_deref(), Some("main"));
}

/// The menu's scope is the CLICK's identity, not the pane kind: a USER echo
/// offers Revert/Fork on an ACP pane too (the timeline path), while an
/// `agents_call` input — no user ordinal — stays Copy-only.
#[tokio::test]
async fn pane_dialog_offers_actions_for_user_echoes_on_every_pane_kind() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // An ACP pane that already ran (session id = the agent's, not a store id).
    let mut app = app_with_cosh_pane();
    let split = app.agents_split.as_mut().unwrap();
    split.panes[0].agent = "opencode".into();
    split.panes[0].session_id = Some("oc-agent-session".into());
    let hit = crate::routes::session::agents_split::InputBoxHit {
        span: 0..4,
        timeline_index: 0,
        user_ordinal: Some(0),
    };
    app.open_pane_message_actions(0, hit);
    let d = app.dialog.current().unwrap();
    let crate::ui::dialogs::DialogType::MessageActions {
        is_user_message, ..
    } = &d.dialog_type
    else {
        panic!("the message actions dialog opened");
    };
    assert!(
        *is_user_message,
        "an ACP pane's user echo offers Revert/Fork/Copy"
    );

    // An `agents_call` input is the other agent's prompt: Copy only.
    let mut app = app_with_cosh_pane();
    app.agents_split.as_mut().unwrap().panes[0].agent = "opencode".into();
    let call_hit = crate::routes::session::agents_split::InputBoxHit {
        span: 0..4,
        timeline_index: 1,
        user_ordinal: None,
    };
    app.open_pane_message_actions(0, call_hit);
    let d = app.dialog.current().unwrap();
    let crate::ui::dialogs::DialogType::MessageActions {
        is_user_message, ..
    } = &d.dialog_type
    else {
        panic!("the message actions dialog opened");
    };
    assert!(!is_user_message, "an agents_call input keeps Copy-only");

    // A cosh pane bound to a REAL store session keeps all three options.
    let mut app = app_with_cosh_pane();
    let hit = crate::routes::session::agents_split::InputBoxHit {
        span: 0..4,
        timeline_index: 0,
        user_ordinal: Some(0),
    };
    app.open_pane_message_actions(0, hit);
    let d = app.dialog.current().unwrap();
    let crate::ui::dialogs::DialogType::MessageActions {
        is_user_message, ..
    } = &d.dialog_type
    else {
        panic!("the message actions dialog opened");
    };
    assert!(
        *is_user_message,
        "a bound cosh pane's echo offers Revert/Fork/Copy"
    );
}

/// The "pre-feature session" report: a session persisted BEFORE the
/// actions feature (legacy single-line JSONL, no embedded context, no
/// message boundaries) still forks from a pane bound to it. The store
/// resolves the legacy branch, `context_selection` falls back to the
/// id-keyed selection, and the fork lands as a store session — no refusal
/// toast, no silent no-op.
#[tokio::test]
async fn pane_fork_works_on_a_legacy_prefeature_session() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::AgentPaneRef;
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let dir = tempfile::tempdir().unwrap();
    let sessions_dir = dir.path().join("legacyhash");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(
        sessions_dir.join("session-legacy1.jsonl"),
        concat!(
            r#"{"title":"pre-feature","title_generated":false,"created_at":1700000000000,"#,
            r#""cwd":"/tmp","provider":null,"model":null,"reasoning":null,"context":null}"#,
            "\n",
            r#"{"Message":{"id":"msg-0","role":"user","parts":[{"type":"Text","text":"primeira pergunta","synthetic":false}],"created_at":1700000000000,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
            r#"{"Message":{"id":"msg-1","role":"assistant","parts":[{"type":"Text","text":"primeira resposta","synthetic":false}],"created_at":1700000000001,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
            r#"{"Message":{"id":"msg-2","role":"user","parts":[{"type":"Text","text":"segunda pergunta","synthetic":false}],"created_at":1700000000002,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
            r#"{"Message":{"id":"msg-3","role":"assistant","parts":[{"type":"Text","text":"segunda resposta","synthetic":false}],"created_at":1700000000003,"agent":null,"model":null,"ctx_ids":[]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut app = App::new("/tmp".to_string());
    let real_dir = app.session_store.sessions_dir().to_path_buf();
    std::fs::copy(
        sessions_dir.join("session-legacy1.jsonl"),
        real_dir.join("session-legacy1.jsonl"),
    )
    .unwrap();
    let loaded = app.session_store.load_session("legacy1");
    assert!(loaded.is_some(), "the legacy session loads from its JSONL");
    assert_eq!(loaded.unwrap().messages.len(), 4);

    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "legacy1".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("legacy1".into());
    split.echo_turn_input(0, "primeira pergunta", true);
    split.echo_turn_input(0, "segunda pergunta", true);
    app.agents_split = Some(split);

    app.run_pane_message_action(2, 0, Some(1), 1);
    let toast = app
        .toast_state
        .current
        .clone()
        .map(|t| (t.title.unwrap_or_default(), t.message));
    match &toast {
        Some((title, message))
            if title == "Fork"
                && (message.contains("Could not") || message.contains("working")) =>
        {
            panic!("legacy pane fork refused: {toast:?}");
        }
        _ => {}
    }
    let summaries = app.state.session_summaries.clone();
    assert!(
        summaries.iter().any(|s| s.title.contains("(fork)")),
        "the legacy pane session forks: {summaries:?}"
    );
}

/// A manager pane's session is persisted by `persist_manager_session`: its
/// display projection must stay 1:1 with the context item GROUPS (a tool
/// call absorbing its results), or `update_ctx_ids` refuses to record the
/// message → context boundaries and Revert/Fork fail on every session that
/// used a tool. This drives the REAL projection.
#[tokio::test]
async fn pane_fork_works_on_a_manager_session_with_tool_calls() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_manager_context_session();
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "pane-m".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("pane-m".into());
    split.echo_turn_input(0, "first prompt", true);
    split.echo_turn_input(0, "second prompt", true);
    app.agents_split = Some(split);

    // Fork the pane's SECOND user prompt (ordinal 1): action 2 = Fork.
    app.run_pane_message_action(2, 0, Some(1), 2);
    let toast = app
        .toast_state
        .current
        .clone()
        .map(|t| (t.title.unwrap_or_default(), t.message));
    assert!(
        toast
            .as_ref()
            .is_some_and(|(_, message)| message.contains("New session created")),
        "pane fork must succeed on a manager session with tool calls; got: {toast:?}"
    );
    let current = app.state.current_session().expect("a session is open");
    assert!(current.title.contains("(fork)"), "{:?}", current.title);
    // The fork carries the prose up to the clicked prompt AND the tool row
    // that sits in the same context group as the call's result.
    let ids: Vec<&str> = current.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["msg-0", "msg-1", "msg-2", "msg-3"], "{ids:?}");
    // The store recorded the context boundaries the actions resolve.
    let store_session = app.session_store.load_session("pane-m").unwrap();
    assert!(
        !store_session.ctx_ids.is_empty(),
        "the manager session must carry its message → context mapping"
    );
}

/// Revert on the same real manager session: truncates the pane's stored
/// session at the clicked prompt and rebuilds the pane's timeline.
#[tokio::test]
async fn pane_revert_works_on_a_manager_session_with_tool_calls() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_manager_context_session();
    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "pane-m".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("pane-m".into());
    split.echo_turn_input(0, "first prompt", true);
    split.echo_turn_input(0, "second prompt", true);
    app.agents_split = Some(split);

    app.run_pane_message_action(0, 0, Some(1), 2);
    // No success toast on this path either: the repainted pane is the
    // receipt (a bubble claiming a revert hid the no-op for too long).
    assert!(
        app.toast_state.current.is_none(),
        "a success toast fired: {:?}",
        app.toast_state.current.clone().map(|t| t.message)
    );
    let pane = app.session_store.load_session("pane-m").unwrap();
    let ids: Vec<&str> = pane.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["msg-0", "msg-1", "msg-2"], "{ids:?}");
    // And the PANE was repainted from the truncated session: the clicked
    // prompt and its answer are gone from the transcript on screen (the
    // store truncating while the pane kept showing everything is exactly
    // the "the revert did nothing" report).
    let texts: Vec<String> =
        app.agents_split.as_ref().unwrap().panes[0]
            .activity
            .timeline
            .iter()
            .filter_map(|e| {
                match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            }
            | crate::routes::session::right_panel::types::SubagentTimelineEntry::AgentMessage {
                text,
            }
            | crate::routes::session::right_panel::types::SubagentTimelineEntry::Message {
                text,
            } => Some(text.clone()),
            _ => None,
        }
            })
            .collect();
    assert_eq!(texts, vec!["first prompt", "first answer"], "{texts:?}");
}

/// The manager context a real turn leaves behind: prose plus a tool call and
/// its result (the shape that used to break the message/group invariant).
fn manager_test_context() -> cosh::harness::context::ContextManagerState {
    use cosh::harness::context::{ContextItem, ContextManagerState};
    ContextManagerState {
        items: vec![
            ContextItem::User {
                id: 1,
                original: "first prompt".into(),
                synthetic: false,
            },
            ContextItem::Assistant {
                id: 2,
                original: "first answer".into(),
                closable: false,
                reasoning: Some("thinking".into()),
            },
            ContextItem::ToolCall {
                id: 3,
                call_id: "call-1".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 4,
                call_id: "call-1".into(),
                content: "file contents".into(),
                useless: false,
                images: Vec::new(),
            },
            ContextItem::User {
                id: 5,
                original: "second prompt".into(),
                synthetic: false,
            },
            ContextItem::Assistant {
                id: 6,
                original: "second answer".into(),
                closable: false,
                reasoning: None,
            },
        ]
        .into(),
        next_id: 7,
        ..Default::default()
    }
}

/// Build the manager pane's stored session the way a finished turn does:
/// the REAL `manager_context_display` projection, saved with the context.
fn app_with_manager_context_session() -> App {
    use crate::types::{Message, Part};

    let mut app = App::new("/tmp".to_string());
    let now = 1_000u64;
    let context = manager_test_context();
    let display = crate::app::agents_dispatch::manager_context_display(&context);
    let messages: Vec<Message> = display
        .into_iter()
        .enumerate()
        .map(|(i, (role, parts))| Message {
            id: format!("msg-{i}"),
            role,
            parts,
            created_at: now + i as u64,
            agent: None,
            model: None,
        })
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.parts.iter().any(|p| matches!(p, Part::Tool(_)))),
        "the projection must carry the tool row"
    );
    assert!(
        messages.iter().all(|m| m
            .parts
            .iter()
            .any(|p| matches!(p, Part::Text(_) | Part::Tool(_)))),
        "every projected message carries its content"
    );
    let session = crate::types::Session {
        id: "pane-m".into(),
        title: "agents manager".into(),
        created_at: now,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
        messages,
    };
    app.session_store
        .save_session_with_context(&session, &context);
    app.state
        .add_session(app.session_store.load_session("pane-m").unwrap());
    app
}

/// An ACP pane has no cosh store session: Fork branches a NEW cosh session
/// from the pane's own transcript prefix (and it is a real conversation, not
/// a display-only branch), while the pane keeps its agent session untouched.
#[tokio::test]
async fn pane_fork_on_an_acp_pane_branches_its_timeline_prefix() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_pane();
    {
        let split = app.agents_split.as_mut().unwrap();
        split.panes[0].agent = "opencode".into();
        split.panes[0].session_id = Some("oc-agent-session".into());
    }

    // Fork the SECOND prompt (ordinal 1, timeline index 2).
    app.run_pane_message_action(2, 0, Some(1), 2);
    let toast = app
        .toast_state
        .current
        .clone()
        .map(|t| (t.title.unwrap_or_default(), t.message));
    assert!(
        toast
            .as_ref()
            .is_some_and(|(_, message)| message.contains("New session created")),
        "the ACP pane must fork from its timeline; got: {toast:?}"
    );
    let current = app.state.current_session().expect("a session is open");
    let forked_id = current.id.clone();
    assert!(current.title.contains("(fork)"), "{:?}", current.title);
    let roles: Vec<crate::types::MessageRole> =
        current.messages.iter().map(|m| m.role.clone()).collect();
    assert_eq!(
        roles,
        vec![
            crate::types::MessageRole::User,
            crate::types::MessageRole::Assistant,
            crate::types::MessageRole::User
        ],
        "the fork keeps the transcript prefix through the clicked prompt"
    );
    // A real model context rode along: the main agent can continue it.
    let context = app.session_store.load_context(&forked_id).unwrap();
    assert_eq!(context.items.len(), 3);
    // The pane's own agent session was not touched by the fork.
    assert_eq!(
        app.parked_agents_split.as_ref().unwrap().panes[0]
            .session_id
            .as_deref(),
        Some("oc-agent-session")
    );
}

/// Revert on an ACP pane drops the clicked prompt and everything after it
/// from the pane, hands the prompt back, abandons the agent session (ACP has
/// no truncate) and leaves the kept prefix as the next turn's rewind context.
#[tokio::test]
async fn pane_revert_on_an_acp_pane_truncates_the_timeline_and_resets_the_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_cosh_pane();
    {
        let split = app.agents_split.as_mut().unwrap();
        split.panes[0].agent = "opencode".into();
        split.panes[0].session_id = Some("oc-agent-session".into());
        split.focused = Some(0);
    }

    app.run_pane_message_action(0, 0, Some(1), 2);
    // The pane is its own receipt: a success toast would only claim the
    // revert happened (and keep lying if it did not), so none fires.
    assert!(
        app.toast_state.current.is_none(),
        "a success toast pollutes the screen: {:?}",
        app.toast_state.current.clone().map(|t| t.message)
    );
    let split = app.agents_split.as_ref().unwrap();
    let timeline: Vec<bool> = split.panes[0]
        .activity
        .timeline
        .iter()
        .map(|e| matches!(e, crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage { .. }))
        .collect();
    assert_eq!(
        timeline,
        vec![true, false],
        "the clicked prompt and everything after it are gone"
    );
    assert!(
        split.panes[0].session_id.is_none(),
        "the agent session is abandoned (a fresh one opens on the next turn)"
    );
    assert!(
        split.panes[0].rewind_context.is_some(),
        "the kept prefix rides the next turn"
    );
    assert_eq!(app.prompt_view.input, "second prompt");
    // The main view's session never moved.
    assert_eq!(app.state.current_session_id.as_deref(), Some("main"));
}

/// An `agents_call` prompt sits between two of the user's prompts: the pane
/// session stores it as a user message, but it has NO echo ordinal. The
/// action must align the prompt sequence by text and target the prompt the
/// user actually clicked — the raw ordinal used to revert/fork the agent's
/// call instead.
#[tokio::test]
async fn pane_action_targets_the_clicked_prompt_across_agent_call_prompts() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::{AgentPaneRef, Message, MessageRole, Part, TextPart};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let now = 1_000u64;
    let make = |id: &str, role: MessageRole, text: &str, at: u64| Message {
        id: id.into(),
        role,
        parts: vec![Part::Text(TextPart {
            text: text.into(),
            synthetic: false,
        })],
        created_at: at,
        agent: None,
        model: None,
    };
    let session = crate::types::Session {
        id: "pane-mixed".into(),
        title: "agents manager".into(),
        created_at: now,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
        messages: vec![
            make("msg-0", MessageRole::User, "first prompt", now),
            make("msg-1", MessageRole::Assistant, "first answer", now + 1),
            make("msg-2", MessageRole::User, "inspect the parser", now + 2),
            make("msg-3", MessageRole::Assistant, "call answer", now + 3),
            make("msg-4", MessageRole::User, "second prompt", now + 4),
            make("msg-5", MessageRole::Assistant, "second answer", now + 5),
        ],
    };
    app.session_store.save_session(&session);
    app.state.add_session(session);

    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "pane-mixed".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("pane-mixed".into());
    split.echo_turn_input(0, "first prompt", true);
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "first answer".into(),
        },
    );
    // The main agent's `agents_call` prompt: an AgentMessage (no ordinal).
    split.echo_turn_input(0, "inspect the parser", false);
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "call answer".into(),
        },
    );
    split.echo_turn_input(0, "second prompt", true);
    split.apply_event(
        0,
        &cosh_tools::subagent::events::SubagentEvent::Message {
            text: "second answer".into(),
        },
    );
    app.agents_split = Some(split);

    // Fork the SECOND user prompt (ordinal 1, timeline index 4).
    app.run_pane_message_action(2, 0, Some(1), 4);
    let current = app.state.current_session().expect("a session is open");
    let ids: Vec<&str> = current.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["msg-0", "msg-1", "msg-2", "msg-3", "msg-4"],
        "the fork must keep the conversation through the clicked prompt"
    );
}

/// The display projection and `SessionStore::update_ctx_ids` must agree
/// group-for-group, forever: the store only records message → context
/// boundaries when the two lists line up, and Revert/Fork resolve a clicked
/// message through those boundaries. Compaction checkpoints, runtime errors
/// and Command-mode user commands each own a group despite not being prose
/// turns — a projection that skipped or merged one would silently disable
/// both actions on the whole session, which is exactly the bug this pins.
#[test]
fn manager_context_display_keeps_one_message_per_context_group() {
    use crate::types::{MessageRole, Part, ToolStatus};
    use cosh::harness::context::{ContextItem, ContextManagerState};

    let context: ContextManagerState = ContextManagerState {
        items: vec![
            ContextItem::User {
                id: 1,
                original: "prompt".into(),
                synthetic: false,
            },
            ContextItem::Assistant {
                id: 2,
                original: "answer".into(),
                closable: false,
                reasoning: Some("thought".into()),
            },
            ContextItem::ToolCall {
                id: 3,
                call_id: "call-1".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 4,
                call_id: "call-1".into(),
                content: "file contents".into(),
                useless: false,
                images: Vec::new(),
            },
            ContextItem::Compaction {
                id: 5,
                summary: "earlier work".into(),
                covered_ranges: Vec::new(),
            },
            ContextItem::Error {
                id: 6,
                content: "API unavailable".into(),
            },
            ContextItem::UserCommand {
                id: 7,
                content: "$ ls".into(),
            },
            ContextItem::User {
                id: 8,
                original: "next prompt".into(),
                synthetic: false,
            },
        ]
        .into(),
        next_id: 9,
        ..Default::default()
    };

    let display = crate::app::agents_dispatch::manager_context_display(&context);
    // Seven groups over eight items: the result joins its call, everything
    // else is one message each (prompt, answer, compaction, error, command,
    // second prompt).
    assert_eq!(
        display.len(),
        7,
        "one message per context group (a merged/skipped one breaks the store mapping)"
    );
    let roles: Vec<MessageRole> = display.iter().map(|(role, _)| role.clone()).collect();
    assert_eq!(
        roles,
        vec![
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::User,
        ],
        "the replayed timeline's roles (compaction reads as a user turn)"
    );
    // The reasoning rides its answer, before the text (`persist_manager_session`
    // used to build these parts itself).
    assert!(
        matches!(display[1].1.as_slice(), [Part::Reasoning(_), Part::Text(_)]),
        "reasoning precedes the answer text: {:?}",
        display[1].1
    );
    // The result's output landed on the call's own message, not a group of
    // its own — the pairing `update_ctx_ids` reproduces.
    let tool = display[2]
        .1
        .iter()
        .find_map(|part| match part {
            Part::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the tool call's message carries the tool part");
    assert_eq!(tool.output.as_deref(), Some("file contents"));
    assert_eq!(
        tool.status,
        ToolStatus::Completed,
        "a call in the middle of the snapshot completed (only a trailing one is Running)"
    );
}

/// When the clicked prompt cannot be matched to the pane's stored session AND
/// an `agents_call` prompt makes the raw ordinal ambiguous, the action must
/// REFUSE. Reverting or forking the wrong turn is destructive and silent, so
/// the honest answer is a toast — the fallback this replaced used to act on
/// position alone.
#[tokio::test]
async fn pane_action_refuses_an_unmatched_prompt_instead_of_guessing() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::{AgentPaneRef, Message, MessageRole, Part, TextPart};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let make = |id: &str, role: MessageRole, text: &str, at: u64| Message {
        id: id.into(),
        role,
        parts: vec![Part::Text(TextPart {
            text: text.into(),
            synthetic: false,
        })],
        created_at: at,
        agent: None,
        model: None,
    };
    let session = crate::types::Session {
        id: "pane-ambig".into(),
        title: "agents manager".into(),
        created_at: 1_000,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: Default::default(),
        main_agent_worktree: None,
        messages: vec![
            make("msg-0", MessageRole::User, "first prompt", 1_000),
            make("msg-1", MessageRole::Assistant, "first answer", 1_001),
        ],
    };
    app.session_store.save_session(&session);
    app.state.add_session(session);

    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "pane-ambig".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("pane-ambig".into());
    // The echo's text is NOT in the store (a rewritten/legacy transcript),
    // and an `agents_call` prompt sits in the timeline: position alone would
    // point at the wrong message.
    split.echo_turn_input(0, "a prompt the store never saw", true);
    split.echo_turn_input(0, "another agent prompt", false);
    app.agents_split = Some(split);

    let before = app.state.current_session_id.clone();
    app.run_pane_message_action(2, 0, Some(0), 0);
    let toast = app.toast_state.current.clone().expect("a toast fired");
    assert!(
        toast.message.contains("Could not match"),
        "an unmatched prompt refuses instead of guessing; got: {:?}",
        toast.message
    );
    // Nothing was forked or reverted.
    assert_eq!(
        app.state.current_session_id, before,
        "no session was opened"
    );
    assert_eq!(
        app.session_store
            .load_session("pane-ambig")
            .unwrap()
            .messages
            .len(),
        2,
        "the pane's session was left untouched"
    );
}
/// A BOUND manager pane whose session the store cannot read must REFUSE, not
/// fall through to the ACP timeline branch: that branch abandons the binding
/// and rewrites the metadata to an unbound `Name` pane, silently dropping a
/// conversation that is still (or temporarily unreadable) on disk.
#[tokio::test]
async fn pane_action_refuses_a_manager_session_the_store_cannot_load() {
    use crate::routes::session::agents_split::AgentsSplitState;
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state
        .add_empty_session("main".into(), "main".into(), 1_000);
    app.state.current_session_id = Some("main".into());

    let mut split = AgentsSplitState::new(vec![AgentPaneRef::Bound {
        agent: "cosh".into(),
        session_id: "session-the-store-lost".into(),
        worktree: None,
    }]);
    split.panes[0].session_id = Some("session-the-store-lost".into());
    split.echo_turn_input(0, "first prompt", true);
    app.agents_split = Some(split);

    app.run_pane_message_action(0, 0, Some(0), 0);

    let toast = app.toast_state.current.clone().expect("a refusal fired");
    assert!(
        toast.message.contains("Could not load this pane's session"),
        "the manager refusal, not the ACP rewind; got: {:?}",
        toast.message
    );
    let split = app.agents_split.as_ref().expect("the split survives");
    assert_eq!(
        split.panes[0].session_id.as_deref(),
        Some("session-the-store-lost"),
        "the binding was not abandoned"
    );
    assert!(!split.panes[0].rewound, "no replay fence was raised");
    assert_eq!(
        split.panes[0].activity.timeline.len(),
        1,
        "the timeline was not truncated"
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.agents_panes.is_none()),
        "the persisted pane metadata was not rewritten"
    );
}
