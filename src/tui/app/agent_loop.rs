use std::io;
use std::io::Write as _;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::App;
use crate::component::agent_spinner_bass::AgentSpinnerBass;
use crate::routes::session::queue_choice::QueueTarget;
use crate::session_store::generate_session_id;
use crate::types::SessionStatus;
use cosh::harness::HarnessEvent;

/// Grace window granted to the user when a Queue Actions box is opened:
/// during the next 5 seconds the TUI may not hand the next queued message
/// to the running loop nor auto-start a new one, giving the user time to
/// edit or delete a message that sits ahead of the effective send order.
pub(super) const QUEUE_ACTIONS_GRACE: Duration = Duration::from_secs(5);

/// Unique `call_id` suffix for Command-mode executions: one per dispatch, so
/// the hidden call/result pairs never collide in `update_ctx_ids` grouping or
/// any future call-id-uniqueness assumption (masking, chain-hiding).
static COMMAND_CALL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One tokio runtime for the whole process, shared by the agent loop and the
/// Command-mode executor: the LSP singleton's ingestion/auto-discovery tasks
/// and the language servers' transport tasks must outlive individual turns —
/// a per-turn runtime would kill them (and, via `kill_on_drop`, the server
/// processes) the moment a turn ends.
static AGENT_RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

impl App {
    /// True while the loop must not consume the next queued message: either
    /// a Queue Actions box is open (the queued indexes it carries must stay
    /// stable until its action runs) or the user is still inside the
    /// [`QUEUE_ACTIONS_GRACE`] window granted when the box was opened.
    pub(super) fn queue_actions_hold_active(&self) -> bool {
        if self.is_queue_actions_dialog_visible() {
            return true;
        }
        self.queue_actions_grace_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// Promote leftover "next request" messages into the "next agent loop"
    /// queue (FIFO, appended behind its existing items). An outstanding
    /// edit-requeue hint pointing at `next_request` — and an open Queue
    /// Actions box targeting it — are remapped so they keep identifying the
    /// same message after the promotion.
    pub(super) fn promote_next_request_to_next_loop(&mut self, id: &str) {
        let old_loop_len;
        {
            let queues = self.state.pending_queues.entry(id.to_string()).or_default();
            old_loop_len = queues.next_loop.len();
            while let Some(text) = queues.next_request.pop_front() {
                queues.next_loop.push_back(text);
            }
        }
        if let Some(hint) = &mut self.edit_requeue_hint
            && hint.session_id == id
            && hint.queue == QueueTarget::NextRequest
        {
            hint.index += old_loop_len;
            hint.queue = QueueTarget::NextLoop;
        }
        if let Some(d) = self.dialog.current_mut()
            && let crate::ui::dialogs::DialogType::QueueActions { queue, index, .. } =
                &mut d.dialog_type
            && *queue == QueueTarget::NextRequest
        {
            *index += old_loop_len;
            *queue = QueueTarget::NextLoop;
        }
    }

    /// Resume the pending queue chain: promote leftover "next request"
    /// messages and start a fresh loop with the head of "next agent loop".
    /// Returns true when a loop was started.
    fn resume_pending_chain(&mut self, id: &str) -> bool {
        self.promote_next_request_to_next_loop(id);
        let next = self
            .state
            .pending_queues
            .get_mut(id)
            .and_then(|q| q.next_loop.pop_front());
        match next {
            Some(msg) => {
                self.start_agent_loop(msg);
                true
            }
            None => false,
        }
    }

    /// Deliver queued messages to the running agent loop and start deferred
    /// loops — but only while the queue-actions hold
    /// ([`App::queue_actions_hold_active`]) is not active.
    ///
    /// "Next request" messages are forwarded ONE PER CALL so the next
    /// effective message is always the head of the queue and can be held,
    /// edited or deleted before it ever reaches the running loop. The deque
    /// entry is only retired when the harness acknowledges the injection
    /// (`UserMessageInjected`), so a message already sitting in the loop's
    /// channel can never be lost if the loop ends before injecting it.
    /// Called once per main-loop iteration from `run()`.
    pub(super) fn pump_queued_messages(&mut self) {
        if self.queue_actions_hold_active() {
            return;
        }

        // A clean loop end wanted to auto-start the first "next loop"
        // message but was held; run it now that the hold expired. The
        // promotion of leftover "next request" messages happens here too —
        // never while a Queue Actions box holds indexes into the deque.
        if self.queue_actions_deferred_start && self.state.status == SessionStatus::Idle {
            let current = self.state.current_session_id.clone();
            if self
                .active_loop_session_id
                .as_ref()
                .is_some_and(|owner| current.as_ref() != Some(owner))
            {
                // The user switched sessions — keep the flag armed so the
                // owning session's auto-start still fires when it (or any
                // session without a live owner) is back in front.
                return;
            }
            self.queue_actions_deferred_start = false;
            let Some(id) = current else {
                return;
            };
            self.resume_pending_chain(&id);
            return;
        }

        // Forward the head of the "next request" queue into the live loop's
        // channel; the harness injects it into the context before its next
        // request. FIFO is preserved: one in-flight message at a time, both
        // sides are queues, and the deque entry retires on acknowledgment.
        if self.state.status == SessionStatus::Working
            && !self.next_request_in_flight
            && let Some(owner) = self.active_loop_session_id.clone()
            && self.state.current_session_id.as_deref() == Some(owner.as_str())
            && let Some(text) = self
                .state
                .pending_queues
                .get(&owner)
                .and_then(|q| q.next_request.front().cloned())
            && let Some(tx) = &self.queued_input_tx
        {
            let _ = tx.send(text);
            self.next_request_in_flight = true;
        }
    }

    /// Start a full agent loop for `msg` (a user message that was just sent
    /// or dequeued from the pending "next agent loop" queue). Creates the
    /// session + history entry, spins the working state, and spawns the
    /// harness thread. Messages still pending in the "next request" queue
    /// stay queued: [`App::pump_queued_messages`] hands them to the loop's
    /// queued-input channel one at a time (honoring the queue-actions hold).
    pub(super) fn start_agent_loop(&mut self, msg: String) {
        // A real agent turn: the Done handler may do LLM work again (title
        // generation) — the per-turn Command-mode suppression ends here.
        self.command_mode_turn = false;

        // Command mode: the TUI is a plain terminal. The input is dispatched
        // straight to the bash tool — the agent loop (and with it the LLM)
        // never runs, so there is no connector to build either.
        if self.state.mode == cosh::harness::Mode::Command {
            self.start_command_execution(msg);
            return;
        }

        // Free gateway recommendation gate: a keyless send that would hit
        // a provider without credentials asks ONCE whether to use the free
        // gateway. The message is parked and only replayed on opt-in;
        // Esc closes without recording anything.
        if self.needs_gateway_recommendation() {
            self.pending_gateway_message = Some(msg);
            if let Some(content) = self.gateway_recommendation_content() {
                self.free_gateway_dialog.show(content);
            }
            return;
        }

        if self.state.current_session_id.is_none() {
            let id = generate_session_id();
            let title: String = msg.chars().take(40).collect();
            self.state.add_empty_session(
                id.clone(),
                title,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            );
            self.state.current_session_id = Some(id);
            // Fresh session: the header's cost widget starts clean — no
            // balance carried over from the previous session.
            self.hypercredit_balance = None;
            // First message from Home/after a deletion: the fresh session
            // starts with a CLEAN right panel, like every other session-
            // selection path (deleting the previous session left a stale,
            // populated panel behind on AppState).
            self.state.right_panel =
                crate::routes::session::right_panel::types::RightPanelState::new();
            // The session inherits the ACTIVE model config. Forcing the global
            // slot here would clobber a selection from the just-fired gateway
            // reroute (the replayed first message must go out on the free
            // model) or from the current UI state. At startup the active
            // config already IS the global selection (`App::new`), and
            // `/new` restores it explicitly.
            self.title_generated = false;
        }

        if let Some(session) = self.state.current_session_mut() {
            session.messages.push(crate::types::Message {
                id: format!("msg-{}", session.messages.len()),
                role: crate::types::MessageRole::User,
                parts: vec![crate::types::Part::Text(crate::types::TextPart {
                    text: msg.clone(),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });

            // Scroll to bottom when user sends a message (matches OpenCode's `toBottom()` on submit)
            self.session_view.scroll_to_bottom();
        }

        self.stop_signal.store(false, Ordering::Relaxed);
        self.state.status = crate::types::SessionStatus::Working;
        // Telemetry aggregate: one full user-request → final-answer cycle.
        self.telemetry_turn();
        // The loop starts in the generic state; the event intake narrows it
        // (pondering / searching / recalling) as harness events arrive.
        self.agent_activity = crate::types::AgentActivity::Working;
        self.agent_spinner_bass = Some(AgentSpinnerBass::new(
            crate::types::AgentActivity::Working.label(),
            &self.theme,
        ));

        let event_tx = self.event_tx.clone();
        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let reasoning = self.llm_config.reasoning.clone();
        let tool_call_mode = self.llm_config.tool_call_mode;
        let base_url = self.base_url_for(&provider);
        let local_base_urls = self.configured_local_base_urls();
        let fallbacks = self.router_view.fallbacks.clone();
        let summarization_models = self
            .setup
            .routing
            .summarization_models
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect();
        // Auto-rotate: move the first working fallback to the front so the
        // next message tries the provider that actually worked before wasting
        // time on failing ones. Rotation is in-memory only (not persisted).
        let fallbacks = if model.as_deref() == Some("auto") {
            let working = fallbacks
                .iter()
                .position(|fb| cosh_sdk::connector::Connector::new(&fb.provider).is_ok());
            match working {
                Some(0) | None => fallbacks,
                Some(idx) => {
                    let mut rotated = fallbacks;
                    rotated.rotate_left(idx);
                    self.router_view.fallbacks = rotated.clone();
                    rotated
                }
            }
        } else {
            fallbacks
        };
        let stop_signal = self.stop_signal.clone();
        let input = msg;
        let cwd = self.state.working_directory.clone();
        let mode = self.state.mode;

        // Session-level prompt-cache settings: the user's TTL/retention
        // choices (Settings screen) and the session id as the OpenAI
        // cache-affinity key — a routing hint, never user data.
        let anthropic_ttl_1h = self.setup.anthropic_cache_ttl_1h();
        let openai_cache_retention = self.setup.openai_cache_retention().map(String::from);
        let cache_key = self.state.current_session_id.clone();

        // Create a fresh answer channel for this agent loop invocation
        let (answer_tx, answer_rx) = mpsc::unbounded_channel();
        self.answer_tx = answer_tx;

        // Create a fresh permission channel for this agent loop invocation
        let (perm_tx, perm_rx) = mpsc::unbounded_channel();
        self.perm_tx = perm_tx;

        // Create a fresh queued-input channel for the "next request" queue.
        // The harness drains it before every request of THIS loop. Queued
        // messages are forwarded by `pump_queued_messages`, one at a time,
        // so an open Queue Actions box can always hold them back.
        let (queued_tx, queued_rx) = mpsc::unbounded_channel();
        self.queued_input_tx = Some(queued_tx.clone());
        self.active_loop_session_id = self.state.current_session_id.clone();

        let mut disabled_tools = self.internal_tools_view.disabled.clone();

        // Registered MCP servers (Settings screen → setup.json). Cloned
        // into the agent thread; the boot `connect_all` isolates
        // per-server failures, so one bad server never blocks the turn.
        let mcp_config = self.setup.mcp.clone();

        // Skill-discovery config (Settings screen → setup.json). Cloned
        // into the agent thread; resolved (tilde expansion + existence
        // check) when the wrapper is built below, inside the thread.
        let skills_config = self.setup.skills.clone();

        // Decision-model checkup config (Settings screen → setup.json).
        // Cloned into the agent thread; the resident model loads at
        // harness assembly time (never hot), inside the thread, exactly
        // once per assembly — fail-open disables the audit on load error.
        #[cfg(feature = "onnx")]
        let checkup_config = self.setup.checkup.clone();

        // RAG recall context
        // 1) Description suffix (what the model sees in the tool doc)
        #[cfg(feature = "embed")]
        let recall_suffix = self.recall_suffix();
        #[cfg(not(feature = "embed"))]
        let _recall_suffix = String::new();

        // 2) DB registry (what the dispatch uses to resolve db_name → connect)
        #[cfg(feature = "embed")]
        let recall_dbs: Vec<cosh::harness::tools::RecallDb> = self.recall_dbs_vec();
        #[cfg(not(feature = "embed"))]
        let _recall_dbs = std::vec::Vec::<()>::new();

        // 3) Auto-exclude tool if no databases exist at all
        self.maybe_disable_recall_tool(&mut disabled_tools);

        let event_tx_panic = event_tx.clone();

        // Load the session's persisted context state (the `Item` records of
        // the session JSONL) for session resumption. This is the ONLY
        // model-facing history source: the display transcript is never parsed
        // into the context. No context records (a fresh session, or a file
        // that is missing/corrupt) means the loop starts with an empty
        // context — the user is told via toast below.
        let ctx_state: Option<cosh::harness::ContextManagerState> = self
            .state
            .current_session_id
            .as_ref()
            .and_then(|id| self.session_store.load_context(id));
        self.warn_if_resuming_without_context(&ctx_state);

        std::thread::spawn(move || {
            use std::panic::AssertUnwindSafe;
            use std::sync::OnceLock;
            use tokio::runtime::Runtime;

            // One runtime for the whole process, not one per turn: the LSP
            // singleton's ingestion/auto-discovery tasks and the language
            // servers' transport tasks must outlive individual turns — a
            // per-turn runtime would kill them (and, via `kill_on_drop`, the
            // server processes) the moment a turn ends.
            static RUNTIME: OnceLock<Runtime> = OnceLock::new();
            let rt = RUNTIME.get_or_init(|| Runtime::new().expect("shared agent runtime"));

            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                rt.block_on(async {
                    use cosh::harness::Harness;
                    use cosh_sdk::connector::Connector;

                    let connector;
                    let mut remaining: Vec<(String, String)> = Vec::new();
                    // Apply the session-level prompt-cache settings to every
                    // connector this loop builds: TTL/retention choices,
                    // the cache-affinity key, the retention choice and the
                    // session-affinity headers. Each field is only read by
                    // the caller that implements it (Claude TTL, OpenAI
                    // key/retention, `x-session-affinity` routing), so
                    // applying them unconditionally is safe across fallback
                    // families. The session-affinity headers pin the whole
                    // session to one cache-warm backend (Charm Hyper et al.).
                    let apply_cache = |c: Connector| {
                        let c = c.with_prompt_cache_ttl_1h(anthropic_ttl_1h);
                        let c = match &cache_key {
                            Some(id) => c.with_prompt_cache_key(id),
                            None => c,
                        };
                        let c = match &cache_key {
                            Some(id) => c.with_session_id(super::session_affinity_id(id)),
                            None => c,
                        };
                        match &openai_cache_retention {
                            Some(retention) => c.with_prompt_cache_retention(retention),
                            None => c,
                        }
                    };
                    if model.as_deref() == Some("auto") {
                        let mut last_err = String::new();
                        let mut found = None;
                        for (i, fb) in fallbacks.iter().enumerate() {
                            match Connector::new(&fb.provider) {
                                Ok(c) => {
                                    let mut c =
                                        c.with_model(&fb.model).with_tool_call_mode(tool_call_mode);
                                    // Honor a configured local server URL for
                                    // this fallback provider, if any.
                                    if let Some(url) = local_base_urls.get(&fb.provider) {
                                        c = c.with_base_url(url.clone());
                                    }
                                    // The user chose a reasoning effort for
                                    // auto mode — apply it, mapped onto the
                                    // closest level THIS model accepts (a
                                    // model without the knob drops it). The
                                    // harness re-resolves per model on every
                                    // fallback switch.
                                    if let Some(r) = reasoning.as_deref()
                                        && let Some(resolved) =
                                            cosh_sdk::connector::resolve_reasoning_effort(
                                                &fb.model,
                                                r,
                                                Some("cosh/cache"),
                                            )
                                    {
                                        c = c.with_reasoning_effort(resolved);
                                    }
                                    found = Some((apply_cache(c), i));
                                    break;
                                }
                                Err(e) => {
                                    last_err = format!("connector for {}: {e}", fb.provider);
                                }
                            }
                        }
                        match found {
                            Some((c, idx)) => {
                                remaining = fallbacks[idx + 1..]
                                    .iter()
                                    .map(|fb| (fb.provider.clone(), fb.model.clone()))
                                    .collect();
                                connector = c;
                            }
                            None => {
                                let _ = event_tx.send(HarnessEvent::Error {
                                    message: format!("auto: no fallback available ({last_err})"),
                                    context: None,
                                    checkup_verdict: None,
                                });
                                return;
                            }
                        }
                    } else {
                        match Connector::new(&provider) {
                            Ok(c) => {
                                let with_model = if let Some(ref m) = model {
                                    c.with_model(m)
                                } else {
                                    c
                                };
                                // Honor a configured local server URL.
                                let with_model = if let Some(ref url) = base_url {
                                    with_model.with_base_url(url.clone())
                                } else {
                                    with_model
                                };
                                // Apply the user's tool-call mode (native /
                                // inline) — the two delivery paths are
                                // mutually exclusive at the request level.
                                let with_model = with_model.with_tool_call_mode(tool_call_mode);
                                let with_model = if let Some(ref r) = reasoning {
                                    // Map the chosen level onto the closest
                                    // one the model actually accepts (e.g.
                                    // "medium" on a low/high-only model) —
                                    // never send a knob that would 400 or be
                                    // silently ignored.
                                    let effective = model
                                        .as_deref()
                                        .and_then(|m| {
                                            cosh_sdk::connector::resolve_reasoning_effort(
                                                m,
                                                r,
                                                Some("cosh/cache"),
                                            )
                                        })
                                        .unwrap_or_else(|| r.clone());
                                    with_model.with_reasoning_effort(effective)
                                } else {
                                    with_model
                                };
                                connector = apply_cache(with_model);
                            }
                            Err(e) => {
                                let _ = event_tx.send(HarnessEvent::Error {
                                    message: format!("connector: {e}"),
                                    context: None,
                                    checkup_verdict: None,
                                });
                                return;
                            }
                        }
                    }

                    let mut harness = Harness::new(connector, &cwd, disabled_tools)
                        .with_mode(mode)
                        .with_fallbacks(remaining)
                        .with_summarization_models(summarization_models)
                        .with_local_base_urls(local_base_urls)
                        .with_skills(skills_config.to_skills())
                        .with_mcp_config(mcp_config);

                    // The resident decision model: loaded exactly once per
                    // harness assembly (never hot) and handed to the harness
                    // as a ready `Arc<dyn Checkup>`. Load failure fails open
                    // — the loop runs unaudited, exactly as without the
                    // feature. The harness never chooses which model to
                    // load; a future consumer of the engine receives the
                    // same resident.
                    #[cfg(feature = "onnx")]
                    if checkup_config.termination.enabled {
                        let kind = match &checkup_config.model {
                            crate::util::setup::CheckupModel::English => {
                                cosh_onnx::ModelKind::English
                            }
                            crate::util::setup::CheckupModel::Multilingual => {
                                cosh_onnx::ModelKind::Multilingual
                            }
                            crate::util::setup::CheckupModel::TypedDecisions => {
                                cosh_onnx::ModelKind::TypedDecisions
                            }
                            crate::util::setup::CheckupModel::Custom { repo, subfolder } => {
                                cosh_onnx::ModelKind::Custom {
                                    repo: repo.clone(),
                                    subfolder: subfolder.clone(),
                                }
                            }
                        };
                        use cosh::harness::events::{HarnessEvent, ToastVariant};
                        match cosh::harness::checkup::OnnxCheckup::load(kind) {
                            Some(checkup) => {
                                harness = harness
                                    .with_checkup(std::sync::Arc::new(checkup))
                                    .with_checkup_min_confidence(
                                        checkup_config.termination.min_confidence,
                                    );
                            }
                            None => {
                                let _ = event_tx.send(HarnessEvent::Toast {
                                    message: "Termination checkup enabled but the model \
                                              failed to load; the audit is disabled."
                                        .to_string(),
                                    variant: ToastVariant::Warning,
                                });
                            }
                        }
                    }

                    // Connect MCP servers before the header snapshot: the
                    // first extractor/native-tools view must see them.
                    if let Err(err) = harness.connect_mcp().await {
                        use cosh::harness::events::{HarnessEvent, ToastVariant};
                        let _ = event_tx.send(HarnessEvent::Toast {
                            message: format!("MCP config invalid: {err}"),
                            variant: ToastVariant::Warning,
                        });
                    }
                    // Per-server connection failures are isolated by the
                    // manager — surface them: a failed server used to show
                    // only as a silent footer count with no explanation.
                    // Detail (including which credential failed and why)
                    // lives in the snapshot's `last_error`; this toast is
                    // the pointer, one per failing server.
                    let failed: Vec<cosh::mcp::ServerSnapshot> = harness
                        .mcp_snapshots()
                        .into_iter()
                        .filter(|s| matches!(s.status, cosh::mcp::ServerStatus::Failed))
                        .collect();
                    if !failed.is_empty() {
                        use cosh::harness::events::{HarnessEvent, ToastVariant};
                        let detail: String = failed
                            .iter()
                            .map(|s| {
                                format!(
                                    "  · {}: {}",
                                    s.name,
                                    s.last_error.as_deref().unwrap_or("unknown error")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = event_tx.send(HarnessEvent::Toast {
                            message: format!(
                                "{} MCP server(s) failed to connect:\n{detail}",
                                failed.len()
                            ),
                            variant: ToastVariant::Error,
                        });
                    }

                    // Restore the authoritative context from the session
                    // file's context records. No context — no history; the
                    // display transcript is never fed to the model.
                    if let Some(ref state) = ctx_state {
                        harness.context_manager.restore_state(state);
                    }
                    #[cfg(feature = "embed")]
                    harness.set_recall_context(recall_suffix);
                    #[cfg(feature = "embed")]
                    harness.set_recall_dbs(recall_dbs);
                    harness.format_header_context();
                    harness
                        .run_agent_loop_with_queued_input(
                            &input,
                            event_tx,
                            answer_rx,
                            perm_rx,
                            stop_signal,
                            queued_rx,
                        )
                        .await;
                });
            }));

            if let Err(panic) = result {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                let _ = event_tx_panic.send(HarnessEvent::Error {
                    message: format!("panic: {msg}"),
                    context: None,
                    checkup_verdict: None,
                });
            }
        });
    }

    /// Command-mode executor: the TUI as a plain terminal. Dispatches the
    /// user-typed input straight to the bash tool — NO LLM call, no agent
    /// loop, no connector. The command and its output are recorded in the
    /// session context as HIDDEN items (persisted to the JSONL, never shown
    /// to the model), and the TUI renders the run through the existing
    /// bash_run event flow (ToolCall → ToolOutput stream → ToolResult).
    pub(super) fn start_command_execution(&mut self, cmd: String) {
        use cosh::harness::Tools;

        // Session bootstrap identical to the agent-loop path: the first
        // command still needs a session to live in.
        if self.state.current_session_id.is_none() {
            let id = generate_session_id();
            let title: String = cmd.chars().take(40).collect();
            self.state.add_empty_session(
                id.clone(),
                title,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            );
            self.state.current_session_id = Some(id);
            self.hypercredit_balance = None;
            self.state.right_panel =
                crate::routes::session::right_panel::types::RightPanelState::new();
        }
        // Command-mode turn: the Done handler skips ALL LLM work (notably
        // the async title generation — the mode's contract is that NO LLM
        // call is ever made) via `command_mode_turn`, WITHOUT latching
        // `title_generated`: a session that later receives real agent turns
        // still gets its semantic title.
        self.command_mode_turn = true;

        // C1: route the Done save to THIS session. Without an owner the
        // handler falls back to a stale `active_loop_session_id` from a
        // previous agent loop and would write this session's context records
        // into that unrelated session's JSONL.
        self.active_loop_session_id = self.state.current_session_id.clone();

        // Display transcript: the command line as a user message (the
        // "terminal echo").
        if let Some(session) = self.state.current_session_mut() {
            session.messages.push(crate::types::Message {
                id: format!("msg-{}", session.messages.len()),
                role: crate::types::MessageRole::User,
                parts: vec![crate::types::Part::Text(crate::types::TextPart {
                    text: cmd.clone(),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });
            self.session_view.scroll_to_bottom();
        }

        self.stop_signal.store(false, Ordering::Relaxed);
        self.state.status = SessionStatus::Working;
        self.agent_activity = crate::types::AgentActivity::for_tool("bash_run");
        self.agent_spinner_bass = Some(AgentSpinnerBass::new(
            crate::types::AgentActivity::for_tool("bash_run").label(),
            &self.theme,
        ));
        // The PTY panel entry is created by the ToolCall event handler below
        // (exactly like an agent-loop bash_run) — starting it here too would
        // leave a second, forever-Running entry in the panel.

        let event_tx = self.event_tx.clone();
        let stop_signal = self.stop_signal.clone();
        let cwd = self.state.working_directory.clone();
        let session_id = self.state.current_session_id.clone();
        let command = cmd;
        // Unique per-execution call_id: the hidden pairs must never collide
        // (grouping, masking and any call-id-uniqueness assumption).
        let call_id = format!(
            "cmd-user-{}",
            COMMAND_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );

        // The authoritative context state is loaded on the UI thread (the
        // session store is not shared with the worker task); the hidden
        // recording happens inside the task, on top of the restored state.
        let ctx_state = session_id
            .as_ref()
            .and_then(|id| self.session_store.load_context(id));

        let rt = AGENT_RUNTIME
            .get_or_init(|| tokio::runtime::Runtime::new().expect("shared agent runtime"));
        rt.spawn(async move {
            // Command-mode context: the restored session history plus the
            // hidden command item. The model never sees any of it.
            let mut context =
                cosh::harness::ContextManager::new(cosh::harness::context::MAX_CONTEXT_TOKENS);
            if let Some(state) = ctx_state {
                context.restore_state(&state);
            }
            context.add_hidden_user_command(&command);

            // Emit the ToolCall event so the TUI attaches the running tool
            // part exactly like an agent-loop bash_run dispatch.
            let _ = event_tx.send(HarnessEvent::ToolCall {
                tool: "bash_run".to_string(),
                input: serde_json::json!({ "command": command }),
            });

            let mut tools = cosh::harness::CoshTools::new(&cwd);
            tools.set_event_tx(event_tx.clone());
            tools.set_stop_signal(stop_signal.clone());

            // ESC/Interrupt sets the stop signal; racing it against the
            // dispatch makes the command cancellable (the losing branch is
            // dropped, which tears down the PTY stream).
            let stop_wait = async {
                while !stop_signal.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            let result = tokio::select! {
                r = tools.dispatch("bash_run", serde_json::json!({ "command": command })) => r,
                _ = stop_wait => Err("interrupted".to_string()),
            };

            // Record the execution (call + result) as HIDDEN items in both
            // paths: persisted to the session JSONL, never model-visible.
            match result {
                Ok(output) => {
                    let _ = event_tx.send(HarnessEvent::ToolResult {
                        output: output.clone(),
                    });
                    context.add_hidden_command_execution(&call_id, &command, &output);
                }
                Err(err) => {
                    let _ = event_tx.send(HarnessEvent::ToolError { error: err.clone() });
                    context.add_hidden_command_execution(&call_id, &command, &err);
                }
            }

            // Finish through the standard Done path: the TUI persists the
            // session WITH this context snapshot (hidden items included) and
            // resets the status — no LLM was ever involved.
            let _ = event_tx.send(HarnessEvent::Done {
                context: context.save_state(),
                checkup_verdict: None,
            });
        });
    }

    /// A persisted session whose context records went missing (deleted session
    /// file, corrupt payload) would silently "forget" the whole conversation —
    /// JSONL: a brand-new session's FIRST prompt also has a message on screen
    /// with no context records yet (they are only written by Done/Stopped/
    /// snapshot saves) — that is normal, not data loss. Known interim noise:
    /// an early TUI-side failure persists the display messages alone, so a
    /// resend warns even though nothing reached the model either (accepted;
    /// TODO.md task 4 routes more saves through the context-aware path).
    pub(super) fn warn_if_resuming_without_context(
        &mut self,
        ctx_state: &Option<cosh::harness::ContextManagerState>,
    ) {
        let Some(id) = self.state.current_session_id.as_ref() else {
            return;
        };
        if ctx_state.is_none()
            && self.session_store.has_session(id)
            && self
                .state
                .current_session()
                .is_some_and(|s| !s.messages.is_empty())
        {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("Context lost".into()),
                message: "Session context not found — the conversation \
                          starts without history."
                    .into(),
                variant: ToastVariant::Warning,
                duration_ms: 6000,
            });
        }
    }

    /// Called when an agent loop terminates. Leftover "next request"
    /// messages that were never injected are promoted to the "next agent
    /// loop" queue (they behave like it: they wait for the next run). When
    /// `start_next` is true (the run ended cleanly — Done or Stopped) the
    /// first queued next-loop message starts a fresh loop (FIFO). If the
    /// queue-actions hold is active, that start is deferred instead:
    /// [`App::pump_queued_messages`] starts it once the user closes the
    /// actions box and the grace window expires. On Error the queues
    /// stay parked: the user decides when to resend (e.g. after switching the
    /// model) — a later manual message rolls them in.
    pub(super) fn handle_loop_end(&mut self, start_next: bool) -> bool {
        self.queued_input_tx = None;
        let Some(id) = self.state.current_session_id.clone() else {
            return false;
        };
        if self.active_loop_session_id.is_some()
            && self.active_loop_session_id.as_deref() != Some(id.as_str())
        {
            // The user switched sessions while the loop ran — leave the queues
            // untouched for the session that owns them (and do NOT touch the
            // viewed session's deferred-start flag).
            return false;
        }
        // Any in-flight message was never confirmed injected: its deque
        // entry is still the head, so it simply takes part in whatever
        // happens to the queue below.
        self.next_request_in_flight = false;
        if start_next && self.queue_actions_hold_active() {
            // The user is operating on queued messages: neither the
            // promotion nor the auto-start may touch the queues (stored
            // indexes must stay valid until the action runs). The pump
            // performs both once the hold expires.
            let pending = self
                .state
                .pending_queues
                .get(&id)
                .is_some_and(|q| q.queued_count() > 0);
            self.queue_actions_deferred_start = pending;
            return pending;
        }
        self.queue_actions_deferred_start = false;
        // Promotion also remaps an open Queue Actions box / edit hint, so
        // stored indexes stay valid on both paths.
        self.promote_next_request_to_next_loop(&id);
        let next = if start_next {
            self.state
                .pending_queues
                .get_mut(&id)
                .and_then(|q| q.next_loop.pop_front())
        } else {
            // Error path: park the queues — no auto-start.
            None
        };
        // Row indexes shifted after the promotion — drop the highlight.
        self.hovered_queue_row = None;
        match next {
            Some(msg) => {
                self.start_agent_loop(msg);
                true
            }
            None => false,
        }
    }

    /// Rings the terminal bell (BEL) so the user notices the agent loop ended.
    pub(super) fn trigger_bell(&self) {
        if !self.bell_enabled {
            return;
        }
        let mut out = io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
    }
}
