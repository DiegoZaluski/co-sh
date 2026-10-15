//! Dispatch for the `/agents` split-pane mode: one submitted prompt goes to
//! the FOCUSED pane — or, when `Ctrl+0` left NO pane focused, to the MAIN
//! agent itself (the normal-mode loop, running as manager while the split
//! is open). External agent CLIs each get an independent ACP turn
//! (`cosh_tools::subagent::acp::call` — the same one-shot contract the prompt
//! corrector uses); each cosh pane (the app's own manager pane, at any split
//! position) runs the MAIN agent itself
//! as the session's MANAGER on its own real cosh session — persisted in the
//! session store (visible in the history panel) with the
//! `INSTRUCTIONS_MANAGER` system prompt, and without the file-writing tools
//! a coordinator never needs.
//!
//! Routing identity: each pane owns its event channel, so pane INDEX
//! replaces the `call_ref` the harness tool path would provide. Events
//! reach the TUI through the shared `HarnessEvent` channel as
//! `AgentsPaneEvent` (live stream) / `AgentsPaneDone` (final report).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use cosh::harness::events::HarnessEvent;
use cosh_sdk::connector::{Connector, ToolCallMode};
use cosh_tools::subagent::events::SubagentEvent;

use super::App;
use crate::routes::session::agents_split::{AgentsSplitState, DAEMON_LABEL};
use cosh::setup::SkillsConfig;
use crate::ui::toast::{ToastOptions, ToastVariant};

/// Sync the slash menu's command source to the split's focus: a FOCUSED
/// pane serves its own (blocklist-filtered) command list — or its spinner
/// row while the snapshot probe is still in flight — and NO focus returns
/// the cosh set (the main agent's commands). Only the LIVE split counts:
/// a parked split's stale `focused` must not hijack the menu (its prompt
/// routing goes to the main agent). Idempotent by content: an unchanged
/// source keeps the menu's selection, so this is safe to call on every
/// focus change and probe arrival.
pub(super) fn sync_slash_source(app: &mut App) {
    let next = app
        .agents_split
        .as_ref()
        .and_then(|split| split.slash_source());
    match next {
        Some((commands, loading)) => {
            if !slash_lists_equal(&commands, &app.slash_menu.commands) {
                app.slash_menu.set_agent_commands(commands);
            }
            app.slash_menu.set_loading(loading);
        }
        None => {
            let cosh = crate::ui::slash_menu::SlashMenu::cosh_commands();
            if !slash_lists_equal(&cosh, &app.slash_menu.commands) {
                app.slash_menu.set_cosh_commands();
            }
            app.slash_menu.set_loading(false);
        }
    }
}

/// Menu-source equality by (name, desc) pairs — a cheap change detector so
/// repeated syncs never reset the user's selection/filter position.
fn slash_lists_equal(
    a: &[crate::ui::slash_menu::SlashCommand],
    b: &[crate::ui::slash_menu::SlashCommand],
) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.name == y.name && x.desc == y.desc)
}

/// Route a slash command picked while a LIVE split pane holds the focus:
/// the command belongs to the AGENT, not to cosh. Returns `true` when
/// consumed (the caller closes the menu and clears the draft).
///
/// - A mapped group with a config target (models / provider / mode) opens
///   the standardized config box bound to the pane's own ACP
///   config option — the agent's exact command name never leaves the menu,
///   only what opens afterwards is cosh's.
/// - Everything else (compact, review, custom harness commands …) is sent
///   to the pane VERBATIM as its next prompt (`/name`) — the harness
///   interprets its own command; no per-agent maintenance.
///
/// Unfocused, the daemon pane focused, or no live split: `false` — the
/// command falls through to the cosh command set.
pub(super) fn route_agent_slash_command(
    app: &mut App,
    cmd: &crate::ui::slash_menu::SlashCommand,
) -> bool {
    let Some(split) = app.agents_split.as_ref() else {
        return false;
    };
    let Some(focused) = split.focused else {
        return false;
    };
    let Some(pane) = split.panes.get(focused) else {
        return false;
    };
    if pane.agent == DAEMON_LABEL {
        return false;
    }
    if let Some(target) = crate::routes::session::agent_command_map::resolve(&cmd.name)
        .and_then(|group| group.target)
    {
        app.open_agent_config_dialog(target);
        return true;
    }
    // Direct passthrough: the command goes to the pane as its prompt.
    app.dispatch_to_agents_split(format!("/{}", cmd.name));
    true
}

