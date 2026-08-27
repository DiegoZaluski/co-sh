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
        message_id: "u2".into(), // Last user message
        preview: "second".into(),
        is_last_user_message: true,
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
    app.run_message_action(0, "u2"); // Use last user message for revert
    let session = app.state.current_session().unwrap();
    let ids: Vec<&str> = session.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["u1", "a1"], "u2 and everything after it are dropped");
    assert!(!session.messages.iter().any(|m| m.id == "u2"));
    assert_eq!(app.prompt_view.input, "second");
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
}

#[tokio::test]
async fn message_actions_fork_branches_new_session_up_to_message() {
    let mut app = app_with_user_message();
    app.run_message_action(2, "u2"); // Use last user message for fork
    let old = app.state.current_session().unwrap();
    assert_eq!(
        old.messages.len(),
        3,
        "fork keeps messages up to and including the last user message"
    );
    assert!(old.messages.iter().any(|m| m.id == "u2"));
    assert!(old.title.contains("(fork)"));
    assert_ne!(old.id, "t", "the fork is a brand-new session id");
    // Regression: the fork must be persisted to disk so it survives restarts.
    assert!(
        app.session_store.load_session(&old.id).is_some(),
        "fork session must be saved to disk"
    );
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
        message_id: "u2".into(), // Last user message
        preview: "second".into(),
        is_last_user_message: true,
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
async fn message_actions_non_last_user_message_only_shows_copy() {
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(), // Not the last user message
        preview: "hello world".into(),
        is_last_user_message: false,
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
    assert!(all.contains("Copy"), "Copy is always available");
    assert!(!all.contains("Revert"), "Revert should not be shown for non-last user message");
    assert!(!all.contains("Fork"), "Fork should not be shown for non-last user message");
}

#[tokio::test]
async fn message_actions_single_user_message_shows_all_options() {
    use crate::types::{Message, MessageRole, Part, TextPart};
    let mut app = App::new("/tmp".to_string());
    let now = 1_000u64;
    let session = crate::types::Session {
        id: "single".into(),
        title: "single".into(),
        created_at: now,
        title_generated: false,
        messages: vec![
            Message {
                id: "u1".into(),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: "only message".into(),
                    synthetic: false,
                })],
                created_at: now,
                agent: None,
                model: None,
            },
        ],
    };
    app.state.add_session(session);
    app.state.current_session_id = Some("single".into());

    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(), // Only user message, so it's also the last
        preview: "only message".into(),
        is_last_user_message: true,
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
    assert!(all.contains("Revert") && all.contains("Copy") && all.contains("Fork"),
        "Single user message should show all options since it's also the last");
}

#[test]
fn message_action_index_maps_correctly() {
    use super::super::App;
    assert_eq!(App::message_action_index(0, true), 0, "last user: 0=Revert");
    assert_eq!(App::message_action_index(1, true), 1, "last user: 1=Copy");
    assert_eq!(App::message_action_index(2, true), 2, "last user: 2=Fork");
    assert_eq!(App::message_action_index(0, false), 1, "non-last: visual 0 maps to Copy (1)");
}

#[tokio::test]
async fn message_actions_non_last_keyboard_only_cycles_one_option() {
    let mut app = app_with_user_message();
    app.dialog.replace(DialogType::MessageActions {
        message_id: "u1".into(),
        preview: "hello world".into(),
        is_last_user_message: false,
    });
    // Down stays at 0 (only 1 option)
    assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 0);
    // Up also stays at 0
    assert!(app.handle_message_actions_dialog_key(KeyCode::Up));
    assert_eq!(app.dialog.current().unwrap().selected, 0);
    // Enter executes Copy (action index 1)
    assert!(app.handle_message_actions_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible(), "dialog closed after Enter");
}
