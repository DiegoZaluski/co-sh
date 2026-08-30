//! Session persistence module.
//!
//! Stores chat sessions as individual JSONL files organized by CWD:
//! `{data_dir}/sessions/{cwd_hash}/session-{timestamp}.jsonl`, each with a
//! bincode-encoded `.ctx` companion file holding the context-manager state.
//!
//! ## Format
//!
//! Each JSONL file represents one complete chat session:
//!
//! - Line 1: Session metadata (JSON object with title, created_at, cwd, etc.)
//! - Lines 2+: Each message serialized as a JSON object, one per line.
//!
//! The JSONL is the DISPLAY-ONLY transcript. The model-facing context lives
//! in the companion `session-{id}.ctx` file: a bincode-encoded
//! [`ContextManagerState`] (items, id counter, budget, overflow state and the
//! split staging) restored verbatim into the harness on resume. The two files
//! are written together but never parsed into each other.
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

use cosh::harness::context::ContextManagerState;

use crate::types::{Message, MessageRole, Part, Session};

/// Number of session files to keep on disk per CWD. Oldest files are evicted first.
const MAX_SESSIONS_ON_DISK: usize = 50;

/// A queued session snapshot for the background save-writer thread. When
/// `context` is `None` the writer preserves the `.ctx` file already on disk (a
/// display-only save — title rename, message edit, session switch — must never
/// clobber the authoritative context).
struct SaveJob {
    session: Box<crate::types::Session>,
    context: Option<ContextManagerState>,
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
    /// Reasoning effort (`None` = model default) for the last model used.
    #[serde(default)]
    reasoning: Option<String>,
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

