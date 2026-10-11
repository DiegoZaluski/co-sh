//! Dispatch for the `/agents` split-pane mode: one submitted prompt fans
//! out to EVERY pane at once. External agent CLIs each get an independent
//! ACP turn (`cosh_tools::subagent::acp::call` — the same one-shot contract
//! the prompt corrector uses); the daemon pane (index 0, the synthetic
//! `cosh` entry) gets a nested cosh harness turn, mirroring the harness's
//! internal sub-agent path (`run_internal_subagent`) minus the tools the
//! delegator must never use.
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
use crate::routes::session::agents_split::DAEMON_LABEL;
use cosh::setup::SkillsConfig;
use crate::ui::toast::{ToastOptions, ToastVariant};

/// Tools the daemon pane never sees. The delegator reads, searches, runs
/// commands and coordinates its panes — it must not edit the workspace
/// (write/edit tools) and cannot ask questions or stop the loop (the
/// harness's own sub-agent blocklist).
const DAEMON_BLOCKED_TOOLS: &[&str] = &[
    "fs_write",
    "fs_edit",
    "fs_edit_lines",
    "fs_ast_edit",
    "fs_rollback",
    "ask_questions",
    "stop_agent_loop",
];

impl App {
    /// Send a submitted prompt to the FOCUSED pane of the active split.
    /// The focus IS the routing decision: Ctrl+N / a click select which
    /// agent receives the message (the delegator daemon pane included), so
    /// a message never propagates to every pane at once. The pane starts
    /// its own turn (fresh context per turn, resumed session for external
    /// ACP agents); the shared prompt is cleared by the caller.
    pub(super) fn dispatch_to_agents_split(&mut self, msg: String) {
        let Some(split) = self.agents_split.as_ref() else {
            return;
        };
        let focused = split.focused;
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

        // Snapshot everything the spawned turn needs BEFORE the mutable
        // pane bookkeeping below: the agent name, resume id and the
        // daemon's connector config (only when the focused pane IS the
        // daemon — the connector resolution runs lazily so a split of
        // ACP agents alone never computes it).
        let Some((agent, resume)) = split
            .panes
            .get(focused)
            .map(|pane| (pane.agent.clone(), pane.session_id.clone()))
        else {
            return;
        };
        let is_daemon = agent == DAEMON_LABEL;
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
        let cwd = PathBuf::from(&self.state.working_directory);
        let tokio_handle = self.tokio_handle.clone();

        // Pane bookkeeping: the pane's own stop signal (Esc stops it through
        // it) and the shared-loop spinner so the "agent loop running"
        // indicator keeps spinning while the turn streams.
        let signal = Arc::new(AtomicBool::new(false));
        if let Some(split) = self.agents_split.as_mut() {
            split.set_running(focused, true);
            split.set_stop_signal(focused, Some(signal.clone()));
        }
        // The pane's mirror in the shared subagent list starts spinning too
        // (same running flag, pane-index routed).
        self.sync_agents_pane_mirrors();
        self.agent_activity = crate::types::AgentActivity::Working;
        self.agent_spinner = Some(crate::component::agent_spinner::AgentSpinner::new(
            crate::types::AgentActivity::Working.label(),
            &self.theme,
        ));

        let index = focused;
        // Dispatch-time split-slot generation: stamped into every event of
        // THIS turn, so a slot swap while it runs (fresh confirm, switch,
        // close) invalidates its terminal delivery — the handler drops
        // stale-epoch events instead of binding them into a replacement.
        let epoch = self.agents_pane_epoch;
        let event_tx = self.event_tx.clone();
        if is_daemon {
            // The delegator pane: a nested cosh harness on its own
            // thread runtime (the harness is not Send-safe across the
            // app's async context — same shape as the TUI's main agent
            // loop thread).
            let input = msg;
            std::thread::spawn(move || {
                run_daemon_turn(
                    index,
                    epoch,
                    input,
                    signal,
                    daemon_connector.expect("daemon pane carries its connector"),
                    fallbacks,
                    summarization_models,
                    local_base_urls,
                    skills_config,
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
            .map(|p| p.session_id.clone())
            .flatten()
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
fn run_daemon_turn(
    pane: usize,
    epoch: u64,
    input: String,
    stop_signal: Arc<AtomicBool>,
    connector: Result<Connector, String>,
    fallbacks: Vec<(String, String)>,
    summarization_models: Vec<(String, String)>,
    local_base_urls: std::collections::HashMap<String, String>,
    skills_config: SkillsConfig,
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
    let disabled: HashSet<String> = DAEMON_BLOCKED_TOOLS
        .iter()
        .map(|tool| (*tool).to_string())
        .collect();
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
                    output: format!("daemon runtime: {e}"),
                    stop_reason: "error".to_string(),
                    session: None,
                });
                return;
            }
        };
        rt.block_on(async move {
            let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
            let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
            let (nested_tx, mut nested_rx) =
                tokio::sync::mpsc::unbounded_channel::<cosh::harness::HarnessEvent>();

            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            let mut nested = cosh::harness::Harness::new(connector, &cwd, disabled)
                .with_mode(cosh::harness::Mode::Yolo)
                .with_instructions(cosh::harness::core::INSTRUCTIONS_SUBAGENT)
                // Foreground-only: the delegator cannot spawn background
                // tasks that would outlive this thread.
                .as_nested()
                .with_fallbacks(fallbacks)
                .with_summarization_models(summarization_models)
                .with_local_base_urls(local_base_urls)
                .with_skills(skills_config.to_skills());
            nested.format_header_context();

            // Bridge: nested-loop events → typed SubagentEvents → the pane
            // channel. Same mapping the harness's internal sub-agent uses,
            // routed by pane index instead of call_ref.
            let bridge_tx = event_tx.clone();
            let bridge = tokio::spawn(async move {
                use cosh_tools::subagent::internal::InternalEventBridge;
                let mut internal = InternalEventBridge::new();
                while let Some(event) = nested_rx.recv().await {
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

            nested
                .run_agent_loop(&input, nested_tx, answer_rx, perm_rx, stop_signal)
                .await;

            // The report is the nested loop's closure (its last
            // post-tool-call message); a stopped turn closes without one.
            nested.context_manager.close_loop();
            let final_answer = nested.context_manager.final_answer();
            // Release the harness so the bridge drains every buffered event.
            drop(nested);
            let _ = bridge.await;

            let (output, stop_reason) = match final_answer {
                Some(report) => (report, "end_turn".to_string()),
                None => (
                    "The daemon turn ended without a report (stopped or errored).".to_string(),
                    "cancelled".to_string(),
                ),
            };
            let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                pane,
                epoch,
                output,
                stop_reason,
                // The nested harness has no resumable ACP session.
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
            output: "the daemon turn panicked".to_string(),
            stop_reason: "error".to_string(),
            session: None,
        });
    }
}
