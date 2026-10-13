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

use cosh::harness::events::HarnessEvent;
use cosh_sdk::connector::{Connector, ToolCallMode};
use cosh_tools::subagent::events::SubagentEvent;

use super::App;
use crate::routes::session::agents_split::{AgentsSplitState, DAEMON_LABEL};
use cosh::setup::SkillsConfig;
use crate::ui::toast::{ToastOptions, ToastVariant};

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
            // interleave two streams inside one pane. Esc stops it first.
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "The focused agent is still working — Esc stops it.".into(),
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
        let Some((agent, resume)) = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .map(|p| (p.agent.clone(), p.session_id.clone()))
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
                let call =
                    cosh_tools::subagent::acp::call(&agent, &input, resume, cwd, signal, chunk_tx);
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
                        if let crate::types::Part::Text(text) = part {
                            let event = if matches!(message.role, crate::types::MessageRole::User) {
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
                session: None,
            });
            return;
        }
    };
    let disabled: HashSet<String> = manager_disabled_tools(&HashSet::new());
    // The panic path (outside the closure) still needs a sender: the
    // original moves into the closure below.
    let panic_tx = event_tx.clone();

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
                    session: None,
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
                .with_instructions(cosh::harness::core::INSTRUCTIONS_MANAGER)
                // The manager loop's pane requests are tagged MANAGER: a
                // background pane report drains ONLY here, never into the
                // main chat loop's context.
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
                // The cosh pane's session binding is NOT an ACP session —
                // its own cosh session id lives on the pane (`pane
                // .session_id`) and is bound at creation, never here.
                session: None,
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
            session: None,
        });
    }
}

/// Project a manager context snapshot into the DISPLAY messages the session
/// store persists: user prompts (synthetic provenance kept) and assistant
/// text/closures. Tool calls/results are structural context, not display
/// prose — the pane's live timeline already showed them.
fn manager_context_display(
    context: &cosh::harness::ContextManagerState,
) -> Vec<(crate::types::MessageRole, String)> {
    context
        .items
        .iter()
        .filter_map(|item| match item {
            cosh::harness::context::ContextItem::User { original, .. } => {
                Some((crate::types::MessageRole::User, original.clone()))
            }
            cosh::harness::context::ContextItem::Assistant { original, .. } => {
                Some((crate::types::MessageRole::Assistant, original.clone()))
            }
            cosh::harness::context::ContextItem::Closure { content, .. } => {
                Some((crate::types::MessageRole::Assistant, content.clone()))
            }
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
    display: Vec<(crate::types::MessageRole, String)>,
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
        .map(|(i, (role, text))| crate::types::Message {
            id: format!("msg-{i}"),
            role,
            parts: vec![crate::types::Part::Text(crate::types::TextPart {
                text,
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        })
        .collect();
    store.save_session_async_with_context(&mut session, context);
}
