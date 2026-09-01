//! Session persistence module.
//!
//! Stores chat sessions as individual JSONL files organized by CWD:
//! `{data_dir}/sessions/{cwd_hash}/session-{timestamp}.jsonl`. The JSONL is
//! the SINGLE source of storage for session data — both the display
//! transcript and the model-facing agent context.
//!
//! ## Format
//!
//! Each JSONL file represents one complete chat session:
//!
//! - Line 1: Session metadata (JSON object with title, created_at, cwd, etc.)
//!   plus the context-manager bookkeeping (see [`ContextBookkeeping`]: id
//!   counter, budget, overflow state, split staging and the visibility
//!   markers).
//! - Lines 2+: Tagged records, one JSON object per line:
//!   `{"Message": …}` — a display message ([`StoredMessage`]);
//!   `{"Item": …}` — one [`ContextItem`] of the model-facing timeline, in
//!   conversation order. Legacy files (written before the unification) hold
//!   bare message objects with no tag; they load as messages.
//!
//! ## Mapping of the former `.ctx` companion file
//!
//! Everything the bincode companion used to carry lives in the JSONL now:
//!
//! | `.ctx` field     | JSONL location                                    |
//! |------------------|---------------------------------------------------|
//! | `items`          | `Item` records, one per line, timeline order      |
//! | `next_id`        | header `context` object                           |
//! | `max_tokens`     | header `context` object                           |
//! | `overflow_model` | header `context` object                           |
//! | `split`          | header `context` object                           |
//! | `visible_from`   | header `context` object                           |
//! | `hidden`         | header `context` object                           |
//!
//! ## Display-only data and display-only context items
//!
//! Some data exists only for display and is never restored into the agent
//! context: reasoning ("+ Thought") blocks and tool statuses live in the
//! message parts; synthetic text parts never reach the model. Two context
//! items are display-only in the model-facing direction: `Error` (persisted
//! as an `Item` record AND a `msg-err-` message, but skipped by
//! `ContextManager::build_messages`) and `Compaction` (persisted as an
//! `Item` record AND a `msg-ctx-` status message). The `ctx_ids` map on each
//! message line is pure bookkeeping that lets display actions (revert, fork)
//! locate the items backing a message; it is never parsed into a context.
//!
//! ## Legacy `.ctx` companions
//!
//! Sessions written by older builds carry a bincode `session-{id}.ctx`
//! companion. [`SessionStore::load_context`] falls back to it when the JSONL
//! header holds no context bookkeeping; the first context-aware save writes
//! the unified JSONL and deletes the companion. Until then, display-only
//! saves leave it untouched, and orphaned companions (JSONL gone) are swept
//! on store creation.
//!
//! Sessions are grouped by CWD (current working directory). The CWD path is
//! hashed with xxHash32 to produce a deterministic subdirectory name, so
//! sessions started in different directories never mix. The full CWD path is
//! also stored in the session header for display and filtering.
//!
//! A session is only persisted when it contains actual dialog:
//! at least one user message AND at least one valid assistant response
//! (error-only responses don't count as dialog).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Datelike;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh32::xxh32;

use cosh::harness::context::{ContextItem, ContextManagerState, SplitState};

use crate::types::{Message, MessageRole, Part, Session};

/// Number of session files to keep on disk per CWD. Oldest files are evicted first.
const MAX_SESSIONS_ON_DISK: usize = 50;

/// A queued session snapshot for the background save-writer thread. A
/// display-only save (title rename, message edit, session switch) must
/// never clobber the authoritative context, so when `context` is `None`
/// the writer preserves the context records already on disk (item lines +
/// header bookkeeping).
struct SaveJob {
    session: Box<crate::types::Session>,
    context: Option<ContextManagerState>,
}

/// The context-manager bookkeeping persisted in the session header: every
/// field of [`ContextManagerState`] except `items` (the items live as `Item`
/// records, one per line, after the message records). Kept in the header so
/// a reload reconstructs the exact snapshot the harness last persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContextBookkeeping {
    next_id: u64,
    max_tokens: usize,
    overflow_model: Option<String>,
    split: Option<SplitState>,
    visible_from: Option<u64>,
    hidden: HashSet<u64>,
}

/// Deserialize the header's `context` object, downgrading a present-but-
/// unreadable object to `None` (with a warning) instead of failing the
/// whole header: a context format break must never take the display
/// transcript down with it (the reader then falls back to the legacy
/// companion, and a resume surfaces the "Context lost" toast).
fn deserialize_context<'de, D>(deserializer: D) -> Result<Option<ContextBookkeeping>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<ContextBookkeeping>::deserialize(deserializer) {
        Ok(context) => Ok(context),
        Err(e) => {
            log::warn!("unreadable context bookkeeping in session header: {e}");
            Ok(None)
        }
    }
}

impl From<&ContextManagerState> for ContextBookkeeping {
    fn from(state: &ContextManagerState) -> Self {
        Self {
            next_id: state.next_id,
            max_tokens: state.max_tokens,
            overflow_model: state.overflow_model.clone(),
            split: state.split.clone(),
            visible_from: state.visible_from,
            hidden: state.hidden.clone(),
        }
    }
}