/// Focus arrived on pane `pane` (hotkey or click): when the pane has never
/// collected its agent's command/config snapshot, spawn the on-demand ACP
/// probe (`session_snapshot` — resume + observe, never prompts) and flag
/// the slash menu's spinner until the result lands. The daemon pane is
/// skipped: it is cosh itself, and the cosh command set already serves the
/// menu (its slash commands are the TUI's own, not an ACP publication).
/// The LIVE split only — a parked pane cannot take focus.
pub(super) fn ensure_pane_snapshot(app: &mut App, pane: usize) {
    let Some(split) = app.agents_split.as_mut() else {
        return;
    };
    let Some(p) = split.panes.get_mut(pane) else {
        return;
    };
    if p.agent == DAEMON_LABEL || p.commands.is_some() || p.snapshot_loading {
        return;
    }
    let agent = p.agent.clone();
    let session_id = p.session_id.clone();
    p.snapshot_loading = true;
    if split.focused == Some(pane) {
        app.slash_menu.set_agent_commands(Vec::new());
        app.slash_menu.set_loading(true);
    }
    let cwd = PathBuf::from(&app.state.working_directory);
    let event_tx = app.event_tx.clone();
    let epoch = app.agents_pane_epoch;
    app.tokio_handle.spawn(async move {
        let result =
            cosh_tools::subagent::acp::session_snapshot(&agent, session_id, cwd).await;
        let (commands, options) = match result {
            Ok(snapshot) => (snapshot.commands, snapshot.config_options),
            Err(err) => {
                let _ = event_tx.send(HarnessEvent::Toast {
                    message: format!("Command list for {agent} unavailable: {err}"),
                    variant: cosh::harness::events::ToastVariant::Info,
                });
                (Vec::new(), Vec::new())
            }
        };
        let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
            pane,
            epoch,
            event: SubagentEvent::Commands { commands },
        });
        let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
            pane,
            epoch,
            event: SubagentEvent::ConfigOptions { options },
        });
    });
}

/// The manager tool set: a base disabled set PLUS the workspace-mutating
/// tools a coordinator never touches. The single constructor behind BOTH
/// manager loops — the MAIN loop's manager mode (`start_agent_loop`,
/// based on the user's own disabled tools) and the cosh-pane manager turn
/// below (based on an empty set: the pane is a fresh harness without user
/// preferences) — so the two managers can never drift apart.
pub(super) fn manager_disabled_tools(user_disabled: &HashSet<String>) -> HashSet<String> {
    user_disabled
        .iter()
        .cloned()
        .chain(MANAGER_BLOCKED_TOOLS.iter().map(|tool| (*tool).to_string()))
        .collect()
}
/// Tools no manager loop ever sees: neither a cosh-pane manager turn nor
/// the MAIN agent while the `/agents` split is open. The manager reads,
/// searches, runs commands and coordinates its panes — it must not edit
/// the workspace or mutate anything outside the pane coordination:
/// - `fs_write`, `fs_edit`, `fs_edit_lines`, `fs_ast_edit`, `fs_rollback`
///   — workspace writes;
/// - `lsp_rename`, `lsp_restart`, `lsp_code_actions` — the mutating half
///   of the language-server tools (navigation/hover stay);
/// - `computer_act`, `computer_control` — desktop control (screenshots
///   and snapshots stay);
/// - `ask_questions` — needs a human in the loop; the manager is headless;
/// - `stop_agent_loop` — the manager's ONLY deliverable is its final text
///   report (the harness's own sub-agent blocklist).
pub(super) const MANAGER_BLOCKED_TOOLS: &[&str] = &[
    "fs_write",
    "fs_edit",
    "fs_edit_lines",
    "fs_ast_edit",
    "fs_rollback",
    "lsp_rename",
    "lsp_restart",
    "lsp_code_actions",
    "computer_act",
    "computer_control",
    "ask_questions",
    "stop_agent_loop",
];

/// The pane set, live or parked (live preferred): the split keeps
/// serving turns with the mode closed, so every pane reader resolves
/// whichever slot currently owns the panes.
///
/// Free functions — not methods — on purpose: the borrow covers only the
/// two slots, while callers routinely hold the split across mutations of
/// the rest of `App` (e.g. the right-panel mirrors).
pub(super) fn either_split<'a>(
    live: &'a Option<AgentsSplitState>,
    parked: &'a Option<AgentsSplitState>,
) -> Option<&'a AgentsSplitState> {
    live.as_ref().or(parked.as_ref())
}

/// Mutable half of [`either_split`].
pub(super) fn either_split_mut<'a>(
    live: &'a mut Option<AgentsSplitState>,
    parked: &'a mut Option<AgentsSplitState>,
) -> Option<&'a mut AgentsSplitState> {
    live.as_mut().or(parked.as_mut())
}

