//! The agent loop: stream → extract → dispatch → repeat, with stop handling,
//! steering-queue injection, MCP draining and completion notes.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::time::Duration;

use cosh_sdk::connector::{Connector, resolve_reasoning_effort};

#[cfg(not(test))]
use super::discovered_context_window;
#[cfg(not(test))]
use super::context::ContextManager;
use super::{
    CONTEXT_WINDOW_MARKER, INTERRUPTED_MARKER, MAX_ITERATIONS, MAX_TOOL_RETRIES, Harness,
    LOOP_DETECTION_WINDOW_SIZE, PrefixCacheProbe, RunOutcome, StreamEvent, background,
};

impl Harness {
    /// Run the full agent loop: stream LLM response, dispatch tool calls,
    /// feed results back to the LLM, and repeat — until the model finishes
    /// without requesting tools, stop is called,
    /// or `stop_signal` is set to `true`.
    ///
    /// Events are sent through `tx` so the caller (typically the TUI) can
    /// render tokens, tool calls, and results in real time.
    ///
    /// When the LLM calls `ask_questions`, the tool is intercepted: the
    /// questions are sent to the TUI via `QuestionRequest` and the loop
    /// waits for an answer on `answer_rx`. The TUI must send back the
    /// user's answers (or an error) before the loop continues.
    ///
    /// The `stop_signal` is an external flag (usually an `Arc<AtomicBool>`)
    /// that allows the caller to interrupt the loop from another thread.
    ///
    /// `perm_rx` receives permission responses from the TUI when the user
    /// approves or denies a tool call that needs permission.
    pub async fn run_agent_loop(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        perm_rx: tokio::sync::mpsc::UnboundedReceiver<super::guardrails::PermissionAction>,
        stop_signal: Arc<AtomicBool>,
    ) {
        // Legacy entry point (used by tests and callers that never queue
        // mid-loop messages): an empty shared queue that nothing pushes into.
        self.run_agent_loop_inner(
            input,
            tx,
            answer_rx,
            perm_rx,
            stop_signal,
            super::queued_input::QueuedInput::default(),
        )
        .await;
        self.drain_mcp().await;
    }

    /// Like [`run_agent_loop`](Self::run_agent_loop) but with an additional
    /// SHARED queue the TUI can push user messages into WHILE the loop is
    /// running: every message is recorded as a regular user turn and delivered
    /// to the model before the NEXT request (the "next request" queue), and
    /// the TUI is acknowledged via [`HarnessEvent::UserMessageInjected`].
    /// The queue is shared by reference with the TUI, which keeps ownership of
    /// its entries until injection — a deleted or edited message is reflected
    /// here immediately (see [`super::queued_input::QueuedInput`]). Messages
    /// still queued when the loop ends are never injected.
    pub async fn run_agent_loop_with_queued_input(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        perm_rx: tokio::sync::mpsc::UnboundedReceiver<super::guardrails::PermissionAction>,
        stop_signal: Arc<AtomicBool>,
        queued_input: super::queued_input::QueuedInput,
    ) {
        self.run_agent_loop_inner(input, tx, answer_rx, perm_rx, stop_signal, queued_input)
            .await;
        self.drain_mcp().await;
    }

    /// Explicit MCP shutdown at agent-loop end (`disconnect_all` with a
    /// bounded cancel per client): there is no async `Drop`, so a stdio
    /// child or HTTP session would otherwise outlive the turn. The TUI
    /// builds a fresh harness per turn; reusing a harness for another loop
    /// would need `connect_mcp` again. A panic in the loop skips this (the
    /// client then dies with the harness on unwind).
    async fn drain_mcp(&mut self) {
        self.mcp.disconnect_all().await;
    }

    /// Shared implementation of [`run_agent_loop`](Self::run_agent_loop) and
    /// [`run_agent_loop_with_queued_input`](Self::run_agent_loop_with_queued_input).
    /// The shared `queued_input` queue is drained before every request; its
    /// messages are injected into the model context (see the docs of the
    /// public wrappers). The drain pops each entry right before injecting it,
    /// so a TUI-side delete/edit of a still-pending message is always honored.
    /// Sleep-poll until `outstanding()` clears or stop is requested: the
    /// shared pre-Done wait for pane reports and background tasks alike.
    async fn wait_for_outstanding(stop_signal: &AtomicBool, outstanding: impl Fn() -> bool) {
        while outstanding() && !stop_signal.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Send pre-rendered completion notes as ToolOutput events + automated
    /// user turns (shared tail of the pre-Done busy-guard waits). True
    /// when anything was injected — the caller clears the steering text
    /// and continues the loop so the model responds to them.
    fn inject_completion_notes(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        notes: Vec<(String, String, Option<String>)>,
    ) -> bool {
        if notes.is_empty() {
            return false;
        }
        for (tool, output, call_ref) in notes {
            let _ = tx.send(super::events::HarnessEvent::ToolOutput {
                tool,
                output: output.clone(),
                finished: true,
                agent: None,
                call_ref,
            });
            self.context_manager.add_user(&output);
        }
        true
    }

    async fn run_agent_loop_inner(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        mut answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        mut perm_rx: tokio::sync::mpsc::UnboundedReceiver<super::guardrails::PermissionAction>,
        stop_signal: Arc<AtomicBool>,
        queued_input: super::queued_input::QueuedInput,
    ) {
        use super::events::{HarnessEvent, ToastVariant};
        self.context_manager.set_binding_sender(tx.clone());
        use super::guardrails::{PermissionCheck, check_tool_permission};
        use cosh_tools::question::types::{QuestionInput, QuestionOutput};

        // The input lives in the context manager; `current_input` only carries
        // the harness's per-iteration steering message (empty for the first
        // iteration so the user prompt is not sent twice).
        let mut current_input = String::new();
        let mut iteration = 0u64;
        // Fresh nudge budget per agent loop: a harness reused across loops
        // must not inherit the previous turn's exhausted budget.
        self.stop_nudges_left = super::limits::STOP_NUDGE_BUDGET;

        // Whether ANY tool ran this turn. A turn with zero tool dispatches
        // is a pure text exchange — the request was answerable directly, so
        // the final text is complete by construction and the termination
        // checkup has nothing to audit (see the natural-stop gate below).
        let mut tools_ran = false;

        // Immediate LSP snapshot: the TUI's footer/tags must reflect the
        // engine state from the first moment of the turn, not only after the
        // first full cycle completes.
        let _ = tx.send(self.lsp_snapshot_event());

        // Same for MCP: the boot `connect_all` already ran, so the first
        // footer view reflects the connections.
        let _ = tx.send(self.mcp_snapshot_event());

        // Crush-style loop detection: watch the last N tool-calling
        // iterations for an identical tool+input+result signature. A
        // signature seen more than `LOOP_DETECTION_MAX_REPEATS` times stops
        // the loop before the model burns tokens up to MAX_ITERATIONS on a
        // stuck repetition.
        let mut loop_window: VecDeque<String> = VecDeque::with_capacity(LOOP_DETECTION_WINDOW_SIZE);

        // Throttle the periodic context snapshots so the TUI can persist the
        // context records incrementally without serializing the whole
        // context on every tool dispatch. The snapshot itself is emitted on
        // the agent thread (the expensive clone never touches the UI thread).
        let mut last_snapshot = std::time::Instant::now();

        // Store the stop signal so stream_chat can check it mid-stream.
        self.stop_signal = Some(stop_signal.clone());

        // Route reasoning/thinking tokens to the TUI as they stream in.
        self.reasoning_tx = Some(tx.clone());

        // Pass the event tx to CoshTools for streaming tool output (e.g. bash)
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_event_tx(tx.clone());
            // Phase 5: share the stop flag so a running `subagent_call`
            // honours ESC via an ACP `session/cancel` notification.
            cosh.set_stop_signal(stop_signal.clone());
        }

        log::debug!(
            "run_agent_loop ENTER input={:?}",
            &input[..input.floor_char_boundary(input.len().min(80))]
        );

        // Populate native tool definitions for the API so the model
        // uses the native tool-calling mechanism instead of inline JSON.
        self.init_native_tools();

        // Route token estimation to the encoding closest to the active model
        // (override or the provider's default) before any budget is computed.
        self.context_manager
            .set_model(self.connector.effective_model());

        // Size the compaction budget to the ACTIVE model's EFFECTIVE context
        // window instead of the hardcoded default: with a far larger window
        // (e.g. 300k vs the 100k default) the 80% trigger would otherwise
        // fire at a fraction of the model's actual capacity, re-triggering the
        // LLM compaction on every overflow — the "infinite summarization"
        // loop. Cached per model + timeout-bounded; on failure the current
        // budget (default or restored snapshot) is kept.
        #[cfg(not(test))]
        {
            // Remember the discovery outcome: it is the KNOWN window that
            // drives the hierarchical MapReduce contingency when the held
            // context exceeds it (see `known_checkpoint_window`). A failed
            // discovery leaves it `None` — unknown window → the split is sized
            // against the current budget.
            self.discovered_window = None;
            if let Some(window) = discovered_context_window(self.connector.effective_model()).await
            {
                self.discovered_window = Some(window);
                // Size the budget to the model's EFFECTIVE window when the
                // toggle is on, otherwise the full window — the advertised
                // capacity is a poor estimate of what the model can actually
                // reason over (the "sweet spot" sizing, see
                // `effective_context_window`). The RAW window is kept above
                // in `discovered_window`, where it still drives the
                // hierarchical MapReduce contingency.
                self.context_manager.set_max_tokens(
                    ContextManager::resolve_window(window, self.effective_context),
                );
            }
        }

        // A model switch clears a previously recorded stuck context-window
        // overflow — the new model gets a fresh chance (keyed by MODEL, so a
        // switch inside the same provider also clears it).
        self.context_manager
            .sync_model(self.connector.effective_model().unwrap_or("?"));
        self.compaction_generic_retries = 0;

        // Rehydrate before dispatch. Restoring a point with no recorded plan
        // must clear an existing tool projection, not resurrect future work.
        if let Some(cosh) = &self.cosh_tools
            && let Some(list) = self
                .context_manager
                .take_restored_todo_list()
                .or_else(|| self.context_manager.todo_list().cloned())
        {
            cosh.restore_todo_list(list);
        }
        // Mirror the tools' current TODO list into the dedicated protected
        // TODO block (a plan may already exist from a previous loop) so the
        // model sees it from the very first iteration.
        self.sync_todo_context();

        // Add the initial user input to the context manager (the single owner
        // of the conversation) so the model sees it as a proper `user` message.
        // Skip when it is already the trailing protected user turn (e.g. it was
        // loaded via with_history) — otherwise the same message is duplicated.
        if !self.context_manager.last_user_equals(input) {
            self.context_manager.add_user(input);
        }

        // A fresh input landing directly on top of a previous user turn with
        // NO output between means the earlier input was abandoned: the user
        // cancelled that run with Esc before the LLM produced anything, then
        // typed this one. Drop the abandoned turn(s) so the model never sees
        // an input the user gave up on — only the newest input stays.
        self.context_manager.remove_abandoned_inputs();

        // A failed automatic pass has already exhausted its bounded retries
        // and configured model chain. Do not renew that budget after every
        // tool dispatch while the unchanged over-trigger history remains.
        // A new user turn or manual compaction gets a fresh attempt; actual
        // provider context-window recovery below remains independent.
        let mut automatic_compaction_failed = false;

        // Apply the 80% compaction before the first LLM request, so the
        // initial context is already within budget. When the useless-chain
        // sweep still leaves the total over the trigger, the LLM compaction
        // (or the hierarchical MapReduce contingency) runs; the in-flight input
        // was folded into the summary, so it is re-added for the model to see
        // the task verbatim.
        if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction)
            || self.context_manager.compaction_staging_active()
        {
            // Select the summarizer before deciding whether its actual
            // request needs MapReduce. The main agent's window may differ.
            automatic_compaction_failed = !self.llm_compact(&tx).await;
            if !self.context_manager.last_user_equals(input) {
                self.context_manager.add_user(input);
            }
        }

