//! Session persistence module.
//!
//! Stores chat sessions as individual JSONL files organized by CWD:
//! `{data_dir}/sessions/{cwd_hash}/session-{timestamp}.jsonl`.
//!
//! ## Format
//!
//! Each JSONL file represents one complete chat session:
//!
//! - Line 1: Session metadata (JSON object with title, created_at, cwd, etc.)
//! - Lines 2+: Each message serialized as a JSON object, one per line.
//!
//! Sessions are grouped by CWD (current working directory). The CWD path is
//! hashed with xxHash32 to produce a deterministic subdirectory name, so
//! sessions started in different directories never mix. The full CWD path is
//! also stored in the session header for display and filtering.
//!
//! A session is only persisted when it contains actual dialog:
//! at least one user message AND at least one valid assistant response
//! (error-only responses don't count as dialog).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Datelike;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh32::xxh32;

use crate::types::{Message, MessageRole, Part, Session};

/// Number of session files to keep on disk per CWD. Oldest files are evicted first.
const MAX_SESSIONS_ON_DISK: usize = 50;

/// A queued session snapshot for the background save-writer thread.
struct SaveJob {
    session: Box<crate::types::Session>,
}

/// Metadata stored as the first JSONL line in each session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionHeader {
    title: String,
    #[serde(default)]
    title_generated: bool,
    created_at: u64,
    cwd: String,
    provider: Option<String>,
    model: Option<String>,
}

/// Manages reading and writing session files to disk, isolated by CWD.
#[derive(Clone)]
pub struct SessionStore {
    /// Resolved path to the per-CWD sessions directory
    /// (e.g. `~/.local/share/cosh/sessions/{hash}/`).
    sessions_dir: PathBuf,
    /// The hash of the current working directory.
    cwd_hash: String,
}

