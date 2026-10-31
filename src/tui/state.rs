use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use lru::LruCache;

use crate::routes::session::right_panel::types::RightPanelState;
use crate::session::store::SessionSummary;
use crate::types::{Session, SessionStatus};
use cosh::harness::Mode;

const SESSION_CACHE_SIZE: usize = 10;

/// In-memory pending user messages for one session, waiting to be delivered.
///
/// - [`Self::next_request`] messages enter the NEXT REQUEST of the currently
///   running agent loop (the "next request" queue).
/// - [`Self::next_loop`] messages wait for the current loop to end and then
///   start a fresh loop (the "next agent loop" queue).
///
/// Deliberately kept out of the persisted session model — this is transient
/// UI state that lives for the app's lifetime only.
#[derive(Debug, Clone, Default)]
pub struct PendingQueues {
    /// FIFO of messages for the next request of the running loop.
    pub next_request: VecDeque<String>,
    /// FIFO of messages for the next agent loop.
    pub next_loop: VecDeque<String>,
}

impl PendingQueues {
    /// Total number of queued messages across both queues.
    pub fn queued_count(&self) -> usize {
        self.next_request.len() + self.next_loop.len()
    }

    /// Clear all queued messages from both queues.
    pub fn clear(&mut self) {
        self.next_request.clear();
        self.next_loop.clear();
    }
}

