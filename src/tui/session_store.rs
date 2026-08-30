//! Session persistence module.
//!
//! Stores chat sessions as individual JSONL files organized by CWD:
//! `{data_dir}/sessions/{cwd_hash}/session-{timestamp}.jsonl`.
//!
//! ## Format
//!
//! Each JSONL file represents one complete chat session:
//!
//! - Line 1: Session metadata (JSON object with title, created_at, cwd, model,
//!   and the context-manager session state — `max_tokens`, `overflow_model`,
//!   `next_id`).
//! - Lines 2+: One [`ContextItem`](cosh::harness::context_manager::ContextItem)
//!   per line — the single source of truth for the conversation context. The
//!   display `Session`/`Message`/`Part` model is DERIVED from these items on
//!   load, so the transcript and the context can never drift apart.
//! - Optional final line: the staging of an in-progress split-and-concatenate
//!   (`{"split": …}`), so an interrupted split resumes exactly where it stopped.
//!
//! Sessions are grouped by CWD (current working directory). The CWD path is
//! hashed with xxHash32 to produce a deterministic subdirectory name, so
//! sessions started in different directories never mix. The full CWD path is
//! also stored in the session header for display and filtering.
//!
//! A session is only persisted when it contains actual dialog:
//! at least one user message AND at least one valid assistant response
//! (error-only responses don't count as dialog).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Datelike;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh32::xxh32;

use cosh::harness::context::{ContextItem, ContextManagerState, SplitState};

use crate::types::{
    Message, MessageRole, Part, ReasoningPart, Session, TextPart, ToolPart, ToolStatus,
};

/// Number of session files to keep on disk per CWD. Oldest files are evicted first.
const MAX_SESSIONS_ON_DISK: usize = 50;

/// A queued session snapshot for the background save-writer thread. When
/// `context` is `None` the writer preserves the context already on disk (a
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
    /// Token budget persisted from the context manager.
    #[serde(default)]
    max_tokens: usize,
    /// Stuck context-window overflow model (see `ContextManagerState`).
    /// The `overflow_provider` alias keeps older session files readable.
    #[serde(default, alias = "overflow_provider")]
    overflow_model: Option<String>,
    /// Monotonic item id counter persisted from the context manager.
    #[serde(default)]
    next_id: u64,
}

/// The split-staging line appended after the item lines when a split is in
/// progress. A dedicated marker object so it can never collide with a
/// [`ContextItem`] line.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SplitLine {
    split: SplitState,
}

/// The display-only reasoning line appended after the item lines: a list of
/// item-id → reasoning-text pairs so the TUI can re-render the "+ Thought"
/// blocks after a reload. Reasoning is display-only — it is deliberately NOT
/// stored on a [`ContextItem`] and NOT parsed by [`SessionStore::load_context`],
/// so it can never leak back into the context the context manager sends to the
/// model.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReasoningLine {
    reasoning: Vec<ReasoningEntry>,
}

/// One item-id → reasoning-text association in a [`ReasoningLine`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReasoningEntry {
    id: u64,
    text: String,
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

/// The parsed contents of a session file: header + context items + split stage
/// + the display-only reasoning map (item id → reasoning text).
struct ParsedSession {
    header: SessionHeader,
    items: Vec<ContextItem>,
    split: Option<SplitState>,
    reasoning: HashMap<u64, String>,
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
    /// subdirectory, PRESERVING any context already on disk.
    ///
    /// This is the display-only save path (title rename, message edit, session
    /// switch, fork, revert): the context items are the authoritative
    /// transcript, and a display edit must never clobber them. If no context
    /// exists yet (a brand-new session that has not produced a terminal
    /// snapshot, or a forked session), the display messages are converted back
    /// into context items best-effort so the transcript is not lost.
    pub fn save_session(&self, session: &Session) {
        let context = self
            .load_context(&session.id)
            .unwrap_or_else(|| context_from_messages(&session.messages));
        self.write_session(session, &context);
    }

    /// Persist a session together with an explicit context-manager snapshot.
    /// This is the harness path (Done / Stopped / ContextSnapshot): the
    /// context items are authoritative.
    pub fn save_session_with_context(&self, session: &Session, context: &ContextManagerState) {
        self.write_session(session, context);
    }

