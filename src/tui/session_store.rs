//! Session persistence module.
//!
//! Stores immutable session event histories as JSONL files organized by CWD:
//! `{data_dir}/sessions/{cwd_hash}/session-{root-id}.jsonl`. A root session
//! and all of its logical branches share one file. The JSONL is the single
//! source of truth; [`Session`] and [`ContextManagerState`] are projections.
//!
//! ## Format
//!
//! Every line in a new history is a versioned [`HistoryEvent`]. `Genesis` is
//! the first delta, followed by fine-grained metadata, message, and context
//! mutations. Revert, rollback, fork, snapshots, and deletion are reference
//! or control deltas. Existing files in the former header/record format are
//! treated as an immutable synthetic event-zero base and may receive modern
//! deltas without migration or replacement.
//!
//! ## Display-only data and display-only context items
//!
//! Some data exists only for display and is never restored into the agent
//! context: reasoning ("+ Thought") blocks and tool statuses live in the
//! message parts; synthetic text parts never reach the model. Two context
//! items are display-only in the model-facing direction: `Error` and
//! `Compaction` have both message and context deltas, but only their message
//! forms are display state. Modern message boundaries are persisted as
//! `ContextDelta::MessageBoundary`; the `ctx_ids` map remains display
//! bookkeeping and a compatibility bridge for older histories.
//!
//! Sessions are grouped by CWD (current working directory). The CWD path is
//! hashed with xxHash32 to produce a deterministic subdirectory name, so
//! sessions started in different directories never mix. The full CWD path is
//! also stored in `Genesis`/`Fork` metadata for display and filtering.
//!
//! A session is only persisted when it contains actual dialog:
//! at least one user message AND at least one valid assistant response
//! (error-only responses don't count as dialog).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Datelike;
use serde::Serialize;
use xxhash_rust::xxh32::xxh32;

use cosh::harness::context::{ContextItem, ContextManagerState, MapReduceState};
use cosh::harness::events::{HarnessEvent, ToastVariant};

use crate::session_history::{
    BranchMetadata, BranchProjection, ContextBoundary, ContextDelta, Delta, HistoryEvent,
    HistoryProjection, MessageDelta, MetadataDelta, Reference, context_groups,
};
use crate::types::{MessageRole, Session};

const UNDO_VERSIONS_CAP: usize = 10;

/// A queued transport projection for the append writer. `context: None`
/// means no context delta is generated, so a display-only save cannot alter
/// model-facing state.
struct SaveJob {
    session: Box<crate::types::Session>,
    context: Option<ContextManagerState>,
}

enum StoreJob {
    Save(Box<SaveJob>),
    Title {
        session_id: String,
        title: String,
    },
    Tombstone {
        session_id: String,
    },
    Revert {
        session_id: String,
        message_id: String,
    },
    Rollback {
        session_id: String,
        version: String,
    },
    Fork {
        parent_id: String,
        message_id: String,
        forked: Box<Session>,
    },
}

struct QueuedStoreJob {
    store: SessionStore,
    job: StoreJob,
    completion: Option<std::sync::mpsc::Sender<bool>>,
}

/// Manages reading and writing session files to disk, isolated by CWD.
#[derive(Clone)]
pub struct SessionStore {
    /// Resolved path to the per-CWD sessions directory
    /// (e.g. `~/.local/share/cosh/sessions/{hash}/`).
    sessions_dir: PathBuf,
    /// The hash of the current working directory.
    cwd_hash: String,
    /// Optional UI channel: lock-contention notifications surface as toasts.
    notify: Option<tokio::sync::mpsc::UnboundedSender<HarnessEvent>>,
    /// How long a write waits for a foreign process's lock before giving up.
    lock_wait: Duration,
}

impl SessionStore {
    /// Create a new `SessionStore`, resolving the CWD and creating the
    /// per-CWD sessions directory. Honours `COSH_DATA_DIR` (test isolation;
    /// see `util::setup::data_dir_override`).
    ///
    /// # Panics
    /// Panics if the data directory cannot be determined (no override AND
    /// `ProjectDirs` fails, e.g. no `$HOME` set).
    pub fn new() -> Self {
        let sessions_dir = crate::util::setup::data_dir_override()
            .join("sessions")
            .join(compute_cwd_hash());
        std::fs::create_dir_all(&sessions_dir).ok();
        Self {
            sessions_dir,
            cwd_hash: compute_cwd_hash(),
            notify: None,
            lock_wait: LOCK_WAIT_TIMEOUT,
        }
    }

    /// Route store-level notifications (lock contention) into the TUI toast
    /// system through the harness event channel.
    #[must_use]
    pub fn with_notify(mut self, notify: tokio::sync::mpsc::UnboundedSender<HarnessEvent>) -> Self {
        self.notify = Some(notify);
        self
    }

    /// Test-only store rooted at an explicit directory (lets integration
    /// tests drive the real load path against fixture session files).
    #[cfg(test)]
    pub(crate) fn with_dir(sessions_dir: std::path::PathBuf, cwd_hash: String) -> Self {
        std::fs::create_dir_all(&sessions_dir).ok();
        Self {
            sessions_dir,
            cwd_hash,
            notify: None,
            lock_wait: LOCK_WAIT_TIMEOUT,
        }
    }

    /// Test-only: shorten the lock wait so contention tests finish fast.
    #[cfg(test)]
    pub(crate) fn with_lock_wait(mut self, lock_wait: Duration) -> Self {
        self.lock_wait = lock_wait;
        self
    }

    /// Test-only: route notifications into a channel the test owns.
    #[cfg(test)]
    pub(crate) fn with_test_notify(
        mut self,
        notify: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
    ) -> Self {
        self.notify = Some(notify);
        self
    }

    // --- Public API ---

    /// Synchronize a display projection through the FIFO append writer and
    /// wait for its deltas to land. Context state is left untouched.
    pub fn save_session(&self, session: &Session) {
        self.enqueue_job(
            StoreJob::Save(Box::new(SaveJob {
                session: Box::new(session.clone()),
                context: None,
            })),
            true,
        );
    }

    /// Synchronize display and context projections through the FIFO append
    /// writer and wait. The writer replays the current branch, computes typed
    /// differences, and appends them; it never serializes a replacement file.
    pub fn save_session_with_context(&self, session: &Session, context: &ContextManagerState) {
        let mut session = session.clone();
        Self::update_ctx_ids(&mut session, context);
        self.enqueue_job(
            StoreJob::Save(Box::new(SaveJob {
                session: Box::new(session),
                context: Some(context.clone()),
            })),
            true,
        );
    }

    /// Synchronize display state without blocking the UI thread. See
    /// [`Self::save_session`].
    pub fn save_session_async(&self, session: &crate::types::Session) {
        self.enqueue_job(
            StoreJob::Save(Box::new(SaveJob {
                session: Box::new(session.clone()),
                context: None,
            })),
            false,
        );
    }

    /// Synchronize session and context projections on the append writer. A
    /// single writer keeps event IDs and state differences FIFO-ordered.
    ///
    /// Takes the session MUTABLY: the `ctx_ids` mapping (message id → context
    /// item ids) is computed from the snapshot and written back into the
    /// caller's session, so the in-memory cache always carries the mapping a
    /// later display-only save (title rename, revert, fork) needs to persist
    /// it unchanged. The context is taken BY VALUE and moved onto the writer
    /// thread; replay, comparison, and serialization run on the writer.
    pub fn save_session_async_with_context(
        &self,
        session: &mut crate::types::Session,
        context: ContextManagerState,
    ) {
        Self::update_ctx_ids(session, &context);
        self.enqueue_job(
            StoreJob::Save(Box::new(SaveJob {
                session: Box::new(session.clone()),
                context: Some(context),
            })),
            false,
        );
    }

