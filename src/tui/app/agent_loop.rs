use std::io;
use std::io::Write as _;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;

use super::App;
use crate::component::agent_spinner::AgentSpinner;
use crate::session_store::generate_session_id;
use cosh::harness::HarnessEvent;

impl App {
    /// Start a full agent loop for `msg` (a user message that was just sent
    /// or dequeued from the pending "next agent loop" queue). Creates the
    /// session + history entry, spins the working state, and spawns the
    /// harness thread. Messages still pending in the "next request" queue
    /// are handed to the new loop's queued-input channel so they enter its
    /// first request.
    pub(super) fn start_agent_loop(&mut self, msg: String) {
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
        self.agent_spinner = Some(AgentSpinner::new("Working", &self.theme));

        let event_tx = self.event_tx.clone();
        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let reasoning = self.llm_config.reasoning.clone();
        let tool_call_mode = self.llm_config.tool_call_mode;
        let base_url = self.base_url_for(&provider);
        let local_base_urls = self.configured_local_base_urls();
        let fallbacks = self.router_view.fallbacks.clone();
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

        // Create a fresh answer channel for this agent loop invocation
        let (answer_tx, answer_rx) = mpsc::unbounded_channel();
        self.answer_tx = answer_tx;

        // Create a fresh permission channel for this agent loop invocation
        let (perm_tx, perm_rx) = mpsc::unbounded_channel();
        self.perm_tx = perm_tx;

        // Create a fresh queued-input channel for the "next request" queue.
        // The harness drains it before every request of THIS loop.
        let (queued_tx, queued_rx) = mpsc::unbounded_channel();
        self.queued_input_tx = Some(queued_tx.clone());
        self.active_loop_session_id = self.state.current_session_id.clone();

        // Carry over messages that were queued for the next request while no
        // loop was running (e.g. the previous run ended in an error and the
        // user typed a fresh message): they enter this loop's first request.
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(queues) = self.state.pending_queues.get(&id)
        {
            for text in &queues.next_request {
                let _ = queued_tx.send(text.clone());
            }
        }

        let mut disabled_tools = self.internal_tools_view.disabled.clone();

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
        // Build conversation history from existing session messages
        let history: Vec<(String, String)> = self
            .state
            .current_session()
            .map(|s| {
                s.messages
                    .iter()
                    .filter_map(|m| {
                        let role = match m.role {
                            crate::types::MessageRole::User => "user",
                            crate::types::MessageRole::Assistant => "assistant",
                        };
                        let text: String = m
                            .parts
                            .iter()
                            .filter_map(|p| match p {
                                crate::types::Part::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if text.is_empty() {
                            None
                        } else {
                            Some((role.to_owned(), text))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Load companion context state (.ctx file) for session resumption
        let ctx_bytes: Option<Vec<u8>> = self
            .state
            .current_session_id
            .as_ref()
            .and_then(|id| self.session_store.load_ctx(id));

        std::thread::spawn(move || {
            use std::panic::AssertUnwindSafe;
            use tokio::runtime::Builder;

            let rt = match Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = event_tx.send(HarnessEvent::Error(format!("runtime: {e}")));
                    return;
                }
            };

            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                rt.block_on(async {
                    use cosh::harness::Harness;
                    use cosh_sdk::connector::Connector;

                    let connector;
                    let mut remaining: Vec<(String, String)> = Vec::new();
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
                                    found = Some((c, i));
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
                                let _ = event_tx.send(HarnessEvent::Error(format!(
                                    "auto: no fallback available ({last_err})"
                                )));
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
                                connector = if let Some(ref r) = reasoning {
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
                            }
                            Err(e) => {
                                let _ =
                                    event_tx.send(HarnessEvent::Error(format!("connector: {e}")));
                                return;
                            }
                        }
                    }

                    let mut harness = Harness::new(connector, &cwd, disabled_tools)
                        .with_mode(mode)
                        .with_history(&history)
                        .with_fallbacks(remaining)
                        .with_local_base_urls(local_base_urls);

                    // Restore compressed context state from .ctx companion file
                    if let Some(ref ctx_bytes) = ctx_bytes {
                        use cosh::harness::ContextManagerState;
                        if let Ok(state) = bincode::deserialize::<ContextManagerState>(ctx_bytes) {
                            harness.context_manager.restore_state(&state);
                        }
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
                let _ = event_tx_panic.send(HarnessEvent::Error(format!("panic: {msg}")));
            }
        });
    }

    /// Called when an agent loop terminates. Leftover "next request"
    /// messages that were never injected are promoted to the "next agent
    /// loop" queue (they behave like it: they wait for the next run). When
    /// `start_next` is true (the run ended cleanly — Done or Stopped) the
    /// first queued next-loop message starts a fresh loop. On Error the queues
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
            // untouched for the session that owns them.
            return false;
        }
        let next: Option<String> = {
            let queues = self.state.pending_queues.entry(id).or_default();
            while let Some(text) = queues.next_request.pop_front() {
                queues.next_loop.push_back(text);
            }
            if start_next {
                queues.next_loop.pop_front()
            } else {
                None
            }
        };
        if let Some(msg) = next {
            self.start_agent_loop(msg);
            true
        } else {
            false
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