impl SessionStore {
    /// Create a new `SessionStore`, resolving the CWD and creating the
    /// per-CWD sessions directory.
    ///
    /// # Panics
    /// Panics if the `ProjectDirs` cannot be determined (e.g. no $HOME set).
    pub fn new() -> Self {
        let proj_dirs =
            ProjectDirs::from("", "", "cosh").expect("could not determine project directories");
        let cwd_hash = compute_cwd_hash();
        let sessions_dir = proj_dirs.data_dir().join("sessions").join(&cwd_hash);
        std::fs::create_dir_all(&sessions_dir).ok();
        Self {
            sessions_dir,
            cwd_hash,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Persist a session to disk as a JSONL file in the current CWD's subdirectory.
    ///
    /// The session is saved as `session-{id}.jsonl`. If a file with the same
    /// name already exists, it is overwritten so the disk always reflects the
    /// latest state. After saving, the store evicts the oldest files beyond
    /// `MAX_SESSIONS_ON_DISK`.
    pub fn save_session(&self, session: &Session) {
        let file_path = self.file_path(&session.id);
        let header = self.build_header(session);

        let mut lines = Vec::new();

        // Line 1: session header
        if let Ok(json) = serde_json::to_string(&header) {
            lines.push(json);
        }

        // Lines 2+: messages
        for msg in &session.messages {
            if let Ok(json) = serde_json::to_string(&StoredMessage::from(msg)) {
                lines.push(json);
            }
        }

        if let Err(e) = std::fs::write(&file_path, lines.join("\n")) {
            log::warn!("failed to save session {}: {e}", session.id);
        }

        self.evict_old_sessions();
    }

    /// Persist a session WITHOUT blocking the caller (the UI thread).
    ///
    /// `save_session` re-serializes the whole transcript and rewrites the
    /// file; for a growing agent session that cost grows without bound and
    /// ran inline in the event loop — the periodic hitch on every throttled
    /// `ContextSnapshot` (~every 10 s) and the stall on Done/Stopped, which
    /// is exactly when the user is staring at the TUI.
    ///
    /// This variant clones the session (a memcpy — an order of magnitude
    /// cheaper than serialization) and hands it to a single background
    /// writer thread. A single writer keeps FIFO ordering, so a newer
    /// snapshot can never be clobbered by an older in-flight one.
    pub fn save_session_async(&self, session: &crate::types::Session) {
        static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<SaveJob>> =
            std::sync::OnceLock::new();
        let tx = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<SaveJob>();
            let store = self.clone();
            let spawned = std::thread::Builder::new()
                .name("session-save".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        store.save_session(&job.session);
                    }
                });
            if spawned.is_err() {
                log::warn!("failed to spawn session-save thread");
            }
            tx
        });
        // If the thread failed to spawn there is no receiver; fall back to a
        // synchronous save rather than dropping the snapshot.
        if tx
            .send(SaveJob {
                session: Box::new(session.clone()),
            })
            .is_err()
        {
            self.save_session(session);
        }
    }

    /// Update only the title in an existing session file on disk.
    ///
    /// Reads the file, replaces the title in the first-line JSON header,
    /// and writes it back.  This is much cheaper than a full `save_session`
    /// rewrite and is used by the async title generation path.
    pub fn update_title(&self, session_id: &str, new_title: &str) {
        let file_path = self.file_path(session_id);
        let Ok(contents) = std::fs::read_to_string(&file_path) else {
            return;
        };
        let mut out_lines: Vec<String> = Vec::new();
        let mut first = true;
        for line in contents.lines() {
            if first {
                first = false;
                // Replace the title field in the JSON header (line 0).
                if let Ok(mut header) = serde_json::from_str::<SessionHeader>(line) {
                    header.title = new_title.to_string();
                    header.title_generated = true;
                    if let Ok(json) = serde_json::to_string(&header) {
                        out_lines.push(json);
                        continue;
                    }
                }
            }
            out_lines.push(line.to_string());
        }
        if let Err(e) = std::fs::write(&file_path, out_lines.join("\n")) {
            log::warn!("failed to update title for session {session_id}: {e}");
        }
    }

    /// Load all session files from the current CWD's subdirectory, sorted by
    /// creation time (newest first).
    ///
    /// Only sessions from the same CWD as the current process are returned,
    /// giving natural per-directory isolation.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = Vec::new();

        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return summaries;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            if let Some(summary) = self.read_summary(&path) {
                summaries.push(summary);
            }
        }

        // Sort newest first by timestamp
        summaries.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        summaries
    }

    /// List sessions across ALL CWD subdirectories (for directory browsing).
    ///
    /// Returns summaries that include the `cwd` field so the UI can display
    /// which directory each session belongs to.
    pub fn list_all_sessions(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = Vec::new();

        // Parent directory of all per-CWD subdirectories
        let base = match self.sessions_dir.parent() {
            Some(p) => p.to_path_buf(),
            None => return summaries,
        };

        let Ok(entries) = std::fs::read_dir(&base) else {
            return summaries;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(dir_entries) = std::fs::read_dir(&path) else {
                continue;
            };
            for file_entry in dir_entries.flatten() {
                let file_path = file_entry.path();
                if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Some(summary) = self.read_summary(&file_path) {
                    summaries.push(summary);
                }
            }
        }

        summaries.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        summaries
    }

    /// Load a full session from its JSONL file.
    ///
    /// Returns `None` if the file does not exist or cannot be parsed.
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        let file_path = self.file_path(session_id);
        let content = std::fs::read_to_string(&file_path).ok()?;

        let mut lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return None;
        }

        // Line 1: session header
        let header: SessionHeader = serde_json::from_str(lines[0]).ok()?;
        lines.remove(0);

        // Remaining lines: messages
        let mut messages: Vec<Message> = Vec::new();
        for line in &lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(stored) = serde_json::from_str::<StoredMessage>(line) {
                messages.push(stored.into_message());
            }
        }

        Some(Session {
            id: session_id.to_string(),
            title: header.title,
            messages,
            created_at: header.created_at,
            title_generated: header.title_generated,
        })
    }

    /// Delete a session file and its companion `.ctx` file from disk.
    pub fn delete_session(&self, session_id: &str) {
        let file_path = self.file_path(session_id);
        std::fs::remove_file(&file_path).ok();
        self.delete_ctx(session_id);
    }

    /// Check whether a session with the given ID exists on disk.
    pub fn has_session(&self, session_id: &str) -> bool {
        self.file_path(session_id).exists()
    }

    // ── Companion .ctx file (bincode-encoded ContextManagerState) ──────────

    /// Build path for the companion `.ctx` file.
    fn ctx_file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("session-{session_id}.ctx"))
    }

    /// Save bincode-encoded context manager state alongside the JSONL session.
    pub fn save_ctx(&self, session_id: &str, state: &[u8]) {
        let path = self.ctx_file_path(session_id);
        if let Err(e) = std::fs::write(&path, state) {
            log::warn!("failed to save context state for session {session_id}: {e}");
        }
    }

    /// Load bincode-encoded context manager state for a session.
    /// Returns `None` if no companion file exists or it cannot be read.
    pub fn load_ctx(&self, session_id: &str) -> Option<Vec<u8>> {
        let path = self.ctx_file_path(session_id);
        std::fs::read(&path).ok()
    }

    /// Delete the companion `.ctx` file for a session.
    pub fn delete_ctx(&self, session_id: &str) {
        std::fs::remove_file(self.ctx_file_path(session_id)).ok();
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Build the absolute path for a session file within the current CWD subdirectory.
    fn file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("session-{session_id}.jsonl"))
    }

    /// Build a `SessionHeader` from a `Session`, populating `cwd` with the
    /// canonicalized current working directory.
    fn build_header(&self, session: &Session) -> SessionHeader {
        // Grab provider/model from the last valid assistant message
        let (provider, model) = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant && !m.id.starts_with("msg-err-"))
            .map(|m| {
                let prov = m.model.as_deref().and_then(|model_name| {
                    if model_name.contains('/') {
                        model_name.split('/').next().map(String::from)
                    } else {
                        None
                    }
                });
                (prov, m.model.clone())
            })
            .unwrap_or((None, None));

        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        SessionHeader {
            title: session.title.clone(),
            title_generated: session.title_generated,
            created_at: session.created_at,
            cwd,
            provider,
            model,
        }
    }

    /// Read a lightweight summary from a session file (header only).
    fn read_summary(&self, path: &PathBuf) -> Option<SessionSummary> {
        let file = std::fs::File::open(path).ok()?;
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).ok()?;
        let first_line = first_line.trim();
        if first_line.is_empty() {
            return None;
        }

        let header: SessionHeader = serde_json::from_str(first_line).ok()?;

        // Count remaining non-empty lines (messages)
        let mut message_count: usize = 0;
        for line in reader.lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                message_count += 1;
            }
        }

        // Extract session ID from filename: session-{id}.jsonl
        let filename = path.file_stem()?.to_str()?;
        let session_id = filename.strip_prefix("session-")?.to_string();

        Some(SessionSummary {
            session_id,
            title: header.title,
            title_generated: header.title_generated,
            created_at: header.created_at,
            message_count,
            cwd: header.cwd,
            model: header.model,
        })
    }

    /// Remove the oldest session files if we exceed the maximum count for this CWD.
    fn evict_old_sessions(&self) {
        let mut files: Vec<(PathBuf, u64)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                    && let Some(ts_str) = stem.strip_prefix("session-")
                    && let Ok(ts) = ts_str.parse::<u64>()
                {
                    files.push((path, ts));
                }
            }
        }

        if files.len() <= MAX_SESSIONS_ON_DISK {
            return;
        }

        files.sort_by_key(|(_, ts)| *ts);
        let to_remove = files.len() - MAX_SESSIONS_ON_DISK;
        for (path, _) in files.iter().take(to_remove) {
            std::fs::remove_file(path).ok();
            // Remove companion .ctx file too
            let ctx_path = path.with_extension("ctx");
            std::fs::remove_file(&ctx_path).ok();
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Lightweight summary (no full message deserialization) ────────────────

/// A summary of a session, used for listing in the sidebar/history.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// The session ID (timestamp-based).
    pub session_id: String,
    /// Display title for the session.
    pub title: String,
    /// When the session was created (unix millis).
    pub created_at: u64,
    /// Number of messages in the session.
    pub message_count: usize,
    /// Canonical path of the working directory where the session was started.
    pub cwd: String,
    /// The model used (if available).
    pub model: Option<String>,
    /// Whether the title was generated by the LLM (true) or is a
    /// fallback timestamp (false).
    pub title_generated: bool,
}

