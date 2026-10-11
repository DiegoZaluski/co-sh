use cosh::harness::HarnessEvent;
use cosh_tui::core::lib::rgba::RGBA;
use std::sync::atomic::Ordering;

use super::App;
use super::BUG_REPORT_URL;
use crate::session_store::format_session_timestamp;
use crate::ui::agents_picker::AgentPick;
use crate::ui::dialogs::DialogType;

impl App {
    /// Enter the `/agents` split-pane mode with the agents the picker
    /// confirmed, in the order they were marked. Each pick carries its own
    /// instance count: the pane list receives `instances` copies of the
    /// agent so two Claude panes are two INDEPENDENT sessions (the
    /// per-pane binding lands in task "binding"). The `cosh` daemon pane is
    /// NOT inserted automatically: the Cosh Manager belongs to the user's
    /// own split choice (Ctrl+0 checks whether pane 0 is the daemon before
    /// contacting it), so only an explicitly marked `cosh` entry creates
    /// its block. Opening the split does NOT spawn any session — panes
    /// start idle and only receive work when the shared prompt is
    /// submitted.
    pub(super) fn open_agents_split(&mut self, picks: Vec<AgentPick>) {
        // One persisted pane per pick instance — every instance of the same
        // agent becomes its own pane (and, later, its own session).
        let mut panes: Vec<crate::types::AgentPaneRef> = picks
            .iter()
            .flat_map(|pick| {
                std::iter::repeat_n(
                    crate::types::AgentPaneRef::Name(pick.name.clone()),
                    pick.instances,
                )
            })
            .collect();
        // Binding reuse: the k-th occurrence of an agent INHERITS the
        // persisted k-th occurrence's bound session. Re-opening the same
        // picker layout therefore resumes every agent context (the
        // "re-entry re-binds instances+sessions" contract); reshuffling
        // keeps the bindings of the occurrences that still exist and
        // starts fresh sessions for genuinely new panes.
        if let Some(persisted) = self
            .state
            .current_session()
            .and_then(|s| s.agents_panes.clone())
        {
            let mut seen: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            // Occurrence index per agent name in the PERSISTED list.
            let mut persisted_occurrence: std::collections::HashMap<(String, usize), &crate::types::AgentPaneRef> =
                std::collections::HashMap::new();
            for entry in &persisted {
                let k = seen.entry(entry.agent().to_string()).or_insert(0);
                persisted_occurrence
                    .insert((entry.agent().to_string(), *k), entry);
                *k += 1;
            }
            seen.clear();
            for fresh in panes.iter_mut() {
                let crate::types::AgentPaneRef::Name(name) = fresh else {
                    continue;
                };
                let k = seen.entry(name.clone()).or_insert(0);
                let bound = persisted_occurrence
                    .get(&(name.clone(), *k))
                    .and_then(|entry| entry.session_id().map(str::to_string))
                    .map(|session_id| crate::types::AgentPaneRef::Bound {
                        agent: name.clone(),
                        session_id,
                    });
                *k += 1;
                if let Some(bound) = bound {
                    *fresh = bound;
                }
            }
        }
        self.set_agents_split(Some(panes));
        self.prompt_view.focus();
    }

    /// Leave the `/agents` split-pane mode WITHOUT killing anything: the
    /// split (with its running turns and stop signals) moves to the
    /// `parked_agents_split` slot and keeps receiving events, the prompt
    /// returns to the normal session view. The pane set + bindings stay in
    /// the session metadata (`agents_panes`), but `agents_mode_open` drops
    /// to `false` — a session switch or app restart replays the manager set
    /// as AVAILABLE (pre-marked picker), never as a live split screen.
    pub(super) fn park_agents_split(&mut self) {
        if let Some(split) = self.agents_split.take() {
            // A running turn keeps its spinner alive through the shared
            // activity — do NOT touch the signals, do NOT clear running.
            self.parked_agents_split = Some(split);
        }
        // Metadata: mode closed, pane set + bindings retained for the
        // pre-marked re-entry. Synchronous: same quit-path rationale as
        // set_agents_split.
        let panes = self
            .state
            .current_session()
            .and_then(|s| s.agents_panes.clone());
        self.save_agents_metadata(panes, false);
        // The mirrors follow the panes into the parked slot: the stream
        // keeps landing in the shared list (same routing as live panes).
        self.sync_agents_pane_mirrors();
        self.prompt_view.focus();
    }

    /// Re-attach the parked split (the `/agents` re-entry): the SAME pane
    /// objects return to the screen — running turns surface mid-stream,
    /// drafts and bound sessions are exactly where the user left them.
    /// Returns `true` when a parked split took the screen (the caller then
    /// skips opening the picker).
    pub(super) fn attach_parked_agents_split(&mut self) -> bool {
        if let Some(split) = self.parked_agents_split.take() {
            self.agents_split = Some(split);
            let panes = self
                .state
                .current_session()
                .and_then(|s| s.agents_panes.clone());
            self.save_agents_metadata(panes, true);
            // A parked set rebuilt from metadata (session switch or app
            // restart) carries bound panes with empty timelines — the
            // rehydrate passes fire only now that the panes take the
            // screen (a parked set is invisible; replaying into it would
            // burn a harness boot for nothing).
            self.trigger_agents_pane_replays();
            // The mirrors are already in place (the parked slot kept them
            // alive); one sync re-affirms their state for the re-open.
            self.sync_agents_pane_mirrors();
            self.prompt_view.focus();
            return true;
        }
        false
    }

