use cosh::harness::HarnessEvent;
use cosh_tui::core::lib::rgba::RGBA;
use std::sync::atomic::Ordering;

use super::App;
use super::BUG_REPORT_URL;
use super::agents_dispatch::either_split;
use crate::session::store::format_session_timestamp;
use crate::ui::agents_picker::AgentPick;
use crate::ui::dialogs::DialogType;

impl App {
    /// The Command mode is HIDDEN while the /agents split owns the screen:
    /// there the shared prompt is intercepted (the focused pane, or the
    /// main-agent manager loop when unfocused — `dispatch_to_agents_split`),
    /// so the plain-terminal shell mode would never run and its prompt
    /// indicator would only mislead. Only the Command mode is hidden; Build,
    /// Ask and Yolo keep cycling (they map onto real agent-loop behaviors
    /// that also apply to the manager loop).
    ///
    /// Called from EVERY entry point that puts a split on the screen
    /// (`open_agents_split` and `attach_parked_agents_split`): a session
    /// persisted in Command mode can only re-enter the split through one of
    /// them, so the invariant "no Command mode while a split is open" holds
    /// without per-site checks. The demotion also updates the persisted
    /// preference (same write the Tab cycle does) so an app restart inside
    /// the split does not restore the hidden mode.
    pub(super) fn hide_command_mode_for_split(&mut self) {
        if self.state.mode == cosh::harness::Mode::Command {
            self.state.mode = cosh::harness::Mode::Build;
            self.setup.set_mode_selection(self.state.mode);
        }
    }