    /// Hand a save job to the single background writer thread. FIFO ordering is
    /// preserved, so a newer snapshot always lands after (and wins over) an
    /// older in-flight job.
    ///
    /// Each job carries its own `SessionStore` clone: the writer thread is a
    /// process-wide singleton, but it must persist to the ENQUEUING store's
    /// directory — binding one store at first use would silently route every
    /// later job to that first store's directory.
    fn enqueue_job(&self, job: StoreJob, wait: bool) -> bool {
        static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<QueuedStoreJob>> =
            std::sync::OnceLock::new();
        let tx = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<QueuedStoreJob>();
            let spawned = std::thread::Builder::new()
                .name("session-save".into())
                .spawn(move || {
                    while let Ok(queued) = rx.recv() {
                        let succeeded = queued.store.run_job(queued.job);
                        if let Some(completion) = queued.completion {
                            let _ = completion.send(succeeded);
                        }
                    }
                });
            if spawned.is_err() {
                log::warn!("failed to spawn session-save thread");
            }
            tx
        });
        let (completion, done) = if wait {
            let (completion, done) = std::sync::mpsc::channel();
            (Some(completion), Some(done))
        } else {
            (None, None)
        };
        let mut fallback_result = true;
        if let Err(send_err) = tx.send(QueuedStoreJob {
            store: self.clone(),
            job,
            completion,
        }) {
            let queued = send_err.0;
            fallback_result = queued.store.run_job(queued.job);
            if let Some(completion) = queued.completion {
                let _ = completion.send(fallback_result);
            }
        }
        if let Some(done) = done {
            done.recv().unwrap_or(false)
        } else {
            fallback_result
        }
    }

    fn run_job(&self, job: StoreJob) -> bool {
        // One exclusive kernel lock covers the whole read-diff-append window
        // of every write job. The lock is bound to the open file
        // description: if this process dies, the OS releases it — no orphan
        // lockfiles can freeze the store.
        let _lock =
            match acquire_store_lock(&self.sessions_dir, self.notify.as_ref(), self.lock_wait) {
                Ok(file) => file,
                Err(error) => {
                    log::warn!("session store write skipped: {error}");
                    return false;
                }
            };
        match job {
            StoreJob::Save(job) => self.persist(&job.session, job.context.as_ref()),
            StoreJob::Title { session_id, title } => self.append_title(&session_id, &title),
            StoreJob::Tombstone { session_id } => self.append_tombstone(&session_id),
            StoreJob::Revert {
                session_id,
                message_id,
            } => self.append_revert(&session_id, &message_id),
            StoreJob::Rollback {
                session_id,
                version,
            } => self.append_rollback(&session_id, &version),
            StoreJob::Fork {
                parent_id,
                message_id,
                forked,
            } => self.append_fork(&parent_id, &message_id, &forked),
        }
    }

    /// Replay the branch, diff a transport projection against it, and append
    /// one event per changed field/item. `update_ctx_ids` is the caller's duty.
    fn persist(&self, session: &Session, context: Option<&ContextManagerState>) -> bool {
        let direct_path = self.file_path(&session.id);
        let (path, contents, mut projection) = match self.load_history_for_branch(&session.id) {
            Some(loaded) => loaded,
            None if direct_path.exists() => {
                log::warn!(
                    "refusing to append session {}: its existing history cannot be replayed",
                    session.id
                );
                return false;
            }
            None => (direct_path, String::new(), HistoryProjection::default()),
        };
        let mut events = Vec::new();
        let mut next_event_id = projection.last_event_id.saturating_add(1).max(1);

        if !projection.branches.contains_key(&session.id) {
            let metadata = self.branch_metadata(session);
            let event = HistoryEvent::new(next_event_id, &session.id, Delta::Genesis { metadata });
            next_event_id += 1;
            events.push(event);
            projection = match HistoryProjection::replay(None, &events) {
                Ok(projection) => projection,
                Err(error) => {
                    log::warn!(
                        "failed to create session {} projection: {error:?}",
                        session.id
                    );
                    return false;
                }
            };
        }

        let Some(current) = projection.branches.get(&session.id) else {
            return false;
        };
        let changes = self.diff_session(current, session, context);
        events.extend(changes.into_iter().map(|delta| {
            let event = HistoryEvent::new(next_event_id, &session.id, delta);
            next_event_id += 1;
            event
        }));
        if let Err(error) = append_events(&path, &contents, &events) {
            log::warn!("failed to append session {}: {error}", session.id);
            return false;
        }
        true
    }

    fn branch_metadata(&self, session: &Session) -> BranchMetadata {
        let (provider, model) = effective_model_selection(session);
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        BranchMetadata {
            session_id: session.id.clone(),
            title: session.title.clone(),
            title_generated: session.title_generated,
            created_at: session.created_at,
            cwd,
            provider,
            model,
            reasoning: session.reasoning.clone(),
        }
    }

    fn diff_session(
        &self,
        current: &BranchProjection,
        requested: &Session,
        requested_context: Option<&ContextManagerState>,
    ) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let metadata = self.branch_metadata(requested);
        if current.session.title != metadata.title
            || current.session.title_generated != metadata.title_generated
        {
            deltas.push(Delta::Metadata {
                change: MetadataDelta::Title {
                    title: metadata.title,
                    title_generated: metadata.title_generated,
                },
            });
        }
        if current.session.provider != metadata.provider
            || current.session.model != metadata.model
            || current.session.reasoning != metadata.reasoning
        {
            deltas.push(Delta::Metadata {
                change: MetadataDelta::Model {
                    provider: metadata.provider,
                    model: metadata.model,
                    reasoning: metadata.reasoning,
                },
            });
        }

        let previous_messages: HashMap<&str, &crate::types::Message> = current
            .session
            .messages
            .iter()
            .map(|m| (m.id.as_str(), m))
            .collect();
        for message in &requested.messages {
            let previous = previous_messages.get(message.id.as_str()).copied();
            let old_ids = current
                .session
                .ctx_ids
                .get(&message.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let new_ids = requested
                .ctx_ids
                .get(&message.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if previous.is_none_or(|previous| !serialized_equal(previous, message))
                || old_ids != new_ids
            {
                deltas.push(Delta::Message {
                    change: MessageDelta::Upsert {
                        message: message.clone(),
                        context_item_ids: new_ids.to_vec(),
                    },
                });
            }
        }
        let requested_ids: HashSet<&str> = requested
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        for message in &current.session.messages {
            if !requested_ids.contains(message.id.as_str()) {
                deltas.push(Delta::Message {
                    change: MessageDelta::Remove {
                        message_id: message.id.clone(),
                    },
                });
            }
        }

        if let Some(context) = requested_context {
            deltas.extend(diff_context(current.context.as_ref(), context));
            let groups = context_groups(&context.items);
            for message in &requested.messages {
                let Some(ids) = requested.ctx_ids.get(&message.id) else {
                    continue;
                };
                let Some(owned) = ids
                    .iter()
                    .map(|id| groups.get(id).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let (Some(first_group), Some(last_group)) =
                    (owned.iter().min(), owned.iter().max())
                else {
                    continue;
                };
                let boundary = ContextBoundary {
                    first_group: *first_group,
                    last_group: *last_group,
                };
                // Validate the association against native context ownership.
                // A stale/partial display map must never invent a boundary.
                if current.message_boundaries.get(&message.id) != Some(&boundary) {
                    deltas.push(Delta::Context {
                        change: ContextDelta::MessageBoundary {
                            message_id: message.id.clone(),
                            boundary,
                        },
                    });
                }
            }
        }
        deltas
    }

    fn load_history_for_branch(
        &self,
        session_id: &str,
    ) -> Option<(PathBuf, String, HistoryProjection)> {
        let direct = self.file_path(session_id);
        if let Some((contents, projection)) = self.load_history_path(&direct)
            && projection.branches.contains_key(session_id)
        {
            return Some((direct, contents, projection));
        }
        let entries = std::fs::read_dir(&self.sessions_dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path == direct
                || path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
            {
                continue;
            }
            if let Some((contents, projection)) = self.load_history_path(&path)
                && projection.branches.contains_key(session_id)
            {
                return Some((path, contents, projection));
            }
        }
        None
    }

    fn load_history_path(&self, path: &std::path::Path) -> Option<(String, HistoryProjection)> {
        crate::session_snapshots::load(path)
            .map_err(|error| {
                log::warn!("failed to load session history {}: {error}", path.display());
            })
            .ok()
    }

    fn append_title(&self, session_id: &str, title: &str) -> bool {
        let Some((path, contents, projection)) = self.load_history_for_branch(session_id) else {
            return false;
        };
        let Some(branch) = projection.branches.get(session_id) else {
            return false;
        };
        if branch.deleted || branch.session.title == title && branch.session.title_generated {
            return !branch.deleted;
        }
        let event = HistoryEvent::new(
            projection.last_event_id.saturating_add(1).max(1),
            session_id,
            Delta::Metadata {
                change: MetadataDelta::Title {
                    title: title.to_string(),
                    title_generated: true,
                },
            },
        );
        if let Err(error) = append_events(&path, &contents, &[event]) {
            log::warn!("failed to append title for session {session_id}: {error}");
            return false;
        }
        true
    }

    fn append_tombstone(&self, session_id: &str) -> bool {
        let Some((path, contents, projection)) = self.load_history_for_branch(session_id) else {
            return false;
        };
        let Some(branch) = projection.branches.get(session_id) else {
            return false;
        };
        if branch.deleted {
            return true;
        }
        let event = HistoryEvent::new(
            projection.last_event_id.saturating_add(1).max(1),
            session_id,
            Delta::Tombstone,
        );
        if let Err(error) = append_events(&path, &contents, &[event]) {
            log::warn!("failed to append deletion for session {session_id}: {error}");
            return false;
        }
        self.remove_file_when_fully_deleted(&path);
        true
    }

    /// Physically remove a history file once NONE of its branches is active
    /// anymore. Tombstones are a soft deletion while any branch (parent,
    /// fork, or sibling fork) still refers to the same JSONL; once the last
    /// active branch is tombstoned the file would be permanently invisible
    /// in the TUI, so keeping it on disk would only accumulate garbage.
    /// A reload failure is conservative: the file stays on disk.
    fn remove_file_when_fully_deleted(&self, path: &std::path::Path) {
        let Some((_, projection)) = self.load_history_path(path) else {
            return;
        };
        if projection.branches.is_empty()
            || !projection.branches.values().all(|branch| branch.deleted)
        {
            return;
        }
        if let Err(error) = std::fs::remove_file(path) {
            log::warn!(
                "failed to remove fully deleted session history {}: {error}",
                path.display()
            );
        }
    }

    fn append_revert(&self, session_id: &str, message_id: &str) -> bool {
        let Some((path, contents, projection)) = self.load_history_for_branch(session_id) else {
            return false;
        };
        let Some(branch) = projection.branches.get(session_id) else {
            return false;
        };
        if branch.deleted
            || !branch
                .session
                .messages
                .iter()
                .any(|message| message.id == message_id)
        {
            return false;
        }
        let selection = match branch.context_selection(message_id, false) {
            Ok(selection) => selection,
            Err(error) => {
                log::warn!("cannot resolve context view for revert: {error:?}");
                return false;
            }
        };
        let next_version = projection
            .reverts
            .iter()
            .filter(|revert| revert.branch_id == session_id)
            .filter_map(|revert| revert.undo_label.strip_prefix('v'))
            .filter_map(|number| number.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let event = HistoryEvent::new(
            projection.last_event_id.saturating_add(1).max(1),
            session_id,
            Delta::Revert {
                target: Reference {
                    branch_id: session_id.to_string(),
                    event_id: branch.head_event_id,
                    selection,
                },
                undo_label: format!("v{next_version}"),
            },
        );
        append_events(&path, &contents, &[event]).is_ok()
    }

    fn append_rollback(&self, session_id: &str, version: &str) -> bool {
        let Some((path, contents, projection)) = self.load_history_for_branch(session_id) else {
            return false;
        };
        let Some(branch) = projection.branches.get(session_id) else {
            return false;
        };
        if branch.deleted {
            return false;
        }
        let Some(revert) = projection
            .reverts
            .iter()
            .rev()
            .find(|revert| revert.branch_id == session_id && revert.undo_label == version)
        else {
            return false;
        };
        let event = HistoryEvent::new(
            projection.last_event_id.saturating_add(1).max(1),
            session_id,
            Delta::Rollback {
                target: revert.previous.clone(),
                undo_label: version.to_string(),
            },
        );
        append_events(&path, &contents, &[event]).is_ok()
    }

    fn append_fork(&self, parent_id: &str, message_id: &str, forked: &Session) -> bool {
        let Some((path, contents, projection)) = self.load_history_for_branch(parent_id) else {
            return false;
        };
        let Some(parent) = projection.branches.get(parent_id) else {
            return false;
        };
        if parent.deleted
            || projection.branches.contains_key(&forked.id)
            || !parent
                .session
                .messages
                .iter()
                .any(|message| message.id == message_id)
        {
            return false;
        }
        let selection = match parent.context_selection(message_id, true) {
            Ok(selection) => selection,
            Err(error) => {
                log::warn!("cannot resolve context view for fork: {error:?}");
                return false;
            }
        };
        let event = HistoryEvent::new(
            projection.last_event_id.saturating_add(1).max(1),
            &forked.id,
            Delta::Fork {
                parent: Reference {
                    branch_id: parent_id.to_string(),
                    event_id: parent.head_event_id,
                    selection,
                },
                metadata: self.branch_metadata(forked),
            },
        );
        append_events(&path, &contents, &[event]).is_ok()
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
            // Live associations are emitted by the context producer. The
            // positional adapter is only for unbound legacy/test transports.
            if let Some(existing) = session.ctx_ids.get(&message.id)
                && *existing != ids
            {
                // The live binding wins; a mismatch means the positional
                // fallback disagrees with it and must never overwrite it.
                log::debug!(
                    "ctx_ids positional fallback disagrees with live binding for message {} of session {}",
                    message.id,
                    session.id
                );
            }
            session.ctx_ids.entry(message.id.clone()).or_insert(ids);
        }
    }

    /// Append a title change on the FIFO writer. The writer resolves the
    /// branch at execution time, so earlier queued context/display deltas land
    /// first and corrupt legacy lines remain untouched.
    pub fn update_title(&self, session_id: &str, new_title: &str) {
        self.enqueue_job(
            StoreJob::Title {
                session_id: session_id.to_string(),
                title: new_title.to_string(),
            },
            false,
        );
    }

    /// Load all session files from the current CWD's subdirectory, sorted by
    /// creation time (newest first).
    ///
    /// Only sessions from the same CWD as the current process are returned,
    /// giving natural per-directory isolation.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut summaries = self.list_in_dir(&self.sessions_dir);
        summaries.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        summaries
    }

    /// List sessions across ALL CWD subdirectories (for directory browsing).
    ///
    /// Returns summaries that include the `cwd` field so the UI can display
    /// which directory each session belongs to.
    pub fn list_all_sessions(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = Vec::new();
        // Parent directory of all per-CWD subdirectories.
        let Some(base) = self.sessions_dir.parent().map(PathBuf::from) else {
            return summaries;
        };
        let Ok(entries) = std::fs::read_dir(&base) else {
            return summaries;
        };
        for entry in entries.flatten().filter(|e| e.path().is_dir()) {
            summaries.extend(self.list_in_dir(&entry.path()));
        }
        summaries.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        summaries
    }

    /// Read every session summary in one directory (unsorted).
    fn list_in_dir(&self, dir: &std::path::Path) -> Vec<SessionSummary> {
        let mut summaries = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return summaries;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            let Some((_, projection)) = self.load_history_path(&path) else {
                continue;
            };
            summaries.extend(
                projection
                    .branches
                    .into_values()
                    .filter(|branch| !branch.deleted)
                    .map(|branch| SessionSummary {
                        session_id: branch.session.id.clone(),
                        title: branch.session.title.clone(),
                        created_at: branch.session.created_at,
                        message_count: branch.session.messages.len(),
                        cwd: branch.cwd.clone(),
                        model: branch.session.model.clone(),
                        title_generated: branch.session.title_generated,
                    }),
            );
        }
        summaries
    }

    /// Load a full logical session by replaying its immutable history.
    ///
    /// Returns `None` if the file does not exist or cannot be parsed.
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        let (_, _, projection) = self.load_history_for_branch(session_id)?;
        let branch = projection.branches.get(session_id)?;
        (!branch.deleted).then(|| branch.session.clone())
    }

    /// Derive the context-manager state for a logical branch and restore it
    /// into the harness on resume.
    ///
    /// Returns `None` if the file does not exist, cannot be parsed, or
    /// carries no embedded context.
    pub fn load_context(&self, session_id: &str) -> Option<ContextManagerState> {
        let (_, _, projection) = self.load_history_for_branch(session_id)?;
        let branch = projection.branches.get(session_id)?;
        if branch.deleted {
            return None;
        }
        branch.context.clone()
    }

    /// Append a revert that selects the state immediately before
    /// `message_id`. The previous head remains in the immutable history and
    /// becomes an undo reference.
    pub fn revert_session(&self, session_id: &str, message_id: &str) -> bool {
        self.enqueue_job(
            StoreJob::Revert {
                session_id: session_id.to_string(),
                message_id: message_id.to_string(),
            },
            true,
        )
    }

    /// Persistent undo labels derived from revert events, oldest first.
    pub fn undo_versions(&self, session_id: &str) -> Vec<String> {
        let Some((_, _, projection)) = self.load_history_for_branch(session_id) else {
            return Vec::new();
        };
        let versions: Vec<String> = projection
            .reverts
            .iter()
            .filter(|revert| revert.branch_id == session_id)
            .map(|revert| revert.undo_label.clone())
            .collect();
        versions
            .into_iter()
            .rev()
            .take(UNDO_VERSIONS_CAP)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Append a rollback to the state captured immediately before a revert.
    pub fn rollback_session(&self, session_id: &str, version: &str) -> bool {
        self.enqueue_job(
            StoreJob::Rollback {
                session_id: session_id.to_string(),
                version: version.to_string(),
            },
            true,
        )
    }

    /// Append a logical branch based on the parent state through
    /// `message_id`. The child is stored in the same physical JSONL.
    pub fn fork_session(&self, parent_id: &str, message_id: &str, forked: &Session) -> bool {
        self.enqueue_job(
            StoreJob::Fork {
                parent_id: parent_id.to_string(),
                message_id: message_id.to_string(),
                forked: Box::new(forked.clone()),
            },
            true,
        )
    }

    /// Append a logical deletion event. The history stays on disk while any
    /// other branch of the same JSONL file is still active; when the deleted
    /// branch was the last active one, the file is removed from disk.
    pub fn delete_session(&self, session_id: &str) {
        self.enqueue_job(
            StoreJob::Tombstone {
                session_id: session_id.to_string(),
            },
            true,
        );
    }

    /// Check whether a session with the given ID exists on disk.
    pub fn has_session(&self, session_id: &str) -> bool {
        self.load_history_for_branch(session_id)
            .and_then(|(_, _, projection)| projection.branches.get(session_id).cloned())
            .is_some_and(|branch| !branch.deleted)
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Build the absolute path for a session file within the current CWD subdirectory.
    fn file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("session-{session_id}.jsonl"))
    }
}