    /// Test-only store rooted at an explicit directory (lets integration
    /// tests drive the real load path against fixture session files).
    #[cfg(test)]
    pub(crate) fn with_dir(sessions_dir: std::path::PathBuf, cwd_hash: String) -> Self {
        std::fs::create_dir_all(&sessions_dir).ok();
        Self {
            sessions_dir,
            cwd_hash,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Persist a session to disk as a JSONL file in the current CWD's
    /// subdirectory.
    ///
    /// This is the display-only save path (title rename, message edit,
    /// session switch, fork, revert): the `.ctx` companion file already on
    /// disk is left untouched, so the authoritative model-facing context can
    /// never be clobbered by a display edit.
    pub fn save_session(&self, session: &Session) {
        self.write_session(session);
    }

    /// Persist a session together with an explicit context-manager snapshot.
    /// This is the harness path (Done / Stopped / ContextSnapshot): the JSONL
    /// transcript and the bincode `.ctx` snapshot are written together.
    pub fn save_session_with_context(&self, session: &Session, context: &ContextManagerState) {
        self.write_session(session);
        self.write_ctx(&session.id, context);
    }

    /// Persist a session WITHOUT blocking the caller (the UI thread),
    /// preserving the `.ctx` file already on disk. See [`Self::save_session`].
    pub fn save_session_async(&self, session: &crate::types::Session) {
        self.enqueue_save(SaveJob {
            session: Box::new(session.clone()),
            context: None,
        });
    }

    /// Persist a session with an explicit context snapshot on the background
    /// writer thread. A single writer keeps FIFO ordering, so a newer snapshot
    /// can never be clobbered by an older in-flight one.
    ///
    /// The context is taken BY VALUE and moved onto the writer thread — no deep
    /// clone of the context items happens on the caller (UI) thread, only a
    /// cheap `VecDeque` pointer move. The bincode pass runs on the writer
    /// thread too.
    pub fn save_session_async_with_context(
        &self,
        session: &crate::types::Session,
        context: ContextManagerState,
    ) {
        self.enqueue_save(SaveJob {
            session: Box::new(session.clone()),
            context: Some(context),
        });
    }

    /// Hand a save job to the single background writer thread. FIFO ordering is
    /// preserved, so a newer snapshot always lands after (and wins over) an
    /// older in-flight job.
    fn enqueue_save(&self, job: SaveJob) {
        static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<SaveJob>> =
            std::sync::OnceLock::new();
        let tx = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<SaveJob>();
            let store = self.clone();
            let spawned = std::thread::Builder::new()
                .name("session-save".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        store.write_session(&job.session);
                        if let Some(context) = &job.context {
                            store.write_ctx(&job.session.id, context);
                        }
                    }
                });
            if spawned.is_err() {
                log::warn!("failed to spawn session-save thread");
            }
            tx
        });
        // If the thread failed to spawn there is no receiver; fall back to a
        // synchronous save rather than dropping the snapshot.
        if let Err(send_err) = tx.send(job) {
            let job = send_err.0;
            self.write_session(&job.session);
            if let Some(context) = &job.context {
                self.write_ctx(&job.session.id, context);
            }
        }
    }

    /// Update only the title in an existing session file on disk.
    ///
    /// Reads the file, replaces the title in the first-line JSON header,
    /// and writes it back. This is much cheaper than a full rewrite and is
    /// used by the async title generation path.
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
        for (idx, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<StoredMessage>(line) {
                Ok(stored) => messages.push(stored.into_message()),
                Err(e) => {
                    // A corrupted line loses exactly one message — log it
                    // (session id, 1-based line number, parse error) rather
                    // than silently dropping a chunk of the transcript.
                    log::warn!(
                        "skipping unparseable line {} in session {session_id}: {e}",
                        idx + 2
                    );
                }
            }
        }

        Some(Session {
            id: session_id.to_string(),
            title: header.title,
            messages,
            created_at: header.created_at,
            title_generated: header.title_generated,
            provider: header.provider,
            model: header.model,
            reasoning: header.reasoning,
        })
    }

    /// Load the context-manager state for a session from its bincode-encoded
    /// `.ctx` companion file — the authoritative model-facing context,
    /// restored verbatim into the harness on resume.
    ///
    /// Returns `None` if no companion file exists or it cannot be decoded.
    pub fn load_context(&self, session_id: &str) -> Option<ContextManagerState> {
        let bytes = self.load_ctx(session_id)?;
        match ContextManagerState::from_bincode(&bytes) {
            Some(state) => Some(state),
            None => {
                log::warn!("failed to decode context state for session {session_id}");
                None
            }
        }
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

    /// Write the header and message lines for a session.
    ///
    /// The write is atomic: the content is written to a `.tmp` sibling and
    /// `fs::rename`d over the final path (atomic on the same filesystem); a
    /// reader only ever sees the complete old or new file.
    fn write_session(&self, session: &Session) {
        let file_path = self.file_path(&session.id);
        let header = self.build_header(session);

        let mut lines = Vec::new();

        // Line 1: session header.
        match serde_json::to_string(&header) {
            Ok(json) => lines.push(json),
            Err(e) => log::warn!("failed to serialize header for session {}: {e}", session.id),
        }

        // Lines 2+: messages.
        for (i, msg) in session.messages.iter().enumerate() {
            match serde_json::to_string(&StoredMessage::from(msg)) {
                Ok(json) => lines.push(json),
                Err(e) => {
                    log::warn!(
                        "failed to serialize message {i} for session {}: {e}",
                        session.id
                    )
                }
            }
        }

        let tmp_path = file_path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp_path, lines.join("\n")) {
            log::warn!("failed to save session {}: {e}", session.id);
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &file_path) {
            log::warn!("failed to finalize session {}: {e}", session.id);
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }

        self.evict_old_sessions();
    }

    /// Bincode-encode the context snapshot and write it to the `.ctx`
    /// companion file.
    fn write_ctx(&self, session_id: &str, context: &ContextManagerState) {
        let bytes = context.to_bincode();
        if bytes.is_empty() {
            log::warn!("failed to serialize context state for session {session_id}");
            return;
        }
        self.save_ctx(session_id, &bytes);
    }

    /// Build a `SessionHeader` from a `Session`, populating `cwd` with the
    /// canonicalized current working directory.
    ///
    /// The session's own recorded model selection (provider + model +
    /// reasoning) wins. Sessions without one — e.g. saved before this
    /// feature — fall back to deriving provider/model from the last valid
    /// assistant message, which preserves the old behavior.
    fn build_header(&self, session: &Session) -> SessionHeader {
        let (provider, model) = match (&session.provider, &session.model.as_deref()) {
            // A recorded selection (provider + model written together) wins.
            (Some(_), Some(_)) => (session.provider.clone(), session.model.clone()),
            // A recorded `auto` selection wins too — it has no provider to
            // pin, so the fallback chain stays the source of truth.
            (None, Some("auto")) => (None, session.model.clone()),
            _ => session
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
                .unwrap_or((None, None)),
        };

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
            reasoning: session.reasoning.clone(),
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
    use crate::types::{ReasoningPart, TextPart, ToolPart, ToolStatus};
    use cosh::harness::context::{ContextItem, SplitState};

    fn make_test_session(id: &str, title: &str, messages: Vec<Message>) -> Session {
        Session {
            id: id.to_string(),
            title: title.to_string(),
            created_at: 0,
            messages,
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
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

    /// Build a context snapshot with the given items.
    fn make_context(items: Vec<ContextItem>) -> ContextManagerState {
        ContextManagerState {
            items: items.into_iter().collect(),
            next_id: 42,
            max_tokens: 100_000,
            overflow_model: None,
            split: None,
        }
    }

    fn user_item(id: u64, text: &str) -> ContextItem {
        ContextItem::User {
            id,
            original: text.to_string(),
        }
    }

    fn assistant_item(id: u64, text: &str) -> ContextItem {
        ContextItem::Assistant {
            id,
            original: text.to_string(),
            closable: true,
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
            "Multiple Errors After",
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
    fn test_session_model_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let mut session = make_test_session(
            "12345",
            "Model Session",
            vec![
                make_user_msg("msg-0", "Hello!"),
                make_assistant_msg("msg-1", "Hi there!"),
            ],
        );
        session.provider = Some("nvidia".to_string());
        session.model = Some("deepseek-ai/deepseek-v4-pro".to_string());
        session.reasoning = Some("high".to_string());

        store.save_session(&session);

        let loaded = store.load_session("12345").unwrap();
        // The recorded selection round-trips through the JSONL header.
        assert_eq!(loaded.model.as_deref(), Some("deepseek-ai/deepseek-v4-pro"));
        assert_eq!(loaded.provider.as_deref(), Some("nvidia"));
        assert_eq!(loaded.reasoning.as_deref(), Some("high"));

        // A legacy header without the reasoning field still loads.
        let file = store.file_path("12345");
        let contents = std::fs::read_to_string(&file).unwrap();
        let mut lines: Vec<&str> = contents.lines().collect();
        let mut header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        header.as_object_mut().unwrap().remove("reasoning");
        let header_line = header.to_string();
        lines[0] = &header_line;
        std::fs::write(&file, lines.join("\n")).unwrap();
        let loaded = store.load_session("12345").unwrap();
        assert_eq!(loaded.reasoning, None);
        assert_eq!(loaded.model.as_deref(), Some("deepseek-ai/deepseek-v4-pro"));
    }

    #[test]
    fn test_build_header_keeps_recorded_auto_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let mut session = make_test_session(
            "12346",
            "Auto Session",
            vec![
                make_user_msg("msg-0", "Hello!"),
                make_assistant_msg("msg-1", "Hi there!"),
            ],
        );
        // The user explicitly picked `auto`; the last assistant message
        // carries a concrete model id the fallback chain actually used.
        session.provider = None;
        session.model = Some("auto".to_string());
        session.reasoning = Some("low".to_string());
        session.messages[1].model = Some("openai/gpt-oss-120b".to_string());

        store.save_session(&session);

        let loaded = store.load_session("12346").unwrap();
        // The recorded `auto` selection wins over deriving the concrete model
        // from the last assistant message — no provider is pinned.
        assert_eq!(loaded.model.as_deref(), Some("auto"));
        assert_eq!(loaded.provider, None);
        assert_eq!(loaded.reasoning.as_deref(), Some("low"));
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

    /// The full context snapshot — items, bookkeeping AND the split staging —
    /// round-trips verbatim through the bincode `.ctx` companion file, so an
    /// interrupted split resumes exactly where it stopped.
    #[test]
    fn test_context_roundtrips_through_the_ctx_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("5000", "Split Session", vec![]);
        let mut context =
            make_context(vec![user_item(1, "Hello!"), assistant_item(2, "Hi there!")]);
        context.overflow_model = Some("gpt-4o-mini".to_string());
        context.split = Some(SplitState {
            buffer: "## Objective\n- summarized so far".to_string(),
            cursor: Some(2),
            continuity: "tail of the last chunk".to_string(),
            window: 100_000,
            buffer_tokens: 1234,
        });

        store.save_session_with_context(&session, &context);

        let loaded = store.load_context("5000").unwrap();
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.next_id, 42);
        assert_eq!(loaded.max_tokens, 100_000);
        assert_eq!(loaded.overflow_model.as_deref(), Some("gpt-4o-mini"));
        let split = loaded.split.expect("split staging was persisted");
        assert_eq!(split.buffer, "## Objective\n- summarized so far");
        assert_eq!(split.cursor, Some(2));
        assert_eq!(split.continuity, "tail of the last chunk");
        assert_eq!(split.window, 100_000);
        assert_eq!(split.buffer_tokens, 1234);
    }

    #[test]
    fn test_save_session_preserves_existing_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "1000",
            "First",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );
        let context = make_context(vec![user_item(1, "Hello"), assistant_item(2, "Hi")]);
        store.save_session_with_context(&session, &context);

        // A display-only save (e.g. title rename) must not clobber the .ctx.
        let mut renamed = session.clone();
        renamed.title = "Renamed".to_string();
        store.save_session(&renamed);

        let context = store.load_context("1000").unwrap();
        assert_eq!(context.items.len(), 2);
        let session = store.load_session("1000").unwrap();
        assert_eq!(session.title, "Renamed");
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

    /// Reasoning ("+ Thought") blocks are part of the display message, so
    /// they survive a reload straight from the JSONL — without ever entering
    /// the model-facing context (the `.ctx` snapshot carries no reasoning).
    #[test]
    fn test_reasoning_persists_for_display_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        // A live display session: the assistant turn streams a "+ Thought"
        // block before its answer.
        let session = make_test_session(
            "6000",
            "Reasoning Session",
            vec![
                make_user_msg("msg-0", "Why is the sky blue?"),
                Message {
                    id: "msg-1".to_string(),
                    role: MessageRole::Assistant,
                    parts: vec![
                        Part::Reasoning(ReasoningPart {
                            text: "THOUGHT: sunlight collides with air molecules".to_string(),
                            collapsed: true,
                        }),
                        Part::Text(TextPart {
                            text: "Because of Rayleigh scattering.".to_string(),
                            synthetic: false,
                        }),
                    ],
                    created_at: 2000,
                    agent: None,
                    model: None,
                },
            ],
        );

        // The harness context carries NO reasoning — it is display-only.
        let context = make_context(vec![
            user_item(1, "Why is the sky blue?"),
            assistant_item(2, "Because of Rayleigh scattering."),
        ]);

        store.save_session_with_context(&session, &context);

        // The reasoning survives for display, attached to its answer.
        let loaded = store.load_session("6000").unwrap();
        assert_eq!(loaded.messages.len(), 2);
        let parts = &loaded.messages[1].parts;
        assert_eq!(parts.len(), 2);
        assert!(
            matches!(&parts[0], Part::Reasoning(r) if r.text == "THOUGHT: sunlight collides with air molecules")
        );
        assert!(matches!(&parts[1], Part::Text(t) if t.text == "Because of Rayleigh scattering."));

        // The .ctx snapshot keeps the reasoning OUT of the model context.
        let state = store.load_context("6000").unwrap();
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&state);
        let msgs = cm.build_messages("");
        assert!(
            msgs.iter().all(|m| !m
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("THOUGHT:")),
            "reasoning must never leak into the model-facing context"
        );
    }

    /// Merged tool chains survive the JSONL round trip with their name, input
    /// AND output — the exact shape the live loop renders — so the transcript
    /// does not visually collapse after a restart.
    #[test]
    fn restored_tool_chains_render_like_the_live_display() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "9000",
            "Tool Session",
            vec![
                make_user_msg("u1", "edit the file"),
                Message {
                    id: "msg-1".to_string(),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Tool(ToolPart {
                        tool: "fs_edit".to_string(),
                        input: serde_json::json!({"path": "a.rs"}),
                        output: Some("--- a.rs\n+++ b.rs\n@@ -1 +1 @@\n-old\n+new".to_string()),
                        status: ToolStatus::Completed,
                        tool_call_id: Some("call-a".to_string()),
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                    })],
                    created_at: 2000,
                    agent: None,
                    model: None,
                },
            ],
        );

        store.save_session(&session);

        let loaded = store.load_session("9000").unwrap();
        let Part::Tool(tp) = &loaded.messages[1].parts[0] else {
            panic!("expected a tool part");
        };
        assert_eq!(tp.tool, "fs_edit", "the tool name survives the restore");
        assert!(
            tp.output.as_deref().unwrap().starts_with("--- a.rs"),
            "the tool OUTPUT survives the restore"
        );
        assert!(!tp.input.is_null(), "the tool input survives the restore");
    }

    /// The full error-display contract across a save/restore: the API error
    /// lives in the JSONL as a `msg-err-` message (the styled red error box)
    /// and in the `.ctx` as a display-only `Error` item that
    /// `build_messages` skips — so the model never inherits a provider
    /// failure after a restart.
    #[test]
    fn api_errors_keep_their_display_semantics_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "8100",
            "Error Semantics",
            vec![
                make_user_msg("msg-0", "fix the build"),
                make_error_msg("msg-err-1", "Error: HTTP 401 - unauthorized"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "fix the build"),
            ContextItem::Error {
                id: 2,
                content: "Error: HTTP 401 - unauthorized".to_string(),
            },
        ]);
        store.save_session_with_context(&session, &context);

        let loaded = store.load_session("8100").unwrap();
        assert_eq!(loaded.messages.len(), 2);
        let err = &loaded.messages[1];
        assert!(
            err.id.starts_with("msg-err-"),
            "the restored error keeps its styled id, got {}",
            err.id
        );
        assert_eq!(err.role, MessageRole::Assistant);
        assert!(
            matches!(&err.parts[0], Part::Text(t) if t.text == "Error: HTTP 401 - unauthorized")
        );

        // The restored snapshot keeps the error OUT of the model context.
        let state = store.load_context("8100").unwrap();
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&state);
        let msgs = cm.build_messages("");
        assert_eq!(msgs.len(), 1, "only the user prompt reaches the model");
        assert_eq!(msgs[0].role, "user");
    }
}