impl App {
    /// Refresh the slash menu from its input — ALWAYS after re-deriving the
    /// source. Every site that used to call `slash_menu.update(...)` directly
    /// routes through here instead: with the menu open, the source must be
    /// re-derived against the CURRENT split state on EVERY recalculation, not
    /// only on focus changes. This closes the leak class where a stale source
    /// (the split opened/closed/parked between two menu updates) let the cosh
    /// command list render over a focused agent pane — or an agent's list over
    /// the main agent — and a pick then executed against the WRONG harness.
    pub(in crate::app) fn slash_menu_refresh(&mut self) {
        sync_slash_source(self);
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// Force-close ONE pane's turn (issue 5 recovery): the pane goes idle,
    /// its timeline closes with a `[turn cancelled]` marker carrying
    /// `reason`, its in-flight `agents_call` request fails (the requester's
    /// busy-guard unblocks), and the shared mirror seals as failed. A real
    /// terminal Done from a turn still winding down arrives against an IDLE
    /// pane and is dropped by the handler's pane-idle guard — no double
    /// closure, no re-binding, no double-completed request.
    ///
    /// Recovery must never depend on the turn thread answering: the stop
    /// signal is stored first (best-effort graceful cancel — ACP sends
    /// `session/cancel`, the manager loop breaks out), then the state is
    /// closed unconditionally. This is the only path back for a pane whose
    /// thread died or whose provider stalled silently.
    fn force_close_pane(&mut self, pane: usize, reason: &str) {
        // The registry request fails keyed by the pane's id, read BEFORE the
        // exclusive state borrow below.
        let registry_id = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.pane_registry_id(pane));
        let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        else {
            return;
        };
        let Some(p) = split.panes.get_mut(pane) else {
            return;
        };
        if !p.running {
            return;
        }
        if let Some(signal) = &p.stop_signal {
            signal.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        p.running = false;
        p.last_activity = None;
        p.stop_signal = None;
        split.finish_turn(pane, reason, "cancelled");
        if let Some(id) = registry_id {
            cosh::harness::panes::fail_serving(&id, reason.to_string());
        }
        self.finish_agents_pane_mirror(pane, true);
    }

    /// Ctrl+X on the split (issue 5 recovery affordance): force-close the
    /// FOCUSED pane's turn. With split-Esc park-only, a pane whose stream
    /// stalled silently had no keyboard path out — the pane sat `running`
    /// forever and the dispatch gate refused every new prompt. Ctrl+X is
    /// that path: the pane goes idle immediately and accepts prompts again.
    /// An idle focused pane (or no pane focused — the MAIN agent, which
    /// plain Esc interrupts in the normal view) is a no-op.
    pub(super) fn interrupt_focused_pane(&mut self) {
        let Some(focused) = self.agents_split.as_ref().and_then(|split| split.focused) else {
            return;
        };
        self.force_close_pane(focused, "interrupted by the user (Ctrl+X)");
    }

    /// Frame-tick watchdog (issue 5): force-close every pane `running` with
    /// NO event of any kind for [`PANE_IDLE_WATCHDOG`] — its turn thread
    /// died or its provider stalled without a way to report, and the pane
    /// would otherwise pin the spinner forever. Runs beside the queue
    /// service (the frame tick is the only guaranteed heartbeat) and
    /// BEFORE it, so a recovered pane can immediately take queued work.
    /// Lives on the LIVE or PARKED split alike: a parked pane's dead turn
    /// must recover too (its mirror keeps spinning otherwise).
    pub(super) fn reconcile_pane_watchdog(&mut self) {
        let stale: Vec<usize> = either_split(&self.agents_split, &self.parked_agents_split)
            .map(|split| split.stale_panes(Instant::now()))
            .unwrap_or_default();
        for pane in stale {
            self.force_close_pane(pane, "the turn stopped responding (watchdog)");
        }
    }

    /// Send a submitted prompt to the FOCUSED pane of the active split —
    /// or, when NO pane holds the focus (`Ctrl+0`), to the MAIN agent
    /// itself (the same normal-mode loop, running as manager while the
    /// split is open). The focus IS the routing decision: the digit
    /// hotkeys / a click select which agent receives the message, so a
    /// message never propagates to every pane at once. A pane starts its
    /// own turn (fresh context per turn, resumed session for external ACP
    /// agents); the shared prompt is cleared by the caller.
    pub(super) fn dispatch_to_agents_split(&mut self, msg: String) {
        let Some(split) = self.agents_split.as_ref() else {
            return;
        };
        let Some(focused) = split.focused else {
            // Unfocused (`Ctrl+0`): the message goes to the MAIN agent —
            // the normal-mode loop on the current session, which runs
            // with the manager instructions + tool set while the split
            // is open (see `start_agent_loop`).
            self.submit_prompt_message(msg);
            return;
        };
        if split
            .focused_pane()
            .is_some_and(|pane| pane.running)
        {
            // Same gate the `/new` command applies to a working loop: a
            // second dispatch while that pane's turn is live would
            // interleave two streams inside one pane. (Issue 3 made
            // split-Esc park-only — it no longer stops a pane's turn — so
            // the toast must not claim Esc stops anything.)
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "The focused agent is still working.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        if split
            .focused_pane()
            .is_some_and(|pane| pane.rehydrating)
        {
            // The pane's bound history is still arriving from the agent's
            // `session/load` replay: a prompt now would interleave with
            // the reconstruction stream (and resume the session from a
            // half-replayed state). It is a one-pass, seconds-scale wait.
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "Restoring this agent's history — try again in a moment.".into(),
                variant: ToastVariant::Info,
                duration_ms: 4000,
            });
            return;
        }
        self.dispatch_to_pane(focused, msg, false);
    }