        // Send initial context info so the TUI budget bar shows immediately
        // even before the first LLM call completes.
        let _ = tx.send(HarnessEvent::ContextInfo {
            info: self.context_manager.display_info(),
        });

        macro_rules! check_stop {
            () => {
                if self.stop || stop_signal.load(Ordering::Relaxed) {
                    log::debug!("run_agent_loop STOPPED");
                    // The dying requester's queued pane requests are dropped
                    // and their waits resolved with the stop failure — an
                    // `agents_call` enqueued by a turn that never gets to
                    // see the answer must not sit in the queue for a
                    // LATER turn to serve (the message was for THIS
                    // conversation's flow). In-flight requests stay: the
                    // pane finishes its turn normally and the delivery
                    // parks for this requester's tag (a later turn of the
                    // same loop drains it — the report is not lost).
                    super::panes::cancel_requester(self.panes_requester);
                    self.context_manager.close_loop();
                    let context = self.context_manager.save_state();
                    let _ = tx.send(HarnessEvent::Stopped { context });
                    true
                } else {
                    false
                }
            };
        }

        loop {
            iteration += 1;
            log::debug!(
                "run_agent_loop ITERATION={} input_len={} pending_tools={}",
                iteration,
                current_input.len(),
                self.tool_issuer.len()
            );

            if check_stop!() {
                break;
            }

            // Tool interactions dispatched during THIS iteration, captured
            // as "name\0args\0result" parts for loop detection.
            let mut tool_interactions: Vec<String> = Vec::new();

            // Inject user messages queued for the NEXT REQUEST (the TUI's
            // "next request" queue) into the model context before this
            // request is built. Each message is popped from the SHARED queue
            // right before it is recorded (pop-on-inject), so a TUI-side
            // delete or edit of a still-pending message is always honored —
            // an entry the TUI removed can never reach the model. Each
            // injected message is a regular protected user turn and is
            // acknowledged via `UserMessageInjected` so the TUI can move it
            // out of its pending area into the normal history. An injected
            // message is the newest instruction: it supersedes the
            // per-iteration steering prompt.
            let mut injected_any = false;
            for text in queued_input.drain() {
                injected_any = true;
                let _ = tx.send(HarnessEvent::UserMessageInjected { text: text.clone() });
                self.context_manager.add_user(&text);
            }
            if injected_any {
                current_input.clear();
            }

            // /agents pane reports (background `agents_call` calls): drain
            // THIS requester's finished deliveries next to the background
            // sub-agent notes above — same contract, same shape. A delivery
            // tagged for the OTHER requester stays parked: the manager loop
            // must never inject a report the main loop asked for, and vice
            // versa (the requester tag on each delivery routes it home).
            // Nested loops never drain (the requester tag is MAIN for a
            // nested harness and every delivery belongs to a real loop).
            let pane_deliveries = if self.nested {
                Vec::new()
            } else {
                super::panes::take_ready_deliveries(self.panes_requester)
            };
            if !pane_deliveries.is_empty() {
                for delivery in pane_deliveries {
                    let note = super::panes::delivery_note(&delivery);
                    // Same channel the sub-agent completion notes ride: the
                    // TUI renders it as a chat line, the model gets it as a
                    // marked automated user turn.
                    let _ = tx.send(HarnessEvent::ToolOutput {
                        tool: "agents_call".to_string(),
                        output: note.clone(),
                        finished: true,
                        agent: None,
                        call_ref: None,
                    });
                    self.context_manager.add_user(&note);
                }
                // New instruction material: the next request must carry it.
                current_input.clear();
            }

            // Background sub-agent completions (push half of the feature):
            // drain the process-wide registry ONCE per iteration, at the top
            // of the turn-building phase, so a task that finished since the
            // last request reaches the model as an explicitly-marked
            // automated turn — never mistaken for user input. Nested agents
            // never drain: a notification belongs to the top-level agent's
            // context (see `as_nested`).
            //
            // Single-main-loop assumption: the registry is process-wide and
            // every non-nested loop drains it. cosh runs one top-level agent
            // loop per process, so ownership is unambiguous; if two
            // top-level loops ever run concurrently, a notification can land
            // in the other session's context — revisit then (e.g. an
            // owner-session tag on each task).
            // The manager loop (the /agents split's cosh pane) must not drain
            // the TOP-LEVEL background registry either: those notifications
            // belong to the main chat loop's context (the same rule a
            // nested loop obeys — the requester tag keeps the streams
            // separated).
            let background_notes =
                if self.nested || self.panes_requester != super::panes::MAIN {
                    Vec::new()
                } else {
                    background::take_ready_notifications()
                };
            if !background_notes.is_empty() {
                for (note, note_call_ref) in background_notes {
                    // The notification rides the EXISTING event variant the
                    // TUI already renders as a chat line (its match is
                    // exhaustive over the enum, so a new variant would
                    // break the TUI build) — the model-facing injection is
                    // the `add_user` below, marked `[automated
                    // notification]` Claude-Code-style. The note's spawn
                    // `call_ref` routes the completion to the EXACT window
                    // that call opened (parallel same-CLI spawns).
                    let _ = tx.send(HarnessEvent::ToolOutput {
                        tool: "subagent_status".to_string(),
                        output: note.clone(),
                        finished: true,
                        agent: None,
                        call_ref: note_call_ref,
                    });
                    self.context_manager.add_user(&note);
                }
                // An injected completion is new instruction material: the
                // next request must carry it even when the per-iteration
                // steering message is empty (same contract as the queued
                // user messages above).
                current_input.clear();
            }

            // Phase 1: stream the LLM response using native tool-call format.
            // The messages are rebuilt on EVERY attempt: when a context-window
            // overflow drains tool chains below, the retry must not re-send
            // the removed call/result pairs.
            log::debug!("run_agent_loop PHASE1_START iteration={iteration}");
            let system_context = self.build_chat_context();
            let mut assistant_response = String::new();
            let mut overflow_recoveries = 0usize;
            let result = loop {
                // Attempt boundary: the TUI scopes its discard-on-reset to the
                // message(s) opened after this point — a reset can then never
                // eat the PREVIOUS iteration's transcript.
                let _ = tx.send(HarnessEvent::BeginAssistant);
                let messages = self.context_manager.build_messages(&current_input);
                let tokens_before_attempt = self.context_manager.display_info().total_tokens;
                // Wire-side accounting (file log, target `cosh::harness`):
                // the request's message count and the total serialized size
                // of the messages array + system prompt — the closest cheap
                // proxy for what the provider actually bills. The
                // per-iteration steering message (`current_input`) is logged
                // separately so a runaway nudge ("continue with...") is
                // visible at a glance.
                log::info!(
                    target: "cosh::harness",
                    "request wire: iteration={} messages={} system_bytes={} steering_bytes={} est_total_bytes={}",
                    iteration,
                    messages.len(),
                    system_context.len(),
                    current_input.len(),
                    system_context.len()
                        + current_input.len()
                        + messages.iter().map(|m| m.content.as_deref().unwrap_or("").len()).sum::<usize>(),
                );
                // Exact-prefix cache probe: fingerprint this request and
                // classify the cache outcome against the previous one from
                // the same model (measured divergence, never inferred).
                self.prefix_cache_probe.record(
                    PrefixCacheProbe::fingerprint(
                        self.connector.model().unwrap_or("?"),
                        &system_context,
                        &messages,
                    ),
                );
                let attempt = self
                    .stream_chat_with_messages(&system_context, &messages, |event| {
                        match event {
                            // The SDK retried a mid-stream failure: drop the
                            // partial assistant text accumulated so far and
                            // tell the TUI to discard what it rendered, so
                            // the retried response does not concatenate.
                            StreamEvent::Reset => {
                                assistant_response.clear();
                                let _ = tx.send(HarnessEvent::ClearAssistant);
                            }
                            StreamEvent::Token(token) => {
                                assistant_response.push_str(&token);
                                let _ = tx.send(HarnessEvent::Token { text: token });
                            }
                        }
                    })
                    .await;
                if let Err(ref e) = attempt
                    && e == CONTEXT_WINDOW_MARKER
                    && overflow_recoveries < 2
                {
                    overflow_recoveries += 1;
                    // Context-window contingency: drive the split — it shrinks
                    // the WHOLE timeline (every item included) into a fitting
                    // anchor. On success retry immediately; on failure
                    // the overflow is stuck — surface the (throttled) warning
                    // and fall through to the normal error handling with a
                    // HUMAN-readable message.
                    if matches!(self.context_manager.run(), RunOutcome::Resolved)
                        && self.context_manager.display_info().total_tokens < tokens_before_attempt
                    {
                        continue;
                    }
                    if self.checkpoint_context(&tx).await {
                        continue;
                    }
                    self.notify_context_overflow(&tx);
                }
                break attempt;
            };
            log::debug!(
                "run_agent_loop PHASE1_END iteration={} result_ok={}",
                iteration,
                result.is_ok()
            );

            // Capture extraction failures from this stream and reset for next.
            let extraction_failures = self.tool_extraction_failure_count;
            self.tool_extraction_failure_count = 0;

            if let Err(e) = result {
                log::debug!("run_agent_loop PHASE1_ERR={e}");

                if e == INTERRUPTED_MARKER {
                    // Record the PARTIAL output the model had streamed before
                    // the stop: the user watched it render, and the next turn
                    // ("what did you just do?") must see it. Dropping it here
                    // left the timeline with the bare user turn, so the next
                    // loop's abandoned-input sweep hid the FIRST message too
                    // — total amnesia about the interrupted exchange. Marked
                    // non-closable: a truncated turn is not a final answer
                    // and must never be promoted to a Closure.
                    // Whitespace-only partial output is not worth recording
                    // either (same trim contract as the normal record site
                    // below — and `add_assistant_with_reasoning` now drops it
                    // anyway, which would consume the reasoning into a no-op).
                    if !assistant_response.trim().is_empty() {
                        // Same DISPLAY-ONLY reasoning contract as the normal
                        // record site: the user watched this thinking render
                        // before the stop — it must survive a reboot too.
                        let reasoning = std::mem::take(&mut self.attempt_reasoning);
                        self.context_manager.add_assistant_with_reasoning(
                            &assistant_response,
                            false,
                            Some(&reasoning),
                        );
                    }
                    // The stop can land mid-stream or during connect; the
                    // `is_empty()` guard above covers the no-output case.
                    // Any calls still queued belong to a response that never
                    // completed. A queued call has NO context item yet
                    // (items are recorded at dispatch), so dropping it
                    // leaves no orphan behind — but leaving it in
                    // `tool_issuer` would replay it on the NEXT turn,
                    // running tools the user just cancelled.
                    self.tool_issuer.clear();
                    self.context_manager.close_loop();
                    let _ = tx.send(HarnessEvent::Stopped {
                        context: self.context_manager.save_state(),
                    });
                    break;
                }

                let mut switched = false;
                while !self.fallbacks.is_empty() {
                    let (provider, model) = self.fallbacks.remove(0);
                    match Connector::new(&provider) {
                        Ok(c) => {
                            // Carry the session-level prompt-cache settings
                            // across the rebuild: the TTL choice, the cache-
                            // affinity key and the retention choice belong to
                            // the SESSION, not the model — a fallback switch
                            // must not silently drop them (each field is only
                            // read by the caller family that implements it).
                            let mut c = c
                                .with_model(&model)
                                .with_prompt_cache_ttl_1h(self.connector.prompt_cache_ttl_1h());
                            if let Some(key) = self.connector.prompt_cache_key() {
                                c = c.with_prompt_cache_key(key);
                            }
                            if let Some(retention) = self.connector.prompt_cache_retention() {
                                c = c.with_prompt_cache_retention(retention);
                            }
                            // Re-apply the session-affinity routing: like the
                            // cache settings above, the affinity headers
                            // belong to the SESSION — a fallback switch must
                            // not drop them or the session's requests would
                            // fan out across cache-cold backends.
                            if let Some(sid) = self.connector.session_id() {
                                c = c.with_session_id(sid);
                            }
                            let mut c = c
                                // Re-apply the tool-call mode: the fallback
                                // connector is built from scratch and would
                                // otherwise silently revert to Native (the
                                // inline path would stop parsing text).
                                .with_tool_call_mode(self.connector.tool_call_mode());
                            // Point a local fallback provider at the user's
                            // configured server URL, if one was saved.
                            if let Some(url) = self.local_base_urls.get(&provider) {
                                c = c.with_base_url(url.clone());
                            }
                            // Re-apply the reasoning effort the user chose:
                            // the fallback connector is built from scratch and
                            // would otherwise silently drop it (the classic
                            // "high feels like low" after a fallback switch).
                            // The level is mapped onto the closest one the
                            // fallback model actually accepts; a model with no
                            // reasoning knob drops the effort entirely.
                            if let Some(effort) = self.connector.reasoning_effort()
                                && let Some(resolved) =
                                    resolve_reasoning_effort(&model, effort, Some("cosh/cache"))
                            {
                                c = c.with_reasoning_effort(resolved);
                            }
                            self.connector = c;
                            // Re-register native tool definitions on the new
                            // connector — otherwise the fallback provider
                            // receives zero tools.
                            self.init_native_tools();
                            // The fallback provider may use a different model
                            // family — re-route the token encoding.
                            self.context_manager
                                .set_model(self.connector.effective_model());
                            // The fallback provider may have a DIFFERENT window
                            // than the model that failed — re-discover it so
                            // the budget (and the split trigger) match the
                            // ACTIVE model. A stale window from the failed
                            // provider must never size the new one.
                            self.last_context_window = None;
                            #[cfg(not(test))]
                            {
                                self.discovered_window = None;
                                if let Some(window) =
                                    discovered_context_window(self.connector.effective_model())
                                        .await
                                {
                                    self.discovered_window = Some(window);
                                    // Same effective sizing as the loop start
                                    // (the raw window stays in
                                    // `discovered_window` for the split).
                                    self.context_manager.set_max_tokens(
                                        ContextManager::resolve_window(window, self.effective_context),
                                    );
                                }
                            }
                            self.tool_issuer.clear();
                            log::debug!("switched to fallback: {provider}/{model}");
                            switched = true;
                            break;
                        }
                        Err(err) => {
                            log::debug!("fallback connector {provider} failed: {err}");
                        }
                    }
                }

                if switched {
                    // The fallback changes the agent's budget and renews its
                    // automatic attempt. Route through the chosen summarizer
                    // before deciding whether MapReduce is necessary.
                    automatic_compaction_failed = false;
                    if self.context_manager.compaction_staging_active()
                        || matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction)
                    {
                        automatic_compaction_failed = !self.llm_compact(&tx).await;
                    }
                    continue;
                }

                // Generic terminal error. No separate toast: the error is
                // rendered inline in the chat (emit_terminal_error records
                // the styled "Error: …" line), so a toast would only repeat
                // what is already on screen. The context-window case keeps
                // its own throttled overflow notification. The
                // CONTEXT_WINDOW_MARKER is internal — never show it to the
                // user: translate it to a human message first.
                let user_msg = if e == CONTEXT_WINDOW_MARKER {
                    format!(
                        "Context window exceeded ({}). Switch model or start new session.",
                        self.connector.provider_name().unwrap_or("?")
                    )
                } else {
                    e.clone()
                };
                self.emit_terminal_error(&tx, user_msg, None);
                break;
            }