    /// Leave the `/agents` split-pane mode: any running pane turn is stopped
    /// through its own stop signal (the pane's partial output stays visible
    /// until the split is re-entered). The shared spinner is dropped with
    /// the split: no pane can keep it alive once the panes are gone.
    pub(super) fn close_agents_split(&mut self) {
        self.set_agents_split(None);
        self.parked_agents_split = None;
        // The pane mirrors leave the shared subagent list with the mode:
        // they exist only while a split (live or parked) owns panes here.
        self.state.right_panel.drop_agents_pane_windows();
        self.prompt_view.focus();
    }

    /// Mirror the `/agents` panes into the right panel's SHARED subagent
    /// list: every pane of the live OR parked split owns one stable window
    /// (opened on demand, pane-index routed — see
    /// `RightPanelState::open_agents_pane_window`) so its agent surfaces
    /// there with the full-color brand logo, alongside the normal
    /// subagents. Runs after every pane event and at every (re)open; a pane
    /// that vanished from the set (a fresh confirm with fewer panes) drops
    /// its mirror.
    pub(super) fn sync_agents_pane_mirrors(&mut self) {
        let Some(split) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
        else {
            return;
        };
        self.state
            .right_panel
            .prune_agents_pane_beyond(split.panes.len());
        for (pane, p) in split.panes.iter().enumerate() {
            self.state.right_panel.open_agents_pane_window(pane, &p.agent);
            self.state.right_panel.sync_agents_pane(
                pane,
                &p.activity,
                // A rehydrating pane spins too: the mirror is the visible
                // signal that the bound history is coming back (the pane
                // itself may be empty until the replay lands).
                p.running || p.rehydrating,
                split.pane_mirror_label(pane).as_deref(),
            );
        }
    }

    /// Seal one pane's mirror at turn end: the final report becomes the
    /// mirror window's BODY (the linear-chat contract), the transcript
    /// seals, the spinner stops. Resolves the live-or-parked split like the
    /// event routing does.
    pub(super) fn finish_agents_pane_mirror(&mut self, pane: usize, output: &str, failed: bool) {
        let Some(split) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
        else {
            return;
        };
        let Some(p) = split.panes.get(pane) else {
            return;
        };
        self.state
            .right_panel
            .finish_agents_pane(pane, &p.activity, output, failed);
    }