/// One tagged record on a session line (lines 2+ of the JSONL): a display
/// message or one item of the model-facing context timeline. Externally
/// tagged, so a message line is `{"Message":{…}}` and an item line is
/// `{"Item":{"User":{…}}}` — legacy untagged message lines fail the
/// [`Record`] parse and are retried as bare [`StoredMessage`]s.
#[derive(Debug, Serialize, Deserialize)]
enum Record {
    Message(StoredMessage),
    Item(ContextItem),
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
    /// Context-manager bookkeeping (see [`ContextBookkeeping`]). `None` on
    /// legacy files written before the JSONL unification — the reader then
    /// falls back to the bincode `.ctx` companion.
    #[serde(default, deserialize_with = "deserialize_context")]
    context: Option<ContextBookkeeping>,
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
        let store = Self {
            sessions_dir,
            cwd_hash,
        };
        store.sweep_orphan_ctx();
        store
    }

    /// Test-only store rooted at an explicit directory (lets integration
    /// tests drive the real load path against fixture session files).
    #[cfg(test)]
    pub(crate) fn with_dir(sessions_dir: std::path::PathBuf, cwd_hash: String) -> Self {
        std::fs::create_dir_all(&sessions_dir).ok();
        let store = Self {
            sessions_dir,
            cwd_hash,
        };
        store.sweep_orphan_ctx();
        store
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Persist a session to disk as a JSONL file in the current CWD's
    /// subdirectory, immediately on the CALLER thread.
    ///
    /// This is the display-only save shape (title rename, message edit,
    /// session switch, fork, revert): the context records already on disk
    /// (item lines + header bookkeeping) are spliced verbatim into the new
    /// file, so the authoritative model-facing context can never be
    /// clobbered by a display edit. SYNCHRONOUS — test plumbing
    /// only; production callers must use [`Self::save_session_async`] so the
    /// write is FIFO-ordered on the writer thread.
    pub fn save_session(&self, session: &Session) {
        self.persist(session, None);
    }

    /// Persist a session together with an explicit context-manager snapshot,
    /// writing the unified JSONL (message records + item records + header
    /// bookkeeping) immediately on the CALLER thread (the harness paths
    /// Done / Stopped / ContextSnapshot use the async variant instead). The
    /// session's `ctx_ids` map is refreshed from the snapshot before persisting
    /// — but only on the CLONE persisted here; the caller's session (and any
    /// cache holding it) is NOT updated. If you need the cache refreshed, use
    /// [`Self::save_session_async_with_context`] with a mutable session.
    /// SYNCHRONOUS — test plumbing only; production saves must go through the
    /// FIFO writer thread.
    pub fn save_session_with_context(&self, session: &Session, context: &ContextManagerState) {
        let mut session = session.clone();
        Self::update_ctx_ids(&mut session, context);
        self.persist(&session, Some(context));
    }

    /// Persist a session WITHOUT blocking the caller (the UI thread),
    /// preserving the context records already on disk. See
    /// [`Self::save_session`].
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
    /// Takes the session MUTABLY: the `ctx_ids` mapping (message id → context
    /// item ids) is computed from the snapshot and written back into the
    /// caller's session, so the in-memory cache always carries the mapping a
    /// later display-only save (title rename, revert, fork) needs to persist
    /// it unchanged. The context is taken BY VALUE and moved onto the writer
    /// thread — no deep clone of the context items happens on the caller (UI)
    /// thread, only a cheap `VecDeque` pointer move. The serialization pass
    /// runs on the writer thread too.
    pub fn save_session_async_with_context(
        &self,
        session: &mut crate::types::Session,
        context: ContextManagerState,
    ) {
        Self::update_ctx_ids(session, &context);
        self.enqueue_save(SaveJob {
            session: Box::new(session.clone()),
            context: Some(context),
        });
    }

    /// Hand a save job to the single background writer thread. FIFO ordering is
    /// preserved, so a newer snapshot always lands after (and wins over) an
    /// older in-flight job.
    ///
    /// Each job carries its own `SessionStore` clone: the writer thread is a
    /// process-wide singleton, but it must persist to the ENQUEUING store's
    /// directory — binding one store at first use would silently route every
    /// later job to that first store's directory.
    fn enqueue_save(&self, job: SaveJob) {
        static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<(SessionStore, SaveJob)>> =
            std::sync::OnceLock::new();
        let tx = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<(SessionStore, SaveJob)>();
            let spawned = std::thread::Builder::new()
                .name("session-save".into())
                .spawn(move || {
                    while let Ok((store, job)) = rx.recv() {
                        store.persist(&job.session, job.context.as_ref());
                    }
                });
            if spawned.is_err() {
                log::warn!("failed to spawn session-save thread");
            }
            tx
        });
        // If the thread failed to spawn there is no receiver; fall back to a
        // synchronous save rather than dropping the snapshot.
        if let Err(send_err) = tx.send((self.clone(), job)) {
            let (_, job) = send_err.0;
            self.persist(&job.session, job.context.as_ref());
        }
    }

    /// Persist one snapshot atomically. A context-aware save writes the
    /// message records, the item records and the header bookkeeping from the
    /// SAME snapshot, then retires a legacy `.ctx` companion. A display-only
    /// save splices the context records (item lines + bookkeeping) from the
    /// file currently on disk — read on the writer thread at execution time,
    /// so any context save queued earlier lands first (FIFO) and the splice
    /// source is always at least as new as the messages being written.
    /// `update_ctx_ids` is the CALLER's duty (the async path refreshes the
    /// in-memory session before enqueueing).
    fn persist(&self, session: &Session, context: Option<&ContextManagerState>) {
        match context {
            Some(state) => {
                let written = self.write_session(
                    session,
                    Some(&ContextBookkeeping::from(state)),
                    state.items.iter(),
                );
                // Retire the legacy bincode companion ONLY after the unified
                // write actually landed: the companion is the last copy of a
                // legacy session's context, so a failed write must leave it
                // in place (context is never left behind display).
                if written {
                    self.delete_ctx(&session.id);
                }
            }
            None => {
                let (bookkeeping, items) = self.read_disk_context(&session.id);
                self.write_session(session, bookkeeping.as_ref(), items.iter());
            }
        }
    }

    /// Read the context section (header bookkeeping + item records) of the
    /// session file currently on disk — the splice source for display-only
    /// saves. Returns `(None, empty)` for a missing file or a legacy file
    /// without embedded context (its `.ctx` companion stays authoritative
    /// until the next context-aware save migrates it).
    fn read_disk_context(
        &self,
        session_id: &str,
    ) -> (Option<ContextBookkeeping>, Vec<ContextItem>) {
        let Ok(content) = std::fs::read_to_string(self.file_path(session_id)) else {
            return (None, Vec::new());
        };
        let mut lines = content.lines();
        let Some(header_line) = lines.next() else {
            return (None, Vec::new());
        };
        let Ok(header) = serde_json::from_str::<SessionHeader>(header_line.trim()) else {
            return (None, Vec::new());
        };
        let Some(mut bookkeeping) = header.context else {
            return (None, Vec::new());
        };
        let mut items = Vec::new();
        let mut skipped = 0usize;
        for line in lines {
            match serde_json::from_str::<Record>(line.trim()) {
                Ok(Record::Item(item)) => items.push(item),
                // Message records (and empty lines) are not context.
                Ok(Record::Message(_)) => continue,
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            log::warn!(
                "skipped {skipped} unreadable record(s) while splicing the context of \
                 session {session_id}"
            );
        }
        bookkeeping.drop_dangling_boundary(items.iter());
        (Some(bookkeeping), items)
    }

    /// Compute the message id → context item ids mapping for a session against
    /// a context snapshot, and store it in `session.ctx_ids`.
    ///
    /// The grouping mirrors the display renderer's expectations exactly: each
    /// `User` item is its own message; a `ToolCall` opens an assistant message
    /// that absorbs every `ToolResult` carrying its `call_id` (matched
    /// order-independently, so interleaved parallel chains group correctly; an
    /// orphan result renders as a message of its own, exactly like the display);
    /// a text-ish item (`Assistant` / `Closure` / `Compaction` / `Error`) is a
    /// message of its own.
    ///
    /// Conservative: if the group count does not match the message count the
    /// mapping is left UNCHANGED — bookkeeping must never corrupt itself by
    /// guessing. With append-only visibility (compaction/sweep hide instead
    /// of delete) the counts match by construction for as long as the session
    /// lives; the guard is a safety net against streaming races and future
    /// mutation sites that break the invariant.
    fn update_ctx_ids(session: &mut Session, context: &ContextManagerState) {
        let mut groups: Vec<Vec<u64>> = Vec::new();
        let mut call_group: HashMap<&str, usize> = HashMap::new();
        for item in &context.items {
            match item {
                ContextItem::User { id, .. } => groups.push(vec![*id]),
                ContextItem::ToolCall { id, call_id, .. } => {
                    call_group.insert(call_id.as_str(), groups.len());
                    groups.push(vec![*id]);
                }
                ContextItem::ToolResult { id, call_id, .. } => {
                    match call_group.get(call_id.as_str()) {
                        Some(&group_idx) => groups[group_idx].push(*id),
                        None => groups.push(vec![*id]),
                    }
                }
                ContextItem::Assistant { id, .. }
                | ContextItem::Closure { id, .. }
                | ContextItem::Compaction { id, .. }
                | ContextItem::Error { id, .. } => groups.push(vec![*id]),
            }
        }

        if groups.len() != session.messages.len() {
            log::warn!(
                "ctx_ids mapping skipped for session {}: {} item groups \
                 vs {} display messages",
                session.id,
                groups.len(),
                session.messages.len()
            );
            return;
        }
        for (message, ids) in session.messages.iter().zip(groups) {
            session.ctx_ids.insert(message.id.clone(), ids);
        }
    }

    /// Rename a session on disk. The rename goes through the FIFO writer
    /// thread as a display-only save (rewrites the file atomically from the
    /// on-disk content, title swapped — the context records are spliced
    /// verbatim). The WRITE is FIFO-ordered, but the content is captured on
    /// the caller thread: a context save queued but not yet executed can be
    /// superseded by the title job's older snapshot (an accepted,
    /// microseconds-wide window — see the round-1 review of this task).
    /// No-op when the session is not on disk (logged), or when the file
    /// holds unparseable lines: a full rewrite would permanently drop them,
    /// so the rename refuses instead.
    pub fn update_title(&self, session_id: &str, new_title: &str) {
        if let Ok(contents) = std::fs::read_to_string(self.file_path(session_id)) {
            // Skip the header (line 1 — not a record) and empty lines; any
            // other unparseable line makes the rewrite refuse.
            let corrupt = contents
                .lines()
                .enumerate()
                .filter(|(i, l)| *i != 0 && !l.trim().is_empty())
                .any(|(_, l)| classify_line(l) == LineKind::Corrupt);
            if corrupt {
                log::warn!(
                    "refusing to rename session {session_id}: corrupt line on disk \
                     (a rewrite would drop it)"
                );
                return;
            }
        } else {
            return;
        }
        let Some(mut session) = self.load_session(session_id) else {
            log::warn!("refusing to rename session {session_id}: unparseable header");
            return;
        };
        session.title = new_title.to_string();
        session.title_generated = true;
        self.save_session_async(&session);
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

    /// Load a full session from its JSONL file (the display transcript —
    /// message records only; item records are skipped).
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

        // Remaining lines: tagged records (legacy untagged lines load as
        // messages)
        let mut messages: Vec<Message> = Vec::new();
        let mut ctx_ids_by_message: HashMap<String, Vec<u64>> = HashMap::new();
        for (idx, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let stored = match serde_json::from_str::<Record>(line) {
                Ok(Record::Message(stored)) => stored,
                // A context item record — not part of the display transcript.
                Ok(Record::Item(_)) => continue,
                Err(record_err) => match serde_json::from_str::<StoredMessage>(line) {
                    Ok(stored) => stored,
                    Err(e) => {
                        // A corrupted line loses exactly one message — log it
                        // (session id, 1-based line number, parse error)
                        // rather than silently dropping a chunk of the
                        // transcript.
                        log::warn!(
                            "skipping unparseable line {} in session {session_id}: {e} \
                             (record parse: {record_err})",
                            idx + 2
                        );
                        continue;
                    }
                },
            };
            if !stored.ctx_ids.is_empty() {
                ctx_ids_by_message.insert(stored.id.clone(), stored.ctx_ids.clone());
            }
            messages.push(stored.into_message());
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
            ctx_ids: ctx_ids_by_message,
        })
    }

    /// Load the context-manager state for a session from its JSONL file —
    /// the authoritative model-facing context, restored verbatim into the
    /// harness on resume. The item records are concatenated in timeline
    /// order and combined with the header bookkeeping into the exact
    /// snapshot the harness last persisted.
    ///
    /// A legacy file (header without embedded context) falls back to the
    /// bincode `.ctx` companion of pre-unification builds. Returns `None`
    /// if neither source exists or decodes.
    pub fn load_context(&self, session_id: &str) -> Option<ContextManagerState> {
        if let Some(state) = self.load_unified_context(session_id) {
            return Some(state);
        }
        self.load_legacy_ctx(session_id)
    }

    /// Reconstruct the context state from the unified JSONL: header
    /// bookkeeping + `Item` records in timeline order. `None` when the file
    /// is missing, its header is unparseable, or it predates the
    /// unification (no embedded context — the legacy companion is the
    /// source then).
    fn load_unified_context(&self, session_id: &str) -> Option<ContextManagerState> {
        let content = std::fs::read_to_string(self.file_path(session_id)).ok()?;
        let mut lines = content.lines();
        let header: SessionHeader = serde_json::from_str(lines.next()?.trim()).ok()?;
        let mut bookkeeping = header.context?;
        let mut items: VecDeque<ContextItem> = VecDeque::new();
        let mut skipped = 0usize;
        for line in lines {
            match serde_json::from_str::<Record>(line.trim()) {
                Ok(Record::Item(item)) => items.push_back(item),
                // Message records are the display transcript, not context.
                Ok(Record::Message(_)) => continue,
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            // The bookkeeping still describes the FULL timeline, but the
            // skipped records' items are gone — say so (the header and the
            // item records can disagree after a corrupt write).
            log::warn!(
                "skipped {skipped} unreadable record(s) while loading the context of \
                 session {session_id}"
            );
        }
        bookkeeping.drop_dangling_boundary(items.iter());
        Some(ContextManagerState {
            items,
            next_id: bookkeeping.next_id,
            max_tokens: bookkeeping.max_tokens,
            overflow_model: bookkeeping.overflow_model,
            split: bookkeeping.split,
            visible_from: bookkeeping.visible_from,
            hidden: bookkeeping.hidden,
        })
    }

    /// Legacy fallback: decode a bincode `.ctx` companion written by
    /// pre-unification builds. Returns `None` if no companion file exists
    /// or it cannot be decoded.
    fn load_legacy_ctx(&self, session_id: &str) -> Option<ContextManagerState> {
        let bytes = self.load_ctx(session_id)?;
        match bincode::deserialize::<ContextManagerState>(&bytes) {
            Ok(state) => Some(state),
            Err(e) => {
                log::warn!("failed to decode context state for session {session_id}: {e}");
                None
            }
        }
    }

    /// Load the context snapshot for a session with the item timeline TRUNCATED
    /// by `keep` (an item id survives when `keep(item_id)` is true) — a real
    /// deletion, backing the destructive display actions (revert, fork). The
    /// visibility markers are adjusted to the surviving timeline: a deleted
    /// compaction anchor clears `visible_from` (un-compaction — the surviving
    /// pre-compaction history becomes model-visible again) and `hidden` ids of
    /// deleted items are pruned. Bookkeeping (`next_id`, `max_tokens`,
    /// `overflow_model`, `split`) is kept as-is: ids only grow, and a gap in
    /// the timeline is harmless. Returns `None` when there is no context
    /// source to filter (the caller falls back to a display-only save).
    pub fn load_ctx_filtered(
        &self,
        session_id: &str,
        keep: impl Fn(u64) -> bool,
    ) -> Option<ContextManagerState> {
        let mut state = self.load_context(session_id)?;
        state.items.retain(|item| keep(item.id()));
        // Adjust the visibility markers to the truncated timeline:
        // - a boundary that no longer exists means the compaction anchor was
        //   cut away (revert past the compaction point) — everything that
        //   survives is visible again ("un-compaction");
        // - hidden ids of deleted items are pruned.
        if let Some(boundary) = state.visible_from
            && !state.items.iter().any(|it| it.id() == boundary)
        {
            state.visible_from = None;
        }
        let live: HashSet<u64> = state.items.iter().map(ContextItem::id).collect();
        state.hidden.retain(|h| live.contains(h));
        Some(state)
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

    /// On-disk paths (JSONL + legacy `.ctx` companion) of a session — for
    /// the /tmp undo snapshots taken before destructive display actions
    /// (revert). The companion path may not exist; the snapshot captures its
    /// absence so a rollback of a pre-unification session stays consistent.
    pub fn session_paths(&self, session_id: &str) -> (PathBuf, PathBuf) {
        (self.file_path(session_id), self.ctx_file_path(session_id))
    }

    // ── Legacy .ctx companions (bincode, pre-unification builds) ───────────

    /// Build path for the legacy bincode `.ctx` companion file.
    fn ctx_file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("session-{session_id}.ctx"))
    }

    /// Load the raw bytes of a legacy `.ctx` companion file.
    /// Returns `None` if no companion file exists or it cannot be read.
    fn load_ctx(&self, session_id: &str) -> Option<Vec<u8>> {
        let path = self.ctx_file_path(session_id);
        std::fs::read(&path).ok()
    }

    /// Delete the legacy `.ctx` companion file for a session (a no-op when
    /// none exists). Called when the unified JSONL becomes authoritative
    /// (context-aware save, session deletion) so a stale companion can
    /// never resurrect old context state.
    fn delete_ctx(&self, session_id: &str) {
        std::fs::remove_file(self.ctx_file_path(session_id)).ok();
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Build the absolute path for a session file within the current CWD subdirectory.
    fn file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("session-{session_id}.jsonl"))
    }

    /// Write the header line, the message records and the item records for a
    /// session. `bookkeeping`/`items` come from the context snapshot on a
    /// context-aware save, or are spliced from the previous on-disk file on a
    /// display-only save (`None`/empty for a fresh or legacy session — a
    /// legacy file keeps its `.ctx` companion as the context source).
    ///
    /// The write is atomic (see [`atomic_write`]): a reader only ever sees the
    /// complete old or new file, so the display transcript and the context
    /// records can never tear apart. Returns whether the write landed (a
    /// failure is logged and leaves the previous file — and any legacy
    /// companion — untouched).
    fn write_session<'a>(
        &self,
        session: &Session,
        bookkeeping: Option<&ContextBookkeeping>,
        items: impl Iterator<Item = &'a ContextItem>,
    ) -> bool {
        let file_path = self.file_path(&session.id);
        let header = self.build_header(session, bookkeeping.cloned());

        let mut lines = Vec::new();

        // Line 1: session header.
        match serde_json::to_string(&header) {
            Ok(json) => lines.push(json),
            Err(e) => log::warn!("failed to serialize header for session {}: {e}", session.id),
        }

        // Message records: one per display message.
        for (i, msg) in session.messages.iter().enumerate() {
            let mut stored = StoredMessage::from(msg);
            if let Some(ids) = session.ctx_ids.get(&msg.id) {
                stored.ctx_ids = ids.clone();
            }
            match serde_json::to_string(&Record::Message(stored)) {
                Ok(json) => lines.push(json),
                Err(e) => {
                    log::warn!(
                        "failed to serialize message {i} for session {}: {e}",
                        session.id
                    )
                }
            }
        }

        // Item records: the model-facing context timeline, one per line.
        for item in items {
            match serde_json::to_string(&Record::Item(item.clone())) {
                Ok(json) => lines.push(json),
                Err(e) => {
                    log::warn!(
                        "failed to serialize context item {} for session {}: {e}",
                        item.id(),
                        session.id
                    )
                }
            }
        }

        if let Err(e) = atomic_write(&file_path, lines.join("\n").as_bytes()) {
            log::warn!("failed to save session {}: {e}", session.id);
            return false;
        }

        self.evict_old_sessions();
        true
    }

    /// Build a `SessionHeader` from a `Session`, populating `cwd` with the
    /// canonicalized current working directory and carrying the context
    /// bookkeeping into the header's `context` field.
    ///
    /// The session's own recorded model selection (provider + model +
    /// reasoning) wins. Sessions without one — e.g. saved before this
    /// feature — fall back to deriving provider/model from the last valid
    /// assistant message, which preserves the old behavior.
    fn build_header(
        &self,
        session: &Session,
        context: Option<ContextBookkeeping>,
    ) -> SessionHeader {
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
            context,
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

        // Count the message records (item records are context, not dialog).
        let mut message_count: usize = 0;
        for line in reader.lines().map_while(Result::ok) {
            if !line.trim().is_empty() && classify_line(&line) != LineKind::Item {
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

    /// Delete orphaned `.ctx` companions — a companion file whose JSONL session
    /// is gone. Orphans can arise from a crash between the two `remove_file`s
    /// of `delete_session`/eviction (JSONL first), from a failed JSONL write
    /// whose `.ctx` write still succeeded, or from manual deletion; they leak
    /// disk silently (nothing lists them) and must never resurrect context
    /// state. Runs once per store creation, O(n) over the sessions directory.
    fn sweep_orphan_ctx(&self) {
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ctx") {
                continue;
            }
            // A `.ctx` is orphaned when its same-stem `.jsonl` is missing.
            if !path.with_extension("jsonl").exists()
                && let Err(e) = std::fs::remove_file(&path)
            {
                log::warn!(
                    "failed to remove orphaned context file {}: {e}",
                    path.display()
                );
            }
        }
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

// ── Line classification ───────────────────────────────────────────────────

/// Drop a compaction boundary whose anchor item is missing from the loaded
/// timeline (the anchor record was skipped, corrupt, or lost): without the
/// anchor the model would see NOTHING below the boundary — the summary and
/// the history both gone. Surfacing the surviving raw history is the better
/// fallback, and it mirrors `load_ctx_filtered`'s un-compaction rule (a
/// deleted anchor clears `visible_from`).
impl ContextBookkeeping {
    /// Drop a compaction boundary whose anchor item is missing from the
    /// loaded timeline (the anchor record was skipped, corrupt, or lost):
    /// without the anchor the model would see NOTHING below the boundary —
    /// the summary and the history both gone. Surfacing the surviving raw
    /// history is the better fallback, and it mirrors `load_ctx_filtered`'s
    /// un-compaction rule (a deleted anchor clears `visible_from`).
    fn drop_dangling_boundary<'a>(&mut self, items: impl Iterator<Item = &'a ContextItem>) {
        let Some(boundary) = self.visible_from else {
            return;
        };
        if !items.into_iter().any(|it| it.id() == boundary) {
            log::warn!(
                "compaction anchor {boundary} is missing from the session records — \
                 clearing the compaction boundary (un-compaction)"
            );
            self.visible_from = None;
        }
    }
}

/// The kind of a session-file body line: a display message record, a context
/// item record, or an unparseable/corrupt line. Used by the cheap counting
/// passes ([`SessionStore::read_summary`], [`SessionStore::update_title`]).
/// The classification is a FULL typed parse — a line that carries the right
/// tag but an unreadable payload (e.g. a `Part` variant written by a newer
/// build) must count as [`LineKind::Corrupt`], or a rewrite would silently
/// drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// A `{"Message": …}` record or a legacy untagged message line.
    Message,
    /// A `{"Item": …}` context record — not part of the display transcript.
    Item,
    /// Neither — a corrupt line a rewrite must never drop.
    Corrupt,
}