            // Record the assistant's text response in the context manager so it
            // is delivered via the messages array on the next iteration. Tool
            // calls are recorded separately as structural items during dispatch;
            // an output that carried a tool call stays structural too — only
            // pure text is closable.
            //
            // Whitespace-only is NOT a response: LLMs routinely emit a bare
            // `" "` token (the TUI drops it before opening a bubble — see
            // `tui/app/events.rs`), so without the trim it would (a) record a
            // `" "` item that bloats the context AND the session JSONL, and
            // (b) slip PAST the natural-stop gate below (which also trims),
            // turning a degenerate cut into a Closure of one space. Skipping
            // the record keeps `attempt_reasoning` intact for the gate's
            // one-shot promotion.
            let had_tools = self.has_pending_tools();
            if !assistant_response.trim().is_empty() {
                // The attempt's reasoning rides the item (DISPLAY-ONLY — see
                // `ContextItem::Assistant::reasoning`); `take` because the
                // accumulator is cleared per attempt and the next stream
                // would otherwise leak this turn's thinking into the next.
                let reasoning = std::mem::take(&mut self.attempt_reasoning);
                self.context_manager.add_assistant_with_reasoning(
                    &assistant_response,
                    !had_tools,
                    Some(&reasoning),
                );
            }

            // Record extraction failures in the correction memory so the model
            // sees the pattern even when no specific dispatch error is available.
            if extraction_failures > 0 {
                let msg = if self.last_failed_raw.is_empty() {
                    "Invalid JSON tool call — no registered schema matched".to_string()
                } else {
                    format!(
                        "Tool call failed — you sent: {}\nThe JSON did not match any registered tool schema.\nFollow the Tool format and Schema definition above exactly.",
                        self.last_failed_raw
                    )
                };
                self.correction_memory.push(&msg);
            }