    /// Fire cold-restore replay passes for every pane that needs one: a
    /// `Bound` pane with an EMPTY live timeline (the post-reboot state —
    /// the process died with the app, only the session id persisted).
    /// Rehydration streams the agent's own `session/load` history back
    /// into the pane (see `dispatch_agents_pane_replay`); a pane with
    /// content already (live turn, prior replay) and unbound `Name` panes
    /// are skipped.
    pub(super) fn trigger_agents_pane_replays(&mut self) {
        let needs = self
            .agents_split
            .as_ref()
            .map(|split| {
                split
                    .panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.session_id.is_some()
                            && p.activity.timeline.is_empty()
                            && !p.rehydrating
                    })
                    .map(|(i, _)| i)
                    .collect::<Vec<usize>>()
            })
            .unwrap_or_default();
        for pane in needs {
            self.dispatch_agents_pane_replay(pane);
        }
    }

    /// The state-only core of open/close: signal-stop every pane of the
    /// outgoing split, swap the pane set, persist mode + pane set on the
    /// session and reconcile the shared spinner. Does NOT touch prompt
    /// focus — the focus is a USER-action concern (open/close from the
    /// picker or Esc), never an automatic one (session switch, `/new`, app
    /// restore).
    ///
    /// Bumps `agents_pane_epoch` on EVERY call (replacement or teardown):
    /// the outgoing set's cancelled turns must deliver their terminal
    /// `AgentsPaneDone` against a stale epoch so the handler drops them
    /// instead of binding old sessions into the new set.
    fn set_agents_split(&mut self, panes: Option<Vec<crate::types::AgentPaneRef>>) {
        // The split slots' generation moves on EVERY replacement or teardown
        // (see the `agents_pane_epoch` field doc): the outgoing set's
        // cancelled turns deliver their terminal Done against the OLD epoch
        // and the handler drops it — a stopped pane can never seed a
        // replacement pane that merely shares the index.
        self.agents_pane_epoch = self.agents_pane_epoch.wrapping_add(1);
        // A split being REPLACED must signal-stop every old pane first.
        // A discarded stop signal is one nobody will ever set again — its
        // turn would run to completion, un-stoppable, and its late events
        // would land in the NEW panes at the same indexes.
        if let Some(old) = &mut self.agents_split {
            for pane in &mut old.panes {
                if let Some(signal) = &pane.stop_signal {
                    signal.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                pane.running = false;
            }
        }
        // A FRESH confirm supersedes a parked set of the same session: the
        // user re-picked, so the parked panes (and any turn still running in
        // them) stop exactly like a live replacement would — leaving them
        // would stream old-set events into whichever slot later takes pane
        // `pane`'s index.
        if panes.is_some()
            && let Some(old) = &mut self.parked_agents_split
        {
            for pane in &mut old.panes {
                if let Some(signal) = &pane.stop_signal {
                    signal.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                pane.running = false;
            }
            self.parked_agents_split = None;
        }
        self.agents_split = panes
            .clone()
            .map(crate::routes::session::agents_split::AgentsSplitState::new);
        let mode_open = self.agents_split.is_some();
        self.save_agents_metadata(panes, mode_open);
        // A fresh pane set mirrors into the shared subagent list (the sync
        // also prunes mirrors of panes the new set no longer has). `None`
        // closes the mode: `close_agents_split` owns the mirror drop.
        if mode_open {
            self.sync_agents_pane_mirrors();
            // A fresh confirm re-binds persisted sessions (the
            // `Bound`-inherit above); panes whose process died with the
            // app rehydrate their transcript from the agent's own replay
            // here.
            self.trigger_agents_pane_replays();
        }
        // Drop the split's spinner only when no NORMAL agent loop is still
        // running behind it (the double-scenario): a Working loop keeps its
        // indicator; an idle app must not inherit a stale spinner.
        if self.agents_split.is_none()
            && self.state.status != crate::types::SessionStatus::Working
        {
            self.agent_spinner = None;
        }
    }

    /// Record the session's `/agents` bookkeeping — the pane set (with its
    /// bindings) AND whether the split-screen mode itself is open — and
    /// persist through the store's normal diff pipeline (`diff_session`
    /// emits a `MetadataDelta::Agents` only when the persisted pane set
    /// actually changed). Both fields are written together so the parked
    /// state (`mode_open = false`, pane set retained) is durable the same
    /// way the open state is. SYNCHRONOUS on purpose: open/close is often
    /// chained with quitting the app, and the async writer's queue has no
    /// flush on the quit path — a fire-and-forget save could never land,
    /// reopening the session with the wrong mode. The job is one tiny
    /// metadata delta, so blocking the frame is negligible. No session
    /// open (Home) → nothing.
    fn save_agents_metadata(
        &mut self,
        panes: Option<Vec<crate::types::AgentPaneRef>>,
        mode_open: bool,
    ) {
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        if session.agents_panes == panes && session.agents_mode_open == mode_open {
            return;
        }
        session.agents_panes = panes;
        session.agents_mode_open = mode_open;
        let session = session.clone();
        self.session_store.save_session(&session);
    }

    /// Promote ONE pane's persisted entry to `Bound` after its first (or any)
    /// turn: the new session id is written back into `session.agents_panes`
    /// so a later re-open resumes THIS agent context instead of forking a
    /// fresh one. The pane may be LIVE (dispatch while the split is open) or
    /// PARKED (a turn finishing after the user Esc'd out of the mode) — both
    /// slots carry the same pane objects, so the agent name resolves from
    /// whichever holds pane `pane`. Indexes align between the live split and
    /// the persisted list because every grow/shrink path routes through
    /// `AgentsPickerAction::Confirmed` / `set_agents_split`; an out-of-range
    /// index is silently ignored.
    pub(crate) fn bind_agents_pane_session(&mut self, pane: usize, session_id: String) {
        let agent_name = self
            .agents_split
            .as_ref()
            .and_then(|split| split.panes.get(pane))
            .or_else(|| {
                self.parked_agents_split
                    .as_ref()
                    .and_then(|split| split.panes.get(pane))
            })
            .map(|p| p.agent.clone());
        let Some(agent_name) = agent_name else {
            return;
        };
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let Some(panes) = &mut session.agents_panes else {
            return;
        };
        let Some(entry) = panes.get_mut(pane) else {
            return;
        };
        if entry.agent() != agent_name {
            // Persisted/live divergence: refusing keeps the metadata keyed
            // to the pane the user actually picked.
            return;
        }
        if entry.session_id() == Some(session_id.as_str()) {
            return;
        }
        *entry = crate::types::AgentPaneRef::Bound {
            agent: agent_name,
            session_id,
        };
        // Write through the cache and sync-save: the OLD dedup here compared
        // against the ALREADY-MUTATED field and silently skipped the save,
        // so bindings never reached disk — `save_agents_metadata` is called
        // BEFORE the field was touched in its own callers; here the value is
        // already in the cache, hence the explicit save.
        let session = session.clone();
        self.session_store.save_session(&session);
    }

    /// Manual compaction has no running agent connector to inherit in auto
    /// mode, so resolve only the user's auto chain (never the literal "auto").
    pub(super) fn manual_summarization_models(&self) -> Vec<(String, String)> {
        let entries = if !self.setup.routing.summarization_models.is_empty() {
            &self.setup.routing.summarization_models
        } else if self.llm_config.model.as_deref() == Some("auto") {
            &self.router_view.fallbacks
        } else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect()
    }
    /// Opens the bug-report page in the default browser and shows a toast.
    pub(super) fn open_bug_report_link(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some("Bug report".to_string()),
            message: format!("Opening {BUG_REPORT_URL}…"),
            variant: ToastVariant::Info,
            duration_ms: 2500,
        });
        std::thread::spawn(move || {
            if let Err(err) = open::that(BUG_REPORT_URL) {
                log::error!("Failed to open bug report URL: {err}");
            }
        });
    }

    /// Handle a click (or Enter/`u` key press) on the home banner's update
    /// announcement: run the update pipeline or open the changelog page.
    #[cfg(feature = "home")]
    pub(super) fn handle_banner_action(&mut self, action: crate::routes::home::BannerAction) {
        use crate::routes::home::BannerAction;

        let Some(crate::routes::home::BannerContent::ReleaseUpdate { tag, .. }) =
            self.home_view.banner.content.clone()
        else {
            return;
        };

        match action {
            BannerAction::OpenChangelog => {
                let url = self.update_changelog_url.clone().unwrap_or_else(|| {
                    format!(
                        "https://github.com/{}/releases/tag/{tag}",
                        crate::update::REPO
                    )
                });
                std::thread::spawn(move || {
                    if let Err(err) = open::that(url) {
                        log::error!("Failed to open changelog URL: {err}");
                    }
                });
            }
            BannerAction::StartUpdate => {
                // Fire-and-forget: the pipeline downloads/installs and
                // RELAUNCHES cosh when it finishes, so the very next step is
                // to close this TUI. No toast, no spinner, no in-app state —
                // the terminal closing and reopening IS the feedback.
                crate::update::spawn_update_pipeline(&tag);
                self.should_quit = true;
            }
        }
    }

    /// Click on the session footer's update button (`v0.1.2` in
    /// `v0.1.1 → v0.1.2`): run the SAME install pipeline the Home banner's
    /// "Update version" button uses — `spawn_update_pipeline` downloads the
    /// release with the pinned installer script, replaces the binary and
    /// relaunches cosh; closing the TUI is the whole feedback.
    pub(super) fn click_footer_update(&mut self) {
        // The RAW release tag (`v0.1.2`) straight from the GitHub check —
        // the same value the Home banner passes, never a reconstruction:
        // `spawn_update_pipeline` pins both the installer-script URL and the
        // `COSH_VERSION` to this tag, so a tag published without the `v`
        // prefix must survive verbatim. It is validated again inside.
        if let Some(tag) = self.update_latest_tag.clone() {
            crate::update::spawn_update_pipeline(&tag);
            self.should_quit = true;
        }
    }

    /// Drain update-related events from the background release check (drives
    /// the banner content). Called once per frame.
    pub(super) fn pump_update_events(&mut self) {
        while let Ok(event) = self.update_event_rx.try_recv() {
            match event {
                crate::update::UpdateEvent::CheckFinished(Some(release)) => {
                    self.update_changelog_url = Some(release.url.clone());
                    // The version also feeds the session footer's
                    // `v0.1.1 → v0.1.2` hint; the raw tag is what the
                    // footer's click hands to the install pipeline (same
                    // value the Home banner passes).
                    self.update_latest_version = Some(release.version.clone());
                    self.update_latest_tag = Some(release.tag.clone());
                    // Announce the release in a toast: title = new version,
                    // message = the manual install command for the running
                    // OS. The ⧉ button on the toast copies the command —
                    // too long to retype by hand.
                    if let Some(command) = crate::update::manual_update_command(&release.tag) {
                        use crate::ui::toast::{ToastOptions, ToastVariant};
                        self.toast_state.show_copyable(
                            ToastOptions {
                                title: Some(format!("Update to v{}", release.version)),
                                message: command.clone(),
                                variant: ToastVariant::Info,
                                // Long enough to read a curl one-liner.
                                duration_ms: 50_000,
                            },
                            command,
                        );
                    }
                    #[cfg(feature = "home")]
                    {
                        use crate::routes::home::{BannerContent, UpdateStatus};
                        self.home_view.banner.content = Some(BannerContent::ReleaseUpdate {
                            version: release.version.clone(),
                            tag: release.tag.clone(),
                        });
                        self.home_view.banner.status = UpdateStatus::Idle;
                    }
                }
                crate::update::UpdateEvent::CheckFinished(None) => {
                    // Up to date or check failed: banner stays hidden.
                }
            }
        }
    }

    /// Rebuild the right panel from the CURRENT session's persisted tool
    /// parts (todos, bash runs, subagent windows). Called after every
    /// session-switch path resets the panel: the panel content belongs to
    /// the session, so it must survive app restarts and session switches
    /// instead of silently disappearing.
    pub(super) fn rehydrate_right_panel(&mut self) {
        if let Some(session) = self.state.current_session().cloned() {
            self.state.right_panel.rehydrate_from_session(&session);
        }
    }

    /// Persist the current session (when it holds real dialog) and RELEASE
    /// its per-session open-lock — without touching `current_session_id`.
    ///
    /// The hand-off half of the open-lock lifecycle: a process must keep a
    /// claim ONLY while the session is its selected one. Every path that
    /// moves the app away from a session (switch, Home, /new) goes through
    /// this so the lock is handed to the next process the moment the
    /// session stops being ours — not when this process dies.
    pub(super) fn persist_and_release_current_session(&mut self) {
        if let Some(id) = self.state.current_session_id.clone() {
            // Single lookup: persist only when the cached session holds
            // real dialog.
            if let Some(session) = self.state.session_cache.get(&id)
                && crate::session_store::is_valid_session(session)
            {
                self.session_store.save_session_async(session);
            }
            self.session_store.release_session_lock(&id);
        }
    }

    /// Leave the current session and return to Home: persist the session
    /// when it holds real dialog, RELEASE its per-session open-lock, and
    /// clear the header/panel state that belongs to a session.
    ///
    /// The release matters: without it the process keeps holding a session
    /// it is not viewing, and any OTHER cosh process trying to open that
    /// session is refused with "already open in another process" even
    /// though nothing here is using it. The lifecycle of the open-lock
    /// mirrors `switch_to_session`: held while the session is this
    /// process's selected one, released the moment it stops being one.
    pub(super) fn return_to_home(&mut self) {
        self.persist_and_release_current_session();
        self.state.current_session_id = None;
        // Back to Home: the header widget is gone, so its state must not
        // survive into the next session.
        self.hypercredit_balance = None;
        // Both `/agents` slots release the same way a switch does: running
        // pane turns stop through their own signals; the pane set + bindings
        // stay behind in the (no longer current) session's metadata. The
        // epoch moves with the teardown: their terminal Done arrives stale
        // and is dropped (Home has no pane set to bind into anyway).
        self.agents_pane_epoch = self.agents_pane_epoch.wrapping_add(1);
        for split in self
            .agents_split
            .take()
            .iter_mut()
            .chain(self.parked_agents_split.take().iter_mut())
        {
            for pane in &mut split.panes {
                if let Some(signal) = &pane.stop_signal {
                    signal.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                pane.running = false;
            }
        }
        // Home has no session view: the whole panel resets below, mirrors
        // included (a switch keeps the panel and drops/rebuilds mirrors
        // explicitly instead).
        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
    }

    /// Switch to a session, refusing the switch when it is already open in
    /// another cosh process (toast warning). The shared tail of every
    /// sidebar keyboard/mouse switch: the guarded [`AppState::
    /// switch_to_session`] plus the panel reset/rehydrate pair.
    ///
    /// Returns `false` (with the toast shown) when the target session is
    /// locked by another process; `true` on a completed switch.
    pub(super) fn switch_session_locked(&mut self, session_id: &str) -> bool {
        // Finalize stale compaction lines on the OLD session BEFORE the
        // switch: `switch_to_session` persists it, and a persisted session
        // must not carry a still-running compaction stopwatch (it would
        // inflate the elapsed time shown on a later return).
        self.finalize_stale_compaction_lines();
        if !self
            .state
            .switch_to_session(session_id.to_string(), &self.session_store)
        {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("Switch session".into()),
                message: "This session is already open in another cosh process.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return false;
        }
        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
        self.rehydrate_right_panel();
        self.session_view.hovered_msg_idx = None;
        self.title_generated = true;
        // The OLD session's split slots stop and clear here: running pane
        // turns are session-bound (their stop signals were armed for the
        // old session's fan-out), so switching away must not leave them
        // streaming. This releases the UI slots, NOT the state — the pane
        // set and bindings live on in each session's own metadata. The
        // epoch moves with the teardown: their terminal Done arrives stale
        // and is dropped instead of binding old-session ACP ids into the
        // NEW session's metadata under a same-named pane.
        self.agents_pane_epoch = self.agents_pane_epoch.wrapping_add(1);
        for split in self
            .agents_split
            .take()
            .iter_mut()
            .chain(self.parked_agents_split.take().iter_mut())
        {
            for pane in &mut split.panes {
                if let Some(signal) = &pane.stop_signal {
                    signal.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                pane.running = false;
            }
        }
        // The session's mirrors leave the shared list with its slots (a
        // switch rebuilds the NEW session's mirrors from its metadata; Home
        // has no session, so none come back).
        self.state.right_panel.drop_agents_pane_windows();
        // Restore the NEW session's `/agents` mode. ONLY a session left
        // with the mode OPEN (`agents_mode_open`) takes the screen back; a
        // session with a parked set (pane list retained, flag false) gets
        // its manager rebuilt into the PARKED slot instead, so its later
        // `/agents` re-attach still works after the round trip.
        // `set_agents_split` is deliberately NOT used here: it persists
        // through the CURRENT (new) session and would wipe a parked pane
        // set with a `None` delta. Focus stays untouched either way (a
        // sidebar switch must leave the prompt UNfocused — an automatic
        // restore never drives it).
        match self
            .state
            .current_session()
            .map(|s| (s.agents_panes.clone(), s.agents_mode_open))
        {
            Some((Some(panes), true)) => {
                self.agents_split = Some(
                    crate::routes::session::agents_split::AgentsSplitState::new(
                        panes.clone(),
                    ),
                );
                self.save_agents_metadata(Some(panes), true);
                // A session left with the mode open carries `Bound` panes
                // whose timelines died with the process — rehydrate them.
                self.trigger_agents_pane_replays();
            }
            Some((Some(panes), false)) => {
                self.parked_agents_split = Some(
                    crate::routes::session::agents_split::AgentsSplitState::new(panes),
                );
            }
            _ => {}
        }
        // The NEW session's rebuilt slots mirror into the shared list the
        // same way a fresh open does (idle panes, no running turns — the
        // old session's turns were stopped above).
        self.sync_agents_pane_mirrors();
        // The shared spinner follows the same rule `set_agents_split`
        // applies: drop it only when no split owns the screen and no NORMAL
        // agent loop is running behind the switch.
        if self.agents_split.is_none()
            && self.state.status != crate::types::SessionStatus::Working
        {
            self.agent_spinner = None;
        }
        // Same cleanup, now against the NEWLY selected session.
        self.finalize_stale_compaction_lines();
        true
    }

    /// Open the rename dialog for the current session, prefilled with its
    /// title (opencode-style prompt: edit in place, Enter applies, Esc
    /// cancels). No-op without a session.
    pub(super) fn open_rename_dialog(&mut self) {
        let Some(session) = self.state.current_session() else {
            return;
        };
        let input = session.title.clone();
        let cursor_pos = input.len();
        self.dialog
            .show(DialogType::RenameSession { input, cursor_pos });
    }

    /// Create a fresh empty session and select it — the shared path behind
    /// Home's "New session" (keyboard + mouse) and the `/new` slash command.
    /// Selecting the session flips the app into Session mode.
    pub(crate) fn start_new_session(&mut self) {
        // Entry funnel: claim the id (unique + exclusively locked for this
        // process) BEFORE the session becomes selectable. A created-but-
        // unlocked session let a second cosh process adopt it via the
        // sidebar switch — the per-session open-lock only refuses when a
        // claim already exists, so a fresh session had none to refuse.
        let Some((id, now_ms)) = self.session_store.claim_fresh_session_id() else {
            self.toast_state.show(crate::ui::toast::ToastOptions {
                title: Some("New session".into()),
                message: "Could not create the session: the id claim failed.".into(),
                variant: crate::ui::toast::ToastVariant::Error,
                duration_ms: 4000,
            });
            return;
        };
        let title = format_session_timestamp(now_ms);
        // Hand off the PREVIOUS session's open-lock before the new one
        // becomes current: a process keeps a claim only while the session
        // is its selected one. Without this, a process that opened session
        // X and then ran /new kept holding X's lock until it died — every
        // other cosh process trying to open X was refused with "already
        // open in another process" while nothing here was using it.
        self.persist_and_release_current_session();
        self.state.add_empty_session(id.clone(), title, now_ms);
        self.state.current_session_id = Some(id);
        // A fresh session NEVER inherits the previous one's /agents split:
        // its persisted pane set is `None`, so the mode closes here (the
        // persist call is a no-op — the fresh session already records None).
        self.close_agents_split();
        // Fresh session: the header's cost widget starts clean — no
        // balance carried over from the previous session.
        self.hypercredit_balance = None;
        // A fresh session starts with a CLEAN right panel: the panel lives
        // on AppState (not inside the Session model), so without this reset
        // the previous session's todos, PTY/subagent sessions, panel focus,
        // scroll offsets and history-navigation state would leak into the
        // new session — the same reset every other session-switch path
        // (sidebar keyboard/mouse switch, Esc back to Home) performs.
        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
        // A new session is restored with the globally persisted model (the
        // last one the user selected), recorded on the session so its history
        // carries it as metadata deltas.
        self.apply_global_model();
        self.title_generated = false;
        self.prompt_view.focus();
    }

    /// `/background` toggle: repaint the TUI's base background with the
    /// terminal default (`Color::Reset`) instead of the theme color. Panels,
    /// elements and menus keep their theme colors.
    ///
    /// Flips the alpha of the current background IN PLACE: every theme
    /// mutation path (`App::set_theme`, the startup loader) preserves the RGB
    /// channels and only zeroes alpha while the toggle is on, so the pristine
    /// color is always recoverable locally — no registry/name lookup that
    /// could drift from what is actually on screen.
    pub(super) fn toggle_transparent_background(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.transparent_background = !self.transparent_background;
        let mut theme = self.theme.clone();
        let (r, g, b, _) = theme.background.to_ints();
        theme.background = if self.transparent_background {
            RGBA::from_ints(r, g, b, 0)
        } else {
            RGBA::from_ints(r, g, b, 255)
        };
        self.theme = theme;
        self.config.theme_gen += 1;
        self.setup.appearance.transparent_background = self.transparent_background;
        self.setup.save();
        let message = if self.transparent_background {
            "Now using the terminal's default background."
        } else {
            "Theme background restored."
        };
        self.toast_state.show(ToastOptions {
            title: Some("Background".into()),
            message: message.into(),
            variant: ToastVariant::Info,
            duration_ms: 3000,
        });
    }

    /// Pre-mark the picker rows from the session's persisted `/agents` pane
    /// set (each distinct agent marks its row, instances included) and seed
    /// each row's count input with the persisted instance multiplicity. A
    /// picker freshly OPENED always resets marks first (the stale ones from
    /// a canceled session must not leak), then the persisted set re-marks.
    /// No session or no persisted set → plain empty picker.
    fn pre_mark_agents_picker(&mut self) {
        // build the occurrence list BEFORE open() wipes the marks.
        let persisted = self
            .state
            .current_session()
            .and_then(|s| s.agents_panes.clone())
            .unwrap_or_default();
        self.agents_picker.open();
        if persisted.is_empty() {
            return;
        }
        // Occurrence multiplicity per agent in the persisted set: how many
        // panes each agent name carries. A plain HashMap is enough — the
        // lookup below goes by name, no order to preserve.
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in &persisted {
            *counts.entry(entry.agent().to_string()).or_insert(0) += 1;
        }
        for (agent, count) in counts {
            if let Some(entry) = self
                .agents_picker
                .entries
                .iter_mut()
                .find(|entry| entry.name == agent)
            {
                entry.selected = true;
                // Persisted multiplicities clamp to the current ceiling: a
                // set built under a LARGER previous MAX_INSTANCES must not
                // pre-fill an input the picker would reject.
                let count = count.min(crate::ui::agents_picker::MAX_INSTANCES);
                if count != crate::ui::agents_picker::DEFAULT_INSTANCES {
                    entry.instances = count.to_string();
                }
            }
        }
    }

    pub(super) fn run_slash_command(&mut self, cmd: &crate::ui::slash_menu::SlashCommand) {
        if cmd.name == "themes" {
            self.open_theme_dialog();
        } else if cmd.name == "models" {
            self.open_model_dialog();
        } else if cmd.name == "agents" {
            // Refuse while the normal agent loop is working (mirrors /new):
            // the split would replace the live transcript mid-stream, and
            // the submit path would fall into the queue-choice dialog
            // instead of ever reaching the fan-out.
            if self.state.status == crate::types::SessionStatus::Working {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("Agents".into()),
                    message: "The agent is working — wait for it to finish.".into(),
                    variant: ToastVariant::Warning,
                    duration_ms: 4000,
                });
                self.slash_menu.visible = false;
                return;
            }
            // Re-entry contract: a PARKED split (Esc'd out with the agent set
            // kept) re-attaches verbatim — the picker step is skipped
            // entirely because the user already picked and the panes keep
            // their streaming turns, drafts and bound sessions.
            if !self.attach_parked_agents_split() {
                // Fresh entry (or a previously DISMISSED manager): open the
                // picker from scratch, but pre-mark the rows the session's
                // persisted pane set carries, so a mode the split was left
                // in re-picks as one Enter. `pre_mark_agents_picker` itself
                // opens (its build-before-open ordering needs it) — do NOT
                // open twice: a future mark-resetting `open()` would wipe
                // the pre-marks applied by the second call.
                self.pre_mark_agents_picker();
            }
            self.prompt_view.clear_all();
        } else if cmd.name == "router" {
            // Same refresh the Home menu performs before opening the router:
            // fallbacks from the prefs cache; models come from the model
            // cache when the view renders.
            let saved = crate::fallback::load_fallbacks(&self.setup);
            self.router_view.set_fallbacks(saved);
            let saved = crate::fallback::load_prompt_corrector_fallbacks(&self.setup);
            self.router_view.set_prompt_corrector_fallbacks(saved);
            self.show_router = true;
        } else if cmd.name == "providers" {
            self.show_add_provider = true;
        } else if cmd.name == "settings" {
            self.show_settings = true;
        } else if cmd.name == "tools" {
            self.show_internal_tools = true;
        } else if cmd.name == "toolcall" {
            self.open_tool_call_dialog();
        } else if cmd.name == "compact" {
            self.start_manual_compaction();
        } else if cmd.name == "export" {
            self.export_transcript();
        } else if cmd.name == "new" {
            if self.state.status == crate::types::SessionStatus::Idle {
                self.start_new_session();
            } else {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("New session".into()),
                    message: "The agent is working — wait for it to finish.".into(),
                    variant: ToastVariant::Warning,
                    duration_ms: 4000,
                });
                self.slash_menu.visible = false;
                return;
            }
        } else if cmd.name == "rename" {
            self.open_rename_dialog();
        } else if cmd.name == "undo" {
            self.open_undo_dialog();
        } else if cmd.name == "background" {
            self.toggle_transparent_background();
        } else if cmd.name == "bell" {
            self.bell_enabled = !self.bell_enabled;
            use crate::ui::toast::{ToastOptions, ToastVariant};
            let state = if self.bell_enabled {
                "enabled"
            } else {
                "disabled"
            };
            self.toast_state.show(ToastOptions {
                title: Some("Bell".into()),
                message: format!("Completion bell {state}."),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            self.setup.appearance.bell_enabled = self.bell_enabled;
            self.setup.save();
        } else if cmd.name == "anim" {
            self.anim_enabled = !self.anim_enabled;
            use crate::ui::toast::{ToastOptions, ToastVariant};
            let message = if self.anim_enabled {
                "Animated logo enabled."
            } else {
                "Animated logo disabled — using static LOGO_CHAT."
            };
            self.toast_state.show(ToastOptions {
                title: Some("Anim".into()),
                message: message.into(),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            self.setup.appearance.anim_enabled = self.anim_enabled;
            self.setup.save();
        } else {
            let cmd_name = format!("/{} ", cmd.name);
            self.prompt_view.set_draft(cmd_name);
        }
        self.slash_menu.visible = false;
    }

    /// User-triggered `/export`: write the CURRENT agent-visible transcript
    /// of the active session to `{cwd}/{session-name}-{uuid}.md`. The export
    /// replays the persisted context records through the context manager, so
    /// only what the agent can see right now (visibility boundary, hidden
    /// items, checkpoint coverage, masked results) reaches the Markdown file.
    ///
    /// The whole load-replay-render-write pipeline runs on a worker thread:
    /// replaying a long JSONL history on the UI thread would freeze the TUI
    /// for the duration. The outcome arrives as a toast through the harness
    /// event channel, the same path the session store's lock notifications
    /// already use.
    pub(super) fn export_transcript(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let Some(session) = self.state.current_session().cloned() else {
            self.toast_state.show(ToastOptions {
                title: Some("Export".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let store = self.session_store.clone();
        let cwd = std::path::PathBuf::from(&self.state.working_directory);
        // The export is a snapshot of the PERSISTED records: a running loop's
        // in-flight turn has not landed on disk yet. Say so on the toast
        // instead of letting the user assume the live tail is in the file.
        let still_working = self.state.status != crate::types::SessionStatus::Idle;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result =
                crate::transcript_export::export_session_transcript(&store, &session, &cwd);
            let (message, variant) = match result {
                Ok(path) => {
                    let mut message = format!("Export: transcript written to {}.", path.display());
                    if still_working {
                        message.push_str(
                            " (snapshot as of the last persisted turn; the agent is still working)",
                        );
                    }
                    (message, cosh::harness::events::ToastVariant::Info)
                }
                Err(error) => {
                    log::warn!("transcript export failed: {error}");
                    (
                        format!("Export: could not write the transcript: {error}"),
                        cosh::harness::events::ToastVariant::Warning,
                    )
                }
            };
            let _ = event_tx.send(HarnessEvent::Toast { message, variant });
        });
    }

    /// User-triggered `/compact`: run the LLM summary NOW instead of waiting
    /// for the 80% trigger. Refusals are surfaced as toasts; the work runs on
    /// a one-off tokio task that rebuilds a Harness from the context records
    /// persisted in the session JSONL — between loops no harness exists, and
    /// while a loop runs its context manager is untouchable, so both cases
    /// refuse.
    pub(super) fn start_manual_compaction(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        fn refuse(app: &mut App, message: String) {
            app.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message,
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
        }
        if self.manual_compaction_active {
            refuse(self, "A compaction is already running.".into());
            return;
        }
        if self.state.status != crate::types::SessionStatus::Idle {
            refuse(
                self,
                "The agent is working — /compact runs between messages.".into(),
            );
            return;
        }
        let Some(id) = self.state.current_session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let Some(ctx_state) = self.session_store.load_context(&id) else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "Nothing to compact yet.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };

        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let base_url = self.base_url_for(&provider);
        let summarization_models = self.manual_summarization_models();
        let local_base_urls = self.configured_local_base_urls();
        let anthropic_ttl = self.setup.anthropic_cache_ttl_1h();
        let retention = self.setup.openai_cache_retention().map(String::from);
        let reasoning = self.llm_config.reasoning.clone();
        let cwd = self.state.working_directory.clone();
        // Clear the shared stop flag BEFORE handing it to the compaction
        // task: the flag belongs to the PREVIOUS interaction (ESC/Interrupt),
        // and every summarizer entry races it before connecting — a flag
        // still set from an earlier turn aborts the manual pass at its
        // pre-connect select, retrying /compact changes nothing, and the
        // mechanism only comes back after an app reboot. Same contract the
        // agent-loop start path already honours (`store(false)` in
        // `start_agent_loop`).
        self.stop_signal.store(false, Ordering::Relaxed);
        let stop_signal = self.stop_signal.clone();
        let event_tx = self.event_tx.clone();
        self.manual_compaction_active = true;
        self.tokio_handle.spawn(async move {
            use cosh::harness::{Harness, ManualCompactionOutcome};
            let base = if !summarization_models.is_empty() {
                summarization_models
                    .iter()
                    .find_map(|(provider, model)| {
                        if model.is_empty() || model == "auto" || provider.is_empty() {
                            return None;
                        }
                        cosh_sdk::connector::Connector::new(provider)
                            .ok()
                            .map(|mut connector| {
                                connector = connector.with_model(model);
                                if let Some(url) = local_base_urls.get(provider) {
                                    connector = connector.with_base_url(url.clone());
                                }
                                connector
                            })
                    })
                    .ok_or_else(|| {
                        "No configured summarization connector is available.".to_string()
                    })
            } else if model.as_deref() == Some("auto") {
                Err("Auto has no configured models.".to_string())
            } else {
                cosh_sdk::connector::Connector::new(&provider).map_err(|error| error.to_string())
            };
            let outcome = match base {
                Ok(mut connector) => {
                    if summarization_models.is_empty() {
                        if let Some(ref m) = model {
                            connector = connector.with_model(m);
                        }
                        if let Some(ref url) = base_url {
                            connector = connector.with_base_url(url.clone());
                        }
                        if let Some(effort) = reasoning {
                            connector = connector.with_reasoning_effort(effort);
                        }
                    }
                    connector = connector
                        .with_prompt_cache_ttl_1h(anthropic_ttl)
                        .with_prompt_cache_key(&id)
                        // Same session-affinity routing as the main loop:
                        // compaction requests carry the session's opaque
                        // hash so they reach the same cache-warm backend.
                        .with_session_id(super::session_affinity_id(&id));
                    if let Some(retention) = retention {
                        connector = connector.with_prompt_cache_retention(retention);
                    }
                    let mut harness =
                        Harness::new(connector, &cwd, std::collections::HashSet::new())
                            .with_summarization_models(summarization_models)
                            .with_local_base_urls(local_base_urls);
                    harness.context_manager.restore_state(&ctx_state);
                    harness.compact_on_demand(&event_tx, stop_signal).await
                }
                Err(e) => {
                    let _ = event_tx.send(HarnessEvent::Error {
                        message: format!("connector: {e}"),
                        context: None,
                        checkup_verdict: None,
                    });
                    ManualCompactionOutcome::Failed
                }
            };
            let _ = event_tx.send(HarnessEvent::CompactOnDemand { outcome });
        });
    }
}