/// Classify one body line of a session file (see [`LineKind`]). Empty lines
/// are `Corrupt` by this classifier; callers skip them beforehand.
fn classify_line(line: &str) -> LineKind {
    let line = line.trim();
    if line.is_empty() {
        return LineKind::Corrupt;
    }
    match serde_json::from_str::<Record>(line) {
        Ok(Record::Message(_)) => LineKind::Message,
        Ok(Record::Item(_)) => LineKind::Item,
        Err(_) => {
            // Legacy pre-unification message line: a bare StoredMessage.
            if serde_json::from_str::<StoredMessage>(line).is_ok() {
                LineKind::Message
            } else {
                LineKind::Corrupt
            }
        }
    }
}

/// Atomically persist `bytes` at `path`: write to a `.tmp` sibling in the same
/// directory, then `fs::rename` over the final path (atomic on the same
/// filesystem). A reader only ever sees the complete old or the complete new
/// file, and a crash mid-write never leaves a torn file at `path` — the tmp
/// sibling is removed on any write/rename failure (a process kill can leave a
/// `.tmp` behind; it is invisible to listing and eviction, which filter on
/// real extensions).
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    // The tmp sibling must live in the SAME directory as the target so the
    // rename stays on one filesystem (cross-device rename fails). The full
    // file name is kept (suffix appended, extension NOT replaced) so the JSONL
    // and the `.ctx` of one session never share a tmp path across the writer
    // thread and a sync save. NOTE: two writers racing on the SAME target
    // still share one deterministic tmp name — production saves all run on
    // the FIFO writer thread now (TODO.md task 4); only tests and the
    // spawn-failure fallback remain synchronous.
    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp_path, bytes)?;
        std::fs::rename(&tmp_path, path)
    };
    match write() {
        Ok(()) => Ok(()),
        Err(e) => {
            std::fs::remove_file(&tmp_path).ok();
            Err(e)
        }
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
    /// Context-manager item ids backing this message (see
    /// `update_ctx_ids`). Bookkeeping for display actions — never parsed
    /// into a context.
    #[serde(default)]
    ctx_ids: Vec<u64>,
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
            ctx_ids: Vec::new(),
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
            ctx_ids: HashMap::new(),
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
            visible_from: None,
            hidden: Default::default(),
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

    /// A legacy session (pre-unification JSONL without embedded context +
    /// bincode `.ctx` companion) still resumes with its model-facing context:
    /// `load_context` falls back to the companion when the header carries no
    /// bookkeeping.
    #[test]
    fn legacy_ctx_companion_still_loads_as_the_context_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        // A legacy on-disk pair: display-only JSONL (no header context) + a
        // bincode companion written by an older build.
        let session = make_test_session(
            "12345",
            "Legacy Session",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );
        store.save_session(&session);
        let context = make_context(vec![user_item(1, "Hello"), assistant_item(2, "Hi")]);
        std::fs::write(
            store.ctx_file_path("12345"),
            bincode::serialize(&context).unwrap(),
        )
        .unwrap();

        // The companion is the context source for the legacy file.
        let loaded = store.load_context("12345").expect("legacy fallback");
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.next_id, 42);

        // Deleting the session removes the companion too — a stale `.ctx`
        // must never resurrect deleted context state on a future resume.
        store.delete_session("12345");
        assert!(!store.has_session("12345"));
        assert!(store.load_ctx("12345").is_none());
        assert!(store.load_context("12345").is_none());
    }

    /// Once the JSONL carries embedded context, a stale legacy companion is
    /// ignored: the unified file is the single source of truth.
    #[test]
    fn legacy_companion_is_ignored_once_the_jsonl_carries_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12346",
            "Migrated Session",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );
        // A stale companion holding DIFFERENT items than the live context.
        let stale = make_context(vec![user_item(1, "stale prompt")]);
        std::fs::write(
            store.ctx_file_path("12346"),
            bincode::serialize(&stale).unwrap(),
        )
        .unwrap();

        let context = make_context(vec![user_item(1, "Hello"), assistant_item(2, "Hi")]);
        store.save_session_with_context(&session, &context);

        // The unified save retired the companion, and the context comes from
        // the JSONL records — never from the stale bincode payload.
        assert!(
            store.load_ctx("12346").is_none(),
            "the companion was retired by the unified save"
        );
        let loaded = store.load_context("12346").unwrap();
        assert_eq!(loaded.items.len(), 2);
        let ContextItem::User { original, .. } = &loaded.items[0] else {
            panic!("expected the live user item");
        };
        assert_eq!(original, "Hello");
    }

    /// A context-aware save whose unified write FAILS must keep the legacy
    /// `.ctx` companion: for a legacy session the companion is the only copy
    /// of the model-facing context, and the context is never left behind the
    /// display. Once a write succeeds, the companion is retired.
    #[test]
    fn a_failed_context_save_keeps_the_legacy_companion() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12347",
            "Legacy Migration",
            vec![make_user_msg("msg-0", "Hello")],
        );
        store.save_session(&session);
        let context = make_context(vec![user_item(1, "Hello")]);
        std::fs::write(
            store.ctx_file_path("12347"),
            bincode::serialize(&context).unwrap(),
        )
        .unwrap();

        // Sabotage the JSONL path: a directory where the file must be written
        // makes the atomic rename fail (the tmp sibling is written fine, the
        // rename over a directory errors).
        let path = store.file_path("12347");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        store.save_session_with_context(&session, &context);

        assert!(
            store.load_ctx("12347").is_some(),
            "the companion survives a failed unified write"
        );

        // Heal the sabotage: the next context save lands, and only then is
        // the companion retired.
        std::fs::remove_dir(&path).unwrap();
        store.save_session_with_context(&session, &context);
        assert!(store.load_ctx("12347").is_none(), "retired after success");
        assert_eq!(store.load_context("12347").unwrap().items.len(), 1);
    }

    /// A header whose `context` object is unreadable (e.g. a bookkeeping
    /// format break) must not take the display transcript down with it:
    /// the session still loads, and the context source degrades to the
    /// legacy companion (here: absent → the resume warns truthfully).
    #[test]
    fn an_unreadable_header_context_degrades_to_display_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12348",
            "Broken Bookkeeping",
            vec![make_user_msg("msg-0", "hi")],
        );
        store.save_session(&session);
        let file = store.file_path("12348");
        let original = std::fs::read_to_string(&file).unwrap();
        // Corrupt ONLY the context object: a string where a number belongs.
        let broken = original.replacen(
            "\"context\":null",
            "\"context\":{\"next_id\":\"not-a-number\"}",
            1,
        );
        assert_ne!(broken, original, "the header carried a context object");
        std::fs::write(&file, broken).unwrap();

        let loaded = store
            .load_session("12348")
            .expect("the display transcript survives the bookkeeping break");
        assert_eq!(loaded.messages.len(), 1);
        assert!(store.load_context("12348").is_none());
    }

    /// A corrupt item record loses exactly that item — and a compaction
    /// boundary whose anchor was among the lost records is CLEARED, so the
    /// surviving raw history reaches the model instead of an amnesiac empty
    /// view (mirrors `load_ctx_filtered`'s un-compaction rule).
    #[test]
    fn a_lost_compaction_anchor_clears_the_boundary_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("12349", "Lost Anchor", vec![]);
        let mut context = make_context(vec![
            user_item(1, "pre-compaction"),
            assistant_item(2, "the anchor"),
        ]);
        context.visible_from = Some(2); // hides id 1
        store.save_session_with_context(&session, &context);

        // Corrupt the anchor's item record on disk.
        let file = store.file_path("12349");
        let contents = std::fs::read_to_string(&file).unwrap();
        let patched: String = contents
            .lines()
            .map(|l| {
                if l.contains("\"the anchor\"") {
                    "{\"Item\":{\"Assistant\":{\"id\":2,\"original\":".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file, patched).unwrap();

        let loaded = store.load_context("12349").unwrap();
        assert_eq!(loaded.items.len(), 1, "the corrupt anchor is dropped");
        assert_eq!(
            loaded.visible_from, None,
            "the boundary cleared with its anchor"
        );
        // The surviving history reaches the model again.
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&loaded);
        assert_eq!(cm.build_messages("").len(), 1);
    }

    /// A line with the right record tag but an unreadable payload (e.g.
    /// written by a newer build) counts as corrupt: `update_title` refuses
    /// the rewrite instead of silently dropping the message.
    #[test]
    fn update_title_refuses_a_tagged_line_with_an_unreadable_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("7301", "Bad Payload", vec![make_user_msg("msg-0", "hi")]);
        store.save_session(&session);
        let file = store.file_path("7301");
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{original}{{\"Message\":123}}\n")).unwrap();

        store.update_title("7301", "Renamed");

        let contents = std::fs::read_to_string(&file).unwrap();
        assert!(
            contents.contains("{\"Message\":123}"),
            "the unreadable record must stay on disk"
        );
        assert!(
            contents.contains("\"Bad Payload\""),
            "the title must stay untouched (the rename refused)"
        );
        assert_eq!(classify_line("{\"Message\":123}"), LineKind::Corrupt);
    }

    /// The full context snapshot — items, bookkeeping AND the split staging —
    /// round-trips verbatim through the session JSONL (item records + header
    /// bookkeeping), so an interrupted split resumes exactly where it stopped.
    #[test]
    fn context_state_roundtrips_through_the_session_jsonl() {
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
        // Visibility markers ride in the header bookkeeping.
        context.visible_from = Some(2);
        context.hidden.insert(1);

        store.save_session_with_context(&session, &context);

        let loaded = store.load_context("5000").unwrap();
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.next_id, 42);
        assert_eq!(loaded.max_tokens, 100_000);
        assert_eq!(loaded.overflow_model.as_deref(), Some("gpt-4o-mini"));
        let split = loaded.split.clone().expect("split staging was persisted");
        assert_eq!(split.buffer, "## Objective\n- summarized so far");
        assert_eq!(split.cursor, Some(2));
        assert_eq!(split.continuity, "tail of the last chunk");
        assert_eq!(split.window, 100_000);
        assert_eq!(split.buffer_tokens, 1234);
        assert_eq!(loaded.visible_from, Some(2));
        assert!(loaded.hidden.contains(&1));

        // The visibility markers are live: the restored manager hides the
        // pre-compaction item from the model.
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&loaded);
        let msgs = cm.build_messages("");
        assert_eq!(msgs.len(), 1, "item 1 is behind the boundary");
    }

    /// A display-only save SPLICES the context records from the on-disk file:
    /// the item records and the header bookkeeping survive verbatim, while
    /// the display messages (even new ones) are rewritten — the display edit
    /// can never clobber the model-facing context.
    #[test]
    fn display_only_save_splices_the_on_disk_context_records() {
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
        let mut context = make_context(vec![user_item(1, "Hello"), assistant_item(2, "Hi")]);
        context.visible_from = Some(2);
        context.hidden.insert(1);
        store.save_session_with_context(&session, &context);

        // A display-only save: renamed title AND an extra display message
        // (e.g. a synthetic line the TUI added without the harness).
        let mut renamed = session.clone();
        renamed.title = "Renamed".to_string();
        renamed.messages.push(make_user_msg("msg-2", "extra line"));
        store.save_session(&renamed);

        // The context records survived the rewrite untouched.
        let loaded = store.load_context("1000").unwrap();
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.visible_from, Some(2));
        assert!(loaded.hidden.contains(&1));
        assert_eq!(loaded.next_id, 42);

        // The display changes landed, and the summary counts messages (not
        // item records).
        let session = store.load_session("1000").unwrap();
        assert_eq!(session.title, "Renamed");
        assert_eq!(session.messages.len(), 3);
        let summary = store.list_sessions().into_iter().next().unwrap();
        assert_eq!(summary.message_count, 3);
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
        // A leftover legacy companion from the pre-unification era.
        std::fs::write(store.ctx_file_path("12345"), b"context-state").ok();
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
            // A leftover legacy companion per session.
            std::fs::write(store.ctx_file_path(&format!("{i:05}")), b"context-state").ok();
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
    /// and as a display-only `Error` item record that `build_messages`
    /// skips — so the model never inherits a provider failure after a
    /// restart.
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

    /// The ctx_ids mapping: a merged tool chain in the display (one assistant
    /// message carrying the call+result) maps to MULTIPLE context items — the
    /// call, its result and the answer text — with ids that let revert/fork
    /// truncate the exact items behind a clicked message. The mapping is
    /// computed from a snapshot, persisted in the JSONL lines, and survives a
    /// reload.
    #[test]
    fn ctx_ids_map_merged_tool_chains_and_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "7000",
            "Mapping Session",
            vec![
                make_user_msg("msg-0", "edit the file"),
                make_assistant_msg("msg-tool", "editing"),
                make_assistant_msg("msg-1", "done"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "edit the file"),
            ContextItem::ToolCall {
                id: 2,
                call_id: "call-a".into(),
                name: "fs_edit".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 3,
                call_id: "call-a".into(),
                content: "--- a.rs\n+++ b.rs\n@@ -1 +1 @@\n-old\n+new".into(),
                useless: false,
            },
            assistant_item(4, "editing"),
        ]);

        let mut persisted = session.clone();
        SessionStore::update_ctx_ids(&mut persisted, &context);
        // User message → the User item; the tool-chain message → call +
        // result (a merged chain maps to MULTIPLE items); the text answer →
        // its own Assistant item.
        assert_eq!(persisted.ctx_ids["msg-0"], vec![1]);
        assert_eq!(persisted.ctx_ids["msg-tool"], vec![2, 3]);
        assert_eq!(persisted.ctx_ids["msg-1"], vec![4]);

        store.save_session_with_context(&persisted, &context);

        // The mapping survives a reload (persisted per JSONL line).
        let loaded = store.load_session("7000").unwrap();
        assert_eq!(loaded.ctx_ids["msg-0"], vec![1]);
        assert_eq!(loaded.ctx_ids["msg-tool"], vec![2, 3]);
        assert_eq!(loaded.ctx_ids["msg-1"], vec![4]);
    }

    /// Interleaved parallel tool chains (the result of call-a comes after the
    /// call of chain b) still group correctly: each result joins its own
    /// call's group, order-independently.
    #[test]
    fn ctx_ids_handle_interleaved_parallel_chains() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "7001",
            "Parallel Session",
            vec![
                make_user_msg("msg-0", "read both"),
                make_assistant_msg("msg-tool-a", "chain a"),
                make_assistant_msg("msg-tool-b", "chain b"),
                make_assistant_msg("msg-1", "chain a answer"),
                make_assistant_msg("msg-2", "chain b answer"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "read both"),
            ContextItem::ToolCall {
                id: 2,
                call_id: "call-a".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolCall {
                id: 3,
                call_id: "call-b".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"b.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 4,
                call_id: "call-a".into(),
                content: "contents of a".into(),
                useless: false,
            },
            ContextItem::ToolResult {
                id: 5,
                call_id: "call-b".into(),
                content: "contents of b".into(),
                useless: false,
            },
            assistant_item(6, "chain a"),
            assistant_item(7, "chain b"),
        ]);

        let mut persisted = session.clone();
        SessionStore::update_ctx_ids(&mut persisted, &context);
        assert_eq!(persisted.ctx_ids["msg-0"], vec![1]);
        assert_eq!(persisted.ctx_ids["msg-tool-a"], vec![2, 4]);
        assert_eq!(persisted.ctx_ids["msg-tool-b"], vec![3, 5]);
        assert_eq!(persisted.ctx_ids["msg-1"], vec![6]);
        assert_eq!(persisted.ctx_ids["msg-2"], vec![7]);

        store.save_session_with_context(&persisted, &context);
        let loaded = store.load_session("7001").unwrap();
        assert_eq!(loaded.ctx_ids["msg-tool-b"], vec![3, 5]);
        assert_eq!(loaded.ctx_ids["msg-2"], vec![7]);
    }

    /// A mismatch between item groups and display messages (e.g. a display
    /// save racing a streaming turn) must leave the existing mapping
    /// UNCHANGED — the bookkeeping never guesses.
    #[test]
    fn ctx_ids_mapping_is_untouched_on_a_count_mismatch() {
        let session = make_test_session("7002", "Mismatch", vec![make_user_msg("msg-0", "hi")]);
        // Two groups on the context side vs one display message.
        let context = make_context(vec![user_item(1, "hi"), assistant_item(2, "hello")]);

        let mut persisted = session.clone();
        let before = persisted.ctx_ids.clone();
        SessionStore::update_ctx_ids(&mut persisted, &context);
        assert_eq!(
            persisted.ctx_ids, before,
            "a mismatched mapping must not be guessed"
        );
    }

    /// `load_ctx_filtered` is the engine behind display actions: revert drops
    /// the items behind the reverted messages (keep = NOT removed), fork keeps
    /// only the items behind the kept messages (keep = IN kept). Bookkeeping
    /// (`next_id` etc.) is untouched so ids keep growing monotonically.
    #[test]
    fn load_ctx_filtered_supports_revert_and_fork() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "7100",
            "Filter Session",
            vec![
                make_user_msg("msg-0", "hi"),
                make_assistant_msg("msg-tool", "editing"),
                make_assistant_msg("msg-1", "done"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "hi"),
            ContextItem::ToolCall {
                id: 2,
                call_id: "call-a".into(),
                name: "fs_edit".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 3,
                call_id: "call-a".into(),
                content: "diff".into(),
                useless: false,
            },
            assistant_item(4, "done"),
        ]);
        store.save_session_with_context(&session, &context);

        // Revert at msg-tool: items 2 and 3 (and everything after) go away.
        let removed: std::collections::HashSet<u64> = [2u64, 3, 4].into_iter().collect();
        let reverted = store
            .load_ctx_filtered("7100", |id| !removed.contains(&id))
            .expect("the context records exist");
        assert_eq!(reverted.items.len(), 1, "only the user item survives");
        let ContextItem::User { id, .. } = &reverted.items[0] else {
            panic!("expected the user item");
        };
        assert_eq!(*id, 1);
        assert_eq!(reverted.next_id, context.next_id, "bookkeeping untouched");

        // Fork at msg-tool: only items 1 and 2 (up to and including the
        // clicked message's chain) are kept.
        let kept: std::collections::HashSet<u64> = [1u64, 2].into_iter().collect();
        let forked = store
            .load_ctx_filtered("7100", |id| kept.contains(&id))
            .expect("the context records exist");
        assert_eq!(forked.items.len(), 2);

        // No context source at all → the caller must fall back to a
        // display-only save.
        assert!(
            store
                .load_ctx_filtered("no-ctx-session", |_| true)
                .is_none()
        );
    }

    /// End-to-end through the FIFO queue: revert derives the filtered
    /// snapshot from the on-disk context records, persists the unified file
    /// async, and a reload shows the truncated timeline with the mapping
    /// intact.
    #[test]
    fn revert_round_trips_the_unified_file_through_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "7200",
            "Queue Revert",
            vec![
                make_user_msg("msg-0", "hi"),
                make_assistant_msg("msg-tool", "editing"),
                make_assistant_msg("msg-1", "done"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "hi"),
            assistant_item(2, "editing"),
            assistant_item(3, "done"),
        ]);
        store.save_session_with_context(&session, &context);

        // The user reverts at msg-1 (drop msg-1 and everything after): the
        // mapping frozen in the session (task 3) says item 3 backs it.
        let mut reverted = session.clone();
        reverted.messages.truncate(2);
        reverted
            .ctx_ids
            .retain(|k, _| reverted.messages.iter().any(|m| &m.id == k));
        let removed: std::collections::HashSet<u64> = [3u64].into_iter().collect();
        let new_ctx = store
            .load_ctx_filtered("7200", |id| !removed.contains(&id))
            .expect("the context records exist");
        store.save_session_async_with_context(&mut reverted, new_ctx);

        // The save runs on the FIFO writer thread — poll for the landing.
        // The message records are written in the same atomic file as the
        // context records, so polling on the transcript is the completion
        // barrier.
        for _ in 0..200 {
            if let Some(loaded) = store.load_session("7200")
                && loaded.messages.len() == 2
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let loaded_ctx = store.load_context("7200").expect("the context reloads");
        assert_eq!(
            loaded_ctx.items.len(),
            2,
            "item 3 was removed from the context"
        );
        let loaded = store.load_session("7200").unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.ctx_ids["msg-0"], vec![1]);
        assert_eq!(loaded.ctx_ids["msg-tool"], vec![2]);
    }

    /// update_title must REFUSE to rewrite a file that holds unparseable
    /// lines: a full rewrite would permanently drop them. The on-disk title
    /// stays untouched instead.
    #[test]
    fn update_title_refuses_a_file_with_a_corrupt_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("7300", "Corrupt Line", vec![make_user_msg("msg-0", "hi")]);
        store.save_session(&session);
        let file = store.sessions_dir.join("session-7300.jsonl");
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{original}{{broken json\n")).unwrap();

        store.update_title("7300", "Renamed");

        let contents = std::fs::read_to_string(&file).unwrap();
        assert!(
            contents.contains("broken json"),
            "the corrupt line must stay on disk"
        );
        assert!(
            contents.contains("\"Corrupt Line\""),
            "the title must stay untouched (the rename refused)"
        );
    }

    /// Orphaned `.ctx` companions (JSONL gone, companion left behind by a
    /// crash between the two removals, a failed JSONL write, or a manual
    /// delete) are swept on store creation. A matching pair is never touched.
    #[test]
    fn orphaned_ctx_files_are_swept_on_store_creation() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("8000", "Kept", vec![make_user_msg("msg-0", "hi")]);
        store.save_session(&session);
        std::fs::write(store.ctx_file_path("8000"), b"kept-context").ok();
        // An orphan: no session-8001.jsonl anywhere.
        std::fs::write(store.ctx_file_path("8001"), b"orphan-context").ok();

        // A fresh store over the same directory sweeps the orphan.
        let fresh = SessionStore::with_dir(dir.path().join("testhash"), "testhash".into());
        assert!(fresh.load_ctx("8000").is_some(), "a matched pair is kept");
        assert!(
            fresh.load_ctx("8001").is_none(),
            "an orphaned .ctx is removed on store creation"
        );
        assert!(fresh.load_session("8000").is_some(), "the session survives");
    }

    /// Reverting past the compaction point deletes the Compaction anchor —
    /// `visible_from` clears and the surviving pre-compaction history becomes
    /// model-visible again (un-compaction). Hidden ids of deleted items are
    /// pruned.
    #[test]
    fn revert_past_the_compaction_boundary_un_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("7400", "Uncompact", vec![]);
        // Timeline: pre-compaction history (id 1, hidden by the boundary),
        // the anchor (id 2), a post-compaction item (id 3), and a swept
        // chain (ids 4-5, individually hidden).
        let mut context = make_context(vec![
            user_item(1, "pre-compaction"),
            assistant_item(2, "the anchor"),
            user_item(3, "post"),
            ContextItem::ToolCall {
                id: 4,
                call_id: "swept".into(),
                name: "find".into(),
                arguments: "{}".into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 5,
                call_id: "swept".into(),
                content: "no matches".into(),
                useless: true,
            },
        ]);
        context.visible_from = Some(2); // hides id 1
        context.hidden.insert(4);
        context.hidden.insert(5);
        store.save_session_with_context(&session, &context);

        // Revert EVERYTHING before id 3 (the cut sits before the anchor): ids
        // 1 and 2 are deleted — including the anchor — and the swept chain is
        // dropped with it.
        let reverted = store
            .load_ctx_filtered("7400", |id| id == 3)
            .expect("the context records exist");
        assert_eq!(reverted.items.len(), 1);
        assert_eq!(reverted.items[0].id(), 3);
        assert_eq!(reverted.visible_from, None, "the anchor is gone");
        assert!(
            reverted.hidden.is_empty(),
            "hidden ids of deleted items are pruned"
        );
        // The model sees the survivor again (no boundary, not hidden).
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&reverted);
        let msgs = cm.build_messages("");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.as_deref(), Some("post"));

        // Control: a cut AFTER the anchor keeps the boundary (compaction
        // still holds).
        let partial = store
            .load_ctx_filtered("7400", |id| id <= 3)
            .expect("the context records exist");
        assert_eq!(partial.visible_from, Some(2), "the anchor survived");
    }

    /// With append-only visibility the ctx_ids mapping NEVER freezes: the
    /// timeline retains every item through compaction, so groups keep
    /// matching the display and the mapping keeps updating.
    #[test]
    fn ctx_ids_keep_updating_across_a_compaction() {
        let mut session = make_test_session(
            "7401",
            "Mapping After Compaction",
            vec![
                make_user_msg("msg-0", "old prompt"),
                make_assistant_msg("msg-1", "old answer"),
                // The compaction summary streams as its own display message.
                make_assistant_msg("msg-summary", "## Objective\n- summarized"),
                make_user_msg("msg-2", "new prompt"),
            ],
        );
        // Snapshot AFTER a compaction: ids 1-2 pre-boundary (hidden), the
        // anchor (3) and the new prompt (4) visible.
        let mut context = make_context(vec![
            user_item(1, "old prompt"),
            assistant_item(2, "old answer"),
            assistant_item(3, "## Objective\n- summarized"),
            user_item(4, "new prompt"),
        ]);
        context.visible_from = Some(3);
        context.hidden.insert(1);
        context.hidden.insert(2);

        SessionStore::update_ctx_ids(&mut session, &context);
        assert_eq!(session.ctx_ids["msg-0"], vec![1]);
        assert_eq!(session.ctx_ids["msg-1"], vec![2]);
        assert_eq!(session.ctx_ids["msg-summary"], vec![3]);
        assert_eq!(session.ctx_ids["msg-2"], vec![4]);
    }
}