    /// Persist a session WITHOUT blocking the caller (the UI thread),
    /// preserving any context already on disk. See [`Self::save_session`].
    ///
    /// Unlike [`Self::save_session`], the context is NOT read here: the job is
    /// enqueued with `context: None` so the writer thread runs
    /// [`Self::save_session`] at write time. That read is ordered after any
    /// earlier snapshot job in the FIFO queue, so a display-only save that
    /// coincides with an in-flight `ContextSnapshot` can never capture the
    /// stale pre-snapshot context and clobber the fresher one.
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
    /// cheap `VecDeque` pointer move.
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
                        match job.context {
                            Some(context) => store.write_session(&job.session, &context),
                            None => store.save_session(&job.session),
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
            match job.context {
                Some(context) => self.write_session(&job.session, &context),
                None => self.save_session(&job.session),
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

    /// Load the display `Session` for a session, reconstructed from the
    /// persisted [`ContextItem`] log. The display model is DERIVED from the
    /// context items, so the transcript always matches the authoritative
    /// context.
    ///
    /// Returns `None` if the file does not exist or cannot be parsed.
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        let parsed = self.parse_file(session_id)?;
        Some(Session {
            id: session_id.to_string(),
            title: parsed.header.title,
            messages: items_to_messages(&parsed.items, &parsed.reasoning),
            created_at: parsed.header.created_at,
            title_generated: parsed.header.title_generated,
            provider: parsed.header.provider,
            model: parsed.header.model,
            reasoning: parsed.header.reasoning,
        })
    }

    /// Load the context-manager state for a session — the authoritative
    /// transcript, restored verbatim into the harness on resume.
    ///
    /// Returns `None` if the file does not exist or cannot be parsed.
    pub fn load_context(&self, session_id: &str) -> Option<ContextManagerState> {
        let parsed = self.parse_file(session_id)?;
        Some(ContextManagerState {
            items: parsed.items.into_iter().collect(),
            next_id: parsed.header.next_id,
            max_tokens: parsed.header.max_tokens,
            overflow_model: parsed.header.overflow_model,
            split: parsed.split,
        })
    }

    /// Delete a session file from disk.
    pub fn delete_session(&self, session_id: &str) {
        let file_path = self.file_path(session_id);
        std::fs::remove_file(&file_path).ok();
    }

    /// Check whether a session with the given ID exists on disk.
    pub fn has_session(&self, session_id: &str) -> bool {
        self.file_path(session_id).exists()
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Build the absolute path for a session file within the current CWD subdirectory.
    fn file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("session-{session_id}.jsonl"))
    }