    /// Enter the `/agents` split-pane mode with the agents the picker
    /// confirmed, in the order they were marked. Each pick carries its own
    /// instance count: the pane list receives `instances` copies of the
    /// agent so two Claude panes are two INDEPENDENT sessions (the
    /// per-pane binding lands in task "binding"). The `cosh` manager pane is
    /// NOT inserted automatically: the Cosh Manager belongs to the user's
    /// own split choice: only an explicitly marked `cosh` entry creates
    /// its block. The MAIN agent needs no pane: `Alt+0` unfocuses every
    /// pane and the shared prompt targets the normal-mode loop (running
    /// as manager while the split is open). Opening the split does NOT spawn any session — panes
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
            let mut persisted_occurrence: std::collections::HashMap<
                (String, usize),
                &crate::types::AgentPaneRef,
            > = std::collections::HashMap::new();
            for entry in &persisted {
                let k = seen.entry(entry.agent().to_string()).or_insert(0);
                persisted_occurrence.insert((entry.agent().to_string(), *k), entry);
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
                    .and_then(|entry| {
                        entry
                            .session_id()
                            .map(|session_id| crate::types::AgentPaneRef::Bound {
                                agent: name.clone(),
                                session_id: session_id.to_string(),
                                worktree: entry.worktree().cloned(),
                            })
                    });
                *k += 1;
                if let Some(bound) = bound {
                    *fresh = bound;
                }
            }
        }
        self.set_agents_split(Some(panes));
        self.hide_command_mode_for_split();
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
        // The MAIN-AGENT mirror belongs to the LIVE split screen only: with
        // the mode parked the user is back in the normal session view (the
        // transcript IS the main agent's view), so the mirror leaves with
        // the mode and its projection resets.
        self.close_main_mirror();
        // Metadata: mode closed, pane set + bindings (and the hidden stack)
        // retained for the pre-marked re-entry. Synchronous: same quit-path
        // rationale as set_agents_split.
        let (panes, hidden) = self
            .state
            .current_session()
            .map(|s| (s.agents_panes.clone(), s.agents_hidden.clone()))
            .unwrap_or_default();
        self.save_agents_metadata(panes, false, hidden);
        // The registry follows the panes into the PARKED slot unchanged:
        // the split keeps serving `agents_call` with the mode closed (the
        // main agent coordinates while the user works the normal view).
        self.sync_agents_pane_registry();
        // The mirrors follow the panes into the parked slot: the stream
        // keeps landing in the shared list (same routing as live panes).
        self.sync_agents_pane_mirrors();
        // The slash menu follows the panes into the PARKED slot: no live
        // split serves the screen, so the cosh set returns (a parked
        // split's stale `focused` must never hijack the menu — see
        // `sync_slash_source`, which only trusts the LIVE split).
        super::agents_dispatch::sync_slash_source(self);
        self.prompt_view.focus();
    }

    /// Re-attach the parked split (the `/agents` re-entry): the SAME pane
    /// objects return to the screen — running turns surface mid-stream,
    /// timelines and bound sessions are exactly where the user left them.
    /// Returns `true` when a parked split took the screen (the caller then
    /// skips opening the picker).
    pub(super) fn attach_parked_agents_split(&mut self) -> bool {
        // A PARKED split with NO panes is not a re-attachable state: it is
        // the legacy shape an emptied split persisted before the auto-close
        // fix (`agents_panes = Some([])`). Re-attaching it verbatim would
        // put an EMPTY split screen up (a chat-like nothing) — the exact
        // symptom this guards against. Drop it and let the caller open the
        // PICKER instead: the user sees the initial-state list, never an
        // empty mode.
        if self
            .parked_agents_split
            .as_ref()
            .is_some_and(|s| s.panes.is_empty())
        {
            self.parked_agents_split = None;
        }
        if let Some(split) = self.parked_agents_split.take() {
            self.agents_split = Some(split);
            let (panes, hidden) = self
                .state
                .current_session()
                .map(|s| (s.agents_panes.clone(), s.agents_hidden.clone()))
                .unwrap_or_default();
            self.save_agents_metadata(panes, true, hidden);
            // The registry follows the panes back onto the screen (same
            // set it parked with — the sync is a re-affirmation, matching
            // the mirrors' one below).
            self.sync_agents_pane_registry();
            // A parked set rebuilt from metadata (session switch or app
            // restart) carries bound panes with empty timelines — the
            // rehydrate passes fire only now that the panes take the
            // screen (a parked set is invisible; replaying into it would
            // burn a harness boot for nothing).
            self.trigger_agents_pane_replays();
            // The mirrors are already in place (the parked slot kept them
            // alive); one sync re-affirms their state for the re-open.
            self.sync_agents_pane_mirrors();
            // The manager panes' own model selections hydrate from their
            // bound sessions' markers (one disk read per cosh pane): a pick
            // made in a previous run shows on the pane's prompt badge the
            // moment the mode re-attaches, not only on its next turn.
            self.hydrate_manager_pane_model_selections();
            // The re-attached pane set carries its focus (pane 0 on a
            // rebuilt one): the slash menu's source follows it — agent
            // list when the pane was already probed, spinner/cosh
            // otherwise, and a fresh probe seed when the cache is empty.
            super::agents_dispatch::ensure_pane_snapshot(self, 0);
            super::agents_dispatch::sync_slash_source(self);
            self.hide_command_mode_for_split();
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
        // The MAIN-AGENT mirror leaves with the mode too (its projection
        // resets; the next open starts fresh).
        self.close_main_mirror();
        // The pane mirrors leave the shared subagent list with the mode:
        // they exist only while a split (live or parked) owns panes here.
        self.state.right_panel.drop_agents_pane_windows();
        self.prompt_view.focus();
    }

    /// Mirror the /agents pane set into the harness-side `agents_call`
    /// registry: each pane's registry id (the agent name, `name#k` for
    /// same-CLI duplicates; the manager pane is addressable like every
    /// other agent) becomes
    /// a [`cosh::harness::agent_coworkers::AgentCoworkerDescriptor`], with the pane's bound
    /// state as the context block reports it. Called on EVERY split
    /// lifecycle change — open, replace, park, re-attach, close — so the
    /// registry tracks the LIVE or PARKED set exactly (a parked split keeps
    /// serving `agents_call`: the main agent coordinates with the mode
    /// closed). `set_coworkers` fails queued/orphaned requests of panes that
    /// left the set, so a removed pane never strands a waiter.
    pub(super) fn sync_agents_pane_registry(&mut self) {
        // The manager (cosh) pane is the main agent itself: its bridge never
        // emits an ACP `SessionModel` report, so `session_info` stays `None`
        // for it no matter how many turns it ran — the model its turns run
        // with is the GLOBAL config (the same `llm_config` the main loop
        // reads). The fallback chain below consults it for manager panes
        // only; an `auto` pick renders as `auto` (truthful: the routing
        // chain serves the pane), an empty model renders nothing.
        let global_model = self
            .llm_config
            .model
            .clone()
            .filter(|model| !model.is_empty());
        let panes: Vec<cosh::harness::agent_coworkers::AgentCoworkerDescriptor> =
            either_split(&self.agents_split, &self.parked_agents_split)
                .map(|split| {
                    split
                        .panes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| {
                            let id = split.pane_registry_id(i)?;
                            let is_manager = p.agent
                                == crate::routes::session::agents_split::MANAGER_LABEL;
                            Some(cosh::harness::agent_coworkers::AgentCoworkerDescriptor {
                                id,
                                // The pane's IDENTITY rides the descriptor:
                                // the registry reconciles by it, so a label
                                // recycled by a sibling's removal can never
                                // vouch for a dead pane's traffic (Issue 3).
                                coworker_id: p.id,
                                agent: p.agent.clone(),
                                is_manager,
                                bound: p.session_id.is_some(),
                                // The worktree NAME (not the path) — the
                                // context block renders it next to the pane
                                // id, and `agents_call` requesters read the
                                // same listing. `/worktree` keeps this in
                                // sync right after each binding change.
                                worktree: p.worktree.as_ref().map(|w| w.name.clone()),
                                // The model the pane's session runs with —
                                // what the ACP harness reported last
                                // (`session_info`), falling back to the
                                // pane's committed `/models` pick when the
                                // harness has not reported yet. `None`
                                // before the first turn: the context block
                                // renders nothing rather than a guess.
                                // An EMPTY reported model (a harness that
                                // names its models with empty strings)
                                // renders nothing either — same contract.
                                model: p
                                    .session_info
                                    .as_ref()
                                    .and_then(|info| info.model.clone())
                                    .filter(|model| !model.is_empty())
                                    .or_else(|| {
                                        p.model_selection
                                            .as_ref()
                                            .map(|sel| sel.model.clone())
                                            .filter(|model| !model.is_empty())
                                    })
                                    // A manager pane's turns run on the
                                    // GLOBAL config (its bridge never
                                    // reports a model, and a `/models`
                                    // pick is optional) — the global model
                                    // is the truthful answer for it, even
                                    // rendered as `auto` (the routing chain
                                    // serves the pane). ACP panes keep the
                                    // no-guess contract: nothing reported,
                                    // nothing rendered.
                                    .or_else(|| is_manager.then(|| global_model.clone()).flatten()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
        cosh::harness::agent_coworkers::set_coworkers(panes);
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
        let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) else {
            return;
        };
        self.state
            .right_panel
            .prune_agents_pane_beyond(split.panes.len());
        for (pane, p) in split.panes.iter().enumerate() {
            self.state
                .right_panel
                .open_agents_pane_window(pane, &p.agent);
            // The mirror's list entry also announces the pane's worktree
            // pin: the SAME badge the pane footer paints (`⎇ name`),
            // composed into the description (rendered `<agent> <desc>`) so
            // the right panel answers "where does this pane work" without
            // opening it.
            let mirror = split.pane_mirror_label(pane);
            let label = p.worktree.as_ref().map(|w| {
                format!(
                    "{}⎇ {}",
                    mirror
                        .as_deref()
                        .map(|m| format!("{m} "))
                        .unwrap_or_default(),
                    w.name
                )
            });
            self.state.right_panel.sync_agents_pane(
                pane,
                &p.activity,
                // A rehydrating pane spins too: the mirror is the visible
                // signal that the bound history is coming back (the pane
                // itself may be empty until the replay lands).
                p.running || p.rehydrating,
                label.as_deref().or(mirror.as_deref()),
            );
        }
    }

    /// Seal one pane's mirror at turn end: the mirror inherits the pane's
    /// ALREADY-CLOSED timeline (`SubagentActivity::close_turn` — the report
    /// is its LAST chronological entry) and stops spinning. Resolves the
    /// live-or-parked split like the event routing does.
    pub(super) fn finish_agents_pane_mirror(&mut self, pane: usize, failed: bool) {
        let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) else {
            return;
        };
        let Some(p) = split.panes.get(pane) else {
            return;
        };
        // The pane closed its turn first (the report is the timeline's LAST
        // entry); the mirror only inherits that closed timeline and flips
        // its marker — never a separate report body (the read-order bug).
        self.state
            .right_panel
            .finish_agents_pane(pane, &p.activity, failed);
    }

    /// Fire cold-restore replay passes for every pane that needs one: a
    /// `Bound` pane with an EMPTY live timeline (the post-reboot state —
    /// the process died with the app, only the session id persisted).
    /// Rehydration streams the agent's own `session/load` history back
    /// into the pane (see `dispatch_agents_pane_replay`); a pane with
    /// content already (live turn, prior replay) and unbound `Name` panes
    /// are skipped.
    pub(super) fn trigger_agents_pane_replays(&mut self) {
        // Absorb the gate-free binding facts FIRST: a fresh confirm that
        // re-picks a pane whose first turn just ended (the Done dropped by
        // the handler's gates) must promote the pane to `Bound` BEFORE the
        // empty-timeline scan below decides it needs a replay — the
        // freshly absorbed session is exactly what the replay rehydrates.
        self.absorb_pane_session_bindings();
        let needs = self
            .agents_split
            .as_ref()
            .map(|split| {
                split
                    .panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.session_id.is_some() && p.activity.timeline.is_empty() && !p.rehydrating
                    })
                    .map(|(i, _)| i)
                    .collect::<Vec<usize>>()
            })
            .unwrap_or_default();
        for pane in needs {
            self.dispatch_agents_pane_replay(pane);
        }
    }

    /// Fail the IN-FLIGHT `agents_call` of every OUTGOING pane that still
    /// owns a turn (the reconcile below keeps the surviving panes and
    /// signal-stops only the dropped ones): a dropped pane's terminal
    /// `AgentsPaneDone` finds no pane to land in — its registry `serving`
    /// entry would otherwise outlive the turn forever, keeping
    /// `has_outstanding_requests` true and hanging the requester loop's
    /// busy-guard (the loop waits at its natural-completion point for an
    /// answer that can never arrive). Kept panes are untouched: their
    /// turns keep running and their own terminal Done resolves the
    /// request normally (failing them would kill a turn that just kept
    /// streaming — the old behavior that cancelled every running agent on
    /// any pane-set change). Teardown (no fresh set) fails everything.
    fn fail_outgoing_pane_requests(&self) {
        let ids: Vec<String> = either_split(&self.agents_split, &self.parked_agents_split)
            .map(|split| {
                split
                    .panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.running || p.stop_signal.is_some())
                    .filter_map(|(i, _)| split.pane_registry_id(i))
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            cosh::harness::agent_coworkers::fail_serving(
                &id,
                "the pane's set was replaced or torn down".into(),
            );
        }
    }

    /// The point-of-no-return variant: EVERY pane of the outgoing split
    /// (running or idle) loses its in-flight AND queued `agents_call`
    /// requests, and the pane leaves the registry. For a whole-set death —
    /// a session switch, going Home — the outgoing panes will never serve
    /// anything again in this process: a leftover queued request would
    /// hang its requester's busy-guard forever (`has_outstanding_requests`
    /// stuck true with nobody left to dispatch it — the harness loop's
    /// pre-Done wait spins eternally), and a request whose id COLLIDES
    /// with the incoming session's panes (same agent name) would be served
    /// cross-session. Must run BEFORE the slots swap, keyed by the
    /// outgoing ids; the caller re-syncs the registry to the incoming set
    /// afterwards.
    fn fail_whole_outgoing_pane_set(&self) {
        let ids: Vec<String> = either_split(&self.agents_split, &self.parked_agents_split)
            .map(|split| {
                split
                    .panes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, _)| split.pane_registry_id(i))
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            cosh::harness::agent_coworkers::fail_coworker(
                &id,
                "the session switched away — this pane's set is gone".into(),
            );
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
        // The DROPPED panes' in-flight `agents_call` requests must fail
        // NOW, keyed by their ids — after the swap their terminal Done
        // finds no pane to land in and could never resolve the request (a
        // stale id may even survive into the new set, where a blind
        // completion could hit the WRONG turn's request). Kept panes'
        // requests stay in flight: their turns keep running.
        self.fail_outgoing_pane_requests();
        // The split slots' generation moves on EVERY replacement or teardown
        // (see the `agents_pane_epoch` field doc). The pane-id routing (see
        // the `AgentsPaneEvent::pane_id` doc) makes this gate secondary —
        // events route by their pane's identity first — but it still cuts
        // off every UNSTAMPED sender's delivery against a replaced set.
        self.agents_pane_epoch = self.agents_pane_epoch.wrapping_add(1);
        // RECONCILE, not rebuild: a fresh confirm that keeps some of the
        // current panes (the common "add N panes while agents are running"
        // case) carries those pane OBJECTS over — timeline, running turn,
        // stop signal, bound session, scroll state all survive.
        // `reconciled_from` signal-stops only the panes the new set
        // dropped. The rebuild-from-scratch this used to do killed every
        // running turn on ANY pane-set change: the stop signal fired, the
        // streams died, and the fresh panes painted empty chat — the
        // split looked reset while the user only meant to add panes.
        if let Some(agents) = &panes
            && let Some(old) = self.agents_split.take()
        {
            self.agents_split = Some(
                crate::routes::session::agents_split::AgentsSplitState::reconciled_from(
                    old, agents,
                ),
            );
        } else if let Some(old) = &mut self.agents_split {
            // Teardown (`None`): every pane's turn stops with the split.
            // A discarded stop signal is one nobody will ever set again —
            // its turn would run to completion, un-stoppable, and its late
            // events would land in whatever set later shares the indexes.
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
        // Only a teardown (or a replace with NO prior live split) still
        // builds a fresh state here; a replace reconciled the surviving
        // panes into the slot above.
        if self.agents_split.is_none() {
            self.agents_split = panes
                .clone()
                .map(crate::routes::session::agents_split::AgentsSplitState::new);
        }
        let mode_open = self.agents_split.is_some();
        // The harness-side registry follows the pane set on EVERY change
        // (open, replace, close) — the `agents_call` tool's addressable ids
        // and the context block are rebuilt from it.
        self.sync_agents_pane_registry();
        // A fresh confirm or a teardown REDEFINES the pane set, so any
        // previously hidden panes are superseded: the hidden stack clears
        // with it (park/attach keep the stack — they only toggle the mode).
        self.save_agents_metadata(panes, mode_open, Vec::new());
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
            // The manager panes' own model selections hydrate from their
            // bound sessions' markers (one disk read per cosh pane): a pick
            // made in a previous run shows on the pane's prompt badge the
            // moment the split takes the screen, not only on its next turn.
            self.hydrate_manager_pane_model_selections();
        }
        // Drop the split's spinner only when no NORMAL agent loop is still
        // running behind it (the double-scenario): a Working loop keeps its
        // indicator; an idle app must not inherit a stale spinner.
        if self.agents_split.is_none() && self.state.status != crate::types::SessionStatus::Working
        {
            self.agent_spinner = None;
        }
        // The slash menu's source follows the pane set on EVERY change
        // (open, replace, close): a fresh confirm leaves pane 0 focused, a
        // teardown restores the cosh set. Without this the menu would keep
        // serving a dead set's command list (and a fresh confirm would
        // leave the cosh list routing into the pane — see
        // `route_agent_slash_command`, which trusts the menu's source).
        if mode_open {
            super::agents_dispatch::ensure_pane_snapshot(self, 0);
        }
        super::agents_dispatch::sync_slash_source(self);
    }

    /// Record the session's `/agents` bookkeeping — the visible pane set
    /// (with its bindings), the LIFO stack of HIDDEN panes, AND whether the
    /// split-screen mode itself is open — and persist through the store's
    /// normal diff pipeline (`diff_session` emits a `MetadataDelta::Agents`
    /// only when the persisted pane set, hidden stack or mode actually
    /// changed). All three fields are written together so the parked state
    /// (`mode_open = false`, pane set retained) is durable the same way the
    /// open state is. SYNCHRONOUS on purpose: open/close is often chained
    /// with quitting the app, and the async writer's queue has no flush on
    /// the quit path — a fire-and-forget save could never land, reopening
    /// the session with the wrong mode. The job is one tiny metadata delta,
    /// so blocking the frame is negligible. No session open (Home) → nothing.
    pub(in crate::app) fn save_agents_metadata(
        &mut self,
        panes: Option<Vec<crate::types::AgentPaneRef>>,
        mode_open: bool,
        hidden: Vec<crate::types::HiddenPane>,
    ) {
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        if session.agents_panes == panes
            && session.agents_mode_open == mode_open
            && session.agents_hidden == hidden
        {
            return;
        }
        session.agents_panes = panes;
        session.agents_mode_open = mode_open;
        session.agents_hidden = hidden;
        let session = session.clone();
        self.session_store.save_session(&session);
    }

    /// Hide the FOCUSED `/agents` pane (Backspace on the focused pane): move
    /// it out of the visible split into the session's LIFO hidden stack
    /// WITHOUT deleting anything. Its running turn is stopped (its stream
    /// ends with its slot) and its bound agent session stays on disk — the
    /// hidden stack keeps the ref, so a later Ctrl+Z rebuilds the pane and
    /// rehydrates its transcript from the agent's own replay.
    pub(super) fn hide_focused_agents_pane(&mut self) {
        // The visible set must exist to record the hide against it.
        if self
            .state
            .current_session()
            .and_then(|s| s.agents_panes.as_ref())
            .is_none()
        {
            return;
        }
        let Some(split) = self.agents_split.as_mut() else {
            return;
        };
        let Some(index) = split.focused else {
            return;
        };
        let Some(pane) = split.remove_pane(index) else {
            return;
        };
        // Hiding the LAST visible pane ends the mode: the split screen has
        // nothing left to show, so the main chat returns — the natural
        // close-the-last-pane behavior — instead of an empty grid.
        let emptied = split.panes.is_empty();
        let pane_ref = pane.to_ref();
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let Some(panes) = session.agents_panes.as_mut() else {
            return;
        };
        if index < panes.len() {
            panes.remove(index);
        }
        // The LIVE pane object id rides the hidden entry: a turn still
        // running at hide time finishes later, and its gate-free binding
        // fact (`record_session_binding`, absorbed by
        // `absorb_pane_session_bindings`) is routed to this entry BY this
        // id. Without it the fact would answer to nothing and the first
        // turn's session would never be bound (issue 1, variant A).
        session.agents_hidden.push(crate::types::HiddenPane {
            pane: pane_ref,
            index,
            pane_id: pane.id,
        });
        if emptied {
            // The metadata follows the screen: the mode is CLOSED and the
            // visible set is GONE (`None`, not an empty list — an empty
            // `Some` would be rebuilt as a paneless split by the session
            // restore/`/agents` re-entry paths, an empty chat-like screen
            // instead of the picker). The HIDDEN STACK stays — Ctrl+Z can
            // still bring the pane back (it re-opens the split).
            session.agents_mode_open = false;
            session.agents_panes = None;
        }
        self.save_current_agents_metadata();
        // The registry drops the pane (its `agents_call` id no longer
        // resolves) and the mirror follows it out of the shared list.
        self.sync_agents_pane_registry();
        self.sync_agents_pane_mirrors();
        if emptied {
            // The leave-the-mode epilogue (the same one park/close run):
            // the empty live state is dropped, the main-agent mirror leaves
            // with the screen, the pane mirrors drop with the mode, an idle
            // app loses the stale spinner and the slash menu serves the
            // cosh set again. The HIDDEN STACK is untouched — restore rides
            // `restore_last_hidden_agents_pane`.
            self.agents_split = None;
            self.close_main_mirror();
            self.state.right_panel.drop_agents_pane_windows();
            if self.state.status != crate::types::SessionStatus::Working {
                self.agent_spinner = None;
            }
        }
        super::agents_dispatch::sync_slash_source(self);
    }

    /// Restore the most recently HIDDEN `/agents` pane (Ctrl+Z): pop the
    /// session's hidden stack and re-insert the pane at (or as close as
    /// possible to) its old position. A bound pane comes back with an empty
    /// live timeline and rehydrates from its agent session. No-op when the
    /// stack is empty or no split owns the screen.
    pub(super) fn restore_last_hidden_agents_pane(&mut self) {
        // A split whose every visible pane was hidden AUTO-CLOSED (the
        // `hide_focused_agents_pane` emptied branch): the mode is off and
        // the persisted set is `None` — the restore RE-OPENS the split
        // fresh (empty set, no persisted bindings to inherit), the popped
        // pane joins it below. Nothing to restore against (no hidden
        // stack): inert, in a normal session too.
        if self.agents_split.is_none() {
            let ready = self
                .state
                .current_session()
                .is_some_and(|s| !s.agents_hidden.is_empty());
            if !ready {
                return;
            }
            self.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
                Vec::new(),
            ));
            if let Some(session) = self.state.current_session_mut() {
                session.agents_mode_open = true;
                session.agents_panes = Some(Vec::new());
            }
            self.hide_command_mode_for_split();
            self.prompt_view.focus();
        }
        let ready = self
            .state
            .current_session()
            .is_some_and(|s| s.agents_panes.is_some() && !s.agents_hidden.is_empty());
        if !ready {
            return;
        }
        let Some(hidden) = self
            .state
            .current_session_mut()
            .and_then(|s| s.agents_hidden.pop())
        else {
            return;
        };
        let index = {
            let split = self.agents_split.as_mut().expect("split checked above");
            let index = hidden.index.min(split.panes.len());
            split.insert_pane(
                index,
                crate::routes::session::agents_split::AgentPane::from_ref(&hidden.pane),
            );
            index
        };
        if let Some(session) = self.state.current_session_mut()
            && let Some(panes) = session.agents_panes.as_mut()
        {
            let at = index.min(panes.len());
            panes.insert(at, hidden.pane);
        }
        self.save_current_agents_metadata();
        self.sync_agents_pane_registry();
        self.sync_agents_pane_mirrors();
        // A bound pane returns with no live timeline — rehydrate it; the
        // focus already sits on it, so the slash menu's source follows.
        self.trigger_agents_pane_replays();
        // The restored pane's own model selection hydrates from its bound
        // session's markers (a hidden manager pane shows its pick on the
        // prompt badge immediately, like every other rebuild path).
        self.hydrate_manager_pane_model_selections();
        super::agents_dispatch::ensure_pane_snapshot(self, index);
        super::agents_dispatch::sync_slash_source(self);
    }

    /// Persist the CURRENT session (its `/agents` pane set, hidden stack and
    /// mode flag already mutated in place) through the store's normal diff
    /// pipeline. Used by the hide/restore paths, which change the metadata
    /// directly rather than through the open/close helpers.
    pub(super) fn save_current_agents_metadata(&mut self) {
        let Some(session) = self.state.current_session() else {
            return;
        };
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
        let pane_state = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .map(|p| (p.agent.clone(), p.worktree.clone(), p.session_id.clone()));
        let Some((agent_name, live_worktree, live_session)) = pane_state else {
            return;
        };
        // Seed the LIVE pane's next resume id — the single write site for
        // the pane object's session binding. The Done handler seeds early
        // as well ONLY because its queue service may dispatch a queued
        // message in the same frame (before this helper runs); every other
        // promotion — the gate-free absorb pass included — lands HERE, so
        // there is exactly one live-pane write outside that pre-dispatch
        // necessity. Deduped: a pane already carrying the id is untouched.
        if live_session.as_deref() != Some(session_id.as_str())
            && let Some(split) =
                super::agents_dispatch::either_split_mut(
                    &mut self.agents_split,
                    &mut self.parked_agents_split,
                )
            && let Some(p) = split.panes.get_mut(pane)
        {
            p.session_id = Some(session_id.clone());
        }
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
        // The LIVE pane's worktree wins over the persisted entry's: a pane
        // the user worktree'd BEFORE its first turn promotes from `Name`
        // (which carries no worktree) to `Bound` here — reading the stale
        // persisted field would silently drop the isolation.
        *entry = crate::types::AgentPaneRef::Bound {
            agent: agent_name,
            session_id,
            worktree: live_worktree,
        };
        // Write through the cache and sync-save: the OLD dedup here compared
        // against the ALREADY-MUTATED field and silently skipped the save,
        // so bindings never reached disk — `save_agents_metadata` is called
        // BEFORE the field was touched in its own callers; here the value is
        // already in the cache, hence the explicit save.
        let session = session.clone();
        self.session_store.save_session(&session);
        // The registry descriptor's `bound` flag follows the promotion: the
        // context block now reports this pane as a bound agent session.
        self.sync_agents_pane_registry();
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
                && crate::session::store::is_valid_session(session)
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
        // and is dropped (Home has no pane set to bind into anyway). The
        // in-flight `agents_call` requests fail first, keyed by the outgoing
        // ids — the stale-dropped Done can never resolve them (see
        // `fail_outgoing_pane_requests`).
        // A whole-set death (session switch / Home): every outgoing pane
        // loses its in-flight AND queued `agents_call` requests and leaves
        // the registry — the incoming set re-syncs it below. Without this
        // the registry keeps the old session's panes: a queued request
        // nobody will ever dispatch hangs the requester's busy-guard
        // forever, and a colliding id (same agent name) would be served by
        // the NEW session's pane — cross-session injection.
        self.fail_whole_outgoing_pane_set();
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
        // The registry follows the outgoing set out: it must stay EMPTY at
        // Home (no session, no panes) — `has_coworkers()` false means the next
        // harness build carries no `agents_call` tool and no stale panes
        // context block, and `has_outstanding_requests` can no longer stick
        // true with nobody left to dispatch a queued request.
        self.sync_agents_pane_registry();
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
        // NEW session's metadata under a same-named pane. The in-flight
        // `agents_call` requests fail first, keyed by the outgoing ids —
        // the stale-dropped Done can never resolve them (see
        // `fail_outgoing_pane_requests`).
        // A whole-set death (session switch / Home): every outgoing pane
        // loses its in-flight AND queued `agents_call` requests and leaves
        // the registry — the incoming set re-syncs it below. Without this
        // the registry keeps the old session's panes: a queued request
        // nobody will ever dispatch hangs the requester's busy-guard
        // forever, and a colliding id (same agent name) would be served by
        // the NEW session's pane — cross-session injection.
        self.fail_whole_outgoing_pane_set();
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
        match self.state.current_session().map(|s| {
            (
                s.agents_panes.clone(),
                s.agents_mode_open,
                s.agents_hidden.clone(),
            )
        }) {
            Some((Some(panes), true, hidden)) => {
                self.agents_split = Some(
                    crate::routes::session::agents_split::AgentsSplitState::new(panes.clone()),
                );
                self.save_agents_metadata(Some(panes), true, hidden);
                // A session left with the mode open carries `Bound` panes
                // whose timelines died with the process — rehydrate them.
                self.trigger_agents_pane_replays();
                // The manager panes' own model selections hydrate from
                // their bound sessions' markers (one disk read per cosh
                // pane): a pick made in a previous run shows on the pane's
                // prompt badge the moment the session takes the screen,
                // not only on its next turn.
                self.hydrate_manager_pane_model_selections();
            }
            Some((Some(panes), false, _)) => {
                // An EMPTY persisted set is the legacy emptied-split shape
                // (pre-auto-close): it is NOT a parkable set — a rebuilt
                // parked split would hold no panes and a later re-attach
                // would put an empty split screen up. Skip the park: the
                // session comes back to the NORMAL chat, a later `/agents`
                // opens the picker (the initial state).
                if !panes.is_empty() {
                    self.parked_agents_split = Some(
                        crate::routes::session::agents_split::AgentsSplitState::new(panes),
                    );
                }
            }
            _ => {}
        }
        // The registry follows the NEW session's rebuilt slots (the missing
        // half of the fix above): the whole-set failure emptied it, so a
        // session restored WITH panes re-registers them and a session with
        // none leaves the registry empty — `has_coworkers()` false, no
        // `agents_call` tool on the next harness build, no stale context
        // block, and `has_outstanding_requests` can no longer stick true.
        self.sync_agents_pane_registry();
        // The NEW session's rebuilt slots mirror into the shared list the
        // same way a fresh open does (idle panes, no running turns — the
        // old session's turns were stopped above).
        self.sync_agents_pane_mirrors();
        // The shared spinner follows the same rule `set_agents_split`
        // applies: drop it only when no split owns the screen and no NORMAL
        // agent loop is running behind the switch.
        if self.agents_split.is_none() && self.state.status != crate::types::SessionStatus::Working
        {
            self.agent_spinner = None;
        }
        // Same cleanup, now against the NEWLY selected session.
        self.finalize_stale_compaction_lines();
        // The slash menu follows the rebuilt pane set: a session restored
        // with the mode open leaves pane 0 focused (agent list, fresh probe
        // seed), every other outcome returns the cosh set. Without this a
        // stale agent list from the OLD session would keep serving the
        // menu into the new one.
        if self.agents_split.is_some() {
            super::agents_dispatch::ensure_pane_snapshot(self, 0);
        }
        super::agents_dispatch::sync_slash_source(self);
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
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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

    /// Execute one slash command from the menu, routed by the command's
    /// DECLARED SCOPE (`slash_menu::COMMAND_INVENTORY`) and, for an
    /// Agent-scoped one, by the focused `/agents` pane:
    ///
    /// - the split's router runs FIRST; it consumes the command when the
    ///   focused pane owns it (an ACP pane's own command, its config box, or
    ///   `/worktree` on any pane kind) and refuses Global-scoped ones
    ///   outright, so a `/background` picked while a claude pane holds the
    ///   focus still toggles THIS terminal's background — never the pane's;
    /// - everything else is handled here, by the App, on the app-level state
    ///   the main agent reads (which is exactly what the manager pane wants,
    ///   since its agent IS this app's own loop).
    ///
    /// Scope, not the menu's incidental `source`, decides: the source only
    /// says which list the user was LOOKING at, while the scope says who the
    /// command belongs to.
    pub(super) fn run_slash_command(&mut self, cmd: &crate::ui::slash_menu::SlashCommand) {
        if super::agents_dispatch::route_agent_slash_command(self, cmd) {
            self.slash_menu.visible = false;
            self.prompt_view.clear_all();
            return;
        }
        // `/worktree` for the MAIN agent (normal chat, no `/agents` split).
        // A cosh-owned command (`CommandScope::Agent`), so it is handled
        // HERE and never dispatched to an agent as prose. Reached only when
        // the pane router above declined — i.e. no live split, or no pane
        // focused — which is exactly the main agent's situation. The
        // argument tail is read from the prompt before it is cleared.
        if cmd.name == "worktree" {
            let tail = crate::ui::slash_menu::SlashMenu::command_arg_tail(
                &self.prompt_view.input,
                "worktree",
            );
            super::agents_dispatch::apply_main_agent_worktree_command(self, tail.as_deref());
            self.slash_menu.visible = false;
            self.prompt_view.clear_all();
            return;
        }
        if cmd.name == "themes" {
            self.open_theme_dialog();
        } else if cmd.name == "models" {
            self.open_model_dialog();
        } else if cmd.name == "agents" {
            // Openable ANY time — including while the main loop streams:
            // the split is an overlay of the SAME session, the loop keeps
            // running behind it (its spinner, queues and stop wiring are
            // untouched), and Esc out of the split PARKS it back without
            // interrupting anything. The old refusal (mirroring /new) is
            // gone: watching an agent mid-run or delegating to an idle one
            // while the main agent works is the point of the mode.
            // Re-entry contract: a PARKED split (Esc'd out with the agent set
            // kept) re-attaches verbatim — the picker step is skipped
            // entirely because the user already picked and the panes keep
            // their streaming turns, timelines and bound sessions.
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
        } else if cmd.name == "spinner" {
            self.open_spinner_dialog();
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
        // The export writes NEXT TO THE SESSION FILE the user thinks of as
        // "this conversation", not next to the agent's checkout: it stays on
        // the startup cwd even when the main agent is pinned to a worktree
        // (a `/worktree` must never relocate the user's artifacts).
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
        // Compaction only SUMMARIZES the transcript; it reads no project
        // file, so it keeps the startup cwd rather than following the
        // worktree binding (a summarizer has no business in a checkout).
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
            use cosh::harness::ManualCompactionOutcome;
            let outcome = match build_manual_compaction_harness(
                &provider,
                model.as_deref(),
                base_url,
                summarization_models,
                local_base_urls,
                anthropic_ttl,
                retention,
                reasoning,
                &id,
                &cwd,
            ) {
                Ok(mut harness) => {
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

/// The ONE compaction-harness builder, shared by the main `/compact`
/// and the `agents_compact` pane path: resolve the summarization route
/// (the user's summarization models when configured, else the primary
/// route — `auto` has no models of its own), apply the session cache
/// headers keyed to `cache_key`, and return the EMPTY-context harness.
/// The caller restores the context it wants compacted — the mechanism
/// (`compact_on_demand`) is identical either way; only the context and
/// the cache affinity differ.
///
/// Free function (not an `App` method) on purpose: the pane path
/// (`agents_dispatch`) reuses it too, and it touches no `App` state —
/// it only builds the harness the caller drives `compact_on_demand`
/// with.
pub(super) fn build_manual_compaction_harness(
    provider: &str,
    model: Option<&str>,
    base_url: Option<String>,
    summarization_models: Vec<(String, String)>,
    local_base_urls: std::collections::HashMap<String, String>,
    anthropic_ttl: bool,
    retention: Option<String>,
    reasoning: Option<String>,
    cache_key: &str,
    cwd: &str,
) -> Result<cosh::harness::Harness, String> {
    let connector = if !summarization_models.is_empty() {
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
            .ok_or_else(|| "No configured summarization connector is available.".to_string())?
    } else if model == Some("auto") {
        return Err("Auto has no configured models.".to_string());
    } else {
        let mut connector =
            cosh_sdk::connector::Connector::new(provider).map_err(|error| error.to_string())?;
        if let Some(m) = model {
            connector = connector.with_model(m);
        }
        if let Some(url) = base_url {
            connector = connector.with_base_url(url);
        }
        if let Some(effort) = reasoning {
            connector = connector.with_reasoning_effort(effort);
        }
        connector
    };
    let mut connector = connector
        .with_prompt_cache_ttl_1h(anthropic_ttl)
        .with_prompt_cache_key(cache_key)
        // Same session-affinity routing as the main loop: compaction
        // requests carry the session's opaque hash so they reach the
        // same cache-warm backend.
        .with_session_id(super::session_affinity_id(cache_key));
    if let Some(retention) = retention {
        connector = connector.with_prompt_cache_retention(retention);
    }
    Ok(cosh::harness::Harness::new(
        connector,
        cwd,
        std::collections::HashSet::new(),
    )
    .with_summarization_models(summarization_models)
    .with_local_base_urls(local_base_urls))
}

