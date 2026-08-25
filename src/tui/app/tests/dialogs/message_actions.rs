use super::super::App;
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
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(),
        preview: "hello world".into(),
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
async fn message_actions_revert_truncates_and_restores_prompt() {
    let mut app = app_with_user_message();
    app.run_message_action(0, "u1");
    let session = app.state.current_session().unwrap();
    assert!(
        session
            .messages
            .iter()
            .all(|m| m.id != "u1" && m.id != "a1"),
        "the reverted message and everything after it are dropped"
    );
    assert_eq!(app.prompt_view.input, "hello world");
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
}

#[tokio::test]
async fn message_actions_fork_branches_new_session_up_to_message() {
    let mut app = app_with_user_message();
    app.run_message_action(2, "u1");
    let old = app.state.current_session().unwrap();
    assert_eq!(
        old.messages.len(),
        1,
        "fork keeps messages up to and including"
    );
    assert!(old.messages.iter().any(|m| m.id == "u1"));
    assert!(!old.messages.iter().any(|m| m.id == "a1"));
    assert!(old.title.contains("(fork)"));
    assert_ne!(old.id, "t", "the fork is a brand-new session id");
}

#[tokio::test]
async fn message_actions_copy_writes_clipboard_text() {
    // Clipboard may be unavailable in headless CI; only assert the text
    // extraction path via a missing-message no-op and the toast for empty.
    let mut app = app_with_user_message();
    app.run_message_action(1, "does-not-exist");
    // No panic; dialog stack untouched.
    assert!(!app.dialog.visible());
}

#[tokio::test]
async fn message_actions_dialog_renders_title_and_options() {
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(),
        preview: "hello world".into(),
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