fn effective_model_selection(session: &Session) -> (Option<String>, Option<String>) {
    match (&session.provider, session.model.as_deref()) {
        (Some(_), Some(_)) => (session.provider.clone(), session.model.clone()),
        (None, Some("auto")) => (None, session.model.clone()),
        _ => session
            .messages
            .iter()
            .rev()
            .find(|message| {
                message.role == MessageRole::Assistant && !message.id.starts_with("msg-err-")
            })
            .map(|message| {
                let provider = message.model.as_deref().and_then(|model| {
                    model
                        .contains('/')
                        .then(|| model.split('/').next().map(String::from))
                        .flatten()
                });
                (provider, message.model.clone())
            })
            .unwrap_or((None, None)),
    }
}

fn diff_context(
    current: Option<&ContextManagerState>,
    requested: &ContextManagerState,
) -> Vec<Delta> {
    let mut deltas = Vec::new();
    let Some(current) = current else {
        deltas.push(Delta::Context {
            change: ContextDelta::Initialize {
                next_id: requested.next_id,
                max_tokens: requested.max_tokens,
                overflow_model: requested.overflow_model.clone(),
                split: requested.split.clone(),
                map_reduce: Box::new(requested.map_reduce.clone()),
                visible_from: requested.visible_from,
                hidden: requested.hidden.clone(),
                masked: requested.masked.clone(),
                todo: requested.todo.clone(),
                todo_after_item_id: Some(requested.items.back().map_or(0, ContextItem::id)),
                last_tool_set: requested.last_tool_set.clone(),
            },
        });
        deltas.extend(requested.items.iter().cloned().map(|item| Delta::Context {
            change: ContextDelta::ItemUpsert { item },
        }));
        return deltas;
    };

    let previous_items: HashMap<u64, &ContextItem> =
        current.items.iter().map(|item| (item.id(), item)).collect();
    for item in &requested.items {
        let previous = previous_items.get(&item.id()).copied();
        if previous.is_none_or(|previous| !serialized_equal(previous, item)) {
            deltas.push(Delta::Context {
                change: ContextDelta::ItemUpsert { item: item.clone() },
            });
        }
    }
    let requested_ids: HashSet<u64> = requested.items.iter().map(ContextItem::id).collect();
    for item in &current.items {
        if !requested_ids.contains(&item.id()) {
            deltas.push(Delta::Context {
                change: ContextDelta::ItemRemove { item_id: item.id() },
            });
        }
    }
    if !serialized_equal(&current.todo, &requested.todo) {
        deltas.push(Delta::Context {
            change: ContextDelta::Todo {
                value: requested.todo.clone(),
                after_item_id: Some(requested.items.back().map_or(0, ContextItem::id)),
            },
        });
    }
    if current.next_id != requested.next_id {
        deltas.push(Delta::Context {
            change: ContextDelta::NextId {
                value: requested.next_id,
            },
        });
    }
    if current.max_tokens != requested.max_tokens {
        deltas.push(Delta::Context {
            change: ContextDelta::MaxTokens {
                value: requested.max_tokens,
            },
        });
    }
    if current.overflow_model != requested.overflow_model {
        deltas.push(Delta::Context {
            change: ContextDelta::OverflowModel {
                value: requested.overflow_model.clone(),
            },
        });
    }
    if let (Some(previous), Some(requested_split)) = (&current.split, &requested.split)
        && previous.version == requested_split.version
        && previous.window == requested_split.window
        && requested_split.buffer.len() > previous.buffer.len()
        && requested_split.buffer.starts_with(previous.buffer.as_str())
    {
        deltas.push(Delta::Context {
            change: ContextDelta::SplitBufferAppend {
                appended: requested_split.buffer[previous.buffer.len()..].to_string(),
                cursor: requested_split.cursor,
                continuity: requested_split.continuity.clone(),
                buffer_tokens: requested_split.buffer_tokens,
            },
        });
    } else if !serialized_equal(&current.split, &requested.split) {
        deltas.push(Delta::Context {
            change: ContextDelta::Split {
                value: requested.split.clone(),
            },
        });
    }
    if let (Some(previous), Some(requested_staging)) = (&current.map_reduce, &requested.map_reduce)
        && let Some((ordinal, summary)) = single_accepted_segment(previous, requested_staging)
    {
        deltas.push(Delta::Context {
            change: ContextDelta::MapSegmentAccepted { ordinal, summary },
        });
    } else if !serialized_equal(&current.map_reduce, &requested.map_reduce) {
        deltas.push(Delta::Context {
            change: ContextDelta::MapReduce {
                value: Box::new(requested.map_reduce.clone()),
            },
        });
    }
    if current.visible_from != requested.visible_from {
        deltas.push(Delta::Context {
            change: ContextDelta::VisibleFrom {
                value: requested.visible_from,
            },
        });
    }
    let hidden: Vec<u64> = requested
        .hidden
        .difference(&current.hidden)
        .copied()
        .collect();
    if !hidden.is_empty() {
        deltas.push(Delta::Context {
            change: ContextDelta::Hidden {
                item_ids: hidden,
                hidden: true,
            },
        });
    }
    let shown: Vec<u64> = current
        .hidden
        .difference(&requested.hidden)
        .copied()
        .collect();
    if !shown.is_empty() {
        deltas.push(Delta::Context {
            change: ContextDelta::Hidden {
                item_ids: shown,
                hidden: false,
            },
        });
    }
    let masked: Vec<u64> = requested
        .masked
        .difference(&current.masked)
        .copied()
        .collect();
    if !masked.is_empty() {
        deltas.push(Delta::Context {
            change: ContextDelta::Masked {
                item_ids: masked,
                masked: true,
            },
        });
    }
    let unmasked: Vec<u64> = current
        .masked
        .difference(&requested.masked)
        .copied()
        .collect();
    if !unmasked.is_empty() {
        deltas.push(Delta::Context {
            change: ContextDelta::Masked {
                item_ids: unmasked,
                masked: false,
            },
        });
    }
    if !serialized_equal(&current.last_tool_set, &requested.last_tool_set) {
        deltas.push(Delta::Context {
            change: ContextDelta::LastToolSet {
                value: requested.last_tool_set.clone(),
            },
        });
    }
    deltas
}

fn serialized_equal<T: Serialize>(left: &T, right: &T) -> bool {
    serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
}

/// The one map summary that went from absent to present, when the two staging
/// states differ by exactly that transition. Replaying it onto `previous`
/// must reproduce `requested` exactly; anything else falls back to the
/// whole-state delta.
fn single_accepted_segment(
    previous: &MapReduceState,
    requested: &MapReduceState,
) -> Option<(usize, String)> {
    if previous.segments.len() != requested.segments.len() {
        return None;
    }
    let mut candidate: Option<(usize, String)> = None;
    for (before, after) in previous.segments.iter().zip(requested.segments.iter()) {
        if before.ordinal != after.ordinal || before.covered_ranges != after.covered_ranges {
            return None;
        }
        match (&before.summary, &after.summary) {
            (None, Some(summary)) => {
                if candidate.is_some() {
                    return None;
                }
                candidate = Some((after.ordinal, summary.clone()));
            }
            (before_summary, after_summary) if before_summary == after_summary => {}
            _ => return None,
        }
    }
    let (ordinal, summary) = candidate?;
    let mut probe = previous.clone();
    let segment = probe
        .segments
        .iter_mut()
        .find(|segment| segment.ordinal == ordinal)?;
    segment.summary = Some(summary.clone());
    if serialized_equal(&probe, requested) {
        Some((ordinal, summary))
    } else {
        None
    }
}

/// How long a write job waits for a foreign process's lock before the job
/// is dropped (the toast already told the user why).
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const LOCK_FILE_NAME: &str = ".cosh-session-store.lock";

/// Send a toast through the UI channel, ignoring a dead receiver.
fn send_toast(
    notify: Option<&tokio::sync::mpsc::UnboundedSender<HarnessEvent>>,
    message: &str,
    variant: ToastVariant,
) {
    if let Some(tx) = notify {
        let _ = tx.send(HarnessEvent::Toast {
            message: message.to_string(),
            variant,
        });
    }
}