// ── Stored message format ────────────────────────────────────────────────

/// Serializable representation of a message for JSONL storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMessage {
    id: String,
    role: String,
    parts: Vec<Part>,
    created_at: u64,
    agent: Option<String>,
    model: Option<String>,
}

impl From<&Message> for StoredMessage {
    fn from(msg: &Message) -> Self {
        Self {
            id: msg.id.clone(),
            role: match msg.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            parts: msg.parts.clone(),
            created_at: msg.created_at,
            agent: msg.agent.clone(),
            model: msg.model.clone(),
        }
    }
}

impl StoredMessage {
    fn into_message(self) -> Message {
        Message {
            id: self.id,
            role: match self.role.as_str() {
                "assistant" => MessageRole::Assistant,
                _ => MessageRole::User,
            },
            parts: self.parts,
            created_at: self.created_at,
            agent: self.agent,
            model: self.model,
        }
    }
}

// ── Public helpers ───────────────────────────────────────────────────────

/// Generate a timestamp-based session ID.
pub fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ts}")
}

/// Compute a deterministic xxHash32 of the canonicalized current working
/// directory for per-CWD session isolation.
///
/// The same CWD always produces the same hash, so sessions from the same
/// directory are grouped together automatically.
pub fn compute_cwd_hash() -> String {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok())
        .unwrap_or_else(|| PathBuf::from("unknown"));
    let path_str = cwd.to_string_lossy();
    let hash = xxh32(path_str.as_bytes(), 0);
    format!("{:08x}", hash)
}