    /// Write the header, item lines and (optional) split line for a session.
    ///
    /// The write is atomic: the JSONL is now the single source of truth (there
    /// is no redundant bincode sidecar), so a crash mid-write must never leave
    /// a half-written session that can't be resumed. The content is written to
    /// a `.tmp` sibling and `fs::rename`d over the final path (atomic on the
    /// same filesystem); a reader only ever sees the complete old or new file.
    fn write_session(&self, session: &Session, context: &ContextManagerState) {
        let file_path = self.file_path(&session.id);
        let header = self.build_header(session, context);

        let mut lines = Vec::new();

        // Line 1: session header.
        match serde_json::to_string(&header) {
            Ok(json) => lines.push(json),
            Err(e) => log::warn!("failed to serialize header for session {}: {e}", session.id),
        }

        // Lines 2+: one ContextItem per line.
        for (i, item) in context.items.iter().enumerate() {
            match serde_json::to_string(item) {
                Ok(json) => lines.push(json),
                Err(e) => log::warn!(
                    "failed to serialize item {i} for session {}: {e}",
                    session.id
                ),
            }
        }

        // Optional final line: the split staging.
        if let Some(split) = &context.split {
            match serde_json::to_string(&SplitLine {
                split: split.clone(),
            }) {
                Ok(json) => lines.push(json),
                Err(e) => log::warn!("failed to serialize split for session {}: {e}", session.id),
            }
        }

        // Optional final line: the display-only reasoning map.
        let reasoning = collect_reasoning(session, context);
        if !reasoning.is_empty() {
            match serde_json::to_string(&ReasoningLine { reasoning }) {
                Ok(json) => lines.push(json),
                Err(e) => {
                    log::warn!(
                        "failed to serialize reasoning for session {}: {e}",
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

    /// Parse a session file into header + items + split stage.
    fn parse_file(&self, session_id: &str) -> Option<ParsedSession> {
        let file_path = self.file_path(session_id);
        let content = std::fs::read_to_string(&file_path).ok()?;

        let mut lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return None;
        }

        // Line 1: session header
        let header: SessionHeader = serde_json::from_str(lines[0]).ok()?;
        lines.remove(0);

        // Remaining lines: items, plus an optional split/reasoning line.
        let mut items: Vec<ContextItem> = Vec::new();
        let mut split: Option<SplitState> = None;
        let mut reasoning: HashMap<u64, String> = HashMap::new();
        for (idx, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ContextItem>(line) {
                Ok(item) => items.push(item),
                Err(item_err) => match serde_json::from_str::<SplitLine>(line) {
                    Ok(split_line) => split = Some(split_line.split),
                    Err(_) => match serde_json::from_str::<ReasoningLine>(line) {
                        Ok(reasoning_line) => {
                            for entry in reasoning_line.reasoning {
                                reasoning.insert(entry.id, entry.text);
                            }
                        }
                        Err(_) => {
                            // A corrupted line loses exactly one item — log it
                            // (session id, 1-based line number, parse error)
                            // rather than silently dropping a chunk of the
                            // restored context.
                            log::warn!(
                                "skipping unparseable line {} in session {session_id}: {item_err}",
                                idx + 2
                            );
                        }
                    },
                },
            }
        }

        Some(ParsedSession {
            header,
            items,
            split,
            reasoning,
        })
    }

    /// Build a `SessionHeader` from a `Session` + the context state, populating
    /// `cwd` with the canonicalized current working directory.
    fn build_header(&self, session: &Session, context: &ContextManagerState) -> SessionHeader {
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
            max_tokens: context.max_tokens,
            overflow_model: context.overflow_model.clone(),
            next_id: context.next_id,
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

        // Count remaining non-empty item lines (skip the split/reasoning
        // staging lines — they are not messages).
        let mut message_count: usize = 0;
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with("{\"split\":")
                || line.starts_with("{\"reasoning\":")
            {
                continue;
            }
            message_count += 1;
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
        }
    }
}

/// Build a fresh context snapshot from the display `Message` list. Used only
/// for the display-only save path when no authoritative context exists yet
/// (a brand-new session, or a forked/reverted transcript). It is a best-effort
/// reverse mapping — tool calls lose their thought signatures/thinking blocks —
/// because it only ever runs for transcripts the harness has not yet persisted.
pub(crate) fn context_from_messages(messages: &[Message]) -> ContextManagerState {
    let mut items: Vec<ContextItem> = Vec::new();
    let mut next_id = 1u64;
    macro_rules! push {
        ($item:expr) => {{
            items.push($item);
            next_id += 1;
        }};
    }

    for m in messages {
        match m.role {
            MessageRole::User => {
                if let Some(text) = m.parts.iter().find_map(|p| match p {
                    Part::Text(t) if !t.text.is_empty() => Some(t.text.clone()),
                    _ => None,
                }) {
                    push!(ContextItem::User {
                        id: next_id,
                        original: text,
                    });
                }
            }
            MessageRole::Assistant => {
                // Terminal API/runtime errors are DISPLAY-ONLY: the live loop
                // never records them as assistant output, so the best-effort
                // fallback must not either — they become Error items (skipped
                // by `build_messages`) instead of model-facing prose.
                if m.id.starts_with("msg-err-") {
                    for part in &m.parts {
                        if let Part::Text(t) = part
                            && !t.text.is_empty()
                        {
                            push!(ContextItem::Error {
                                id: next_id,
                                content: t.text.clone(),
                            });
                        }
                    }
                    continue;
                }
                for part in &m.parts {
                    match part {
                        Part::Text(t) if !t.synthetic && !t.text.is_empty() => {
                            push!(ContextItem::Assistant {
                                id: next_id,
                                original: t.text.clone(),
                                closable: true,
                            });
                        }
                        Part::Tool(tp) => {
                            // Live tool parts carry no provider id (the display
                            // is id-less); the harness would synthesize one (see
                            // `Harness::push_tool_history`). Mirror that here so
                            // a restored ToolCall never reaches the provider
                            // with an empty `call_id` (OpenAI-compatible APIs
                            // reject it).
                            let call_id = match &tp.tool_call_id {
                                Some(id) if !id.is_empty() => id.clone(),
                                _ => {
                                    log::warn!(
                                        "tool part without call_id in context_from_messages; \
                                         synthesizing call_{next_id:016x}"
                                    );
                                    format!("call_{next_id:016x}")
                                }
                            };
                            if tp.is_start {
                                push!(ContextItem::ToolCall {
                                    id: next_id,
                                    call_id,
                                    name: tp.tool.clone(),
                                    arguments: tp.input.to_string(),
                                    thought_signature: String::new(),
                                    thinking_blocks: Vec::new(),
                                });
                            } else if let Some(output) = &tp.output {
                                push!(ContextItem::ToolResult {
                                    id: next_id,
                                    call_id,
                                    content: output.clone(),
                                    useless: false,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    ContextManagerState {
        items: items.into_iter().collect(),
        next_id,
        max_tokens: cosh::harness::context::MAX_CONTEXT_TOKENS,
        overflow_model: None,
        split: None,
    }
}

/// Reconstruct the display `Message` list from the authoritative
/// [`ContextItem`] log plus the display-only reasoning map. This is a
/// display-only mapping: the context items are the source of truth for the
/// model, and the display model is derived so the transcript always matches.
/// Reasoning is rendered as a leading [`Part::Reasoning`] ("+ Thought") and is
/// never part of the model-facing context.
fn items_to_messages(items: &[ContextItem], reasoning: &HashMap<u64, String>) -> Vec<Message> {
    // Tool chains are MERGED back into the display shape the live loop
    // produces: one ToolPart carrying name + input + output (the call's
    // result). Restoring them unmerged would lose the tool's identity on the
    // result half (nameless parts render as generic) and the output on the
    // call half (diffs/edits/reads/bash blocks render from `output`) — the
    // transcript would visually collapse after a restart.
    // Pending result indices per call_id, built in one pass — the merge
    // below is O(n) total (each result is consumed at most once).
    let mut pending_results: HashMap<&str, std::collections::VecDeque<usize>> = items
        .iter()
        .enumerate()
        .fold(HashMap::new(), |mut acc, (j, it)| {
            if let ContextItem::ToolResult { call_id, .. } = it {
                acc.entry(call_id.as_str()).or_default().push_back(j);
            }
            acc
        });
    let mut consumed_result = vec![false; items.len()];
    let mut messages: Vec<Message> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let id = format!("msg-{idx}");
        let thought = |item_id: u64| {
            reasoning.get(&item_id).map(|text| {
                Part::Reasoning(ReasoningPart {
                    text: text.clone(),
                    collapsed: true,
                })
            })
        };
        match item {
            ContextItem::User { original, .. } => messages.push(Message {
                id,
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: original.clone(),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            }),
            ContextItem::Assistant { original, .. }
            | ContextItem::Closure {
                content: original, ..
            }
            | ContextItem::Compaction {
                summary: original, ..
            } => {
                let mut parts = Vec::with_capacity(2);
                if let Some(thought) = thought(context_item_id(item)) {
                    parts.push(thought);
                }
                parts.push(Part::Text(TextPart {
                    text: original.clone(),
                    synthetic: false,
                }));
                messages.push(Message {
                    id,
                    role: MessageRole::Assistant,
                    parts,
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
            ContextItem::Error { content, .. } => {
                // Terminal API/runtime errors keep their display semantics:
                // the `msg-err-` id prefix is what the renderer styles as the
                // red error box, so a restored error never collapses into
                // plain assistant prose.
                messages.push(Message {
                    id: format!("msg-err-{idx}"),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Text(TextPart {
                        text: content.clone(),
                        synthetic: false,
                    })],
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
            ContextItem::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                // This call's result: the next unconsumed ToolResult with the
                // same call_id — order-independent, so interleaved parallel
                // chains merge correctly too.
                let result = pending_results
                    .get_mut(call_id.as_str())
                    .and_then(|queue| queue.pop_front())
                    .and_then(|j| {
                        consumed_result[j] = true;
                        match &items[j] {
                            ContextItem::ToolResult { content, .. } => Some(content.clone()),
                            _ => None,
                        }
                    });
                let mut parts = Vec::with_capacity(2);
                if let Some(thought) = thought(context_item_id(item)) {
                    parts.push(thought);
                }
                parts.push(Part::Tool(ToolPart {
                    tool: name.clone(),
                    input: serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null),
                    output: result,
                    status: ToolStatus::Completed,
                    tool_call_id: Some(call_id.clone()),
                    is_start: true,
                    is_streaming: false,
                    cached_line_count: None,
                }));
                messages.push(Message {
                    id,
                    role: MessageRole::Assistant,
                    parts,
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
            ContextItem::ToolResult {
                call_id, content, ..
            } => {
                // Consumed by its call above. An orphan result (its call is
                // not in the log) still renders as a result-only part so the
                // output is never dropped.
                if consumed_result[idx] {
                    continue;
                }
                messages.push(Message {
                    id,
                    role: MessageRole::Assistant,
                    parts: vec![Part::Tool(ToolPart {
                        tool: String::new(),
                        input: serde_json::Value::Null,
                        output: Some(content.clone()),
                        status: ToolStatus::Completed,
                        tool_call_id: Some(call_id.clone()),
                        is_start: false,
                        is_streaming: false,
                        cached_line_count: None,
                    })],
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
        }
    }
    messages
}

/// The stable id of a context item, used to key the display-only reasoning map.
fn context_item_id(item: &ContextItem) -> u64 {
    match item {
        ContextItem::User { id, .. }
        | ContextItem::Assistant { id, .. }
        | ContextItem::ToolCall { id, .. }
        | ContextItem::ToolResult { id, .. }
        | ContextItem::Closure { id, .. }
        | ContextItem::Compaction { id, .. }
        | ContextItem::Error { id, .. } => *id,
    }
}

/// Map the display `Session`'s reasoning blocks onto the context items they
/// precede, producing item-id → reasoning-text pairs for the display-only
/// reasoning line. The context items are NOT modified: reasoning is
/// display-only and must never enter the model-facing context.
///
/// The mapping is positional — the k-th assistant "output" in the display
/// (a tool start or a text part) corresponds to the k-th output item in the
/// context (a `ToolCall`/`Assistant`/`Closure`). Reasoning blocks that sit
/// immediately before an output attach to that output.
fn collect_reasoning(session: &Session, context: &ContextManagerState) -> Vec<ReasoningEntry> {
    // One reasoning slot per display output, in order.
    let mut slots: Vec<Option<String>> = Vec::new();
    let mut pending: Option<String> = None;
    for msg in &session.messages {
        if msg.role != MessageRole::Assistant {
            // A user turn resets any dangling reasoning (it has nowhere to go).
            pending = None;
            continue;
        }
        for part in &msg.parts {
            match part {
                Part::Reasoning(r) => {
                    pending.get_or_insert_with(String::new).push_str(&r.text);
                }
                Part::Tool(tp) if tp.is_start => slots.push(pending.take()),
                Part::Text(t) if !t.synthetic && !t.text.is_empty() => slots.push(pending.take()),
                _ => {}
            }
        }
    }

    let mut result = Vec::new();
    let mut slots = slots.into_iter();
    for item in &context.items {
        match item {
            ContextItem::ToolCall { id, .. }
            | ContextItem::Assistant { id, .. }
            | ContextItem::Closure { id, .. } => {
                if let Some(Some(text)) = slots.next() {
                    result.push(ReasoningEntry { id: *id, text });
                }
            }
            _ => {}
        }
    }
    result
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

    /// A session file written by an OLDER build carries the header key
    /// `overflow_provider`; the alias must keep it loadable (and the stale
    /// provider-shaped value harmless).
    #[test]
    fn old_header_with_overflow_provider_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let file = store.sessions_dir.join("session-legacy.jsonl");
        let header = r#"{"title":"legacy","title_generated":false,"created_at":0,"cwd":"/tmp","provider":"openai","model":null,"max_tokens":100000,"overflow_provider":"openai","next_id":2}"#;
        let item = r#"{"User":{"id":1,"original":"hi"}}"#;
        std::fs::write(&file, format!("{header}\n{item}\n")).unwrap();

        let state = store
            .load_context("legacy")
            .expect("an old-format session must load");
        assert_eq!(state.overflow_model.as_deref(), Some("openai"));
        // The stale provider-shaped value is inert: the next loop start calls
        // `sync_model(effective_model)`, which differs from "openai" and
        // clears the state.
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&state);
        cm.sync_model("gpt-4o-mini");
        assert!(!cm.overflow_stuck("gpt-4o-mini"));
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

        let session = make_test_session("12345", "Test Session", vec![]);
        let context = make_context(vec![user_item(1, "Hello!"), assistant_item(2, "Hi there!")]);

        store.save_session_with_context(&session, &context);

        // Context round-trips verbatim.
        let loaded = store.load_context("12345");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().items.len(), 2);

        // Display session is reconstructed from the items.
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

        let mut session = make_test_session("12345", "Model Session", vec![]);
        session.provider = Some("nvidia".to_string());
        session.model = Some("deepseek-ai/deepseek-v4-pro".to_string());
        session.reasoning = Some("high".to_string());

        store.save_session_with_context(&session, &make_context(vec![]));

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

        let mut session = make_test_session("12346", "Auto Session", vec![]);
        // The user explicitly picked `auto`.
        session.provider = None;
        session.model = Some("auto".to_string());
        session.reasoning = Some("low".to_string());

        store.save_session_with_context(&session, &make_context(vec![]));

        let loaded = store.load_session("12346").unwrap();
        assert_eq!(loaded.model.as_deref(), Some("auto"));
        assert_eq!(loaded.provider, None);
        assert_eq!(loaded.reasoning.as_deref(), Some("low"));
    }

    /// Restoring a session must rebuild the tool chains in the SAME shape the
    /// live loop renders: one ToolPart with the tool's name, its input AND its
    /// output. Unmerged parts would lose the diff/edit/read/bash blocks after
    /// a restart (the call half has no output, the result half has no name).
    #[test]
    fn restored_tool_chains_render_like_the_live_display() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "9000",
            "Tool Session",
            vec![make_user_msg("u1", "edit the file")],
        );

        let context = make_context(vec![
            user_item(1, "edit the file"),
            ContextItem::Assistant {
                id: 2,
                original: "editing".into(),
                closable: false,
            },
            ContextItem::ToolCall {
                id: 3,
                call_id: "call-a".into(),
                name: "fs_edit".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            // Interleaved parallel chain: the result of call-a comes AFTER
            // the call of chain b — the merge must be order-independent.
            ContextItem::ToolCall {
                id: 4,
                call_id: "call-b".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"b.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 5,
                call_id: "call-a".into(),
                content: "--- a.rs\n+++ b.rs\n@@ -1 +1 @@\n-old\n+new".into(),
                useless: false,
            },
            ContextItem::ToolResult {
                id: 6,
                call_id: "call-b".into(),
                content: "contents of b".into(),
                useless: false,
            },
        ]);
        store.save_session_with_context(&session, &context);

        let loaded = store.load_session("9000").unwrap();
        // user + assistant + 2 merged tool parts (NOT 2 raw pairs = 4 parts).
        let tool_msgs: Vec<&Message> = loaded
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant)
            .filter(|m| m.parts.iter().any(|p| matches!(p, Part::Tool(_))))
            .collect();
        assert_eq!(
            tool_msgs.len(),
            2,
            "each tool chain restores as ONE message"
        );
        for msg in &tool_msgs {
            let Part::Tool(tp) = &msg.parts[0] else {
                panic!("expected a tool part");
            };
            assert!(!tp.tool.is_empty(), "the tool name survives the restore");
            assert!(
                tp.output.is_some(),
                "the tool OUTPUT survives the restore (diffs/edits/render blocks)"
            );
            assert!(!tp.input.is_null(), "the tool input survives the restore");
        }
        let edit = tool_msgs
            .iter()
            .find(|m| matches!(&m.parts[0], Part::Tool(tp) if tp.tool == "fs_edit"))
            .unwrap();
        let Part::Tool(tp) = &edit.parts[0] else {
            panic!();
        };
        assert!(
            tp.output.as_deref().unwrap().starts_with("--- a.rs"),
            "fs_edit keeps its diff content"
        );
    }

    #[test]
    fn test_save_session_preserves_existing_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("1000", "First", vec![]);
        let context = make_context(vec![user_item(1, "Hello"), assistant_item(2, "Hi")]);
        store.save_session_with_context(&session, &context);

        // A display-only save (e.g. title rename) must not clobber the context.
        let mut renamed = session.clone();
        renamed.title = "Renamed".to_string();
        store.save_session(&renamed);

        let context = store.load_context("1000").unwrap();
        assert_eq!(context.items.len(), 2);
        let session = store.load_session("1000").unwrap();
        assert_eq!(session.title, "Renamed");
    }

    #[test]
    fn test_split_staging_persists_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("5000", "Split Session", vec![]);
        let mut context =
            make_context(vec![user_item(1, "Hello!"), assistant_item(2, "Hi there!")]);
        // An in-progress split-and-concatenate must survive the JSONL round-trip
        // verbatim, so an interrupted split resumes where it stopped.
        context.split = Some(SplitState {
            buffer: "## Objective\n- summarized so far".to_string(),
            cursor: Some(2),
            continuity: "tail of the last chunk".to_string(),
            window: 100_000,
            buffer_tokens: 1234,
        });

        store.save_session_with_context(&session, &context);

        let loaded = store.load_context("5000").unwrap();
        let split = loaded.split.expect("split staging was persisted");
        assert_eq!(split.buffer, "## Objective\n- summarized so far");
        assert_eq!(split.cursor, Some(2));
        assert_eq!(split.continuity, "tail of the last chunk");
        assert_eq!(split.window, 100_000);
        assert_eq!(split.buffer_tokens, 1234);

        // The staging is a distinct final line, so it never collides with an item.
        let file = store.file_path("5000");
        let contents = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert!(lines.last().unwrap().starts_with("{\"split\":"));
    }

    #[test]
    fn test_reasoning_persists_for_display_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        // A live display session: the assistant turn streams a "+ Thought"
        // block before its answer.
        let mut session = make_test_session(
            "6000",
            "Reasoning Session",
            vec![make_user_msg("msg-0", "Why is the sky blue?")],
        );
        session.messages.push(Message {
            id: "msg-1".to_string(),
            role: MessageRole::Assistant,
            parts: vec![
                Part::Reasoning(ReasoningPart {
                    text: "Rayleigh scattering...".to_string(),
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
        });

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
        assert!(matches!(&parts[0], Part::Reasoning(r) if r.text == "Rayleigh scattering..."));
        assert!(matches!(&parts[1], Part::Text(t) if t.text == "Because of Rayleigh scattering."));

        // The reasoning line is written as a distinct final line, so it never
        // enters the context the context manager loads.
        let file = store.file_path("6000");
        let contents = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert!(lines.last().unwrap().starts_with("{\"reasoning\":"));
        assert!(lines.last().unwrap().contains("Rayleigh scattering"));
    }

    #[test]
    fn test_reasoning_between_tool_calls_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        // Reasoning blocks interleaved with tool calls: R1 precedes the read
        // call, R2 precedes the final answer.
        let mut session = make_test_session(
            "7000",
            "Reasoning Tools",
            vec![make_user_msg("msg-0", "Read the file")],
        );
        session.messages.push(Message {
            id: "msg-1".to_string(),
            role: MessageRole::Assistant,
            parts: vec![
                Part::Reasoning(ReasoningPart {
                    text: "I'll read the file first.".to_string(),
                    collapsed: true,
                }),
                Part::Tool(ToolPart {
                    tool: "read".to_string(),
                    input: serde_json::json!({"path": "x"}),
                    output: Some("contents".to_string()),
                    status: ToolStatus::Completed,
                    tool_call_id: None,
                    is_start: true,
                    is_streaming: false,
                    cached_line_count: None,
                }),
                Part::Reasoning(ReasoningPart {
                    text: "Now I can answer.".to_string(),
                    collapsed: true,
                }),
                Part::Text(TextPart {
                    text: "The file says contents.".to_string(),
                    synthetic: false,
                }),
            ],
            created_at: 2000,
            agent: None,
            model: None,
        });

        // Harness context: user, tool call, tool result, closure (no reasoning).
        let context = make_context(vec![
            user_item(1, "Read the file"),
            ContextItem::ToolCall {
                id: 2,
                call_id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: "{\"path\":\"x\"}".to_string(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 3,
                call_id: "call_1".to_string(),
                content: "contents".to_string(),
                useless: false,
            },
            ContextItem::Closure {
                id: 4,
                content: "The file says contents.".to_string(),
            },
        ]);

        store.save_session_with_context(&session, &context);

        let loaded = store.load_session("7000").unwrap();
        // Messages: user, MERGED tool chain (call + result in one part, with
        // R1), closure (with R2). The reasoning blocks keep their positions.
        assert_eq!(loaded.messages.len(), 3);
        let call_parts = &loaded.messages[1].parts;
        assert_eq!(call_parts.len(), 2);
        assert!(
            matches!(&call_parts[0], Part::Reasoning(r) if r.text == "I'll read the file first.")
        );
        assert!(
            matches!(&call_parts[1], Part::Tool(t)
                if t.tool == "read" && t.output.as_deref() == Some("contents")),
            "the tool chain restores merged, with name and output"
        );
        let answer_parts = &loaded.messages[2].parts;
        assert_eq!(answer_parts.len(), 2);
        assert!(matches!(&answer_parts[0], Part::Reasoning(r) if r.text == "Now I can answer."));
    }

    #[test]
    fn test_session_store_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session1 = make_test_session("1000", "First", vec![]);
        let session2 = make_test_session("2000", "Second", vec![]);

        store.save_session_with_context(&session1, &make_context(vec![user_item(1, "Hi")]));
        store.save_session_with_context(&session2, &make_context(vec![user_item(1, "Hey")]));

        let list = store.list_sessions();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "2000");
        assert_eq!(list[1].session_id, "1000");
    }

    #[test]
    fn test_delete_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("12345", "To Delete", vec![]);
        store.save_session_with_context(&session, &make_context(vec![user_item(1, "Hello")]));
        assert!(store.has_session("12345"));

        store.delete_session("12345");
        assert!(!store.has_session("12345"));
        assert!(store.load_context("12345").is_none());
    }

    #[test]
    fn test_evict_old_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        for i in 0..(MAX_SESSIONS_ON_DISK + 5) {
            let session = make_test_session(&format!("{i:05}"), &format!("Session {i}"), vec![]);
            store.save_session_with_context(&session, &make_context(vec![user_item(1, "Hello")]));
        }

        let list = store.list_sessions();
        assert!(list.len() <= MAX_SESSIONS_ON_DISK);
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

        let session_a = make_test_session("1000", "Project A", vec![]);
        store_a.save_session_with_context(&session_a, &make_context(vec![user_item(1, "Hi")]));

        let session_b = make_test_session("2000", "Project B", vec![]);
        store_b.save_session_with_context(&session_b, &make_context(vec![user_item(1, "Yo")]));

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

        let session = make_test_session("100", "CWD Test", vec![]);
        store.save_session_with_context(&session, &make_context(vec![]));

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

    /// The full error-display contract across a save/restore: an API error
    /// lives in the item log as a display-only `Error` item, and the restored
    /// transcript brings it back with the `msg-err-` id the renderer styles as
    /// the red error box — never as plain assistant prose. The fallback
    /// conversion (no authoritative log on disk) maps the display message to
    /// the same item shape, so the model never inherits a provider failure.
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

        // The restored item log keeps the error OUT of the model context.
        let state = store.load_context("8100").unwrap();
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&state);
        let msgs = cm.build_messages("");
        assert_eq!(msgs.len(), 1, "only the user prompt reaches the model");
        assert_eq!(msgs[0].role, "user");

        // Fallback conversion (no authoritative log): the display error
        // message becomes an Error item, not assistant prose.
        let fallback = context_from_messages(&loaded.messages);
        assert!(matches!(
            fallback.items[1],
            ContextItem::Error { ref content, .. } if content == "Error: HTTP 401 - unauthorized"
        ));
        assert_eq!(fallback.items.len(), 2);
    }
}