/// Acquire the store's exclusive advisory lock. Kernel-managed (`flock` via
/// the standard library): releasing happens on drop AND automatically when
/// the holder dies, so a crashed process can never orphan the lock. On
/// contention the caller is notified once (toast), then the call polls until
/// the timeout expires.
fn acquire_store_lock(
    sessions_dir: &Path,
    notify: Option<&tokio::sync::mpsc::UnboundedSender<HarnessEvent>>,
    wait: Duration,
) -> std::io::Result<std::fs::File> {
    let path = sessions_dir.join(LOCK_FILE_NAME);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => return Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
    }
    send_toast(notify, "Session store busy.", ToastVariant::Warning);
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                send_toast(
                    notify,
                    "Session store stayed busy: the save was skipped and will be retried on the next change.",
                    ToastVariant::Error,
                );
                return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn append_events(
    path: &std::path::Path,
    previous_contents: &str,
    events: &[HistoryEvent],
) -> std::io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = Vec::new();
    if !previous_contents.is_empty() && !previous_contents.ends_with('\n') {
        bytes.push(b'\n');
    }
    for event in events {
        serde_json::to_writer(&mut bytes, event).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&bytes)?;
    // Durability against an OS crash, not just a process crash: the kernel
    // keeps appended data across a process death, but only an explicit sync
    // orders it onto the storage device. fdatasync flushes the new bytes and
    // the file size they depend on.
    file.sync_data()?;
    // Snapshots are a rebuildable acceleration structure. Once the event
    // append is durable, a failed snapshot must not report a failed action
    // (retrying a successful revert would produce a second undo record).
    if let Err(error) = crate::session_snapshots::checkpoint(path) {
        log::warn!("session snapshot deferred for {}: {error}", path.display());
    }
    Ok(())
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

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
    use crate::types::{Message, Part, ReasoningPart, TextPart, ToolPart, ToolStatus};
    use cosh::harness::context::{ContextItem, SplitState};

    #[test]
    fn versioned_view_survives_display_only_mapping_loss_and_restores_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut session = make_test_session(
            "view-loss",
            "View",
            vec![
                make_user_msg("m1", "keep"),
                make_assistant_msg("m2", "already hidden"),
                make_user_msg("m3", "discard secret"),
                make_assistant_msg("m4", "discard answer"),
            ],
        );
        let mut context = make_context(vec![
            user_item(1, "keep"),
            assistant_item(2, "already hidden"),
            user_item(3, "discard secret"),
            assistant_item(4, "discard answer"),
        ]);
        context.hidden.insert(2);
        store.save_session_with_context(&session, &context);
        session.ctx_ids.clear();
        store.save_session(&session);
        let path = store.file_path(&session.id);
        let prefix = std::fs::read(&path).unwrap();
        assert!(store.revert_session(&session.id, "m3"));
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        let selected = store.load_context(&session.id).unwrap();
        assert_eq!(
            selected
                .items
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(selected.hidden.contains(&2));
        let mut manager = cosh::harness::context::ContextManager::new(1_000);
        manager.restore_state(&selected);
        let model = serde_json::to_string(&manager.build_messages("")).unwrap();
        assert!(!model.contains("discard secret"));
        assert!(!model.contains("already hidden"));
        crate::session_snapshots::force_checkpoint(&path).unwrap();
        crate::session_snapshots::evict(&path);
        assert!(store.rollback_session(&session.id, "v1"));
        let restored = store.load_context(&session.id).unwrap();
        assert_eq!(
            serde_json::to_value(restored).unwrap(),
            serde_json::to_value(context).unwrap()
        );
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("context_view")
        );
    }

    #[test]
    fn versioned_fork_keeps_interleaved_results_and_internal_items() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let call = |id, name: &str| ContextItem::ToolCall {
            id,
            call_id: name.into(),
            name: "fs_read".into(),
            arguments: "{}".into(),
            thought_signature: String::new(),
            thinking_blocks: Vec::new(),
        };
        let result = |id, name: &str| ContextItem::ToolResult {
            id,
            call_id: name.into(),
            content: name.into(),
            useless: false,
        };
        let mut session = make_test_session(
            "view-interleave",
            "View",
            vec![
                make_user_msg("user", "task"),
                make_assistant_msg("a", "A"),
                make_assistant_msg("b", "B"),
            ],
        );
        // The internal item has no display message. Native call ownership,
        // rather than ctx_ids membership, determines the selected context.
        let context = make_context(vec![
            user_item(1, "task"),
            assistant_item(2, "internal"),
            call(3, "a"),
            call(4, "b"),
            result(5, "a"),
            result(6, "b"),
        ]);
        session.ctx_ids.insert("user".into(), vec![1]);
        session.ctx_ids.insert("a".into(), vec![3, 5]);
        session.ctx_ids.insert("b".into(), vec![4, 6]);
        store.save_session_with_context(&session, &context);
        session.ctx_ids.clear();
        store.save_session(&session);
        let mut child = session.clone();
        child.id = "view-child".into();
        assert!(store.fork_session(&session.id, "a", &child));
        assert_eq!(
            store
                .load_context(&child.id)
                .unwrap()
                .items
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 5]
        );
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 6);
        assert!(store.revert_session(&session.id, "b"));
        assert_eq!(
            store
                .load_context(&session.id)
                .unwrap()
                .items
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 5]
        );
    }

    #[test]
    fn versioned_view_supports_multiple_context_groups_in_one_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut session = make_test_session(
            "view-merged",
            "View",
            vec![
                make_user_msg("u", "task"),
                make_assistant_msg("merged", "several outputs"),
                make_user_msg("future", "future"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "task"),
            assistant_item(2, "first"),
            assistant_item(3, "second"),
            user_item(4, "future"),
        ]);
        session.ctx_ids.insert("u".into(), vec![1]);
        session.ctx_ids.insert("merged".into(), vec![2, 3]);
        session.ctx_ids.insert("future".into(), vec![4]);
        store.save_session_with_context(&session, &context);
        let mut child = session.clone();
        child.id = "merged-child".into();
        assert!(store.fork_session(&session.id, "merged", &child));
        assert_eq!(store.load_context(&child.id).unwrap().items.len(), 3);
        assert!(store.revert_session(&session.id, "merged"));
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 1);
    }

    #[test]
    fn missing_context_boundary_refuses_action_without_appending_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "view-unbound",
            "View",
            vec![
                make_user_msg("u", "task"),
                make_assistant_msg("a", "answer"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "task"),
            assistant_item(2, "answer"),
            assistant_item(3, "internal"),
        ]);
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        let before = std::fs::read(&path).unwrap();
        assert!(!store.revert_session(&session.id, "a"));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

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
            notify: None,
            lock_wait: LOCK_WAIT_TIMEOUT,
        }
    }

    /// Build a context snapshot with the given items.
    #[test]
    fn historical_selection_restores_plan_and_rollback_recovers_head_plan() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut session =
            make_test_session("plan-parent", "Plan", vec![make_user_msg("m1", "first")]);
        let mut context = make_context(vec![user_item(1, "first")]);
        let plan = |title: &str| {
            serde_json::from_value(serde_json::json!({"items":[{
                "id": "task-1", "description": title, "status": "pending",
                "depends_on": []
            }]}))
            .unwrap()
        };
        context.todo = Some(plan("first plan"));
        session.ctx_ids.insert("m1".into(), vec![1]);
        store.save_session_with_context(&session, &context);
        session.messages.push(make_assistant_msg("m2", "second"));
        session.ctx_ids.insert("m2".into(), vec![2]);
        context.items.push_back(assistant_item(2, "second"));
        context.todo = Some(plan("future plan"));
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        let prefix = std::fs::read(&path).unwrap();
        let mut child = session.clone();
        child.id = "plan-child".into();
        assert!(store.fork_session(&session.id, "m1", &child));
        assert_eq!(
            store.load_context(&child.id).unwrap().todo.unwrap().items[0].description,
            "first plan"
        );
        assert!(!store.file_path(&child.id).exists());
        assert!(store.revert_session(&session.id, "m2"));
        assert_eq!(
            store.load_context(&session.id).unwrap().todo.unwrap().items[0].description,
            "first plan"
        );
        assert!(store.rollback_session(&session.id, "v1"));
        assert_eq!(
            store.load_context(&session.id).unwrap().todo.unwrap().items[0].description,
            "future plan"
        );

        // Child changes cannot leak back into the parent or an earlier fork.
        let mut child = store.load_session(&child.id).unwrap();
        let mut child_context = store.load_context(&child.id).unwrap();
        child
            .messages
            .push(make_assistant_msg("child-end", "finished"));
        child.ctx_ids.insert("child-end".into(), vec![3]);
        child_context.items.push_back(assistant_item(3, "finished"));
        child_context.todo = Some(Default::default());
        store.save_session_with_context(&child, &child_context);
        let mut grandchild = child.clone();
        grandchild.id = "plan-grandchild".into();
        assert!(store.fork_session(&child.id, "m1", &grandchild));
        assert_eq!(
            store
                .load_context(&grandchild.id)
                .unwrap()
                .todo
                .unwrap()
                .items[0]
                .description,
            "first plan"
        );
        assert!(store.revert_session(&child.id, "m1"));
        assert!(store.load_context(&child.id).unwrap().todo.is_none());
        assert!(store.rollback_session(&child.id, "v1"));
        assert!(
            store
                .load_context(&child.id)
                .unwrap()
                .todo
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(
            store.load_context(&session.id).unwrap().todo.unwrap().items[0].description,
            "future plan"
        );

        // A checkpoint retains the plan whose source items it now covers.
        let mut session = store.load_session(&session.id).unwrap();
        context.items.push_back(ContextItem::Compaction {
            id: 3,
            summary: "checkpoint".into(),
            covered_ranges: vec![cosh::harness::context::ContextItemRange {
                start_id: 1,
                end_id: 2,
            }],
        });
        context.visible_from = Some(3);
        session
            .messages
            .push(make_assistant_msg("checkpoint", "checkpoint"));
        session.ctx_ids.insert("checkpoint".into(), vec![3]);
        store.save_session_with_context(&session, &context);
        let mut checkpoint_child = session.clone();
        checkpoint_child.id = "plan-checkpoint-child".into();
        assert!(store.fork_session(&session.id, "checkpoint", &checkpoint_child));
        assert_eq!(
            store
                .load_context(&checkpoint_child.id)
                .unwrap()
                .todo
                .unwrap()
                .items[0]
                .description,
            "future plan"
        );
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
    }

    #[test]
    fn unbound_legacy_plan_is_not_guessed_for_historical_selections() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut session =
            make_test_session("unbound-plan", "Legacy", vec![make_user_msg("m1", "work")]);
        session.ctx_ids.insert("m1".into(), vec![1]);
        let mut context = make_context(vec![user_item(1, "work")]);
        context.todo = Some(Default::default());
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        // Construct an older fixture without a plan-to-source binding.
        let legacy = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
                if let Some(change) = event["delta"]["change"].as_object_mut() {
                    change.remove("todo_after_item_id");
                }
                serde_json::to_string(&event).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, &legacy).unwrap();
        assert!(store.load_context(&session.id).unwrap().todo.is_some());
        let mut child = session.clone();
        child.id = "unbound-child".into();
        assert!(store.fork_session(&session.id, "m1", &child));
        assert!(store.load_context(&child.id).unwrap().todo.is_none());
        assert!(std::fs::read_to_string(&path).unwrap().starts_with(&legacy));
    }

    #[test]
    fn protected_plan_changes_are_replayed_from_append_only_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session =
            make_test_session("plan-history", "Plan", vec![make_user_msg("msg-0", "work")]);
        let mut context = make_context(vec![user_item(1, "work")]);
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        let mut prefix = std::fs::read(&path).unwrap();
        for status in ["pending", "in_progress", "completed"] {
            context.todo = Some(
                serde_json::from_value(serde_json::json!({"items":[{
                    "id":"task-2", "description":"Verify compatibility", "status":status,
                    "depends_on":["task-1"]
                }]}))
                .unwrap(),
            );
            store.save_session_with_context(&session, &context);
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(&prefix));
            let appended = std::str::from_utf8(&bytes[prefix.len()..]).unwrap();
            assert_eq!(appended.lines().count(), 1, "only the plan delta changed");
            assert!(appended.contains("\"field\":\"todo\""));
            assert_eq!(
                serde_json::to_value(store.load_context(&session.id).unwrap().todo).unwrap(),
                serde_json::to_value(&context.todo).unwrap()
            );
            prefix = bytes;
        }
        context.todo = Some(Default::default());
        store.save_session_with_context(&session, &context);
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        assert!(
            store
                .load_context(&session.id)
                .unwrap()
                .todo
                .unwrap()
                .items
                .is_empty()
        );
    }

    fn make_context(items: Vec<ContextItem>) -> ContextManagerState {
        ContextManagerState {
            items: items.into_iter().collect(),
            next_id: 42,
            max_tokens: 100_000,
            overflow_model: None,
            split: None,
            map_reduce: None,
            visible_from: None,
            hidden: Default::default(),
            masked: Default::default(),
            todo: None,
            last_tool_set: None,
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
    fn every_session_mutation_preserves_the_existing_byte_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut session = make_test_session(
            "append-only",
            "Initial",
            vec![
                make_user_msg("msg-0", "hello"),
                make_assistant_msg("msg-1", "hi"),
            ],
        );
        store.save_session(&session);
        let path = store.file_path("append-only");
        let first = std::fs::read(&path).unwrap();
        let first_line = String::from_utf8(first.clone())
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(matches!(
            serde_json::from_str::<HistoryEvent>(&first_line)
                .unwrap()
                .delta,
            Delta::Genesis { .. }
        ));

        session.messages[1].parts = vec![Part::Text(TextPart {
            text: "updated answer".into(),
            synthetic: false,
        })];
        store.save_session(&session);
        let after_message = std::fs::read(&path).unwrap();
        assert!(after_message.starts_with(&first));
        assert!(after_message.len() > first.len());

        let context = make_context(vec![user_item(1, "hello"), assistant_item(2, "updated")]);
        store.save_session_with_context(&session, &context);
        let after_context = std::fs::read(&path).unwrap();
        assert!(after_context.starts_with(&after_message));

        store.enqueue_job(
            StoreJob::Title {
                session_id: session.id.clone(),
                title: "Renamed".into(),
            },
            true,
        );
        let after_title = std::fs::read(&path).unwrap();
        assert!(after_title.starts_with(&after_context));
        assert_eq!(store.load_session(&session.id).unwrap().title, "Renamed");

        session = store.load_session(&session.id).unwrap();
        session.provider = Some("openai".into());
        session.model = Some("gpt-test".into());
        session.reasoning = Some("high".into());
        store.save_session(&session);
        let after_model = std::fs::read(&path).unwrap();
        assert!(after_model.starts_with(&after_title));

        let mut changed_context = context;
        changed_context.hidden.insert(1);
        changed_context.overflow_model = Some("gpt-test".into());
        store.save_session_with_context(&session, &changed_context);
        let after_bookkeeping = std::fs::read(&path).unwrap();
        assert!(after_bookkeeping.starts_with(&after_model));

        store.delete_session(&session.id);
        assert!(!store.has_session(&session.id));
        // The deleted branch was the file's only branch: the tombstone makes
        // it the last active one, so the history file is physically removed.
        assert!(!path.exists());
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
        // The recorded selection round-trips through metadata deltas.
        assert_eq!(loaded.model.as_deref(), Some("deepseek-ai/deepseek-v4-pro"));
        assert_eq!(loaded.provider.as_deref(), Some("nvidia"));
        assert_eq!(loaded.reasoning.as_deref(), Some("high"));
    }

    #[test]
    fn recorded_auto_selection_is_not_replaced_by_a_message_model() {
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

    /// A session whose file carries no embedded context (never saved with
    /// context — e.g. a display-only session) has no model-facing context:
    /// `load_context` returns `None`, and the resume warns truthfully.
    #[test]
    fn a_session_without_embedded_context_loads_display_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session(
            "12345",
            "Display Only",
            vec![
                make_user_msg("msg-0", "Hello"),
                make_assistant_msg("msg-1", "Hi"),
            ],
        );
        store.save_session(&session);

        assert!(store.load_context("12345").is_none());

        // Deleting the session appends a logical tombstone.
        store.delete_session("12345");
        assert!(!store.has_session("12345"));
        assert!(store.load_context("12345").is_none());
    }

    /// A legacy header whose `context` object is unreadable must not take the
    /// display transcript down with it:
    /// the session still loads, and the context degrades to absent
    /// (the resume warns truthfully).
    #[test]
    fn an_unreadable_header_context_degrades_to_display_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let file = store.file_path("12348");
        let fixture = concat!(
            "{\"title\":\"Broken Bookkeeping\",\"title_generated\":false,",
            "\"created_at\":0,\"cwd\":\"/work\",\"provider\":null,",
            "\"model\":null,\"reasoning\":null,",
            "\"context\":{\"next_id\":\"not-a-number\"}}\n",
            "{\"Message\":{\"id\":\"msg-0\",\"role\":\"user\",",
            "\"parts\":[{\"type\":\"Text\",\"text\":\"hi\",",
            "\"synthetic\":false}],\"created_at\":1000,\"agent\":null,",
            "\"model\":null,\"ctx_ids\":[]}}\n"
        );
        std::fs::write(&file, fixture).unwrap();

        let loaded = store
            .load_session("12348")
            .expect("the display transcript survives the bookkeeping break");
        assert_eq!(loaded.messages.len(), 1);
        assert!(store.load_context("12348").is_none());
    }

    /// A corrupt item record loses exactly that item — and a compaction
    /// boundary whose anchor was among the lost records is CLEARED, so the
    /// surviving raw history reaches the model instead of an amnesiac empty
    /// view (the same repair applied to reference-selected projections).
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

    /// A line with the right record tag but an unreadable payload remains
    /// byte-for-byte intact while a title delta is appended after it.
    #[test]
    fn update_title_appends_after_a_tagged_line_with_an_unreadable_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("7301", "Bad Payload", vec![make_user_msg("msg-0", "hi")]);
        store.save_session(&session);
        let file = store.file_path("7301");
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{original}{{\"Message\":123}}\n")).unwrap();

        let prefix = std::fs::read(&file).unwrap();
        store.enqueue_job(
            StoreJob::Title {
                session_id: "7301".into(),
                title: "Renamed".into(),
            },
            true,
        );

        let bytes = std::fs::read(&file).unwrap();
        assert!(bytes.starts_with(&prefix));
        let contents = String::from_utf8(bytes).unwrap();
        assert!(
            contents.contains("{\"Message\":123}"),
            "the unreadable record must stay on disk"
        );
        assert!(
            store.load_session("7301").unwrap().title == "Renamed",
            "the appended delta must update the projection"
        );
    }

    /// Split chunk appends are incremental: each advance appends only the new
    /// summary bytes (plus a bounded header), and replay reproduces the exact
    /// staging so an interrupted split resumes where it stopped.
    #[test]
    fn split_buffer_appends_are_incremental_and_replay_exactly() {
        use cosh::harness::context::ContextManager;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut manager = ContextManager::new(4_000);
        for turn in 0..6 {
            manager.add_user(&format!("chunk source {turn}: {}", "detail ".repeat(300)));
            manager.add_assistant("acknowledged", false);
        }
        let session = make_test_session("split-append", "Split Append", vec![]);
        let path = store.file_path(&session.id);
        store.save_session_with_context(&session, &manager.save_state());
        manager.begin_split(2_000);
        store.save_session_with_context(&session, &manager.save_state());
        let mut prefix = std::fs::read(&path).unwrap();
        let mut last_summary = String::new();
        while let Some(request) = manager.split_next_chunk() {
            let summary = format!("chunk summary\n{}", "note ".repeat(200));
            manager.advance_split(&summary, request.chunk_end);
            store.save_session_with_context(&session, &manager.save_state());
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(&prefix), "appends must never rewrite");
            let appended = bytes.len() - prefix.len();
            assert!(
                appended < summary.len() * 2 + 400,
                "each append must carry only the new summary, got {appended} bytes"
            );
            prefix = bytes;
            last_summary = summary;
            let replayed = store.load_context(&session.id).unwrap();
            assert_eq!(
                serde_json::to_value(replayed).unwrap(),
                serde_json::to_value(manager.save_state()).unwrap()
            );
        }
        assert!(!last_summary.is_empty());
        assert!(manager.commit_split());
        store.save_session_with_context(&session, &manager.save_state());
        let replayed = store.load_context(&session.id).unwrap();
        assert_eq!(
            serde_json::to_value(replayed).unwrap(),
            serde_json::to_value(manager.save_state()).unwrap()
        );
    }

    // multi-process access to one session file

    /// The cooperative case: a second process appends events with a FRESH
    /// view of the file (correct next event id). The first process replays
    /// the file on its next save, so foreign ITEM-level state survives and
    /// every byte stays append-only. Metadata AND context scalars are
    /// field-level last-writer-wins: this process's stale in-memory copy is
    /// re-asserted over foreign changes on its next save.
    #[test]
    fn cooperative_cross_process_append_is_picked_up_without_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("cm09-coop", "Coop", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);
        let path = store.file_path(&session.id);

        // The second process replays the file fresh and appends its deltas.
        let contents = std::fs::read_to_string(&path).unwrap();
        let last_id = contents
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["branch_id"] == "cm09-coop")
            .map(|event| event["event_id"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0);
        let foreign = [
            HistoryEvent::new(
                last_id + 1,
                "cm09-coop",
                Delta::Metadata {
                    change: MetadataDelta::Title {
                        title: "Renamed by the other process".into(),
                        title_generated: true,
                    },
                },
            ),
            HistoryEvent::new(
                last_id + 2,
                "cm09-coop",
                Delta::Context {
                    change: ContextDelta::MaxTokens { value: 555 },
                },
            ),
        ];
        assert!(append_events(&path, &contents, &foreign).is_ok());
        let with_foreign = std::fs::read(&path).unwrap();

        // The first process saves again: it re-replays the file (fresh view),
        // appends after the foreign events, and preserves every byte.
        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        assert!(std::fs::read(&path).unwrap().starts_with(&with_foreign));
        // Foreign ITEM-level changes survive the other process's save
        // (items are diffed per identity)...
        let reloaded_context = store.load_context(&session.id).unwrap();
        assert_eq!(reloaded_context.items.len(), 2);
        // ...but every context SCALAR is also last-writer-wins: this
        // process re-asserts its full scalar state, reverting the foreign
        // max_tokens exactly like the foreign title.
        assert_eq!(reloaded_context.max_tokens, 100_000);
        let reloaded = store.load_session(&session.id).unwrap();
        assert_eq!(reloaded.title, "Coop");
        let events = std::fs::read_to_string(&path).unwrap();
        assert!(events.contains("Renamed by the other process"));
        let reloaded = store.load_session(&session.id).unwrap();
        assert_eq!(reloaded.title, "Coop");
        let events = std::fs::read_to_string(&path).unwrap();
        assert!(events.contains("Renamed by the other process"));
    }

    /// Two processes with STALE views both compute the same next event id and
    /// both append. Replay heals deterministically: the first writer's event
    /// wins, the duplicate is skipped with a warning, and every byte stays on
    /// disk. The next save continues from the winning id without collision.
    #[test]
    fn stale_concurrent_writer_collision_heals_first_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("cm09-race", "Race", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);
        let path = store.file_path(&session.id);
        let contents = std::fs::read_to_string(&path).unwrap();
        let last_id = contents
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["branch_id"] == "cm09-race")
            .map(|event| event["event_id"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0);

        // Both processes read the same file tail and both pick last_id + 1.
        let writer_b = HistoryEvent::new(
            last_id + 1,
            "cm09-race",
            Delta::Metadata {
                change: MetadataDelta::Title {
                    title: "process B".into(),
                    title_generated: true,
                },
            },
        );
        let writer_a = HistoryEvent::new(
            last_id + 1,
            "cm09-race",
            Delta::Context {
                change: ContextDelta::MaxTokens { value: 123 },
            },
        );
        assert!(append_events(&path, &contents, &[writer_b]).is_ok());
        let after_b = std::fs::read_to_string(&path).unwrap();
        assert!(append_events(&path, &after_b, &[writer_a]).is_ok());
        let collided = std::fs::read(&path).unwrap();

        // First writer wins: B's title applies, A's duplicate is skipped.
        let reloaded = store.load_session(&session.id).unwrap();
        assert_eq!(reloaded.title, "process B");
        assert!(
            store.load_context(&session.id).is_none(),
            "the skipped duplicate must not apply its change"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            collided,
            "healing must not rewrite history"
        );

        // The next save continues from the winning id without colliding.
        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        assert!(std::fs::read(&path).unwrap().starts_with(&collided));
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);
        let tail_id: u64 = {
            let contents = std::fs::read_to_string(&path).unwrap();
            contents
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .and_then(|event| event["event_id"].as_u64())
                .unwrap_or(0)
        };
        assert!(
            tail_id > last_id + 1,
            "the recovery save must continue past the winning id: tail={tail_id}"
        );
    }

    /// A crash mid-write leaves a torn partial-JSON tail without a newline.
    /// The torn bytes stay on disk untouched, the next append inserts the
    /// missing newline first, and replay skips the fragment.
    #[test]
    fn torn_tail_line_is_preserved_and_skipped_while_appends_continue() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("cm09-torn", "Torn", vec![make_user_msg("m1", "hi")]);
        store.save_session_with_context(&session, &make_context(vec![user_item(1, "hi")]));
        let path = store.file_path(&session.id);
        let intact = std::fs::read(&path).unwrap();

        let torn = {
            let mut bytes = intact.clone();
            bytes.extend_from_slice(b"{\"schema_version\":1,\"event_i");
            bytes
        };
        std::fs::write(&path, &torn).unwrap();

        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);

        let after = std::fs::read(&path).unwrap();
        assert!(after.starts_with(&torn), "torn bytes must be preserved");
        let tail = &after[torn.len()..];
        assert!(
            tail.starts_with(b"\n"),
            "the next append must start on its own line"
        );
        let loaded = store.load_context(&session.id).unwrap();
        assert_eq!(loaded.items.len(), 2, "the torn line is skipped, not fatal");
        assert_eq!(
            serde_json::to_value(loaded.items).unwrap(),
            serde_json::to_value(context.items).unwrap()
        );
    }

    /// A foreign process holding the store lock is detected: the write waits
    /// (with one warning toast), proceeds once the lock is released, and
    /// never rewrites previously appended bytes.
    #[test]
    fn lock_contention_notifies_then_saves_once_released() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = test_store(&dir)
            .with_lock_wait(Duration::from_secs(5))
            .with_test_notify(tx);
        let session = make_test_session("cm09-lock", "Lock", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);
        let path = store.file_path(&session.id);
        let before = std::fs::read(&path).unwrap();

        // A foreign process holds the lock, releasing it after 150ms.
        let lock_path = dir.path().join("testhash").join(LOCK_FILE_NAME);
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        // The writer thread may still be releasing the baseline job's lock;
        // poll briefly instead of failing on one scheduling overlap.
        let mut waited = Duration::ZERO;
        loop {
            match holder.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if waited < Duration::from_secs(2) => {
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                    waited += LOCK_POLL_INTERVAL;
                }
                other => other.unwrap(),
            }
        }
        // One second: long enough that the save's acquire is guaranteed to
        // observe the contention even under a loaded writer queue, short
        // enough to keep the test fast.
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            drop(holder);
        });

        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        releaser.join().unwrap();

        assert!(std::fs::read(&path).unwrap().starts_with(&before));
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);
        let mut saw_busy_toast = false;
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::Toast { message, variant } = event
                && variant == ToastVariant::Warning
                && message.contains("busy")
            {
                saw_busy_toast = true;
            }
        }
        assert!(saw_busy_toast, "contention must surface as a warning toast");
    }

    /// A lock held past the wait budget skips the write with an error toast;
    /// the file is untouched, and a later save after release succeeds.
    #[test]
    fn lock_timeout_skips_the_write_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = test_store(&dir)
            .with_lock_wait(Duration::from_millis(200))
            .with_test_notify(tx);
        let session = make_test_session("cm09-timeout", "Timeout", vec![make_user_msg("m1", "hi")]);
        store.save_session_with_context(&session, &make_context(vec![user_item(1, "hi")]));
        let path = store.file_path(&session.id);
        let before = std::fs::read(&path).unwrap();

        let lock_path = dir.path().join("testhash").join(LOCK_FILE_NAME);
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        // The writer thread may still be releasing the baseline job's lock;
        // poll briefly instead of failing on one scheduling overlap.
        let mut waited = Duration::ZERO;
        loop {
            match holder.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if waited < Duration::from_secs(2) => {
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                    waited += LOCK_POLL_INTERVAL;
                }
                other => other.unwrap(),
            }
        }

        // Deterministic contention: wait until a fresh probe sees the test
        // thread holding the lock, so the save below can never win the
        // startup race by acquiring it first.
        let mut probe_wait = Duration::ZERO;
        loop {
            let probe = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap();
            match probe.try_lock() {
                Err(std::fs::TryLockError::WouldBlock) => break,
                Ok(()) => drop(probe),
                Err(std::fs::TryLockError::Error(error)) => panic!("lock probe failed: {error}"),
            }
            assert!(
                probe_wait < Duration::from_secs(5),
                "holder never locked the store"
            );
            std::thread::sleep(LOCK_POLL_INTERVAL);
            probe_wait += LOCK_POLL_INTERVAL;
        }

        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a timed-out write must not touch the file"
        );
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 1);

        drop(holder);
        store.save_session_with_context(&session, &context);
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);

        let mut saw_busy = false;
        let mut saw_skipped = false;
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::Toast { message, variant } = event {
                if message.contains("busy") {
                    saw_busy = true;
                    assert!(matches!(
                        variant,
                        ToastVariant::Warning | ToastVariant::Error
                    ));
                }
                if message.contains("skipped") {
                    saw_skipped = true;
                    assert_eq!(variant, ToastVariant::Error);
                }
            }
        }
        assert!(saw_busy && saw_skipped);
    }

    /// Multi-event collision: two stale writers each append a multi-delta
    /// block with the same starting id. Replay keeps the first block whole
    /// and skips every duplicate that follows it.
    #[test]
    fn multi_event_collision_blocks_heal_with_first_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("cm09-blocks", "Blocks", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);
        let path = store.file_path(&session.id);
        let contents = std::fs::read_to_string(&path).unwrap();
        let last_id = contents
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["branch_id"] == "cm09-blocks")
            .map(|event| event["event_id"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0);

        let block = |start: u64, marker: &str| {
            [
                HistoryEvent::new(
                    start,
                    "cm09-blocks",
                    Delta::Metadata {
                        change: MetadataDelta::Title {
                            title: marker.into(),
                            title_generated: true,
                        },
                    },
                ),
                HistoryEvent::new(
                    start + 1,
                    "cm09-blocks",
                    Delta::Context {
                        change: ContextDelta::MaxTokens { value: 42 },
                    },
                ),
            ]
        };
        let writer_a = block(last_id + 1, "process A");
        let writer_b = block(last_id + 1, "process B");
        assert!(append_events(&path, &contents, &writer_a).is_ok());
        let after_a = std::fs::read_to_string(&path).unwrap();
        assert!(append_events(&path, &after_a, &writer_b).is_ok());

        // A's whole block wins; both of B's duplicates are skipped.
        let reloaded = store.load_session(&session.id).unwrap();
        assert_eq!(reloaded.title, "process A");
        assert_eq!(
            store.load_context(&session.id).unwrap().max_tokens,
            42,
            "the second delta of A's block must still apply"
        );
    }

    /// Unequal collision blocks: the first writer appends a ONE-event block,
    /// the stale second writer a TWO-event block starting at the same id.
    /// The log records no save boundaries, so replay cannot tell where the
    /// stale writer's block ends — its trailing ids continue contiguously
    /// exactly like the next healthy save's would. First-writer-wins
    /// therefore holds per ID (B's rename is skipped), and B's trailing
    /// deltas merge deterministically on top of the first writer's state.
    /// Skipping the tail would mean guessing a boundary absent from the log;
    /// the only sound alternative (skip to end of file) would drop possibly
    /// healthy later saves. So the merge IS the healing semantics: no data
    /// loss, no byte rewritten, and the next save continues cleanly.
    #[test]
    fn unequal_collision_blocks_merge_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("cm09-unequal", "Unequal", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);
        let path = store.file_path(&session.id);
        let contents = std::fs::read_to_string(&path).unwrap();
        let last_id = contents
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["branch_id"] == "cm09-unequal")
            .map(|event| event["event_id"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0);

        // A saves one delta (a context scalar); B's stale view saves two
        // deltas (a rename plus a different scalar) starting at the same id.
        let writer_a = [HistoryEvent::new(
            last_id + 1,
            "cm09-unequal",
            Delta::Context {
                change: ContextDelta::MaxTokens { value: 123 },
            },
        )];
        let writer_b = [
            HistoryEvent::new(
                last_id + 1,
                "cm09-unequal",
                Delta::Metadata {
                    change: MetadataDelta::Title {
                        title: "process B".into(),
                        title_generated: true,
                    },
                },
            ),
            HistoryEvent::new(
                last_id + 2,
                "cm09-unequal",
                Delta::Context {
                    change: ContextDelta::MaxTokens { value: 999 },
                },
            ),
        ];
        assert!(append_events(&path, &contents, &writer_a).is_ok());
        let after_a = std::fs::read_to_string(&path).unwrap();
        assert!(append_events(&path, &after_a, &writer_b).is_ok());
        let collided = std::fs::read(&path).unwrap();

        // First writer wins the colliding ID: B's rename is skipped.
        let reloaded = store.load_session(&session.id).unwrap();
        assert_eq!(
            reloaded.title, "Unequal",
            "B's colliding rename must be skipped"
        );
        // B's trailing scalar merges on top of A's state — the deterministic,
        // data-preserving outcome (see the test doc comment).
        assert_eq!(
            store.load_context(&session.id).unwrap().max_tokens,
            999,
            "the stale writer's trailing delta merges deterministically"
        );
        // Healing must not rewrite history.
        assert_eq!(std::fs::read(&path).unwrap(), collided);

        // The next save continues past the merged ids without colliding.
        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        assert!(std::fs::read(&path).unwrap().starts_with(&collided));
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);
        let tail_id: u64 = {
            let contents = std::fs::read_to_string(&path).unwrap();
            contents
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .and_then(|event| event["event_id"].as_u64())
                .unwrap_or(0)
        };
        assert!(
            tail_id >= last_id + 3,
            "the recovery save must continue past the merged ids: tail={tail_id}"
        );
    }

    /// The lock is bound to the open file description: a holder KILLED while
    /// holding it is cleaned up by the kernel, so the store writes straight
    /// through with no orphan lockfile and no visible contention.
    ///
    /// Unix-only by design: the harness drives the external `flock(1)` CLI
    /// and `kill -9 -- -<pgid>` to simulate a foreign killed process — tools
    /// that do not exist on Windows.
    #[test]
    #[cfg(unix)]
    fn killed_lock_holder_releases_the_store() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = test_store(&dir)
            .with_lock_wait(Duration::from_secs(10))
            .with_test_notify(tx);
        let session = make_test_session("cm09-kill", "Kill", vec![make_user_msg("m1", "hi")]);
        store.save_session(&session);

        // A foreign PROCESS locks the store directory (flock CLI), then is
        // killed while holding it.
        let lock_path = dir.path().join("testhash").join(LOCK_FILE_NAME);
        let mut holder = std::process::Command::new("flock")
            .arg(&lock_path)
            .arg("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("flock helper must start");
        // Poll with a FRESH handle each round: reusing one handle would
        // let the first successful try_lock hold the lock itself.
        let mut waited = Duration::ZERO;
        loop {
            let probe = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap();
            match probe.try_lock() {
                Err(std::fs::TryLockError::WouldBlock) => {
                    break;
                }
                Ok(()) => drop(probe),
                Err(std::fs::TryLockError::Error(error)) => {
                    let _ = std::process::Command::new("kill")
                        .arg("-9")
                        .arg("--")
                        .arg(format!("-{}", holder.id()))
                        .status();
                    panic!("lock probe failed: {error}");
                }
            }
            if waited > Duration::from_secs(5) {
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(format!("-{}", holder.id()))
                    .status();
                panic!("flock helper never locked the store");
            }
            std::thread::sleep(LOCK_POLL_INTERVAL);
            waited += LOCK_POLL_INTERVAL;
        }
        // flock(1) runs its command on an inherited lock fd, so killing the
        // parent alone would leave the lock alive in the child. Kill the
        // whole process group instead.
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg("--")
            .arg(format!("-{}", holder.id()))
            .status();
        holder.wait().unwrap();

        // The store writes straight through: the OS freed the lock.
        let mut context = make_context(vec![user_item(1, "hi")]);
        context.items.push_back(assistant_item(2, "hello"));
        store.save_session_with_context(&session, &context);
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);
        let mut contention = false;
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::Toast { .. } = event {
                contention = true;
            }
        }
        assert!(
            !contention,
            "a dead holder must not cause visible contention"
        );
    }

    /// The full context projection — items, scalar state, and split staging —
    /// round-trips through deltas, so an interrupted split resumes exactly
    /// where it stopped.
    #[test]
    fn context_state_roundtrips_through_the_session_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let session = make_test_session("5000", "Split Session", vec![]);
        let mut context =
            make_context(vec![user_item(1, "Hello!"), assistant_item(2, "Hi there!")]);
        context.overflow_model = Some("gpt-4o-mini".to_string());
        context.split = Some(SplitState {
            version: 0,
            buffer: "## Objective\n- summarized so far".to_string(),
            cursor: Some(2),
            continuity: "tail of the last chunk".to_string(),
            window: 100_000,
            buffer_tokens: 1234,
        });
        // Visibility markers are explicit context deltas.
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

    #[test]
    fn every_context_field_and_item_replacement_is_replayed_from_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "context-deltas",
            "Context deltas",
            vec![
                make_user_msg("msg-0", "prompt"),
                make_assistant_msg("msg-1", "draft"),
            ],
        );
        let initial = ContextManagerState {
            items: [
                user_item(1, "prompt"),
                ContextItem::Assistant {
                    id: 2,
                    original: "draft".into(),
                    closable: true,
                },
            ]
            .into_iter()
            .collect(),
            next_id: 3,
            max_tokens: 100_000,
            overflow_model: None,
            split: None,
            map_reduce: None,
            visible_from: None,
            hidden: HashSet::new(),
            masked: HashSet::new(),
            todo: None,
            last_tool_set: None,
        };
        store.save_session_with_context(&session, &initial);
        let path = store.file_path(&session.id);
        let first = std::fs::read(&path).unwrap();

        let mut changed = initial.clone();
        changed.items[1] = ContextItem::Closure {
            id: 2,
            content: "draft".into(),
        };
        changed.next_id = 9;
        changed.max_tokens = 64_000;
        changed.overflow_model = Some("model-a".into());
        changed.split = Some(SplitState {
            version: 0,
            buffer: "partial".into(),
            cursor: Some(2),
            continuity: "tail".into(),
            window: 64_000,
            buffer_tokens: 2,
        });
        let mut staging_manager = cosh::harness::context::ContextManager::new(128);
        staging_manager.add_user(&"map source ".repeat(200));
        assert!(staging_manager.begin_map_reduce(128));
        changed.map_reduce = staging_manager.save_state().map_reduce;
        changed.visible_from = Some(2);
        changed.hidden.insert(1);
        store.save_session_with_context(&session, &changed);
        let second = std::fs::read(&path).unwrap();
        assert!(second.starts_with(&first));
        let restored = store.load_context(&session.id).unwrap();
        assert!(matches!(
            restored.items[1],
            ContextItem::Closure { id: 2, .. }
        ));
        assert_eq!(restored.next_id, 9);
        assert_eq!(restored.max_tokens, 64_000);
        assert_eq!(restored.overflow_model.as_deref(), Some("model-a"));
        assert!(restored.split.is_some());
        assert!(restored.map_reduce.is_some());
        assert_eq!(restored.visible_from, Some(2));
        assert_eq!(restored.hidden, HashSet::from([1]));

        let mut cleared = changed;
        cleared.items.pop_back();
        cleared.next_id = 10;
        cleared.overflow_model = None;
        cleared.split = None;
        cleared.map_reduce = None;
        cleared.visible_from = None;
        cleared.hidden.clear();
        store.save_session_with_context(&session, &cleared);
        let third = std::fs::read(&path).unwrap();
        assert!(third.starts_with(&second));
        let restored = store.load_context(&session.id).unwrap();
        assert_eq!(restored.items.len(), 1);
        assert_eq!(restored.next_id, 10);
        assert_eq!(restored.overflow_model, None);
        assert!(restored.split.is_none());
        assert!(restored.map_reduce.is_none());
        assert_eq!(restored.visible_from, None);
        assert!(restored.hidden.is_empty());
    }

    #[test]
    fn context_manager_lifecycle_round_trips_as_history_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut manager = cosh::harness::context::ContextManager::new(100_000);
        manager.add_user("prompt");
        manager.add_assistant("answer", true);
        manager.close_loop();
        let mut session = make_test_session(
            "manager-lifecycle",
            "Lifecycle",
            vec![
                make_user_msg("msg-0", "prompt"),
                make_assistant_msg("msg-1", "answer"),
            ],
        );
        store.save_session_with_context(&session, &manager.save_state());
        let path = store.file_path(&session.id);
        let first = std::fs::read(&path).unwrap();

        manager.set_max_tokens(64_000);
        manager.mark_overflow("model-a");
        manager.begin_split(64_000);
        manager.advance_split("partial summary", Some(1));
        store.save_session_with_context(&session, &manager.save_state());
        let second = std::fs::read(&path).unwrap();
        assert!(second.starts_with(&first));
        let staged = store.load_context(&session.id).unwrap();
        assert_eq!(staged.max_tokens, 64_000);
        assert_eq!(staged.overflow_model.as_deref(), Some("model-a"));
        assert_eq!(
            staged.split.as_ref().map(|split| split.buffer.as_str()),
            Some("partial summary")
        );

        manager.abort_split();
        assert!(manager.apply_llm_summary("compacted".into()));
        session
            .messages
            .push(make_assistant_msg("msg-2", "compacted"));
        store.save_session_with_context(&session, &manager.save_state());
        let third = std::fs::read(&path).unwrap();
        assert!(third.starts_with(&second));
        let restored = store.load_context(&session.id).unwrap();
        assert!(restored.split.is_none());
        assert_eq!(restored.overflow_model, None);
        assert_eq!(restored.visible_from, None);
        assert!(
            restored.hidden.is_empty(),
            "checkpoint coverage is derived from its ranges, not hidden debris"
        );
        assert!(matches!(
            restored.items.back(),
            Some(ContextItem::Compaction {
                id: 3,
                summary,
                covered_ranges,
            }) if summary == "compacted"
                && covered_ranges == &[cosh::harness::context::ContextItemRange {
                    start_id: 1,
                    end_id: 2,
                }]
        ));
        let mut replayed = cosh::harness::context::ContextManager::new(1);
        replayed.restore_state(&restored);
        let messages = replayed.build_messages("");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("compacted"));
    }

    #[test]
    fn masked_tool_results_round_trip_as_append_only_view_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "masked-results",
            "Masked results",
            vec![make_assistant_msg("msg-0", "tool interaction")],
        );
        let mut context = make_context(vec![
            ContextItem::ToolCall {
                id: 1,
                call_id: "call-1".into(),
                name: "fs_read".into(),
                arguments: r#"{"path":"large.rs"}"#.into(),
                thought_signature: String::new(),
                thinking_blocks: Vec::new(),
            },
            ContextItem::ToolResult {
                id: 2,
                call_id: "call-1".into(),
                content: "complete immutable payload".into(),
                useless: false,
            },
        ]);
        context.masked.insert(2);
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        let first = std::fs::read(&path).unwrap();

        let restored = store.load_context(&session.id).unwrap();
        assert_eq!(restored.masked, HashSet::from([2]));
        assert!(matches!(
            &restored.items[1],
            ContextItem::ToolResult { content, .. } if content == "complete immutable payload"
        ));
        let mut manager = cosh::harness::context::ContextManager::new(100_000);
        manager.restore_state(&restored);
        let messages = manager.build_messages("");
        assert!(
            messages[1]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("source context item #2"))
        );

        let mut unmasked = restored;
        unmasked.masked.clear();
        store.save_session_with_context(&session, &unmasked);
        let second = std::fs::read(&path).unwrap();
        assert!(second.starts_with(&first));
        assert!(
            store
                .load_context(&session.id)
                .is_some_and(|state| state.masked.is_empty())
        );
    }

    /// A display-only save emits no context deltas, so display changes can
    /// never clobber the model-facing context projection.
    #[test]
    fn display_only_save_emits_no_context_changes() {
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

        // Replay still derives the unchanged context.
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
        assert!(store.has_session("12345"));

        let path = store.file_path("12345");
        assert!(path.exists());
        // Deleting the session appends a tombstone; because it was the file's
        // only active branch, the history file is removed from disk.
        store.delete_session("12345");
        assert!(!store.has_session("12345"));
        assert!(!path.exists());
    }

    /// A fork shares its parent's JSONL. Deleting the parent while the fork
    /// is still active must keep the file on disk: the fork depends on it.
    #[test]
    fn deleting_a_parent_with_an_active_fork_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let parent = make_test_session(
            "6000",
            "Parent",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
            ],
        );
        store.save_session(&parent);
        let mut fork = parent.clone();
        fork.id = "6000-fork".into();
        fork.messages.truncate(1);
        assert!(store.fork_session("6000", "msg-1", &fork));

        let path = store.file_path("6000");
        store.delete_session("6000");

        assert!(!store.has_session("6000"), "the parent is tombstoned");
        assert!(
            path.exists(),
            "the file must survive while its fork is active"
        );
        assert!(
            store.has_session("6000-fork"),
            "the fork must remain loadable"
        );
        assert!(
            store
                .list_sessions()
                .iter()
                .any(|summary| summary.session_id == "6000-fork"),
            "the fork stays visible in the sidebar"
        );
    }

    /// Deleting a fork while its parent is still active keeps the shared
    /// file on disk: only the fork branch is tombstoned.
    #[test]
    fn deleting_a_fork_with_an_active_parent_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let parent = make_test_session(
            "6100",
            "Parent",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
            ],
        );
        store.save_session(&parent);
        let mut fork = parent.clone();
        fork.id = "6100-fork".into();
        fork.messages.truncate(1);
        assert!(store.fork_session("6100", "msg-1", &fork));

        let path = store.file_path("6100");
        store.delete_session("6100-fork");

        assert!(!store.has_session("6100-fork"), "the fork is tombstoned");
        assert!(
            path.exists(),
            "the file must survive while its parent is active"
        );
        assert!(store.has_session("6100"), "the parent must remain loadable");
    }

    /// Deleting one fork keeps sibling forks (and the parent) active, so the
    /// file stays; only when the LAST branch of the file is deleted is the
    /// file physically removed.
    #[test]
    fn deleting_the_last_active_branch_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let parent = make_test_session(
            "6200",
            "Parent",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
            ],
        );
        store.save_session(&parent);
        let mut fork_a = parent.clone();
        fork_a.id = "6200-fork-a".into();
        fork_a.messages.truncate(1);
        assert!(store.fork_session("6200", "msg-1", &fork_a));
        let mut fork_b = parent.clone();
        fork_b.id = "6200-fork-b".into();
        fork_b.messages.truncate(1);
        assert!(store.fork_session("6200", "msg-1", &fork_b));

        let path = store.file_path("6200");

        // First branch dies: file and the surviving branches remain.
        store.delete_session("6200-fork-a");
        assert!(path.exists(), "sibling fork and parent still need the file");
        assert!(store.has_session("6200"));
        assert!(store.has_session("6200-fork-b"));

        // Second branch dies: the parent still keeps the file alive.
        store.delete_session("6200-fork-b");
        assert!(path.exists(), "the parent still needs the file");
        assert!(store.has_session("6200"));

        // Last branch dies: nothing active depends on the file anymore.
        store.delete_session("6200");
        assert!(!path.exists(), "the fully deleted history must be removed");
        assert!(store.list_sessions().is_empty());
    }

    /// A fork whose parent was already deleted can still be the file's last
    /// active branch; deleting it removes the file too.
    #[test]
    fn deleting_the_last_surviving_fork_after_its_parent_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let parent = make_test_session(
            "6300",
            "Parent",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
            ],
        );
        store.save_session(&parent);
        let mut fork = parent.clone();
        fork.id = "6300-fork".into();
        fork.messages.truncate(1);
        assert!(store.fork_session("6300", "msg-1", &fork));

        let path = store.file_path("6300");
        store.delete_session("6300");
        assert!(path.exists());
        store.delete_session("6300-fork");
        assert!(!path.exists(), "no active branch remains: the file goes");
        assert!(store.load_session("6300-fork").is_none());
        assert!(store.load_session("6300").is_none());
    }

    #[test]
    fn histories_are_not_physically_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        const HISTORY_COUNT: usize = 55;

        for i in 0..HISTORY_COUNT {
            let session = make_test_session(
                &format!("{i:05}"),
                &format!("Session {i}"),
                vec![
                    make_user_msg("msg-0", "Hello"),
                    make_assistant_msg("msg-1", "Hi"),
                ],
            );
            store.save_session(&session);
        }

        let list = store.list_sessions();
        assert_eq!(list.len(), HISTORY_COUNT);

        // Immutable histories are retained; storage policy must not erase
        // previously written session events.
        for i in 0..HISTORY_COUNT {
            let id = format!("{i:05}");
            assert!(store.has_session(&id), "session {id} remains available");
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
            notify: None,
            lock_wait: LOCK_WAIT_TIMEOUT,
        };
        std::fs::create_dir_all(base.join("proja")).ok();

        // Store for "projb" CWD
        let store_b = SessionStore {
            sessions_dir: base.join("projb"),
            cwd_hash: "projb".to_string(),
            notify: None,
            lock_wait: LOCK_WAIT_TIMEOUT,
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
    fn genesis_contains_cwd() {
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
        // The fake hash only chooses the directory; Genesis still records
        // the canonical current directory.
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
    /// the model-facing context (the persisted context records carry no
    /// reasoning).
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

        // The persisted context records keep the reasoning OUT of the model
        // context.
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
                        lsp_notes: None,
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

    #[test]
    fn revert_and_rollback_append_references_and_restore_exact_projections() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "history-actions",
            "History",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
                make_user_msg("msg-2", "three"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "one"),
            assistant_item(2, "two"),
            user_item(3, "three"),
        ]);
        store.save_session_with_context(&session, &context);
        let path = store.file_path(&session.id);
        let before_revert = std::fs::read(&path).unwrap();

        assert!(store.revert_session(&session.id, "msg-2"));
        let after_revert = std::fs::read(&path).unwrap();
        assert!(after_revert.starts_with(&before_revert));
        assert_eq!(store.undo_versions(&session.id), vec!["v1"]);
        let reverted = store.load_session(&session.id).unwrap();
        assert_eq!(
            reverted
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-0", "msg-1"]
        );
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 2);

        assert!(store.rollback_session(&session.id, "v1"));
        let after_rollback = std::fs::read(&path).unwrap();
        assert!(after_rollback.starts_with(&after_revert));
        assert_eq!(store.load_session(&session.id).unwrap().messages.len(), 3);
        assert_eq!(store.load_context(&session.id).unwrap().items.len(), 3);
    }

    #[test]
    fn legacy_fragment_restart_appends_whole_item_staging_without_rewriting_history() {
        use cosh::harness::context::ContextManager;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "legacy-fragments",
            "Legacy fragment recovery",
            vec![make_user_msg("msg-0", "immutable source")],
        );
        let mut manager = ContextManager::new(2_000);
        for index in 0..12 {
            manager.add_user(&format!("source {index}: {}", "evidence ".repeat(300)));
            manager.add_assistant("acknowledged", true);
        }
        manager.add_assistant("recent raw tail", true);
        let original_items = serde_json::to_value(manager.save_state().items).unwrap();
        store.save_session_with_context(&session, &manager.save_state());
        let path = store.file_path(&session.id);
        let original_prefix = std::fs::read(&path).unwrap();
        assert!(manager.begin_map_reduce(2_000));
        let mut legacy = serde_json::to_value(manager.save_state()).unwrap();
        legacy["map_reduce"]["version"] = serde_json::json!(1);
        legacy["map_reduce"]
            .as_object_mut()
            .unwrap()
            .remove("map_target_tokens");
        for segment in legacy["map_reduce"]["segments"].as_array_mut().unwrap() {
            let id = segment["item_ids"][0].as_u64().unwrap();
            segment.as_object_mut().unwrap().remove("item_ids");
            segment["slices"] = serde_json::json!([{
                "item_id": id, "start_byte": 0, "end_byte": 5
            }]);
            segment["summary"] = serde_json::json!("legacy fragment summary");
        }
        store.save_session_with_context(&session, &serde_json::from_value(legacy).unwrap());
        let legacy_prefix = std::fs::read(&path).unwrap();
        assert!(legacy_prefix.starts_with(&original_prefix));

        manager.restore_state(&store.load_context(&session.id).unwrap());
        assert!(manager.begin_map_reduce(2_000));
        let whole = manager.save_state();
        assert_eq!(serde_json::to_value(&whole.items).unwrap(), original_items);
        let staging = whole.map_reduce.as_ref().unwrap();
        assert_eq!(staging.version, 3);
        assert!(staging.segments.iter().all(|segment| {
            !segment.item_ids.is_empty() && segment.slices.is_empty() && segment.summary.is_none()
        }));
        assert_eq!(
            staging
                .segments
                .iter()
                .flat_map(|segment| segment.item_ids.iter().copied())
                .collect::<Vec<_>>(),
            staging.source_item_ids
        );
        store.save_session_with_context(&session, &whole);
        let whole_prefix = std::fs::read(&path).unwrap();
        assert!(whole_prefix.starts_with(&legacy_prefix));
        assert!(whole_prefix.len() > legacy_prefix.len());
        assert_eq!(
            serde_json::to_value(store.load_context(&session.id).unwrap()).unwrap(),
            serde_json::to_value(&whole).unwrap()
        );

        let first = manager.pending_map_requests()[0].ordinal;
        assert!(manager.accept_map_summary(first, "whole-item summary"));
        store.save_session_with_context(&session, &manager.save_state());
        assert!(std::fs::read(&path).unwrap().starts_with(&whole_prefix));
        manager.restore_state(&store.load_context(&session.id).unwrap());
        assert!(manager.begin_map_reduce(2_000));
        assert_eq!(manager.map_progress().0, 1);
        assert!(
            manager
                .pending_map_requests()
                .iter()
                .all(|request| request.ordinal != first)
        );
        assert_eq!(
            serde_json::to_value(manager.save_state().items).unwrap(),
            original_items
        );
    }

    #[test]
    fn unverified_staging_migration_only_appends_and_preserves_committed_checkpoints() {
        use cosh::harness::context::ContextManager;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("staging-upgrade", "Upgrade", vec![]);
        let mut manager = ContextManager::new(4_000);
        manager.add_user("previous task");
        manager.add_assistant("previous work", true);
        manager.begin_manual_compaction();
        assert!(manager.apply_llm_summary("already committed checkpoint".into()));
        manager.add_user(&"new source evidence ".repeat(600));
        manager.add_assistant("new work", true);
        assert!(manager.begin_map_reduce(4_000));
        let mut legacy = manager.save_state();
        let staging = legacy.map_reduce.as_mut().unwrap();
        staging.version = 2;
        for segment in &mut staging.segments {
            segment.summary = Some("unverified map".into());
        }
        store.save_session_with_context(&session, &legacy);
        let path = store.file_path(&session.id);
        let prefix = std::fs::read(&path).unwrap();
        manager.restore_state(&store.load_context(&session.id).unwrap());
        assert!(manager.discard_unverified_compaction_staging());
        store.save_session_with_context(&session, &manager.save_state());
        let invalidated = std::fs::read(&path).unwrap();
        assert!(invalidated.starts_with(&prefix));
        assert!(manager.begin_map_reduce(4_000));
        store.save_session_with_context(&session, &manager.save_state());
        assert!(std::fs::read(&path).unwrap().starts_with(&invalidated));
        let restored = store.load_context(&session.id).unwrap();
        assert_eq!(
            serde_json::to_value(&restored.items).unwrap(),
            serde_json::to_value(&legacy.items).unwrap()
        );
        assert!(restored.items.iter().any(|item| matches!(item, ContextItem::Compaction { summary, .. } if summary == "already committed checkpoint")));
        let staging = restored.map_reduce.unwrap();
        assert_eq!(staging.version, 3);
        assert!(
            staging
                .segments
                .iter()
                .all(|segment| segment.summary.is_none())
        );
    }

    #[test]
    fn map_reduce_progress_checkpoint_and_branches_replay_from_one_immutable_history() {
        use cosh::harness::context::ContextManager;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut manager = ContextManager::new(2_000);
        manager.add_user(&"immutable task state ".repeat(2_000));
        manager.add_assistant("recent verified work", true);
        let mut session = make_test_session(
            "map-history",
            "MapReduce history",
            vec![
                make_user_msg("msg-0", &"immutable task state ".repeat(2_000)),
                make_assistant_msg("msg-1", "recent verified work"),
            ],
        );
        let path = store.file_path(&session.id);
        let mut prefix = Vec::new();
        let mut persist = |manager: &ContextManager, session: &Session| {
            let expected = manager.save_state();
            store.save_session_with_context(session, &expected);
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(&prefix));
            prefix = bytes;
            let replayed = store.load_context(&session.id).unwrap();
            assert_eq!(
                serde_json::to_value(replayed).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        };
        persist(&manager, &session);
        assert!(manager.begin_map_reduce(2_000));
        persist(&manager, &session);
        let requests = manager.pending_map_requests();
        for request in requests.into_iter().rev() {
            assert!(manager.accept_map_summary(request.ordinal, "task constraints and open work"));
            persist(&manager, &session);
            manager.restore_state(&store.load_context(&session.id).unwrap());
        }
        while let Some(request) = manager.next_reduce_request() {
            assert!(
                manager.accept_reduce_summary(&request, "checkpoint constraints and open work")
            );
            persist(&manager, &session);
        }
        while let Some(request) = manager.next_validation_request() {
            assert!(manager.accept_validation(&request, "PASS"));
            persist(&manager, &session);
        }
        assert!(manager.commit_map_reduce());
        session.messages.push(make_assistant_msg(
            "msg-checkpoint",
            "checkpoint constraints and open work",
        ));
        persist(&manager, &session);
        let checkpoint_state = store.load_context(&session.id).unwrap();
        let before_fork = std::fs::read(&path).unwrap();
        let mut forked = session.clone();
        forked.id = "map-child".into();
        assert!(store.fork_session(&session.id, "msg-checkpoint", &forked));
        assert!(!store.file_path(&forked.id).exists());
        assert!(std::fs::read(&path).unwrap().starts_with(&before_fork));
        let before_revert = std::fs::read(&path).unwrap();
        assert!(store.revert_session(&session.id, "msg-checkpoint"));
        let after_revert = std::fs::read(&path).unwrap();
        assert!(after_revert.starts_with(&before_revert));
        assert!(
            !store
                .load_context(&session.id)
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(item, ContextItem::Compaction { .. }))
        );
        assert!(store.rollback_session(&session.id, "v1"));
        assert!(std::fs::read(&path).unwrap().starts_with(&after_revert));
        assert_eq!(
            serde_json::to_value(store.load_context(&session.id).unwrap()).unwrap(),
            serde_json::to_value(checkpoint_state).unwrap()
        );
        assert!(
            store
                .load_context(&forked.id)
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(item, ContextItem::Compaction { .. }))
        );
    }

    #[test]
    fn fork_is_a_logical_branch_in_the_parent_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "parent",
            "Parent",
            vec![
                make_user_msg("msg-0", "one"),
                make_assistant_msg("msg-1", "two"),
                make_user_msg("msg-2", "three"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "one"),
            assistant_item(2, "two"),
            user_item(3, "three"),
        ]);
        store.save_session_with_context(&session, &context);
        let parent_path = store.file_path("parent");
        let before_fork = std::fs::read(&parent_path).unwrap();

        let mut forked = session.clone();
        forked.id = "child".into();
        forked.title = "Parent (fork)".into();
        forked.created_at = 20;
        forked.messages.truncate(2);
        forked.ctx_ids.retain(|message_id, _| message_id != "msg-2");
        assert!(store.fork_session("parent", "msg-1", &forked));

        let after_fork = std::fs::read(&parent_path).unwrap();
        assert!(after_fork.starts_with(&before_fork));
        assert!(!store.file_path("child").exists());
        let jsonl_count = std::fs::read_dir(&store.sessions_dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
            .count();
        assert_eq!(jsonl_count, 1, "a fork must share its parent's history");
        assert_eq!(
            store.load_history_for_branch("child").unwrap().0,
            parent_path
        );
        let loaded = store.load_session("child").unwrap();
        assert_eq!(loaded.title, "Parent (fork)");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(store.load_context("child").unwrap().items.len(), 2);
        assert_eq!(store.list_sessions().len(), 2);

        let mut changed_child = loaded;
        changed_child.title = "Child renamed".into();
        store.save_session(&changed_child);
        let after_child_save = std::fs::read(&parent_path).unwrap();
        assert!(after_child_save.starts_with(&after_fork));
        assert!(!store.file_path("child").exists());
        assert_eq!(store.load_session("child").unwrap().title, "Child renamed");
        assert_eq!(store.load_session("parent").unwrap().messages.len(), 3);
    }

    /// A title change appends safely even when an older line is corrupt.
    #[test]
    fn update_title_appends_after_a_corrupt_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session("7300", "Corrupt Line", vec![make_user_msg("msg-0", "hi")]);
        store.save_session(&session);
        let file = store.sessions_dir.join("session-7300.jsonl");
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{original}{{broken json\n")).unwrap();

        let prefix = std::fs::read(&file).unwrap();
        store.enqueue_job(
            StoreJob::Title {
                session_id: "7300".into(),
                title: "Renamed".into(),
            },
            true,
        );

        let bytes = std::fs::read(&file).unwrap();
        assert!(bytes.starts_with(&prefix));
        let contents = String::from_utf8(bytes).unwrap();
        assert!(
            contents.contains("broken json"),
            "the corrupt line must stay on disk"
        );
        assert!(
            store.load_session("7300").unwrap().title == "Renamed",
            "the appended title delta must remain replayable"
        );
    }

    /// Reference selection across a checkpoint derives visibility from the
    /// selected anchor ranges without deleting the original anchor event.
    #[test]
    fn revert_past_the_compaction_boundary_un_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let session = make_test_session(
            "7400",
            "Uncompact",
            vec![
                make_user_msg("msg-0", "pre-compaction"),
                make_assistant_msg("msg-1", "the anchor"),
                make_user_msg("msg-2", "post"),
            ],
        );
        let context = make_context(vec![
            user_item(1, "pre-compaction"),
            ContextItem::Compaction {
                id: 2,
                summary: "the anchor".into(),
                covered_ranges: vec![cosh::harness::context::ContextItemRange {
                    start_id: 1,
                    end_id: 1,
                }],
            },
            user_item(3, "post"),
        ]);
        store.save_session_with_context(&session, &context);
        let path = store.file_path("7400");
        let before = std::fs::read(&path).unwrap();

        assert!(store.revert_session("7400", "msg-1"));
        assert!(std::fs::read(&path).unwrap().starts_with(&before));
        let reverted = store.load_context("7400").unwrap();
        assert_eq!(reverted.items.len(), 1);
        assert_eq!(reverted.items[0].id(), 1);
        assert_eq!(reverted.visible_from, None, "the anchor is gone");
        let mut cm = cosh::harness::context::ContextManager::new(100_000);
        cm.restore_state(&reverted);
        let msgs = cm.build_messages("");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.as_deref(), Some("pre-compaction"));

        assert!(store.rollback_session("7400", "v1"));
        let mut forked = store.load_session("7400").unwrap();
        forked.id = "7400-child".into();
        forked.messages.truncate(2);
        assert!(store.fork_session("7400", "msg-1", &forked));
        let child = store.load_context("7400-child").unwrap();
        assert_eq!(child.items.len(), 2);
        assert_eq!(child.visible_from, None);
        let mut child_manager = cosh::harness::context::ContextManager::new(100_000);
        child_manager.restore_state(&child);
        let messages = child_manager.build_messages("");
        assert_eq!(messages.len(), 1, "the checkpoint covers its source");
        assert_eq!(messages[0].content.as_deref(), Some("the anchor"));
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

    #[test]
    fn map_reduce_staging_write_amplification_measurement() {
        use cosh::harness::context::ContextManager;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let mut manager = ContextManager::new(2_000);
        for turn in 0..8 {
            manager.add_user(&format!("turn {turn}: {}", "history ".repeat(600)));
            manager.add_assistant("acknowledged", false);
        }
        let session = make_test_session("staging-amp", "Amplification", Vec::new());
        let path = store.file_path(&session.id);
        let mut prefix = Vec::new();
        let mut stages: Vec<(&str, usize)> = Vec::new();
        fn persist<'a>(
            store: &SessionStore,
            path: &std::path::Path,
            label: &'a str,
            manager: &ContextManager,
            session: &Session,
            prefix: &mut Vec<u8>,
            stages: &mut Vec<(&'a str, usize)>,
        ) {
            let state = manager.save_state();
            store.save_session_with_context(session, &state);
            let bytes = std::fs::read(path).unwrap();
            assert!(bytes.starts_with(prefix), "appends must never rewrite");
            stages.push((label, bytes.len() - prefix.len()));
            *prefix = bytes;
            let replayed = store.load_context(&session.id).unwrap();
            assert_eq!(
                serde_json::to_value(replayed).unwrap(),
                serde_json::to_value(state).unwrap()
            );
        }
        persist(
            &store,
            &path,
            "initialize",
            &manager,
            &session,
            &mut prefix,
            &mut stages,
        );
        assert!(manager.begin_map_reduce(2_000));
        persist(
            &store,
            &path,
            "begin",
            &manager,
            &session,
            &mut prefix,
            &mut stages,
        );
        let segments = manager.pending_map_requests().len();
        for request in manager.pending_map_requests() {
            assert!(manager.accept_map_summary(
                request.ordinal,
                &format!("segment summary\n{}", "note ".repeat(450))
            ));
            persist(
                &store,
                &path,
                "map",
                &manager,
                &session,
                &mut prefix,
                &mut stages,
            );
        }
        while let Some(request) = manager.next_reduce_request() {
            assert!(manager.accept_reduce_summary(&request, "reduced checkpoint"));
            persist(
                &store,
                &path,
                "reduce",
                &manager,
                &session,
                &mut prefix,
                &mut stages,
            );
        }
        while let Some(request) = manager.next_validation_request() {
            assert!(manager.accept_validation(&request, "PASS"));
            persist(
                &store,
                &path,
                "validate",
                &manager,
                &session,
                &mut prefix,
                &mut stages,
            );
        }
        let total_appended: usize = stages.iter().map(|(_, bytes)| bytes).sum();
        let final_state = serde_json::to_string(&manager.save_state().map_reduce)
            .unwrap()
            .len();
        for (label, bytes) in &stages {
            println!("{label}: {bytes} bytes");
        }
        println!(
            "segments={segments} total_appended={total_appended} final_map_reduce_state={final_state} amplification={}x",
            total_appended / final_state.max(1)
        );
    }
}