            // Honor stop only once pending tool calls have been dispatched —
            // never drop queued work because stop was requested mid-stream.
            // Note: `check_stop!()` sends a `Stopped` event as a side effect,
            // so it must only be evaluated when there is nothing left to
            // dispatch (short-circuit ordering).
            if !self.has_pending_tools() && check_stop!() {
                break;
            }

            // Phase 2: dispatch all pending tool calls
            // log::debug!("run_agent_loop PHASE2 had_tools={had_tools}");

            // A terminal event (Done/Error) may already have been emitted by
            // the MAX_ITERATIONS / MAX_TOOL_RETRIES guards inside the dispatch
            // loop. Track it so the `check_stop!()` below does NOT emit a
            // second terminal event (the TUI would receive Done+Stopped or
            // Error+Stopped).
            let mut terminal_sent = false;

            while self.has_pending_tools() {
                // Safety: stop the loop if we've exceeded the maximum
                // number of iterations. This prevents runaway tool-calling
                // when the model fails to recognise that its request is
                // complete.
                if iteration >= MAX_ITERATIONS {
                    // log::debug!("run_agent_loop MAX_ITERATIONS={MAX_ITERATIONS} reached");
                    // Heuristic guard: the stop stands, but the decision
                    // model audits it so an improper stop is never silent.
                    let verdict = self.review_guard_stop("max-iterations");
                    self.context_manager.close_loop();
                    let _ = tx.send(self.lsp_snapshot_event());
                    let _ = tx.send(HarnessEvent::Done {
                        context: self.context_manager.save_state(),
                        checkup_verdict: verdict,
                    });
                    terminal_sent = true;
                    break;
                }

                // A model-requested stop (`self.stop` via stop_agent_loop) must
                // NOT interrupt dispatch of already-queued tool calls — the
                // queue is drained first, then stop is honored after phase 2.
                // Only an external stop_signal (user Esc) interrupts here.
                if stop_signal.load(Ordering::Relaxed) {
                    break;
                }

                // Peek at tool info before consuming the item. The Gemini 3.x
                // thought signature is captured alongside so it survives the
                // dispatch and can be replayed in the follow-up request.
                let mut info = self
                    .tool_issuer
                    .front()
                    .map(|tc| (tc.id.clone(), tc.name.clone(), tc.arguments.clone()));
                let info_sig = self
                    .tool_issuer
                    .front()
                    .map(|tc| tc.thought_signature.clone());

                // Per-call routing identity (same rule the history uses
                // below): the provider id, or a synthetic one for inline
                // calls. The synthetic branch uses the harness's MONOTONIC
                // counter — `iteration` only advances in the outer loop,
                // while this inner loop dispatches every tool call of ONE
                // response, so two parallel inline `subagent_call`s must
                // never share a ref (that would re-cross-pair exactly the
                // streams this routing separates). The `ToolCall` event,
                // the dispatch slot and every result/error emission of
                // THIS call share the ref, so the TUI can bind a
                // sub-agent window to the EXACT call that spawned it.
                let dispatch_call_ref: Option<String> = info.as_ref().map(|(id, _, _)| {
                    if id.is_empty() {
                        format!("call_{:016x}", self.context_manager_pending_id())
                    } else {
                        id.clone()
                    }
                });
                if let Some((_, ref name, ref args)) = info {
                    // log::debug!("run_agent_loop DISPATCH tool={name}");
                    let _ = tx.send(HarnessEvent::ToolCall {
                        tool: name.clone(),
                        input: args.clone(),
                        call_ref: dispatch_call_ref.clone(),
                    });
                }

                // Intercept `ask_questions` — send to TUI, wait for user answer
                if info.as_ref().is_some_and(|(_, n, _)| n == "ask_questions") {
                    // log::debug!("run_agent_loop ASK_QUESTIONS intercepted");

                    // Clone info before consuming in .map() below
                    let info_clone = info.clone();
                    let input: Result<QuestionInput, String> = info_clone
                        .map(|(_, _, args)| args)
                        .ok_or_else(|| "missing tool arguments".to_string())
                        .and_then(|args| serde_json::from_value(args).map_err(|e| e.to_string()));

                    self.tool_issuer.pop_front();

                    match input {
                        Ok(q_input) => {
                            // Shared validation + normalization with the direct
                            // tool path (`Question::ask`): SingleChoice requires
                            // `recommended` and is auto-sorted to the top so the
                            // TUI can badge it. On error the model gets a
                            // ToolError and can fix + retry (no dialog shown).
                            let normalized =
                                cosh_tools::question::Question::validate_and_normalize(&q_input);
                            let questions = match normalized {
                                Ok(qs) => qs,
                                Err(e) => {
                                    let _ = tx.send(HarnessEvent::ToolError {
                                        error: e,
                                        call_ref: dispatch_call_ref.clone(),
                                    });
                                    continue;
                                }
                            };
                            let _ = tx.send(HarnessEvent::QuestionRequest {
                                questions: questions.clone(),
                            });

                            log::debug!("run_agent_loop WAITING for answer_rx");
                            // Wait for the TUI to send back answers, but stay
                            // interruptible by the user's stop signal.
                            let answer = tokio::select! {
                                a = answer_rx.recv() => a,
                                _ = async {
                                    loop {
                                        if stop_signal.load(Ordering::Relaxed) {
                                            break;
                                        }
                                        tokio::time::sleep(Duration::from_millis(50)).await;
                                    }
                                } => None,
                            };
                            // log::debug!("run_agent_loop GOT answer={:?}", answer.is_some());
                            match answer {
                                Some(Ok(answers)) => {
                                    let output = QuestionOutput { questions, answers };
                                    let json = serde_json::to_string(&output)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    // Save to structured history (native tool format)
                                    if let Some((_, ref name, ref args)) = info {
                                        let tool_id = dispatch_call_ref.clone().unwrap_or_default();
                                        self.push_tool_history(
                                            &tool_id,
                                            name,
                                            args,
                                            info_sig.as_deref().unwrap_or_default(),
                                            &json,
                                        );
                                        // Loop-detection signature part.
                                        tool_interactions
                                            .push(format!("{name}\u{0}{args}\u{0}{json}"));
                                    }
                                    let _ = tx.send(HarnessEvent::ToolResult {
                                        output: json,
                                        call_ref: dispatch_call_ref.clone(),
                                    });
                                }
                                Some(Err(e)) => {
                                    let _ = tx.send(HarnessEvent::ToolError {
                                        error: e,
                                        call_ref: dispatch_call_ref.clone(),
                                    });
                                }
                                None => {
                                    // `None` here means either the channel was
                                    // dropped or the stop signal fired. When the
                                    // user stopped, break out quietly — the outer
                                    // `check_stop!()` sends the Stopped event.
                                    if stop_signal.load(Ordering::Relaxed) {
                                        log::debug!("run_agent_loop answer_rx interrupted by stop");
                                        break;
                                    }
                                    log::debug!("run_agent_loop answer_rx CLOSED");
                                    let _ = tx.send(HarnessEvent::ToolError {
                                        error: "Internal error: question channel closed"
                                            .to_string(),
                                        call_ref: dispatch_call_ref.clone(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(HarnessEvent::ToolError {
                                error: e,
                                call_ref: dispatch_call_ref.clone(),
                            });
                        }
                    }
                } else {
                    // Normal dispatch for all other tools
                    // log::debug!("run_agent_loop dispatch_next start");

                    // PreToolUse hooks: run user-defined shell commands
                    // before the permission check. Deny/halt results block
                    // the tool call without reaching the permission dialog;
                    // `updated_input` patches the arguments before dispatch.
                    let tool_name_str =
                        info.as_ref().map_or("?".to_string(), |(_, n, _)| n.clone());
                    let tool_input_str = info
                        .as_ref()
                        .map_or("{}".to_string(), |(_, _, a)| a.to_string());
                    let hook_result = self
                        .hook_runner
                        .as_ref()
                        .map(|r| r.run_pre(&tool_name_str, &tool_input_str));

                    if let Some(hr) = &hook_result
                        && hr.decision != super::hooks::HookDecision::Deny
                        && !hr.updated_input.is_empty()
                        && let Ok(patched) =
                            serde_json::from_str::<serde_json::Value>(&hr.updated_input)
                    {
                        log::debug!(
                            "run_agent_loop HOOK_REWRITE tool={tool_name_str} input patched by hook"
                        );
                        if let Some((_, _, args)) = info.as_mut() {
                            *args = patched;
                            // dispatch_next consumes the entry straight from
                            // tool_issuer; sync it or the tool runs with the
                            // ORIGINAL arguments while permission/history
                            // show the patched ones.
                            if let Some(tc) = self.tool_issuer.front_mut() {
                                tc.arguments = args.clone();
                            }
                        }
                    }
                    if let Some(hr) = &hook_result
                        && (hr.decision == super::hooks::HookDecision::Deny || hr.halt)
                    {
                        let reason = if hr.halt {
                            format!("Turn halted by hook: {}", hr.reason)
                        } else {
                            format!("Tool call blocked by hook: {}", hr.reason)
                        };
                        log::debug!(
                            "run_agent_loop HOOK_DENY tool={tool_name_str} halt={} reason={}",
                            hr.halt,
                            hr.reason,
                        );
                        self.tool_issuer.pop_front();
                        self.tool_failure_count += 1;
                        // Record the denial as the call's conversation turn —
                        // same rationale as the guardrail-deny path below: a
                        // call that never gets a visible answer gets re-issued,
                        // burning MAX_TOOL_RETRIES lives on the same mistake.
                        if let Some((_, ref name, ref args)) = info {
                            let tool_id = dispatch_call_ref.clone().unwrap_or_default();
                            self.push_tool_history(
                                &tool_id,
                                name,
                                args,
                                info_sig.as_deref().unwrap_or_default(),
                                &reason,
                            );
                            tool_interactions.push(format!("{name}\u{0}{args}\u{0}{reason}"));
                        }
                        self.correction_memory.push(&reason);
                        let _ = tx.send(HarnessEvent::ToolError {
                            error: reason,
                            call_ref: dispatch_call_ref.clone(),
                        });
                        // Halt stops the entire turn
                        if hr.halt {
                            let msg = "Turn halted by hook".to_string();
                            // Explicit user intent via hook config — never
                            // reviewed by the model.
                            self.emit_terminal_error(&tx, msg, None);
                            terminal_sent = true;
                            break;
                        }
                        // Run the loop detector before the `continue` skips
                        // the end-of-iteration check (same rationale as the
                        // guardrail-deny path below).
                        if self.check_loop_detection(&tool_interactions, &mut loop_window, &tx) {
                            terminal_sent = true;
                            break;
                        }
                        if self.tool_failure_count >= MAX_TOOL_RETRIES {
                            let msg = format!(
                                "{MAX_TOOL_RETRIES} consecutive tool call failures. Agent loop interrupted."
                            );
                            let verdict = self.review_guard_stop("max-tool-retries");
                            self.emit_terminal_error(&tx, msg, verdict);
                            terminal_sent = true;
                            break;
                        }
                        continue;
                    }

                    // Permission check
                    // Before dispatching, check if the tool needs user approval.
                    // This only applies to cosh tools (fs_read, bash_run, etc.)
                    // MCP server tools are passed through unchecked.
                    let perm_check = check_tool_permission(
                        info.as_ref().map_or("?", |(_, n, _)| n.as_str()),
                        info.as_ref()
                            .map_or(&serde_json::Value::Null, |(_, _, a)| a),
                        self.mode,
                        self.cosh_tools
                            .as_ref()
                            .map(|ct| ct.project_root().as_path()),
                    );

                    // Tracks whether the user chose AllowOnce, so we can remove
                    // the approved paths from the allowlist after dispatch.
                    // Initialised here (before the match) so it survives the
                    // scoped match arms below.
                    let mut allow_once_paths: Vec<std::path::PathBuf> = Vec::new();
                    let mut allow_once_tool: String = String::new();

                    match perm_check {
                        PermissionCheck::Denied(reason) => {
                            // Blocked by mode restrictions (Ask mode, or the Build
                            // guardrail denying a coordinate form) — skip dispatch
                            // entirely. The denial MUST be recorded as the call's
                            // conversation turn (like the user-deny path below):
                            // the schema was registered, so a denied call is part
                            // of normal operation — if the model never sees its
                            // call answered, it re-issues it and each re-issue
                            // burns one of the MAX_TOOL_RETRIES lives.
                            log::debug!(
                                "run_agent_loop PERM_DENIED tool={:?} reason={reason}",
                                info.as_ref().map(|(_, n, _)| n)
                            );
                            self.tool_issuer.pop_front();
                            self.tool_failure_count += 1;
                            if let Some((_, ref name, ref args)) = info {
                                let tool_id = dispatch_call_ref.clone().unwrap_or_default();
                                self.push_tool_history(
                                    &tool_id,
                                    name,
                                    args,
                                    info_sig.as_deref().unwrap_or_default(),
                                    &reason,
                                );
                                // Loop-detection signature part: identical
                                // denials repeat identically, so this also lets
                                // the loop detector fire its own (earlier,
                                // louder) break instead of burning all lives.
                                tool_interactions.push(format!("{name}\u{0}{args}\u{0}{reason}"));
                            }
                            self.correction_memory.push(&reason);
                            let _ = tx.send(HarnessEvent::ToolError {
                                error: reason,
                                call_ref: dispatch_call_ref.clone(),
                            });
                            // The denial just became this iteration's tool
                            // interaction — run the loop detector HERE, before
                            // the `continue` skips the end-of-iteration check:
                            // a stuck model re-issuing an identical denied call
                            // must hit the detector, not the retry budget.
                            if self.check_loop_detection(&tool_interactions, &mut loop_window, &tx)
                            {
                                terminal_sent = true;
                                break;
                            }
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call failures. Agent loop interrupted."
                                );
                                let verdict = self.review_guard_stop("max-tool-retries");
                                self.emit_terminal_error(&tx, msg, verdict);
                                terminal_sent = true;
                                break;
                            }
                            continue;
                        }
                        PermissionCheck::NeedsApproval(req) => {
                            // Cached agent permission
                            // For subagent_call, skip the dialog entirely if the
                            // agent was previously allowed with a permanent Allow.
                            let is_cached_subagent = req.tool == "subagent_call"
                                && req
                                    .args
                                    .strip_prefix("agent: ")
                                    .is_some_and(|agent| self.agent_permissions.contains(agent));

                            // Pre-approved path check
                            // If all tool paths were previously Allowed (permanent),
                            // skip the permission dialog — the harness remembers.
                            let paths = super::guardrails::extract_paths_from_args(
                                &req.tool,
                                info.as_ref()
                                    .map_or(&serde_json::Value::Null, |(_, _, a)| a),
                            );
                            let all_pre_approved = !paths.is_empty()
                                && paths
                                    .iter()
                                    .all(|p| self.approved_paths.contains(std::path::Path::new(p)));

                            if is_cached_subagent || all_pre_approved {
                                if all_pre_approved {
                                    log::debug!("run_agent_loop PERM_CACHED paths={:?}", paths,);
                                } else {
                                    log::debug!(
                                        "run_agent_loop PERM_CACHED subagent={}",
                                        req.args.strip_prefix("agent: ").unwrap_or("?")
                                    );
                                }
                                // Skip dialog, proceed to dispatch
                            } else {
                                // Send permission request to TUI, wait for user response
                                log::debug!("run_agent_loop PERM_NEEDS_APPROVAL tool={}", req.tool);
                                let _ = tx.send(HarnessEvent::PermissionRequest {
                                    tool: req.tool.clone(),
                                    description: req.description.clone(),
                                    args: req.args.clone(),
                                });

                                // Wait for the TUI to send back the permission action
                                let perm_action = tokio::select! {
                                    action = perm_rx.recv() => action,
                                    _ = async {
                                        loop {
                                            if stop_signal.load(Ordering::Relaxed) {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(50)).await;
                                        }
                                    } => None,
                                };

                                match perm_action {
                                    Some(action) => {
                                        match action {
                                            super::guardrails::PermissionAction::Allow => {
                                                // Cache subagent permission (persists across the session)
                                                if req.tool == "subagent_call"
                                                    && let Some(agent_name) =
                                                        req.args.strip_prefix("agent: ")
                                                {
                                                    self.agent_permissions
                                                        .insert(agent_name.to_string());
                                                    log::debug!(
                                                        "run_agent_loop PERM_ALLOW agent={agent_name} (cached)"
                                                    );
                                                }
                                                // Remember approved paths in the harness
                                                // so future calls skip the permission dialog.
                                                let approved =
                                                    super::guardrails::extract_paths_from_args(
                                                        &req.tool,
                                                        info.as_ref().map_or(
                                                            &serde_json::Value::Null,
                                                            |(_, _, a)| a,
                                                        ),
                                                    );
                                                for p in &approved {
                                                    self.approved_paths
                                                        .insert(std::path::PathBuf::from(p));
                                                }
                                                // Add allowed paths to the tool's allowlist
                                                // so PathGuard lets them through.
                                                Self::add_paths_to_allowlist(
                                                    &req.tool,
                                                    info.as_ref().map_or(
                                                        &serde_json::Value::Null,
                                                        |(_, _, a)| a,
                                                    ),
                                                    &mut self.cosh_tools,
                                                );
                                                // Proceed with dispatch
                                            }
                                            super::guardrails::PermissionAction::AllowOnce => {
                                                // Save paths and tool name so we can
                                                // remove from allowlist after dispatch.
                                                allow_once_paths =
                                                    super::guardrails::extract_paths_from_args(
                                                        &req.tool,
                                                        info.as_ref().map_or(
                                                            &serde_json::Value::Null,
                                                            |(_, _, a)| a,
                                                        ),
                                                    )
                                                    .into_iter()
                                                    .map(std::path::PathBuf::from)
                                                    .collect();
                                                allow_once_tool = req.tool.clone();
                                                // Add paths to allowlist so PathGuard
                                                // lets them through during dispatch.
                                                Self::add_paths_to_allowlist(
                                                    &req.tool,
                                                    info.as_ref().map_or(
                                                        &serde_json::Value::Null,
                                                        |(_, _, a)| a,
                                                    ),
                                                    &mut self.cosh_tools,
                                                );
                                            }
                                            super::guardrails::PermissionAction::Deny => {
                                                // Deny always pops and reports — the harness
                                                // is the gatekeeper. Path tools that were
                                                // previously Allowed retain their allowlist
                                                // entry, but this Deny means the user does not
                                                // want THIS call to proceed regardless.
                                                self.tool_issuer.pop_front();
                                                let tool_name = info
                                                    .as_ref()
                                                    .map_or("?", |(_, n, _)| n.as_str());
                                                let agent_info =
                                                    info.as_ref().and_then(|(_, _, a)| {
                                                        a.get("agent").and_then(|v| v.as_str())
                                                    });
                                                let msg = match agent_info {
                                                    Some(agent) => format!(
                                                        "Tool `{tool_name}` (agent: {agent}) denied by user"
                                                    ),
                                                    None => {
                                                        format!("Tool `{tool_name}` denied by user")
                                                    }
                                                };
                                                // Record the denial in history so the model
                                                // sees its own tool call + the "denied" result
                                                // as a proper conversation turn.
                                                if let Some((_, ref name, ref args)) = info {
                                                    let tool_id = dispatch_call_ref
                                                        .clone()
                                                        .unwrap_or_default();
                                                    self.push_tool_history(
                                                        &tool_id,
                                                        name,
                                                        args,
                                                        info_sig.as_deref().unwrap_or_default(),
                                                        &msg,
                                                    );
                                                }
                                                self.correction_memory.push(&msg);
                                                let _ = tx.send(HarnessEvent::ToolError {
                                                    error: msg,
                                                    call_ref: dispatch_call_ref.clone(),
                                                });
                                                continue;
                                            }
                                        }
                                    }
                                    None => {
                                        // Channel closed or stop requested
                                        // log::debug!("run_agent_loop PERM_CHANNEL_CLOSED");
                                        self.tool_issuer.pop_front();
                                        let _ = tx.send(HarnessEvent::ToolError {
                                            error: "Internal error: permission channel closed"
                                                .to_string(),
                                            call_ref: dispatch_call_ref.clone(),
                                        });
                                        continue;
                                    }
                                }
                            }
                        }
                        PermissionCheck::Allowed => {
                            // Proceed with normal dispatch
                        }
                    }

                    /// Outcome of a tool dispatch, possibly interrupted by stop.
                    #[derive(Debug)]
                    enum DispatchOut {
                        Ok(String),
                        Err(String),
                        Stopped,
                    }

                    // Scope dispatch_fut tightly so its &mut self borrow is
                    // released BEFORE we match on the result below.
                    let dispatch_out = {
                        tools_ran = true;
                        // Per-call routing identity: the SAME ref the
                        // `ToolCall` event above carried. Every stream the
                        // dispatch emits (sub-agent mini-chat, reports,
                        // receipts) is stamped with it, so the TUI pairs a
                        // window's input with ITS OWN traffic even when
                        // several sub-agents run in parallel.
                        self.active_call_ref = dispatch_call_ref.clone();
                        // The Tier-1 dispatcher (CoshTools) reads the SAME
                        // identity to stamp its sub-agent streams/reports.
                        if let Some(cosh) = self.cosh_tools.as_ref() {
                            cosh.set_active_call_ref(dispatch_call_ref.clone());
                        }
                        let dispatch_fut = self.dispatch_next();
                        tokio::pin!(dispatch_fut);

                        tokio::select! {
                            result = &mut dispatch_fut => match result {
                                Ok(output) => DispatchOut::Ok(output),
                                Err(e) => DispatchOut::Err(e),
                            },
                            _ = async {
                                loop {
                                    if stop_signal.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(
                                        50,
                                    ))
                                    .await;
                                }
                            } => DispatchOut::Stopped,
                        }
                        // dispatch_fut dropped here → &mut self released
                    };
                    // The dispatch is over — every stream it emitted carries
                    // the ref; drop it so a LATER non-dispatch emission can
                    // never inherit a stale identity.
                    self.active_call_ref = None;

                    // If this was AllowOnce, remove the approved paths from the
                    // allowlist in BOTH success and error cases — we never want
                    // AllowOnce paths to leak into future calls.
                    if !allow_once_paths.is_empty()
                        && let Some(cosh) = self.cosh_tools.as_mut()
                    {
                        for p in &allow_once_paths {
                            match allow_once_tool.as_str() {
                                "fs_read" | "fs_write" | "fs_edit" | "fs_edit_lines"
                                | "fs_ast_edit" | "fs_rollback" => {
                                    cosh.remove_fs_allowlist_path(p);
                                }
                                "find_glob" | "find_grep" => {
                                    cosh.remove_find_allowlist_path(p);
                                }
                                _ => {}
                            }
                        }
                    }

                    match dispatch_out {
                        DispatchOut::Ok(mut output) => {
                            // PostToolUse hooks: run after a successful
                            // execution and BEFORE history/ToolResult, so
                            // `context` enriches the output the model sees;
                            // deny (tool already ran) degrades to a warning
                            // line; halt stops the turn.
                            let tool_name_str = info.as_ref().map_or("?", |(_, n, _)| n.as_str());
                            if let Some(runner) = self.hook_runner.as_ref()
                                && !runner.is_empty()
                            {
                                let tool_input_str = info
                                    .as_ref()
                                    .map_or("{}".to_string(), |(_, _, a)| a.to_string());
                                let post = runner.run_post(tool_name_str, &tool_input_str, &output);
                                if post.halt {
                                    let reason = if post.reason.is_empty() {
                                        "Turn halted by post-tool hook".to_string()
                                    } else {
                                        format!("Turn halted by post-tool hook: {}", post.reason)
                                    };
                                    log::debug!(
                                        "run_agent_loop POST_HOOK_HALT tool={tool_name_str}"
                                    );
                                    // Explicit user intent via hook config —
                                    // never reviewed by the model.
                                    self.emit_terminal_error(&tx, reason, None);
                                    terminal_sent = true;
                                    break;
                                }
                                let mut extra = String::new();
                                if !post.context.is_empty() {
                                    extra.push_str(&post.context);
                                }
                                if post.decision == super::hooks::HookDecision::Deny {
                                    use std::fmt::Write as _;
                                    if !extra.is_empty() {
                                        extra.push('\n');
                                    }
                                    let _ = writeln!(
                                        extra,
                                        "[hook] {}",
                                        if post.reason.is_empty() {
                                            "flagged by post-tool hook".to_string()
                                        } else {
                                            post.reason.clone()
                                        }
                                    );
                                }
                                if !extra.is_empty() {
                                    output.push_str("\n\n");
                                    output.push_str(&extra);
                                }
                            }

                            self.tool_failure_count = 0;
                            log::debug!("run_agent_loop dispatch_next OK len={}", output.len());
                            // Record the tool call + result in native history
                            if let Some((_, ref name, ref args)) = info {
                                let tool_id = dispatch_call_ref.clone().unwrap_or_default();
                                self.push_tool_history(
                                    &tool_id,
                                    name,
                                    args,
                                    info_sig.as_deref().unwrap_or_default(),
                                    &output,
                                );
                                // Loop-detection signature part.
                                tool_interactions.push(format!("{name}\u{0}{args}\u{0}{output}"));
                            }
                            let _ = tx.send(HarnessEvent::ToolResult {
                                output,
                                call_ref: dispatch_call_ref.clone(),
                            });
                        }
                        DispatchOut::Err(e) => {
                            // Record the error in history so the model sees
                            // its tool call + the error as a proper turn.
                            if let Some((_, ref name, ref args)) = info {
                                let tool_id = dispatch_call_ref.clone().unwrap_or_default();
                                self.push_tool_history(
                                    &tool_id,
                                    name,
                                    args,
                                    info_sig.as_deref().unwrap_or_default(),
                                    &e,
                                );
                                // Loop-detection signature part.
                                tool_interactions.push(format!("{name}\u{0}{args}\u{0}{e}"));
                            }
                            self.tool_failure_count += 1;
                            log::debug!(
                                "run_agent_loop dispatch_next ERR={e} \
                                 (failure #{}/{MAX_TOOL_RETRIES})",
                                self.tool_failure_count,
                            );
                            // Feed the correction memory like the other
                            // failure paths (hooks, permissions, extraction)
                            // so repeated tool errors accumulate in the
                            // tail-rendered Correction History block instead
                            // of scrolling away as bare ToolError turns.
                            // Enriched edit errors carry live-content blocks;
                            // keep only the diagnostic head (error line +
                            // current anchor) so the block stays lean.
                            let head: Vec<&str> = e.lines().take(3).collect();
                            self.correction_memory.push(&head.join("\n"));
                            let _ = tx.send(HarnessEvent::ToolError {
                                error: e,
                                call_ref: dispatch_call_ref.clone(),
                            });
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call \
                                     failures. Agent loop interrupted."
                                );
                                let verdict = self.review_guard_stop("max-tool-retries");
                                self.emit_terminal_error(&tx, msg, verdict);
                                terminal_sent = true;
                                break;
                            }
                        }
                        DispatchOut::Stopped => {
                            // log::debug!("run_agent_loop dispatch_next STOPPED by user");
                            self.stop = true;
                            self.context_manager.close_loop();
                            let _ = tx.send(HarnessEvent::Stopped {
                                context: self.context_manager.save_state(),
                            });
                            return; // Exit run_agent_loop entirely
                        }
                    }
                }
            }