pub struct AppState {
    /// Header-only session summaries (always in RAM, sidebar uses these).
    pub session_summaries: Vec<SessionSummary>,
    /// Full session cache (LRU eviction, only recently accessed sessions stored).
    pub session_cache: LruCache<String, Session>,
    /// Pending queued messages per session id (in-memory only).
    pub pending_queues: HashMap<String, PendingQueues>,
    pub current_session_id: Option<String>,
    pub status: SessionStatus,
    pub scroll_y: i32,
    pub content_height: i32,
    /// Where cosh itself starts: the process cwd, captured once in
    /// `App::new` and never mutated afterwards. Two things read it — the
    /// session store root (it LOCATES the session, so it must not move) and
    /// `/worktree exit` (it RESTORES this, so it must not move). Anything
    /// describing where the MAIN AGENT works reads
    /// [`Self::main_agent_cwd`] instead.
    pub working_directory: String,
    /// Current git branch (or short SHA on detached HEAD) for the session
    /// footer; `None` when the working directory is outside a repository.
    /// Refreshed by the app loop via the git branch tracker.
    pub git_branch: Option<String>,
    pub mode: Mode,
    /// Whether LSP is enabled at all (`COSH_LSP`). Distinct from how many
    /// servers are running — the footer shows "disabled" only when false.
    pub lsp_available: bool,
    /// Catalog names of the language servers currently active; rendered as
    /// colored tags in the prompt footer. Empty disables the tags.
    pub lsp_servers: Vec<String>,
    pub mcp_count: usize,
    pub mcp_errors: usize,
    pub permission_count: usize,
    /// Right panel state (TODOs, PTY sessions).
    pub right_panel: RightPanelState,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session_summaries: vec![],
            session_cache: LruCache::new(NonZeroUsize::new(SESSION_CACHE_SIZE).unwrap()),
            pending_queues: HashMap::new(),
            current_session_id: None,
            status: SessionStatus::Idle,
            scroll_y: 0,
            content_height: 0,
            working_directory: String::new(),
            git_branch: None,
            mode: Mode::Build,
            lsp_available: false,
            lsp_servers: Vec::new(),
            mcp_count: 0,
            mcp_errors: 0,
            permission_count: 0,
            right_panel: RightPanelState::new(),
        }
    }

    /// Pending queues for the current session (if one is selected).
    pub fn current_pending_queues(&self) -> Option<&PendingQueues> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.pending_queues.get(id))
    }

    /// Mutable pending queues for the current session (creating an empty
    /// entry when missing). `None` when no session is selected.
    pub fn current_pending_queues_mut(&mut self) -> Option<&mut PendingQueues> {
        let id = self.current_session_id.clone()?;
        Some(self.pending_queues.entry(id).or_default())
    }

    /// Remove a session by ID. Clears `current_session_id` if it matches.
    pub fn remove_session(&mut self, session_id: &str) {
        self.session_summaries
            .retain(|s| s.session_id != session_id);
        self.session_cache.pop(session_id);
        self.pending_queues.remove(session_id);
        if self
            .current_session_id
            .as_deref()
            .is_some_and(|id| id == session_id)
        {
            self.current_session_id = None;
        }
    }

    /// Add a full session (with its messages) and a matching summary.
    ///
    /// Every tool part is assigned a unique `tool_call_id` first (see
    /// [`Session::ensure_tool_call_ids`]) — restored/legacy sessions must not
    /// share per-tool-type expansion keys, or expanding one box would toggle
    /// every other box of the same tool.
    pub fn add_session(&mut self, mut session: Session) {
        session.ensure_tool_call_ids();
        // Live ids must never collide with a restored transcript's ids (see
        // `seed_tool_call_seq_beyond_session`).
        crate::app::events::seed_tool_call_seq_beyond_session(&session);
        let summary = SessionSummary {
            session_id: session.id.clone(),
            title: session.title.clone(),
            created_at: session.created_at,
            message_count: session.messages.len(),
            cwd: self.working_directory.clone(),
            model: session.model.clone(),
            title_generated: session.title_generated,
        };
        self.session_summaries.push(summary);
        self.pending_queues.entry(session.id.clone()).or_default();
        self.session_cache.put(session.id.clone(), session);
    }

    /// Add a placeholder empty session to the cache. No summary is created yet —
    /// the session only appears in the sidebar once it has been persisted (i.e.,
    /// it has valid content and has been saved to disk).
    pub fn add_empty_session(&mut self, id: String, title: String, created_at: u64) {
        self.pending_queues.entry(id.clone()).or_default();
        self.session_cache.put(
            id.clone(),
            Session {
                id: id.clone(),
                title,
                created_at,
                messages: vec![],
                title_generated: false,
                provider: None,
                model: None,
                reasoning: None,
                agents_panes: None,
                agents_mode_open: false,
                agents_hidden: Vec::new(),
                ctx_ids: Default::default(),
                main_agent_worktree: None,
            },
        );
    }

    /// Ensure a summary exists in `session_summaries` for the session with the
    /// given id. If it already exists, this is a no-op.
    pub fn ensure_session_summary(&mut self, session_id: &str) {
        if self
            .session_summaries
            .iter()
            .any(|s| s.session_id == session_id)
        {
            return;
        }
        if let Some(session) = self.session_cache.peek(session_id) {
            let summary = SessionSummary {
                session_id: session.id.clone(),
                title: session.title.clone(),
                created_at: session.created_at,
                message_count: session.messages.len(),
                cwd: self.working_directory.clone(),
                model: session.model.clone(),
                title_generated: session.title_generated,
            };
            self.session_summaries.push(summary);
        }
    }

    /// Ensure a session is in the cache. If not, attempts to load from the
    /// provided store. Returns `true` if the session is available after the call.
    pub fn ensure_session_cached(
        &mut self,
        session_id: &str,
        store: &crate::session::store::SessionStore,
    ) -> bool {
        if self.session_cache.contains(session_id) {
            return true;
        }
        if let Some(mut session) = store.load_session(session_id) {
            // Same normalization as `add_session`: restored sessions must not
            // share per-tool-type expansion keys across boxes.
            session.ensure_tool_call_ids();
            // And live ids must never collide with this restored transcript's
            // ids (see `seed_tool_call_seq_beyond_session`).
            crate::app::events::seed_tool_call_seq_beyond_session(&session);
            self.session_cache.put(session_id.to_string(), session);
            true
        } else {
            false
        }
    }

    /// Returns the current session if one is selected and in cache.
    /// Uses `peek` to avoid mutating LRU order on reads — the current session
    /// is kept alive by `current_session_mut()` and `switch_to_session()`.
    pub fn current_session(&self) -> Option<&Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.session_cache.peek(id.as_str()))
    }

    /// Where the MAIN AGENT works right now: the session's `/worktree`
    /// binding when one is pinned, else [`Self::working_directory`].
    ///
    /// This is the ONE seam every display/resolution of the agent's cwd goes
    /// through, so a new consumer cannot accidentally describe the startup
    /// tree while the agent edits a checkout. Deliberately on `AppState`
    /// rather than `App`: footer rendering and the file explorer only hold
    /// `&AppState`, and they must report the same directory the agent's
    /// turns use.
    ///
    /// Not for `working_directory`'s own two readers — the session store
    /// root and `/worktree exit` both need the STARTUP cwd and must keep
    /// reading the field directly.
    #[must_use]
    pub fn main_agent_cwd(&self) -> std::path::PathBuf {
        self.current_session()
            .and_then(|session| session.main_agent_worktree.as_ref())
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| std::path::PathBuf::from(&self.working_directory))
    }

    /// Returns a mutable reference to the current session if one is selected
    /// and in cache.
    pub fn current_session_mut(&mut self) -> Option<&mut Session> {
        let id = self.current_session_id.clone()?;
        self.session_cache.get_mut(&id)
    }

    /// Swap the current session: save one, load another. The old session is
    /// upserted into the cache (so subsequent switches are fast).
    ///
    /// Returns `false` when the target session is already open in another
    /// cosh process — the switch is refused and the caller is expected to
    /// surface that to the user. Otherwise releases the previous session's
    /// open-lock, claims the target's and performs the switch.
    pub fn switch_to_session(
        &mut self,
        session_id: String,
        store: &crate::session::store::SessionStore,
    ) -> bool {
        // Refuse a session another process already holds open, BEFORE any
        // state mutation: the switch must be all-or-nothing.
        if !store.try_lock_session(&session_id) {
            return false;
        }
        // Save current session to disk only if it has real content
        let old_id = self.current_session_id.clone();
        if let Some(ref oid) = old_id {
            let should_save = self
                .session_cache
                .get(oid)
                .is_some_and(crate::session::store::is_valid_session);
            if should_save {
                // Reborrow to avoid borrow conflict with ensure_session_summary
                if let Some(session) = self.session_cache.get(oid) {
                    store.save_session_async(session);
                }
                self.ensure_session_summary(oid);
            }
            // The old session is no longer open in this process: free its
            // open-lock so another cosh instance can take it over. Re-selecting
            // the session already being viewed must NOT release — the claim
            // just re-verified above is the one this process keeps holding.
            if oid != &session_id {
                store.release_session_lock(oid);
            }
        }

        // Ensure target is cached
        self.ensure_session_cached(&session_id, store);
        self.current_session_id = Some(session_id);
        true
    }

    pub fn unique_agents(&self) -> Vec<String> {
        let mut seen = Vec::new();
        if let Some(session) = self.current_session() {
            for msg in &session.messages {
                if let Some(agent) = &msg.agent
                    && !seen.contains(agent)
                {
                    seen.push(agent.clone());
                }
            }
        }
        seen
    }
}