    /// Dispatch ONE message to pane `pane` as its next turn — the shared
    /// entry point for BOTH senders: the user's submit (the focused pane,
    /// `from_agent = false`) and the `agents_call` queue service (an agent's
    /// input that waited for the pane to free, `from_agent = true`). The
    /// turn's input echoes into the pane's timeline as a FRESH entry right
    /// before the turn starts — the user's prompt with the brand rail,
    /// another agent's input with the distinct rail — so the pane shows WHO
    /// is speaking even while the turn's stream has yet to produce its
    /// first chunk.
    fn dispatch_to_pane(&mut self, pane: usize, msg: String, from_agent: bool) {
        // Snapshot everything the spawned turn needs BEFORE any effect
        // below: the agent name, resume id and the daemon's connector
        // config (only when the focused pane IS the daemon — the connector
        // resolution runs lazily so a split of ACP agents alone never
        // computes it).
        let Some((agent, resume, picks)) =
            either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.panes.get(pane))
                .map(|p| {
                    (
                        p.agent.clone(),
                        p.session_id.clone(),
                        // The user's picks from the /agents config boxes,
                        // mirrored into the pane's config cache (the box
                        // confirm writes the pick onto the cached option's
                        // current AND records the id in `picked_config_ids`).
                        // Only USER-PICKED ids ride the turn — ACP config
                        // options are per-connection state, so
                        // `apply_session_picks` re-applies them inside the
                        // turn's own connection. Untouched options keep the
                        // harness's own default, letting `acp::call`'s
                        // registry default (e.g. a working free model where
                        // the harness's default is a paid-tier login wall)
                        // still apply.
                        p.config_options
                            .as_ref()
                            .map(|opts| {
                                opts.iter()
                                    .filter(|o| {
                                        !o.current.is_empty()
                                            && p.picked_config_ids
                                                .iter()
                                                .any(|id| id == &o.id)
                                    })
                                    .map(|o| (o.id.clone(), o.current.clone()))
                                    .collect::<Vec<(String, String)>>()
                            })
                            .unwrap_or_default(),
                    )
                })
        else {
            return;
        };
        let is_daemon = agent == DAEMON_LABEL;
        // The registry id this pane answers to while THIS turn runs. For the
        // manager pane it is the id `agents_call` would use to reach it; the
        // manager harness carries it so it refuses to address itself (a
        // self-call would queue behind the very turn making it — deadlock).
        let own_registry_id = if is_daemon {
            either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.pane_registry_id(pane))
        } else {
            None
        };
        // Fallible pre-flight BEFORE any bookkeeping: minting (or reusing)
        // the manager pane's own cosh session can fail (I/O), and a refused
        // turn must leave the pane idle — never stranded `running` with a
        // spinner and a mirror spinning.
        let manager_session: Option<String> = if is_daemon {
            let Some(id) = self.ensure_manager_pane_session(pane) else {
                return;
            };
            Some(id)
        } else {
            None
        };
        // The turn's input echoes into the pane's timeline as a FRESH
        // entry right before the turn starts, so the pane shows WHO is
        // speaking even while the turn's stream has yet to produce its
        // first chunk: the user's own prompt with the brand rail, another
        // agent's queued input with the distinct rail. After the guards
        // above — a refused turn must not write to the timeline either.
        // The echo goes wherever the pane lives (live or parked).
        if let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        {
            split.echo_turn_input(pane, &msg, !from_agent);
        }
        let daemon_connector = if is_daemon {
            Some(self.daemon_connector())
        } else {
            None
        };
        let fallbacks: Vec<(String, String)> = self
            .router_view
            .fallbacks
            .iter()
            .map(|fb| (fb.provider.clone(), fb.model.clone()))
            .collect();
        let summarization_models: Vec<(String, String)> = self
            .setup
            .routing
            .summarization_models
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect();
        let local_base_urls = self.configured_local_base_urls();
        let skills_config = self.setup.skills.clone();
        // The manager session's persistence rides a CLONE: the turn thread
        // outlives the &mut self borrow.
        let store = self.session_store.clone();
        let cwd = PathBuf::from(&self.state.working_directory);
        let tokio_handle = self.tokio_handle.clone();

        // Pane bookkeeping: the pane's own stop signal (Esc stops it through
        // it) and the shared-loop spinner so the "agent loop running"
        // indicator keeps spinning while the turn streams. The signal lands
        // wherever the pane lives (a PARKED pane's turn must still be
        // stoppable when the user re-enters the mode).
        let signal = Arc::new(AtomicBool::new(false));
        if let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split) {
            split.set_running(pane, true);
            split.set_stop_signal(pane, Some(signal.clone()));
        }
        // The pane's mirror in the shared subagent list starts spinning too
        // (same running flag, pane-index routed).
        self.sync_agents_pane_mirrors();
        self.agent_activity = crate::types::AgentActivity::Working;
        self.agent_spinner = Some(crate::component::agent_spinner::AgentSpinner::new(
            crate::types::AgentActivity::Working.label(),
            &self.theme,
        ));

        let index = pane;
        // Dispatch-time split-slot generation: stamped into every event of
        // THIS turn, so a slot swap while it runs (fresh confirm, switch,
        // close) invalidates its terminal delivery — the handler drops
        // stale-epoch events instead of binding them into a replacement.
        let epoch = self.agents_pane_epoch;
        let event_tx = self.event_tx.clone();
        if is_daemon {
            // The manager pane: the MAIN agent itself, running as the
            // session's coordinator on its OWN cosh session — a real,
            // persisted session (visible in the history panel, openable
            // like any other), not a throwaway nested harness. The
            // manager's instructions tell it to delegate; its tool set
            // keeps bash/inspection but drops the file-writing tools. The
            // session was bound by the pre-flight above (a second ensure
            // would just re-read it), so this only unwraps.
            let Some(cosh_session) = manager_session else {
                return;
            };
            let context = self.session_store.load_context(&cosh_session);
            let input = msg;
            std::thread::spawn(move || {
                run_manager_turn(
                    index,
                    epoch,
                    input,
                    signal,
                    own_registry_id,
                    daemon_connector.expect("manager pane carries its connector"),
                    fallbacks,
                    summarization_models,
                    local_base_urls,
                    skills_config,
                    cosh_session,
                    context,
                    store,
                    event_tx,
                );
            });
        } else {
            // External ACP pane: one tokio task, one channel, the same
            // select! shape the prompt corrector drains with. The turn
            // resumes the pane's stored session so the agent keeps its
            // context across prompts.
            let input = msg;
            tokio_handle.spawn(async move {
                let (chunk_tx, mut chunk_rx) =
                    tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
                let call = cosh_tools::subagent::acp::call(
                    &agent,
                    &input,
                    resume,
                    &picks,
                    cwd,
                    signal,
                    chunk_tx,
                );
                tokio::pin!(call);
                let mut chunks_open = true;
                let result = loop {
                    if chunks_open {
                        tokio::select! {
                            result = &mut call => break result,
                            chunk = chunk_rx.recv() => match chunk {
                                Some(event) => {
                                    let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
                                        pane: index,
                                        epoch,
                                        event,
                                    });
                                }
                                // The channel closes only when the call
                                // future is dropped (turn stopped): keep
                                // awaiting the future for its partial
                                // output + stop reason.
                                None => chunks_open = false,
                            },
                        }
                    } else {
                        break call.await;
                    }
                };
                let (output, stop_reason, session) = match result {
                    Ok((output, stop_reason, session)) => (output, stop_reason, session),
                    Err(error) => (error, "error".to_string(), None),
                };
                let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                    pane: index,
                    epoch,
                    output,
                    stop_reason,
                    session,
                });
            });
        }
    }

    /// The wake promise (user-reported: "se o agente principal mandou uma
    /// mensagem para um agente em um pane, ele precisa ter um output no
    /// futuro"). A pane turn whose `agents_call` request was made by the
    /// MAIN loop resolves the promise here: when the main loop is NOT
    /// running (it ended before the report arrived — it may even have been
    /// stopped), the parked deliveries are drained and become the input of
    /// a FRESH main-agent turn — the main agent is woken to receive the
    /// report. A USER-initiated pane turn tracks no request
    /// (`complete_request` → `None`) and wakes nothing: the pane works in
    /// harmony with a running or idle main agent. A live main loop is left
    /// alone — it drains its deliveries at its own natural points (the
    /// per-iteration drain and the pre-Done busy-guard).
    pub(super) fn wake_main_agent_if_promised(&mut self, requester: Option<&'static str>) {
        if requester != Some(cosh::harness::panes::MAIN) {
            return;
        }
        if self.state.status == crate::types::SessionStatus::Working {
            return;
        }
        let notes: Vec<String> =
            cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN)
                .iter()
                .map(cosh::harness::panes::delivery_note)
                .collect();
        if notes.is_empty() {
            // Nothing parked (a wait-mode resolution whose receiver died —
            // the requester's turn was stopped): no promise left to keep.
            return;
        }
        let report = notes.join("\n\n");
        self.start_agent_loop(report);
    }

    /// Dispatch every queued `agents_call` request whose target pane is
    /// idle. Called from the frame tick (a request enqueued while every
    /// pane idles would otherwise wait for a pane event that never comes —
    /// the requester's busy-guard would hang) and right after a pane frees
    /// (the `AgentsPaneDone` handler). One request per pane per pass: the
    /// dispatch marks the pane running, so the rest of its queue drains on
    /// the following passes, FIFO.
    pub(super) fn serve_agents_pane_queues(&mut self) {
        let targets: Vec<(usize, String)> = {
            let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) else {
                return;
            };
            split
                .panes
                .iter()
                .enumerate()
                // A running pane serves later (the queue holds), a
                // rehydrating pane first finishes its replay. The cosh pane
                // is a normal queue target — its own turn's `agents_call` to
                // itself is rejected at the tool layer (see
                // `Harness::with_own_pane_id`), so a queued request always
                // belongs to a pane that CAN free itself.
                .filter(|(_, p)| !p.running && !p.rehydrating)
                .filter_map(|(i, _)| {
                    let id = split.pane_registry_id(i)?;
                    cosh::harness::panes::has_queued_request(&id).then_some((i, id))
                })
                .collect()
        };
        for (pane, id) in targets {
            let Some(request) = cosh::harness::panes::pop_next_request(&id) else {
                continue;
            };
            self.dispatch_to_pane(pane, request.text, true);
        }
    }

    /// The manager pane's OWN cosh session id, minted on first use: the
    /// pane is not a throwaway nested harness — it is a real session in the
    /// store (the history panel lists it from birth, and reopening it
    /// restores the manager's conversation). The claim open-locks the id,
    /// so a second process can never adopt it.
    fn ensure_manager_pane_session(&mut self, pane: usize) -> Option<String> {
        if let Some(session_id) = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.session_id.clone())
        {
            return Some(session_id);
        }
        let Some((session_id, now_ms)) = self.session_store.claim_fresh_session_id() else {
            // Every candidate failed to lock (I/O trouble): refuse the turn
            // rather than run an unpersisted manager session.
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "Could not create the manager session — try again.".into(),
                variant: ToastVariant::Error,
                duration_ms: 4000,
            });
            return None;
        };
        // Persist the empty shell synchronously so the history lists the
        // manager session even if the first turn never completes.
        self.session_store.save_session(&crate::types::Session {
            id: session_id.clone(),
            title: "agents manager".to_string(),
            messages: Vec::new(),
            created_at: now_ms,
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            agents_panes: None,
            agents_mode_open: false,
            ctx_ids: std::collections::HashMap::new(),
        });
        if let Some(pane) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
            .and_then(|split| split.panes.get_mut(pane))
        {
            pane.session_id = Some(session_id.clone());
        }
        self.state.session_summaries = self.session_store.list_sessions();
        Some(session_id)
    }

    /// Cold-restore ONE pane's bound session: drive an ACP `session/load`
    /// replay pass against a fresh harness connection and stream the
    /// replayed history into the pane's timeline through the SAME
    /// `AgentsPaneEvent` channel a live turn uses (epoch-stamped like any
    /// other pane event). Nothing is prompted; the terminal
    /// `AgentsPaneReplayed` clears the pane's `rehydrating` flag.
    ///
    /// Called from the split (re)open paths for a `Bound` pane whose
    /// timeline is empty — the post-reboot state. A failed pass (harness
    /// gone, no `loadSession` support) delivers `ok: false` and the pane
    /// stays on its empty live timeline; the next prompt falls back to
    /// `session/new` through the normal turn path.
    pub(super) fn dispatch_agents_pane_replay(&mut self, pane: usize) {
        let Some(session_id) = self
            .agents_split
            .as_ref()
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.session_id.clone())
        else {
            return;
        };
        // Re-borrow mutably to set the in-flight flag (the read above is
        // done — the session id snapshot moved out).
        let agent = self
            .agents_split
            .as_mut()
            .and_then(|split| split.panes.get_mut(pane))
            .map(|p| {
                p.rehydrating = true;
                p.agent.clone()
            });
        let Some(agent) = agent else {
            return;
        };
        // The manager pane is NOT an ACP client — its bound session is a
        // real cosh session in THIS app's store, so cold-restore reads the
        // session directly (no `session/load` round-trip): the display
        // projection replays as UserMessage / Message chunks through the
        // SAME pane-event channel, then the terminal replayed event clears
        // the in-flight flag.
        if agent == DAEMON_LABEL {
            let epoch = self.agents_pane_epoch;
            let mut events = Vec::new();
            if let Some(session) = self.session_store.load_session(&session_id) {
                for message in &session.messages {
                    for part in &message.parts {
                        // Reasoning parts replay as Thought timeline entries
                        // (dimmed, same as the live pane's thinking blocks);
                        // arrival order is preserved because the persist
                        // side wrote reasoning BEFORE the text part.
                        match part {
                            crate::types::Part::Text(text) => {
                                let event =
                                    if matches!(message.role, crate::types::MessageRole::User) {
                                        SubagentEvent::UserMessage {
                                            text: text.text.clone(),
                                        }
                                    } else {
                                        SubagentEvent::Message {
                                            text: text.text.clone(),
                                        }
                                    };
                                events.push(event);
                            }
                            crate::types::Part::Reasoning(reasoning) => {
                                events.push(SubagentEvent::Thought {
                                    text: reasoning.text.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            for event in events {
                let _ = self.event_tx.send(HarnessEvent::AgentsPaneEvent { pane, epoch, event });
            }
            if let Some(p) = self
                .agents_split
                .as_mut()
                .and_then(|split| split.panes.get_mut(pane))
            {
                p.rehydrating = false;
            }
            let _ = self.event_tx.send(HarnessEvent::AgentsPaneReplayed {
                pane,
                epoch,
                ok: true,
                error: String::new(),
            });
            return;
        }
        let cwd = PathBuf::from(&self.state.working_directory);
        let epoch = self.agents_pane_epoch;
        let event_tx = self.event_tx.clone();
        let tokio_handle = self.tokio_handle.clone();
        // The mirror follows into the rehydrating spinner.
        self.sync_agents_pane_mirrors();
        // Tests never spawn real ACP harnesses: the replay pipeline is
        // exercised by feeding `HarnessEvent`s straight into the app's
        // event channel (the same way the pane lifecycle tests drive
        // turns). The in-flight flag stays set until a synthetic
        // `AgentsPaneReplayed` arrives — which is exactly what those
        // tests assert on.
        #[cfg(not(test))]
        tokio_handle.spawn(async move {
            let (chunk_tx, mut chunk_rx) =
                tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
            let pass = cosh_tools::subagent::acp::replay(&agent, session_id, cwd, chunk_tx);
            tokio::pin!(pass);
            let mut chunks_open = true;
            let result = loop {
                if chunks_open {
                    tokio::select! {
                        result = &mut pass => break result,
                        chunk = chunk_rx.recv() => match chunk {
                            Some(event) => {
                                let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
                                    pane,
                                    epoch,
                                    event,
                                });
                            }
                            None => chunks_open = false,
                        },
                    }
                } else {
                    break pass.await;
                }
            };
            let (ok, error) = match result {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error),
            };
            let _ = event_tx.send(HarnessEvent::AgentsPaneReplayed {
                pane,
                epoch,
                ok,
                error,
            });
        });
        #[cfg(test)]
        let _ = (agent, session_id, cwd, epoch, event_tx, tokio_handle);
    }

    /// Connector for the daemon pane: the session's own route (provider +
    /// model), falling back to the router's first fallback for the symbolic
    /// `auto` — the same resolution the prompt corrector applies, minus its
    /// hard error on an unresolved route (the daemon degrades to an error
    /// report in its pane instead of blocking the whole fan-out).
    fn daemon_connector(&self) -> Result<Connector, String> {
        let provider = self.llm_config.provider.trim().to_string();
        let model = self.llm_config.model.clone().unwrap_or_default();
        let (provider, model) = if provider.is_empty() || model.is_empty() || model == "auto" {
            match self.router_view.fallbacks.first() {
                Some(fb) => (fb.provider.clone(), fb.model.clone()),
                None => return Err("no provider configured for the cosh daemon".into()),
            }
        } else {
            (provider, model)
        };
        let mut connector = Connector::new(&provider).map_err(|e| e.to_string())?;
        connector = connector.with_model(&model);
        if let Some(url) = self.base_url_for(&provider) {
            connector = connector.with_base_url(url);
        }
        connector = connector.with_tool_call_mode(ToolCallMode::Native);
        Ok(connector)
    }
}

/// One daemon-pane turn: a nested cosh harness (Yolo, sub-agent
/// instructions, delegator blocklist) whose plain event stream is bridged
/// into the SAME typed `SubagentEvent` stream the external ACP panes emit —
/// both pane flavors render identically. The final report (the nested
/// loop's closure) arrives as `AgentsPaneDone`.
#[allow(clippy::too_many_arguments)]
fn run_manager_turn(
    pane: usize,
    epoch: u64,
    input: String,
    stop_signal: Arc<AtomicBool>,
    own_registry_id: Option<String>,
    connector: Result<Connector, String>,
    fallbacks: Vec<(String, String)>,
    summarization_models: Vec<(String, String)>,
    local_base_urls: std::collections::HashMap<String, String>,
    skills_config: SkillsConfig,
    session_id: String,
    context: Option<cosh::harness::ContextManagerState>,
    store: crate::session_store::SessionStore,
    event_tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
) {
    let connector = match connector {
        Ok(connector) => connector,
        Err(error) => {
            let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                pane,
                epoch,
                output: error,
                stop_reason: "error".to_string(),
                // The manager session was already minted and its shell
                // persisted by the pre-flight — the binding must reach the
                // metadata even on a failed first turn, or a reboot would
                // rebuild the pane UNBOUND (issue 6).
                session: Some(session_id),
            });
            return;
        }
    };
    let disabled: HashSet<String> = manager_disabled_tools(&HashSet::new());
    // The panic path (outside the closure) still needs a sender: the
    // original moves into the closure below. The session id is cloned for
    // the same reason — the panic Done must carry the binding too (issue 6:
    // a panicked FIRST turn must still leave the pane bound on disk).
    let panic_tx = event_tx.clone();
    let panic_session = session_id.clone();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                    pane,
                    epoch,
                    output: format!("manager runtime: {e}"),
                    stop_reason: "error".to_string(),
                    // Same as the connector failure above: the session
                    // shell exists, the binding must persist (issue 6).
                    session: Some(session_id),
                });
                return;
            }
        };
        rt.block_on(async move {
            let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
            let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
            let (loop_tx, mut loop_rx) =
                tokio::sync::mpsc::unbounded_channel::<cosh::harness::HarnessEvent>();

            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            let mut manager = cosh::harness::Harness::new(connector, &cwd, disabled)
                .with_mode(cosh::harness::Mode::Yolo)
                // PANE instructions, not the manager's: this loop IS a pane
                // doing delegated work — its deliverable is the task report,
                // never further coordination (the replication bug: manager
                // instructions + `agents_call` made one delegation from the
                // main agent fan out to every sibling cosh pane).
                .with_instructions(cosh::harness::core::INSTRUCTIONS_PANE)
                // The manager loop's pane requests are tagged MANAGER: a
                // background pane report drains ONLY here, never into the
                // main chat loop's context. (Moot since the pane lost
                // `agents_call` — the tag routes nothing anymore — but it
                // keeps the loop outside the MAIN drain path by typing.)
                .as_manager()
                // Bind the pane id this loop runs in, so `agents_call`
                // refuses to target its own running pane (self-call
                // deadlock) while sibling cosh panes stay addressable.
                .with_own_pane_id(own_registry_id)
                // Foreground-only for `subagent_call` (gated on the
                // requester tag inside the dispatch arm): a background
                // sub-agent task spawned here would park its completion
                // notification for a loop that never drains it.
                .with_fallbacks(fallbacks)
                .with_summarization_models(summarization_models)
                .with_local_base_urls(local_base_urls)
                .with_skills(skills_config.to_skills());
            // Restore the manager session's authoritative context: the pane
            // IS a real cosh session (visible in the history panel), so its
            // model context survives across app restarts exactly like the
            // main session's does.
            if let Some(state) = context {
                manager.context_manager.restore_state(&state);
            }
            manager.format_header_context();

            // Bridge: manager-loop events → typed SubagentEvents → the pane
            // channel. Same mapping the harness's internal sub-agent uses,
            // routed by pane index instead of call_ref.
            let bridge_tx = event_tx.clone();
            let bridge = tokio::spawn(async move {
                use cosh_tools::subagent::internal::InternalEventBridge;
                let mut internal = InternalEventBridge::new();
                while let Some(event) = loop_rx.recv().await {
                    let events: Vec<SubagentEvent> = match event {
                        HarnessEvent::Token { text } => {
                            vec![InternalEventBridge::message(text)]
                        }
                        HarnessEvent::Reasoning { text } => {
                            vec![InternalEventBridge::thought(text)]
                        }
                        HarnessEvent::ToolCall { tool, input, .. } => {
                            let mut events = vec![internal.tool_call(&tool, &input)];
                            if tool == "plan_todo_write"
                                && let Some(plan) =
                                    InternalEventBridge::plan_from_todo_write(&input)
                            {
                                events.push(plan);
                            }
                            events
                        }
                        HarnessEvent::ToolResult { output, .. } => {
                            internal.tool_result(&output).into_iter().collect()
                        }
                        HarnessEvent::ToolError { error, .. } => {
                            internal.tool_error(&error).into_iter().collect()
                        }
                        HarnessEvent::ContextInfo { info } => {
                            vec![InternalEventBridge::usage(
                                info.total_tokens,
                                info.max_tokens,
                            )]
                        }
                        _ => Vec::new(),
                    };
                    for event in events {
                        let _ =
                            bridge_tx.send(HarnessEvent::AgentsPaneEvent { pane, epoch, event });
                    }
                }
            });

            manager
                .run_agent_loop(&input, loop_tx, answer_rx, perm_rx, stop_signal)
                .await;

            // The report is the loop's closure (its last post-tool-call
            // message); a stopped turn closes without one.
            manager.context_manager.close_loop();
            let final_answer = manager.context_manager.final_answer();
            // Persist the manager session BEFORE dropping the harness: the
            // context snapshot must be taken while the manager still owns
            // it. The pane is a REAL cosh session — the history panel lists
            // it and reopening it restores the conversation.
            let context_state = manager.context_manager.save_state();
            let display = manager_context_display(&context_state);
            drop(manager);
            let _ = bridge.await;
            persist_manager_session(&store, &session_id, display, context_state);

            let (output, stop_reason) = match final_answer {
                Some(report) => (report, "end_turn".to_string()),
                None => (
                    "The manager turn ended without a report (stopped or errored).".to_string(),
                    "cancelled".to_string(),
                ),
            };
            let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                pane,
                epoch,
                output,
                stop_reason,
                // The cosh pane's session id IS the bound session: carried
                // here so the Done handler seeds `pane.session_id` AND
                // promotes the persisted pane ref to `Bound { agent: cosh,
                // session_id }` — without it the binding never reached
                // disk and a reboot rebuilt the pane UNBOUND (empty chat,
                // issue 6), even though the manager session itself was
                // fully persisted in the store.
                session: Some(session_id),
            });
        });
    }));
    if outcome.is_err() {
        // A panicking turn must never leave the pane Running forever (the
        // spinner would spin with nothing behind it and Esc would have
        // nothing to stop).
        let _ = panic_tx.send(HarnessEvent::AgentsPaneDone {
            pane,
            epoch,
            output: "the manager turn panicked".to_string(),
            stop_reason: "error".to_string(),
            session: Some(panic_session),
        });
    }
}