            if terminal_sent {
                // A terminal event was already emitted by the dispatch guards;
                // skip check_stop!() so it cannot send a second one.
                break;
            }

            if check_stop!() {
                break;
            }

            // Crush-style loop detection: an identical tool+input+result
            // signature seen more than LOOP_DETECTION_MAX_REPEATS times
            // within the recent window means the model is stuck repeating
            // the exact same action (e.g. re-running a failing command).
            // Stop the loop here instead of burning tokens up to
            // MAX_ITERATIONS — same safety intent, caught far earlier.
            // (The `break` exits the outer iteration loop directly, so no
            // terminal_sent bookkeeping is needed at this call site.)
            if self.check_loop_detection(&tool_interactions, &mut loop_window, &tx) {
                break;
            }

            if !had_tools {
                if extraction_failures > 0 {
                    // Model tried but all tool calls failed validation.
                    // Give it another chance with the correction prompt.
                    if iteration >= MAX_ITERATIONS {
                        // Safety net — emit a terminal event so the TUI does
                        // not stay in a "running" state. Heuristic guard:
                        // the stop stands, but the decision model audits it.
                        let verdict = self.review_guard_stop("max-iterations");
                        self.context_manager.close_loop();
                        let _ = tx.send(self.lsp_snapshot_event());
                        let _ = tx.send(HarnessEvent::Done {
                            context: self.context_manager.save_state(),
                            checkup_verdict: verdict,
                        });
                        break;
                    }
                    // log::debug!("run_agent_loop RETRY (extraction failures)");
                    current_input.clear();
                    continue;
                }
                // A truncated, filtered or provider-paused turn is NOT a
                // completed answer. The old inverted test (`== Some("length")`)
                // silently emitted Done whenever a provider dialect slipped
                // through unnormalized (Anthropic `max_tokens`, Gemini
                // `MAX_TOKENS`/`SAFETY`) or the stream ended without any
                // finish reason (EOF mid-response) — the loop stopped
                // mid-action with no error and no user Esc. The whitelist
                // mirrors `validate_summary_completion`: only a deliberate
                // text completion counts as finished; anything else
                // continues the loop (bounded by MAX_ITERATIONS above).
                let finished = matches!(
                    self.last_finish_reason.as_deref(),
                    // `STOP` never arrives from a live provider (the SDK
                    // normalizes lowercase-first); it survives here only for
                    // mock-queued raw values — kept deliberately to mirror
                    // `validate_summary_completion`.
                    // `tool_calls` is reachable in this no-pending-tools
                    // branch only when every call of the turn was
                    // harness-consumed mid-stream (e.g. a lone
                    // `mask_tool_result`): nothing is left to dispatch, so
                    // the turn IS complete — continuing would send a bogus
                    // "cut off" prompt.
                    Some("stop" | "STOP" | "end_turn" | "tool_calls")
                ) && !self.last_stream_unfinished;
                if !finished {
                    if iteration >= MAX_ITERATIONS {
                        // Safety net — emit a terminal event so the TUI does
                        // not stay in a "running" state. Heuristic guard:
                        // the stop stands, but the decision model audits it.
                        let verdict = self.review_guard_stop("max-iterations");
                        self.context_manager.close_loop();
                        let _ = tx.send(self.lsp_snapshot_event());
                        let _ = tx.send(HarnessEvent::Done {
                            context: self.context_manager.save_state(),
                            checkup_verdict: verdict,
                        });
                        break;
                    }
                    let reason = self.last_finish_reason.as_deref().unwrap_or("none");
                    // Known non-completion dialects get a toast; `none`
                    // (plain EOF) and `length` stay silent — the continuation
                    // prompt below is enough for them.
                    if matches!(
                        reason,
                        "content_filter" | "pause_turn" | "model_context_window_exceeded"
                    ) {
                        let _ = tx.send(HarnessEvent::Toast {
                            message: format!(
                                "Provider cut the response (finish reason: {reason}); continuing."
                            ),
                            variant: ToastVariant::Warning,
                        });
                    }
                    // log::debug!("run_agent_loop TRUNCATED (finish reason {reason}) — continuing");
                    log::info!(
                        target: "cosh::harness",
                        "continuation: truncated-stream retry (finish reason {reason}) — full context will be re-sent with a cut-off nudge"
                    );
                    current_input = "Your previous response was cut off before completion. \
                         Please continue exactly where you left off."
                        .to_string();
                    continue;
                }

                // Deterministic stop-veto layer (no decision model required).
                // The provider's `stop` describes the RESPONSE, not the TASK:
                // two shapes prove the turn is not actually finished, and
                // each grants a continuation that consumes one scarce nudge
                // (bounded per loop, independent of MAX_ITERATIONS) so a model
                // that keeps insisting on stopping still terminates visibly.
                let reason = self.last_finish_reason.as_deref().unwrap_or("none");
                if self.stop_nudges_left > 0 {
                    // (1) Reasoning ended, the answer never came: the attempt
                    // produced thinking deltas but zero answer text (the
                    // accumulator is only drained when a non-empty response
                    // is recorded, so it still holds the thinking here).
                    // `trim().is_empty()`: a bare whitespace token (e.g. a
                    // lone `" "` after the thinking block) is NOT an answer —
                    // without the trim it would render as a Closure of one
                    // space and the gate would never fire.
                    // Promote the reasoning into the ONE-SHOT steering message
                    // so the next request reuses it instead of re-deriving
                    // it; steering is per-iteration state, so the promotion
                    // vanishes from the very next turn.
                    if assistant_response.trim().is_empty() && !self.attempt_reasoning.is_empty() {
                        self.stop_nudges_left -= 1;
                        let reasoning = std::mem::take(&mut self.attempt_reasoning);
                        #[cfg(test)]
                        self.promoted_reasoning_log.push(reasoning.clone());
                        log::info!(
                            target: "laya",
                            "natural-stop gate: reasoning-only stream (finish reason {reason}) — continuing, reasoning promoted for one turn ({bytes} bytes)",
                            bytes = reasoning.len()
                        );
                        current_input = format!(
                            "[Harness note: your previous response ended right after your \
                             reasoning, before any answer. Reuse the thinking below instead \
                             of reasoning again — continue exactly where you left off.]\n\n\
                             <your-previous-reasoning>\n{reasoning}\n</your-previous-reasoning>"
                        );
                        continue;
                    }
                    // (2) Empty response with no reasoning at all: a bare cut
                    // (or a degenerate empty completion) — never a finished
                    // turn. Same `trim()` contract as (1): whitespace-only
                    // counts as empty here too.
                    if assistant_response.trim().is_empty() {
                        self.stop_nudges_left -= 1;
                        log::info!(
                            target: "laya",
                            "natural-stop gate: empty response (finish reason {reason}) — retrying"
                        );
                        current_input =
                            "Your previous response was empty. Continue with your work."
                                .to_string();
                        continue;
                    }
                }

                // No tools, no extraction failures, deliberate completion —
                // conversation is complete. The final text response becomes
                // the loop's Closure.
                //
                // Ambiguous stop: the model stopped talking, but only the
                // text itself says whether the task is actually done. When a
                // checkup is attached, a confident `NotTerminated` verdict
                // vetoes the stop and steers the loop to continue (bounded
                // by MAX_ITERATIONS, like every other continuation).
                if let Some(checkup) = self.checkup.as_ref()
                    // An empty final response is a degenerate review input
                    // for the decision model — treat it as a plain stop.
                    // `trim()`: a whitespace-only "answer" is equally
                    // degenerate (same contract as the gate above).
                    && !assistant_response.trim().is_empty()
                    // A cycle with no tool dispatches is a pure text
                    // exchange: the request was answerable directly, so the
                    // final text is complete by construction and there is
                    // nothing to audit. Running the model here vetoes
                    // exactly the answers it cannot see the point of (a
                    // measured probe — `crates/cosh-onnx/tests/eval/
                    // shape_probe.rs` — showed every request-blind verdict
                    // on a direct answer reads `running`).
                    //
                    // The flag is set only at the `dispatch_next` funnel, so
                    // a cycle whose tool calls were all REFUSED (hook deny,
                    // permission deny) also lands here unaudited —
                    // intentional: an explicit user refusal is user-directed
                    // work, and user intent is never reviewed by the model.
                    && tools_ran
                {
                    let verdict = checkup.review_termination(&assistant_response);
                    // Observability: every natural-stop review lands in
                    // `laya.log`, including the ones that do NOT veto —
                    // a low-confidence `NotTerminated` (or a `Terminated`)
                    // verdict otherwise leaves NO trace, and "why did it
                    // stop?" becomes unanswerable.
                    log::info!(
                        target: "laya",
                        "checkup: natural-stop review → {decision:?} (confidence {confidence:.2}, floor {floor:.2}, iteration {iteration})",
                        decision = verdict.decision,
                        confidence = verdict.confidence,
                        floor = self.checkup_min_confidence,
                    );
                    if verdict.decision == super::checkup::TerminationDecision::NotTerminated
                        && verdict.confidence >= self.checkup_min_confidence
                        // Bound the veto like every other continuation: this
                        // arm is reached with `finished == true`, so the
                        // `!finished` MAX_ITERATIONS check above never sees
                        // it — without this bound a persistent model/checkup
                        // disagreement on text-only turns would never stop.
                        && iteration < MAX_ITERATIONS
                    {
                        log::info!(
                            target: "laya",
                            "checkup: VETOED the natural stop (confidence {:.2}, floor {:.2}) — continuing",
                            verdict.confidence,
                            self.checkup_min_confidence
                        );
                        log::debug!(
                            "run_agent_loop checkup vetoed natural completion \
                             (confidence {:.2}) — continuing",
                            verdict.confidence
                        );
                        let _ = tx.send(HarnessEvent::Toast {
                            message: "Termination checkup: the task does not look \
                                      finished; the agent is continuing."
                                .to_string(),
                            variant: ToastVariant::Info,
                        });
                        // The injected turn is deliberately a bare
                        // `continue`: the veto disagrees with the model's
                        // own stop, so the loop must hand the decision
                        // BACK to the model, not argue with it. Any
                        // coaching ("finish the remaining work") confirms
                        // a running state the model can never disprove —
                        // it would keep producing summaries and the
                        // checkup would keep vetoing them, the worst-case
                        // infinite loop. A bare `continue` lets the next
                        // response either stand by its completion (the
                        // checkup audits a final summary again) or
                        // dispatch actual work.
                        current_input = "continue".to_string();
                        continue;
                    }
                }
                // log::debug!("run_agent_loop DONE (no tools)");
                // /agents pane requests still outstanding: the turn does
                // NOT end while a queued or in-flight `agents_call` answer
                // is missing — ending it would orphan the wait and lose
                // the report (same nesting rule as the background
                // sub-agents below). Wait for the completions; Esc
                // interrupts the wait and ends the turn normally.
                if !self.nested && super::panes::has_outstanding_requests(self.panes_requester) {
                    log::debug!("run_agent_loop WAITING for /agents pane reports before Done");
                    Self::wait_for_outstanding(&stop_signal, || {
                        super::panes::has_outstanding_requests(self.panes_requester)
                    })
                    .await;
                    if !stop_signal.load(Ordering::Relaxed) {
                        let notes: Vec<(String, String, Option<String>)> =
                            super::panes::take_ready_deliveries(self.panes_requester)
                                .iter()
                                .map(|delivery| {
                                    (
                                        "agents_call".to_string(),
                                        super::panes::delivery_note(delivery),
                                        None,
                                    )
                                })
                                .collect();
                        self.inject_completion_notes(&tx, notes);
                        // New input (or just the resolved waits): the stale
                        // steering text must not shadow it. The loop
                        // continues so the model responds to it.
                        current_input.clear();
                        continue;
                    }
                }
                // Background tasks still outstanding: the turn does NOT end
                // while work it spawned is running (Claude Code's nesting
                // rule — an interactive loop waits for its background
                // sub-agents before finishing). Wait for the completions,
                // inject the notifications, and CONTINUE the loop so the
                // model responds to them; Esc interrupts the wait and ends
                // the turn normally (undelivered notifications survive in
                // the registry and drain on the next turn).
                if !self.nested && background::has_running_tasks() {
                    log::debug!(
                        "run_agent_loop WAITING for background sub-agent tasks before Done"
                    );
                    Self::wait_for_outstanding(&stop_signal, background::has_running_tasks).await;
                    // Esc during the wait ends the turn normally: no
                    // injection, fall through to the standard Done path
                    // below (never break without emitting Done — the TUI
                    // would stay in a "running" state).
                    let notes: Vec<(String, String, Option<String>)> = if stop_signal
                        .load(Ordering::Relaxed)
                    {
                        Vec::new()
                    } else {
                        background::take_ready_notifications()
                            .into_iter()
                            .map(|(note, call_ref)| ("subagent_status".to_string(), note, call_ref))
                            .collect()
                    };
                    if self.inject_completion_notes(&tx, notes) {
                        // The injected completions are the new input; the
                        // stale steering text must not shadow them.
                        current_input.clear();
                        continue;
                    }
                }
                self.context_manager.close_loop();
                // Final LSP snapshot: this break skips the per-cycle emission
                // below, so send the freshest state here.
                let _ = tx.send(self.lsp_snapshot_event());
                let _ = tx.send(HarnessEvent::Done {
                    context: self.context_manager.save_state(),
                    // The natural completion IS the audited decision: an
                    // approved stop carries no annotation.
                    checkup_verdict: None,
                });
                break;
            }

