//! The internal sub-agent: a nested full harness running on its own thread,
//! bridging events to the parent's ToolOutput stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    Harness, INSTRUCTIONS_SUBAGENT, Mode, SUBAGENT_BLOCKED_TOOLS, background,
};

impl Harness {
    /// Run the INTERNAL sub-agent path of the merged `subagent_call` tool:
    /// a nested [`Harness`] that reuses this connector (same provider/model),
    /// starts with an EMPTY context (no history), runs in
    /// [`Mode::Yolo`] (approval was already asked at the parent tool gate),
    /// and persists nothing. It runs on its OWN THREAD with a dedicated
    /// current-thread runtime (the same pattern the TUI uses): a nested
    /// harness is itself a full agent loop, and awaiting it inline would
    /// make `run_agent_loop`'s future recursive (an infinitely sized type),
    /// while the harness type is not `Send` (it owns the MCP manager). Its
    /// events are bridged to the TUI as a plain `ToolOutput` stream — the
    /// tools it calls and any snapshot/summary events are dropped, so the
    /// main agent never sees its intermediate work and nothing is written
    /// to disk. Only the final report (the nested loop's `Closure`) is
    /// returned as the tool result.
    ///
    /// # Errors
    ///
    /// Returns an error when the nested harness ends WITHOUT a final text
    /// report (connector error, user stop, model never answering), when its
    /// thread panicked, or when the channel closed — the real reason is
    /// surfaced instead of a silent empty result.
    #[allow(clippy::unwrap_used)]
    pub(super) async fn run_internal_subagent(
        &mut self,
        input: String,
        is_review: bool,
        task_id: Option<String>,
        call_ref: Option<String>,
    ) -> Result<String, String> {
        // The sub-agent cannot ask the user questions, capture the live
        // terminal, or stop the loop on its own (it must end with a written
        // report — see SUBAGENT_BLOCKED_TOOLS); those tools are disabled so
        // the model never sees them.
        let mut disabled = self.disabled_tools.clone();
        disabled.extend(SUBAGENT_BLOCKED_TOOLS.iter().map(|t| t.to_string()));

        let cwd = self
            .cosh_tools
            .as_ref()
            .map(|c| c.project_root().to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        let connector = self.connector.clone();
        let fallbacks = self.fallbacks.clone();
        let summarization_models = self.summarization_models.clone();
        let local_base_urls = self.local_base_urls.clone();
        let parent_skills = self.cosh_tools.as_ref().map(|c| c.skills());
        let parent_tx = self.cosh_tools.as_ref().and_then(|c| c.event_tx());
        let stop_signal = self
            .stop_signal
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        // Clone used only to distinguish "user stopped" after the loop
        // (the original is moved into the nested harness).
        let stop_check = stop_signal.clone();

        let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // Clone used only inside the completion-recording thread: the
        // original stays owned by the caller for the spawn receipt below.
        let bg_task_id = task_id.clone();
        // Per-call routing identity: stamps the nested loop's mirrored
        // SubagentEvents AND the final report so the TUI pairs them with
        // THIS internal call's window (parallel internal calls never
        // cross-pair).
        let internal_call_ref = call_ref.clone();

        std::thread::spawn(move || {
            use std::panic::AssertUnwindSafe;

            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    // Zombie-Running guard: a background task whose runtime
                    // failed to build must be marked failed HERE — the
                    // thread returns before the normal completion path
                    // runs, and a task left Running never notifies, is
                    // never pruned, and (busy guard) blocks later
                    // same-agent spawns.
                    let error = format!("internal sub-agent runtime: {e}");
                    if let Some(id) = bg_task_id.as_ref() {
                        background::complete(id, None, Some(error.clone()));
                    }
                    let _ = result_tx.send(Err(error));
                    return;
                }
            };
            // A panic in the nested loop must not take down the parent:
            // catch it and surface it as a tool error instead.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                rt.block_on(async move {
                    let mut nested = Harness::new(connector, &cwd, disabled)
                        .with_mode(Mode::Yolo)
                        .with_instructions(INSTRUCTIONS_SUBAGENT)
                        // A nested harness runs FOREGROUND-ONLY: background
                        // sub-agent spawns are rejected (the nested loop
                        // cannot outlive its parent to deliver a
                        // notification) and completion notifications are
                        // never drained into its context — they belong to
                        // the top-level agent (see `as_nested`).
                        .as_nested()
                        .with_fallbacks(fallbacks)
                        .with_summarization_models(summarization_models)
                        .with_local_base_urls(local_base_urls)
                        // Same skill sources as the parent harness.
                        .with_skills(parent_skills.unwrap_or_default());
                    // The header (instructions + tool list) is NOT built by
                    // run_agent_loop itself — the TUI does it before every
                    // loop. Build it here so the sub-agent sees its own
                    // prompt and its own (blocklisted) tool set.
                    nested.format_header_context();

                    // Bridge: translate the nested loop's plain event stream
                    // into the SAME typed SubagentEvent stream the external
                    // ACP path emits — message and thought chunks, tool
                    // calls with their results/errors (FIFO-paired: the
                    // harness guarantees the Nth result matches the Nth
                    // call), the plan_todo_write TODO as the plan, and the
                    // context budget as the usage footer. The TUI sub-agent
                    // box renders both sub-agent flavors identically. Nested
                    // usage and explicit summarization routing notices still
                    // pass through unchanged (nested accounting); snapshots,
                    // compaction and Done are dropped; the loop's fatal
                    // Error (if any) is captured so a failure is surfaced
                    // instead of masked. No per-chunk ToolOutput mirror: the
                    // report reaches the chat through the ONE final
                    // `ToolOutput { finished: true }` below, exactly like
                    // the ACP path.
                    let (nested_tx, mut nested_rx) =
                        tokio::sync::mpsc::unbounded_channel::<super::events::HarnessEvent>();
                    let bridge_tx = parent_tx.clone();
                    // The bridge task owns its own copy: `internal_call_ref`
                    // stays available for the final report emission below.
                    let bridge_call_ref = internal_call_ref.clone();
                    let last_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                    let err_capture = last_error.clone();
                    let bridge = tokio::spawn(async move {
                        use cosh_tools::subagent::internal::InternalEventBridge;
                        let mut internal = InternalEventBridge::new();
                        while let Some(event) = nested_rx.recv().await {
                            if Harness::forward_nested_accounting(&event, bridge_tx.as_ref()) {
                                continue;
                            }
                            let subagent_events: Vec<cosh_tools::subagent::events::SubagentEvent> =
                                match event {
                                    super::events::HarnessEvent::Token { text } => {
                                        vec![InternalEventBridge::message(text)]
                                    }
                                    super::events::HarnessEvent::Reasoning { text } => {
                                        vec![InternalEventBridge::thought(text)]
                                    }
                                    super::events::HarnessEvent::ToolCall {
                                        tool, input, ..
                                    } => {
                                        let mut events = vec![internal.tool_call(&tool, &input)];
                                        if tool == "plan_todo_write"
                                            && let Some(plan) =
                                                InternalEventBridge::plan_from_todo_write(&input)
                                        {
                                            events.push(plan);
                                        }
                                        events
                                    }
                                    super::events::HarnessEvent::ToolResult { output, .. } => {
                                        internal.tool_result(&output).into_iter().collect()
                                    }
                                    super::events::HarnessEvent::ToolError { error, .. } => {
                                        internal.tool_error(&error).into_iter().collect()
                                    }
                                    super::events::HarnessEvent::ContextInfo { info } => {
                                        vec![InternalEventBridge::usage(
                                            info.total_tokens,
                                            info.max_tokens,
                                        )]
                                    }
                                    super::events::HarnessEvent::Error { message, .. } => {
                                        *err_capture.lock().unwrap() = Some(message);
                                        Vec::new()
                                    }
                                    _ => Vec::new(),
                                };
                            if let Some(tx) = &bridge_tx {
                                for event in subagent_events {
                                    let _ = tx.send(super::events::HarnessEvent::SubagentEvent {
                                        tool: "subagent_call".to_string(),
                                        call_ref: bridge_call_ref.clone(),
                                        event,
                                    });
                                }
                            }
                        }
                    });

                    nested
                        .run_agent_loop(&input, nested_tx, answer_rx, perm_rx, stop_signal)
                        .await;

                    // The nested loop already closes its own context on every
                    // normal exit; the defensive call is idempotent (a no-op
                    // when the last item is not an assistant draft). The
                    // report is the Closure content.
                    nested.context_manager.close_loop();
                    let final_answer = nested.context_manager.final_answer();
                    // Release the nested harness (and its event-tx clones) so
                    // the bridge drains every buffered event, then join it.
                    drop(nested);
                    let _ = bridge.await;

                    match final_answer {
                        Some(report) => {
                            // The severity header is MANDATORY on a
                            // completed internal review turn: a report that
                            // arrived without the first-line header gets it
                            // injected (inferred from its own findings), so
                            // the box tint works even when the sub-agent
                            // ignored the DSL. ASYMMETRY vs the external
                            // arm: the internal loop has no stop-reason
                            // signal here, so ANY `Some(final_answer)` is
                            // treated as completed — the external arm
                            // additionally requires `end_turn`. In practice
                            // a stopped internal turn closes without a
                            // final answer (final_answer is `None`), so the
                            // risk is thin; if the internal loop ever
                            // learns to emit partial answers on stop, gate
                            // this on the stop reason like tools.rs does.
                            // Failed turns (the `None` arm below) are never
                            // touched — tinting a partial or errored turn
                            // would misreport it.
                            let report = if is_review && !report.trim().is_empty() {
                                cosh_tools::subagent::severity::enforce_severity_header(
                                    &report, true,
                                )
                            } else {
                                report
                            };
                            if let Some(tx) = &parent_tx {
                                let _ = tx.send(super::events::HarnessEvent::ToolOutput {
                                    tool: "subagent_call".to_string(),
                                    output: report.clone(),
                                    finished: true,
                                    // The internal window registers as a
                                    // BLANK agent CLI (`subagent: `); the
                                    // literal `internal` name routes through
                                    // `is_window_of`.
                                    agent: Some("internal".to_string()),
                                    call_ref: internal_call_ref.clone(),
                                });
                            }
                            Ok(report)
                        }
                        None => {
                            // The loop ended WITHOUT a final text (e.g. a
                            // connector error, a user stop, or the model
                            // never writing an answer). Surface the real
                            // reason instead of silently returning an empty
                            // result — the caller must be able to adapt.
                            let reason = last_error.lock().unwrap().clone().unwrap_or_else(|| {
                                if stop_check.load(Ordering::Relaxed) {
                                    "the internal sub-agent was stopped by the user".to_string()
                                } else {
                                    "the internal sub-agent ended without producing a \
                                         final report"
                                        .to_string()
                                }
                            });
                            Err(format!("internal sub-agent failed: {reason}"))
                        }
                    }
                })
            }));
            // Background mode: the sub-agent's OWN thread records the
            // outcome in the process-wide registry — never a watcher on the
            // spawning runtime, which may be gone by the time the task
            // finishes (the TUI builds a fresh runtime per turn). The
            // completion notification is delivered by the TOP-LEVEL loop's
            // next drain (a nested loop never drains). Captured BEFORE the
            // send below: the panic payload is partially moved into the
            // message construction.
            let completion = bg_task_id.as_ref().map(|task_id| match &outcome {
                Ok(Ok(report)) => (task_id.clone(), Some(report.clone()), None),
                Ok(Err(error)) => (task_id.clone(), None, Some(error.clone())),
                Err(_) => (
                    task_id.clone(),
                    None,
                    Some("internal sub-agent thread ended without a result".to_string()),
                ),
            });
            let _ = result_tx.send(match outcome {
                Ok(report) => report,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    Err(format!("internal sub-agent panicked: {msg}"))
                }
            });
            if let Some((task_id, output, error)) = completion {
                background::complete(&task_id, output, error);
            }
        });

        // Background mode: return the spawn receipt IMMEDIATELY — the
        // caller keeps working and the report arrives later as an automated
        // completion notification (push) or via `subagent_status` (poll).
        // The receipt is enveloped as a `SubAgentCallOutput` (the shape the
        // TUI unwraps for the sub-agent box) with a HUMAN-READABLE summary:
        // a bare `{"task_id": ...}` object would leak as raw JSON in the
        // panel (the TUI's parser falls back to the raw string on unknown
        // shapes). The task_id is kept verbatim in the text for the model.
        if let Some(task_id) = task_id {
            let receipt = cosh_tools::subagent::types::SubAgentCallOutput {
                output: format!(
                    "Background sub-agent spawned (task_id: {task_id}, agent: internal). \
                     Its final report will arrive automatically as an [automated \
                     notification]; poll subagent_status for progress in the meantime."
                ),
                stop_reason: "spawned".to_string(),
            };
            return serde_json::to_string(&receipt).map_err(|e| e.to_string());
        }

        let report = result_rx
            .await
            .map_err(|_| "internal sub-agent thread ended without a result".to_string())??;
        Ok(report)
    }
}