/// Project a manager context snapshot into the DISPLAY messages the session
/// store persists: user prompts (synthetic provenance kept) and assistant
/// text/closures, each carrying its DISPLAY-ONLY reasoning (`None` for user
/// prompts and reasoning-less responses — see
/// [`cosh::harness::context::ContextItem::Assistant::reasoning`]). Tool
/// calls/results are structural context, not display prose — the pane's live
/// timeline already showed them.
fn manager_context_display(
    context: &cosh::harness::ContextManagerState,
) -> Vec<(crate::types::MessageRole, String, Option<String>)> {
    context
        .items
        .iter()
        .filter_map(|item| match item {
            cosh::harness::context::ContextItem::User { original, .. } => {
                Some((crate::types::MessageRole::User, original.clone(), None))
            }
            cosh::harness::context::ContextItem::Assistant {
                original,
                reasoning,
                ..
            } => Some((
                crate::types::MessageRole::Assistant,
                original.clone(),
                reasoning.clone(),
            )),
            cosh::harness::context::ContextItem::Closure {
                content, reasoning, ..
            } => Some((
                crate::types::MessageRole::Assistant,
                content.clone(),
                reasoning.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// Persist one manager turn: load the session shell (for `created_at`),
/// replace its display messages with the fresh projection, and enqueue the
/// save with the context snapshot. A missing shell (first turn) builds one.
fn persist_manager_session(
    store: &crate::session_store::SessionStore,
    session_id: &str,
    display: Vec<(crate::types::MessageRole, String, Option<String>)>,
    context: cosh::harness::ContextManagerState,
) {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut session = store.load_session(session_id).unwrap_or_else(|| {
        crate::types::Session {
            id: session_id.to_string(),
            title: "agents manager".to_string(),
            created_at,
            messages: Vec::new(),
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            agents_panes: None,
            agents_mode_open: false,
            ctx_ids: std::collections::HashMap::new(),
        }
    });
    session.messages = display
        .into_iter()
        .enumerate()
        .map(|(i, (role, text, reasoning))| {
            // Reasoning precedes the text, mirroring the live timeline's
            // arrival order (`push_reasoning` before the text tokens).
            // `None` reasoning emits no part, so reasoning-less turns
            // persist exactly as before (byte-identical messages).
            let mut parts = Vec::new();
            if let Some(reasoning) = reasoning.filter(|r| !r.is_empty()) {
                parts.push(crate::types::Part::Reasoning(crate::types::ReasoningPart {
                    text: reasoning,
                    collapsed: true,
                }));
            }
            parts.push(crate::types::Part::Text(crate::types::TextPart {
                text,
                synthetic: false,
            }));
            crate::types::Message {
                id: format!("msg-{i}"),
                role,
                parts,
                created_at: 0,
                agent: None,
                model: None,
            }
        })
        .collect();
    store.save_session_async_with_context(&mut session, context);
}