            // log::debug!("run_agent_loop RESTARTING with tool results");
            // The tool results have already been recorded in the context
            // manager via `push_tool_history` during dispatch. The follow-up
            // request is built with NO steering text: the conversation
            // already ends in `tool_calls → tool results`, which is the
            // provider-standard signal that the next assistant turn is the
            // REACTION to those results (the same structural continuation
            // Codex uses — `while turnInput.length > 0` re-sends only the
            // history). A synthetic "please continue with your response"
            // user turn would ask for a RESPONSE instead of the reaction —
            // the exact trigger of the premature status-message stop — and
            // would also inject a fabricated user turn into every cycle of
            // the wire, breaking the exact-prefix property.
            current_input.clear();

            // Compact before the next request. The LLM compaction runs when
            // the total is still over the trigger — the harness performs the
            // model call.
            if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction)
                && !automatic_compaction_failed
            {
                automatic_compaction_failed = !self.llm_compact(&tx).await;
            }
            let _ = tx.send(HarnessEvent::ContextInfo {
                info: self.context_manager.display_info(),
            });

            // Keep the TUI's LSP status tags fresh: servers spawn lazily as
            // tools touch files, so re-snapshot every cycle.
            let _ = tx.send(HarnessEvent::LspServers {
                available: self.lsp_available(),
                servers: self.active_lsp_server_names(),
            });

            // Incremental persistence: hand the TUI a snapshot of the running
            // context at a throttled cadence so a crash/restart mid-run does
            // not lose the in-flight run's context.
            if last_snapshot.elapsed() >= self.snapshot_interval {
                last_snapshot = std::time::Instant::now();
                let _ = tx.send(HarnessEvent::ContextSnapshot {
                    context: self.context_manager.save_state(),
                });
            }
        }
        // log::debug!("run_agent_loop EXIT");

        // Send final context info — the TUI shows this as the last known
        // state until the next agent loop starts.
        let _ = tx.send(HarnessEvent::ContextInfo {
            info: self.context_manager.display_info(),
        });
    }

    /// Add paths from tool arguments to the appropriate allowlist so that
    /// [`PathGuard`](cosh_tools::util::path_guard::PathGuard) lets them through.
    ///
    /// Reuses [`extract_paths_from_args`](super::guardrails::extract_paths_from_args)
    /// to avoid duplicating argument-shape logic.
    fn add_paths_to_allowlist(
        tool_name: &str,
        args: &serde_json::Value,
        cosh_tools: &mut Option<super::tools::CoshTools>,
    ) {
        let paths: Vec<std::path::PathBuf> =
            super::guardrails::extract_paths_from_args(tool_name, args)
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect();

        if paths.is_empty() {
            return;
        }

        if let Some(cosh) = cosh_tools.as_mut() {
            for p in &paths {
                match tool_name {
                    "fs_read" | "fs_write" | "fs_edit" | "fs_edit_lines" | "fs_ast_edit"
                    | "fs_rollback" => {
                        log::debug!("add_paths_to_allowlist fs allowed: {:?}", p);
                        cosh.add_fs_allowlist_path(p.clone());
                    }
                    "find_glob" | "find_grep" => {
                        log::debug!("add_paths_to_allowlist find allowed: {:?}", p);
                        cosh.add_find_allowlist_path(p.clone());
                    }
                    _ => {}
                }
            }
        }
    }
}