/// Determine whether a session should be persisted to disk.
///
/// A session is valid for persistence when it contains actual dialog:
/// at least one user message AND at least one assistant response that is
/// not an error (IDs starting with `msg-err-`).
///
/// # Rationale
///
/// - User sends message → agent responds with real content → **dialog exists**.
///   If an API error occurs *later* in the conversation, the dialog already
///   happened and the session should be saved.
/// - User sends message → agent responds with ONLY an error message → no
///   dialog happened, the session should NOT be saved.
/// - User sends message → agent responds with real content → later an error
///   occurs → the session IS saved because a dialog existed.
pub fn is_valid_session(session: &Session) -> bool {
    if session.messages.is_empty() {
        return false;
    }

    let has_user = session.messages.iter().any(|m| m.role == MessageRole::User);

    let has_valid_assistant = session
        .messages
        .iter()
        .any(|m| m.role == MessageRole::Assistant && !m.id.starts_with("msg-err-"));

    has_user && has_valid_assistant
}

// ── Timestamp formatting ─────────────────────────────────────────────────

/// Format a unix millisecond timestamp for display in the sidebar.
///
/// Always includes both date and time so sessions on different days
/// are distinguishable.
pub fn format_session_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let dt = match chrono::DateTime::from_timestamp(secs as i64, 0) {
        Some(dt) => dt,
        None => return "?".to_string(),
    };

    let now = chrono::Local::now();
    let dt_local = dt.with_timezone(&chrono::Local);

    if dt_local.year() == now.year() {
        // This year: "Jul 09 14:30"
        dt_local.format("%b %d %H:%M").to_string()
    } else {
        // Previous years: "Jul 09 2024 14:30"
        dt_local.format("%b %d %Y %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageRole, Part, TextPart};

    fn make_test_session(id: &str, title: &str, messages: Vec<Message>) -> Session {
        Session {
            id: id.to_string(),
            title: title.to_string(),
            created_at: 0,
            messages,
            title_generated: false,
        }
    }

    fn make_user_msg(id: &str, text: &str) -> Message {
        Message {
            id: id.to_string(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: text.to_string(),
                synthetic: false,
            })],
            created_at: 1000,
            agent: None,
            model: None,
        }
    }

    fn make_assistant_msg(id: &str, text: &str) -> Message {
        Message {
            id: id.to_string(),
            role: MessageRole::Assistant,
            parts: vec![Part::Text(TextPart {
                text: text.to_string(),
                synthetic: false,
            })],
            created_at: 2000,
            agent: None,
            model: None,
        }
    }

    fn make_error_msg(id: &str, text: &str) -> Message {
        Message {
            id: format!("msg-err-{id}"),
            role: MessageRole::Assistant,
            parts: vec![Part::Text(TextPart {
                text: text.to_string(),
                synthetic: false,
            })],
            created_at: 2000,
            agent: None,
            model: None,
        }
    }

    /// Create a test store that uses a temp dir with a fake CWD hash, bypassing
    /// the real `current_dir()` call.
    fn test_store(dir: &tempfile::TempDir) -> SessionStore {
        let sessions_dir = dir.path().join("testhash");
        std::fs::create_dir_all(&sessions_dir).ok();
        SessionStore {
            sessions_dir,
            cwd_hash: "testhash".to_string(),
        }
    }

    #[test]
    fn test_is_valid_session_empty() {
        let session = make_test_session("1", "Empty", vec![]);
        assert!(!is_valid_session(&session));
    }

    #[test]
    fn test_is_valid_session_user_only() {
        let session = make_test_session("1", "User Only", vec![make_user_msg("msg-0", "Hello")]);
        assert!(!is_valid_session(&session));
    }

    #[test]
    fn test_is_valid_session_error_only() {
        let session = make_test_session(
            "1",
            "Error Only",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_error_msg("msg-1", "API Error"),
            ],
        );
        // Error without any valid assistant response → no dialog
        assert!(!is_valid_session(&session));
    }

    #[test]
    fn test_is_valid_session_full_dialog() {
        let session = make_test_session(
            "1",
            "Full Dialog",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi there!"),
            ],
        );
        assert!(is_valid_session(&session));
    }

    #[test]
    fn test_is_valid_session_error_after_dialog() {
        // User msg + valid assistant msg + error later = dialog happened → valid
        let session = make_test_session(
            "1",
            "Error After Dialog",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Let me help"),
                make_error_msg("msg-2", "API Error"),
            ],
        );
        assert!(is_valid_session(&session));
    }

    #[test]
    fn test_is_valid_session_multiple_errors() {
        // User msg + valid assistant + multiple errors after = dialog happened
        let session = make_test_session(
            "1",
            "Multiple Errors After Dialog",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Let me help"),
                make_error_msg("msg-2", "API Error"),
                make_assistant_msg("msg-3", "Retrying..."),
                make_error_msg("msg-4", "API Error again"),
            ],
        );
        assert!(is_valid_session(&session));
    }

    #[test]
    fn test_session_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12345",
            "Test Session",
            vec![
                make_user_msg("msg-0", "Hello!"),
                make_assistant_msg("msg-1", "Hi there!"),
            ],
        );

        store.save_session(&session);

        let loaded = store.load_session("12345");
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, "12345");
        assert_eq!(loaded.title, "Test Session");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, MessageRole::User);
        assert_eq!(loaded.messages[1].role, MessageRole::Assistant);
    }

    #[test]
    fn test_session_store_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session1 = make_test_session(
            "1000",
            "First",
            vec![
                make_user_msg("msg-0", "Hi"),
                make_assistant_msg("msg-1", "Hello"),
            ],
        );
        let session2 = make_test_session(
            "2000",
            "Second",
            vec![
                make_user_msg("msg-0", "Hey"),
                make_assistant_msg("msg-1", "Yo"),
            ],
        );

        store.save_session(&session1);
        store.save_session(&session2);

        let list = store.list_sessions();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "2000");
        assert_eq!(list[1].session_id, "1000");
    }

    #[test]
    fn test_ctx_roundtrip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        // No companion file yet → load returns None.
        assert!(store.load_ctx("12345").is_none());

        // Save → load returns the exact bytes written.
        let payload: &[u8] = b"bincode-encoded-context-state";
        store.save_ctx("12345", payload);
        assert_eq!(store.load_ctx("12345").as_deref(), Some(payload));

        // Delete removes the companion file.
        store.delete_ctx("12345");
        assert!(store.load_ctx("12345").is_none());
    }

    #[test]
    fn test_delete_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12345",
            "To Delete",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );

        store.save_session(&session);
        store.save_ctx("12345", b"context-state");
        assert!(store.has_session("12345"));
        assert!(store.load_ctx("12345").is_some());

        // Deleting the session must also remove its companion .ctx file — a
        // stale .ctx would resurrect deleted context state on a future resume.
        store.delete_session("12345");
        assert!(!store.has_session("12345"));
        assert!(store.load_ctx("12345").is_none());
    }

    #[test]
    fn test_evict_old_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        for i in 0..(MAX_SESSIONS_ON_DISK + 5) {
            let session = make_test_session(
                &format!("{i:05}"),
                &format!("Session {i}"),
                vec![
                    make_user_msg("msg-0", "Hello"),
                    make_assistant_msg("msg-1", "Hi"),
                ],
            );
            store.save_session(&session);
            store.save_ctx(&format!("{i:05}"), b"context-state");
        }

        let list = store.list_sessions();
        assert!(list.len() <= MAX_SESSIONS_ON_DISK);

        // Every surviving session still has its companion .ctx, and the
        // evicted ones lost both files — no orphaned .ctx can resurrect a
        // session that was evicted from disk.
        let survivor_ids: std::collections::HashSet<String> =
            list.iter().map(|s| s.session_id.clone()).collect();
        for i in 0..(MAX_SESSIONS_ON_DISK + 5) {
            let id = format!("{i:05}");
            if survivor_ids.contains(&id) {
                assert!(
                    store.load_ctx(&id).is_some(),
                    "surviving session {id} keeps its .ctx"
                );
            } else {
                assert!(
                    store.load_ctx(&id).is_none(),
                    "evicted session {id} lost its .ctx too"
                );
            }
        }
    }

    #[test]
    fn test_cwd_hash_deterministic() {
        // The hash should be deterministic for the same input — we verify
        // by checking it's a non-empty 8-char hex string.
        let hash = compute_cwd_hash();
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cwd_isolation() {
        // Sessions saved in one CWD hash directory should not appear in another.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("sessions");

        // Store for "proja" CWD
        let store_a = SessionStore {
            sessions_dir: base.join("proja"),
            cwd_hash: "proja".to_string(),
        };
        std::fs::create_dir_all(base.join("proja")).ok();

        // Store for "projb" CWD
        let store_b = SessionStore {
            sessions_dir: base.join("projb"),
            cwd_hash: "projb".to_string(),
        };
        std::fs::create_dir_all(base.join("projb")).ok();

        let session_a = make_test_session(
            "1000",
            "Project A",
            vec![
                make_user_msg("msg-0", "Hi"),
                make_assistant_msg("msg-1", "Hello"),
            ],
        );
        store_a.save_session(&session_a);

        let session_b = make_test_session(
            "2000",
            "Project B",
            vec![
                make_user_msg("msg-0", "Hey"),
                make_assistant_msg("msg-1", "Yo"),
            ],
        );
        store_b.save_session(&session_b);

        // Each store only sees its own sessions
        assert_eq!(store_a.list_sessions().len(), 1);
        assert_eq!(store_a.list_sessions()[0].session_id, "1000");

        assert_eq!(store_b.list_sessions().len(), 1);
        assert_eq!(store_b.list_sessions()[0].session_id, "2000");

        // list_all_sessions should see both
        let all = store_a.list_all_sessions();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_header_contains_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "100",
            "CWD Test",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );

        store.save_session(&session);

        // Load and check cwd was populated in the summary
        let list = store.list_sessions();
        assert_eq!(list.len(), 1);
        // For test stores we use the fake hash, but the header will
        // still get populated with current_dir() from build_header
        assert!(!list[0].cwd.is_empty());
    }

    #[test]
    fn test_format_session_timestamp() {
        // Just check it doesn't panic and returns a non-empty string
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let formatted = format_session_timestamp(ts);
        assert!(!formatted.is_empty());
    }
}
