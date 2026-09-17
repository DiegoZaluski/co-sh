//! Export of the session's model-facing transcript to Markdown.
//!
//! The export transpiles exactly the view the agent sees at this moment: the
//! persisted context records replayed through
//! [`cosh::harness::context::ContextManager::export_markdown`], whose
//! projection mirrors `build_messages` (the `visible_from` boundary, the
//! `hidden` set, checkpoint coverage and masked tool results all apply).
//! Nothing else reaches the file: no JSONL envelope, no event ids, no
//! display-only bookkeeping — each context item becomes one Markdown section
//! separated by a blank line.
//!
//! The file is written to the user's current working directory as
//! `{session-name}-{uuid}.md`, where the name is the session title sanitized
//! into a slug and the uuid is a fresh v4 identifier (OS entropy, the same
//! generator the telemetry pipeline uses).

use std::path::{Path, PathBuf};

use cosh::harness::context::ContextManager;

use crate::session_store::SessionStore;
use crate::types::Session;

/// Export the CURRENT agent-visible transcript of `session` as Markdown into
/// `cwd`, returning the path of the written file.
///
/// The agent's view lives in the persisted context records, not in the
/// display messages: when the store has no embedded context for the session
/// (a display-only history) the export cannot be honest about what the agent
/// sees and fails instead of exporting a different transcript.
pub fn export_session_transcript(
    store: &SessionStore,
    session: &Session,
    cwd: &Path,
) -> std::io::Result<PathBuf> {
    let state = store.load_context(&session.id).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the session has no recorded agent context yet",
        )
    })?;
    let mut manager = ContextManager::new(state.max_tokens);
    manager.restore_state(&state);
    let transcript = manager.export_markdown();
    if transcript.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the agent-visible transcript is empty",
        ));
    }
    let uuid = cosh::telemetry::events::uuid_v4().ok_or_else(|| {
        std::io::Error::other("OS entropy unavailable for the export file name")
    })?;
    let path = cwd.join(format!("{}-{uuid}.md", slugify(&session.title)));
    std::fs::write(&path, transcript)?;
    Ok(path)
}

/// Slugify a session title into a file-name-safe base: lowercase ASCII
/// alphanumerics, runs of anything else collapsed into single dashes. Empty
/// titles fall back to a neutral base so the file is still nameable.
fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut dashed = true; // trim leading dashes
    for ch in title.chars() {
        let lower: Option<char> = match ch {
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match lower {
            Some(c) => {
                slug.push(c);
                dashed = false;
            }
            None if !dashed => {
                slug.push('-');
                dashed = true;
            }
            None => {}
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    const MAX_SLUG: usize = 60;
    while slug.len() > MAX_SLUG {
        slug.pop();
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, MessageRole, Part, TextPart};
    use cosh::harness::context::{ContextItem, ContextManagerState};
    use std::collections::{HashMap, HashSet, VecDeque};

    fn session(id: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            created_at: 0,
            messages: vec![Message {
                id: "msg-0".into(),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: "hello".into(),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            }],
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            ctx_ids: HashMap::new(),
        }
    }

    fn state(items: Vec<ContextItem>) -> ContextManagerState {
        ContextManagerState {
            items: items.into_iter().collect::<VecDeque<_>>(),
            next_id: 42,
            max_tokens: 100_000,
            overflow_model: None,
            split: None,
            map_reduce: None,
            visible_from: None,
            hidden: HashSet::new(),
            masked: HashSet::new(),
            todo: None,
            last_tool_set: None,
        }
    }

    #[test]
    fn slugify_produces_file_name_safe_slugs() {
        assert_eq!(slugify("Fix the Build!"), "fix-the-build");
        assert_eq!(slugify("  spaces  everywhere "), "spaces-everywhere");
        assert_eq!(slugify("açores não-ASCII"), "a-ores-n-o-ascii");
        assert_eq!(slugify("---"), "session");
        assert_eq!(slugify(""), "session");
        assert!(slugify(&"long ".repeat(40)).len() <= 60);
    }

    #[test]
    fn export_writes_the_agent_view_and_hides_invisible_items() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("hash");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = SessionStore::with_dir(sessions_dir, "hash".into());
        let session = session("export-1", "Export Me");
        let mut hidden = HashSet::new();
        hidden.insert(2);
        let context = ContextManagerState {
            hidden,
            ..state(vec![
                ContextItem::User {
                    id: 1,
                    original: "the prompt".into(),
                },
                ContextItem::Assistant {
                    id: 2,
                    original: "hidden answer".into(),
                    closable: true,
                },
                ContextItem::Assistant {
                    id: 3,
                    original: "visible answer".into(),
                    closable: true,
                },
            ])
        };
        store.save_session_with_context(&session, &context);

        let path =
            export_session_transcript(&store, &session, dir.path()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("export-me-"), "slug prefix, got {name}");
        assert!(name.ends_with(".md"));
        // `session id` length + uuid shape: "export-me-" + 36 chars + ".md".
        assert_eq!(name.len(), "export-me-".len() + 36 + 3);

        let transcript = std::fs::read_to_string(&path).unwrap();
        assert!(transcript.contains("## User\n\nthe prompt"));
        assert!(transcript.contains("## Assistant\n\nvisible answer"));
        assert!(
            !transcript.contains("hidden answer"),
            "hidden items are outside the agent's view"
        );
    }

    #[test]
    fn export_refuses_a_display_only_session() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("hash");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = SessionStore::with_dir(sessions_dir, "hash".into());
        let session = session("export-2", "Display Only");
        // A display-only save: no context records at all.
        store.save_session(&session);

        let result = export_session_transcript(&store, &session, dir.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }
}
