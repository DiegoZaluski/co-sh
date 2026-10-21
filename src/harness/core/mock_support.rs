//! Test-only mock support: the cfg(test) `impl Harness` with the mock
//! connector builders, scripted chat/stream queues and test accessors.

use super::*;

#[cfg(test)]
impl Harness {
    /// Create a harness for testing without a real connector.
    pub(crate) fn new_test() -> Self {
        let connector = Connector::new("openai")
            .unwrap()
            // Test mocks drive the stream with TEXT tokens (the inline-JSON
            // path), so the test harness defaults to Inline mode — matching
            // the historical behavior of every mock-driven test. Production
            // `Harness::new` keeps the connector default (Native).
            .with_tool_call_mode(ToolCallMode::Inline);
        Self {
            connector,
            mcp: McpManager::new(),
            mcp_config: McpConfig::default(),
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: None,
            pending_images: Vec::new(),
            attempt_reasoning: String::new(),
            mode: Mode::Build,
            instructions: None,
            stop_signal: None,
            reasoning_tx: None,
            pending_thinking_blocks: Vec::new(),
            stop: false,
            tool_issuer: VecDeque::new(),
            active_call_ref: None,
            nested: false,
            panes_requester: super::panes::MAIN,
            panes_own_id: None,
            tool_call_synthetic: 0,
            context_manager: ContextManager::new(MAX_CONTEXT_TOKENS),
            disabled_tools: HashSet::new(),
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            last_finish_reason: None,
            last_stream_unfinished: false,
            stop_nudges_left: super::limits::STOP_NUDGE_BUDGET,
            prefix_cache_probe: PrefixCacheProbe::default(),
            #[cfg(test)]
            promoted_reasoning_log: Vec::new(),
            correction_memory: CorrectionMemory::new(5),
            agent_permissions: HashSet::new(),
            approved_paths: HashSet::new(),
            fallbacks: Vec::new(),
            checkup: None,
            // Measured, not conventional: the confidence-floor sweep over the
            // `loop_termination` corpus (`confidence_floor_sweep` in
            // crates/cosh-onnx/tests/eval/loop_termination.rs) — 0.75 keeps
            // the sweep's optimum (23/24 correct) on the zero-false-continue
            // edge: no legitimate ending is vetoed, the single error is one
            // missed stop. Lower floors trade that for false continues
            // (0.70 → 1, 0.60 → 3); 0.80+ starts missing real stops in bulk
            // (5 at 0.80, 7 at 0.85).
            checkup_min_confidence: 0.75,
            summarization_models: Vec::new(),
            summarization_connector: None,
            summarization_window: None,
            compaction_interrupted: false,
            mock_compaction_models: Vec::new(),
            mock_summarization_windows: std::collections::HashMap::new(),
            local_base_urls: std::collections::HashMap::new(),
            last_context_window: None,
            discovered_window: None,
            effective_context: false,
            last_overflow_toast: None,
            hook_runner: None,
            lsp: None,
            compaction_generic_retries: 0,
            snapshot_interval: std::time::Duration::from_secs(10),
            mock_chat_response: None,
            mock_chat_queue: VecDeque::new(),
            mock_map_delay_queue: VecDeque::new(),
            mock_stream_queue: VecDeque::new(),
            mock_reasoning_queue: VecDeque::new(),
            mock_stream_delay_ms: 0,
            mock_native_stream_queue: VecDeque::new(),
            #[cfg(test)]
            mock_finish_reasons: VecDeque::new(),
            #[cfg(test)]
            mock_stream_resets: VecDeque::new(),
            test_tools: Vec::new(),
        }
    }

    /// Override the tool-call delivery mode (test-only; the production path
    /// is set on the connector by the caller).
    #[cfg(test)]
    pub(crate) fn with_tool_call_mode(mut self, mode: ToolCallMode) -> Self {
        self.connector = self.connector.with_tool_call_mode(mode);
        self
    }

    /// Register a tool schema for testing extraction without needing a real MCP session.
    pub(crate) fn with_test_tool(mut self, name: &str, schema: serde_json::Value) -> Self {
        self.test_tools.push(ToolSchema {
            name: name.to_string(),
            input_schema: schema,
            example_args: None,
        });
        self
    }

    /// Set a mock response for `chat()`. `Ok(text)` simulates a successful reply;
    /// `Err(msg)` simulates a connector failure.
    pub(crate) fn with_mock_chat(mut self, response: Result<&str, &str>) -> Self {
        self.mock_chat_response = Some(response.map(|s| s.to_string()).map_err(|s| s.to_string()));
        self
    }

    /// The summarizer calls made so far — one `(provider, model)` per call.
    /// Test accessor for test modules outside `core.rs` (e.g. `test/`).
    pub(crate) fn mock_compaction_calls(&self) -> &[(String, String)] {
        &self.mock_compaction_models
    }

    /// Replace the agent connector (test-only): points the loop and the
    /// summarizer at a local streaming server so the REAL SDK path runs with
    /// no test mocks in the way.
    pub(crate) fn with_connector(mut self, connector: Connector) -> Self {
        self.connector = connector;
        self
    }

    /// Queue sequential mock CHAT (summarizer) responses, consumed one per
    /// call; when the queue is empty the single [`Self::with_mock_chat`]
    /// response is used. Lets a test simulate a SEQUENCE of summarizer
    /// outcomes — e.g. a failing single-shot compaction that reports its
    /// window (the `"{CONTEXT_WINDOW_MARKER}:{window}"` error convention
    /// parsed in [`Self::stream_summarize_for_compaction`]) followed by the
    /// successful split chunks of the reactive fork.
    pub(crate) fn with_mock_chats(mut self, responses: Vec<Result<&str, &str>>) -> Self {
        for response in responses {
            self.mock_chat_queue
                .push_back(response.map(|s| s.to_string()).map_err(|s| s.to_string()));
        }
        self
    }

    /// Delay each assigned parallel map response by the corresponding number
    /// of milliseconds. This lets tests force completion order independently
    /// from stable segment ordinals.
    pub(crate) fn with_mock_map_delays(mut self, delays: &[u64]) -> Self {
        self.mock_map_delay_queue.extend(delays.iter().copied());
        self
    }

    /// Resolve the next mock CHAT (summarizer) response: the queued one when
    /// present, else the single repeated [`Self::mock_chat_response`].
    #[cfg(test)]
    pub(super) fn next_mock_chat(&mut self) -> Option<Result<String, String>> {
        let response = self
            .mock_chat_queue
            .pop_front()
            .or_else(|| self.mock_chat_response.clone());
        if response.is_some() {
            let connector = self.compaction_connector();
            self.mock_compaction_models.push((
                connector.provider_name().unwrap_or("?").to_string(),
                connector.effective_model().unwrap_or("?").to_string(),
            ));
        }
        response
    }

    /// Set the KNOWN context window of the active model, as if discovery had
    /// succeeded — lets tests drive the hierarchical MapReduce contingency (a
    /// known window with a held context that exceeds it).
    pub(crate) fn with_discovered_window(mut self, window: usize) -> Self {
        self.discovered_window = Some(window);
        self
    }

    /// Set mock tokens for `stream_chat()`. `Ok(tokens)` simulates successful
    /// streaming; `Err(msg)` simulates a stream start failure.
    pub(crate) fn with_mock_stream(mut self, response: Result<Vec<&str>, &str>) -> Self {
        self.mock_stream_queue.push_back(
            response
                .map(|v| v.into_iter().map(|s| s.to_string()).collect())
                .map_err(|s| s.to_string()),
        );
        self
    }

    /// Queue multiple sequential mock stream responses.
    ///
    /// Each agent-loop iteration pops the next response. When the queue runs
    /// empty the loop falls through to the real connector (which fails fast
    /// without an API key), letting tests observe how many iterations ran.
    pub(crate) fn with_mock_streams(mut self, responses: Vec<Result<Vec<&str>, &str>>) -> Self {
        for response in responses {
            self.mock_stream_queue.push_back(
                response
                    .map(|v| v.into_iter().map(|s| s.to_string()).collect())
                    .map_err(|s| s.to_string()),
            );
        }
        self
    }

    /// Test-only per-token pause for the mock stream (ms). Lets a test
    /// interleave with the streamed tokens — e.g. set the stop signal
    /// mid-stream and assert the loop surfaces `Stopped` (never a spurious
    /// `Done` from a truncated turn).
    #[cfg(test)]
    pub(crate) fn with_mock_stream_delay_ms(mut self, ms: u64) -> Self {
        self.mock_stream_delay_ms = ms;
        self
    }

    /// Queue mock REASONING chunks for the NEXT stream (parallel to
    /// [`Self::with_mock_streams`]): one entry per mock stream, popped in
    /// order. Each entry's strings ride the SAME accumulation +
    /// `HarnessEvent::Reasoning` path the real stream uses, BEFORE that
    /// stream's text tokens — so a test can drive the full
    /// reasoning-persistence contract (capture → context item → session
    /// replay) end to end without a real provider.
    #[cfg(test)]
    pub(crate) fn with_mock_reasonings(mut self, chunks: Vec<Vec<&str>>) -> Self {
        for entry in chunks {
            self.mock_reasoning_queue
                .push_back(entry.into_iter().map(|s| s.to_string()).collect());
        }
        self
    }

    /// Test-only: install a stop signal directly (mirrors the wiring
    /// `run_agent_loop_inner` / `compact_on_demand` perform) so tests can
    /// exercise the stream functions' stop contract in isolation.
    #[cfg(test)]
    pub(crate) fn set_stop_signal_for_test(&mut self, stop_signal: Arc<AtomicBool>) {
        self.stop_signal = Some(stop_signal);
    }

    /// Queue NATIVE (structured) tool calls for the NEXT stream, delivered
    /// right after its string tokens — mirrors a provider emitting
    /// `tool_calls`/`tool_use`/`functionCall` parts. Consumed one list per
    /// stream.
    #[cfg(test)]
    pub(crate) fn with_mock_native_calls(
        mut self,
        calls: Vec<cosh_sdk::extract_action::NativeToolCall>,
    ) -> Self {
        self.mock_native_stream_queue.push_back(calls);
        self
    }

    /// Queue a mock `finish_reason` for the NEXT stream (consumed one per
    /// stream). Call once per stream to simulate truncation on just the first
    /// response and a normal completion on the following one.
    #[cfg(test)]
    pub(crate) fn with_mock_finish_reason(mut self, reason: Option<&str>) -> Self {
        self.mock_finish_reasons.push_back(reason.map(String::from));
        self
    }

    /// Queue a mock reset marker for the NEXT stream: the mock path emits
    /// [`StreamEvent::Reset`] before the tokens, driving the same
    /// discard-then-restart flow the SDK's mid-stream retry produces.
    #[cfg(test)]
    pub(crate) fn with_mock_stream_reset(mut self) -> Self {
        self.mock_stream_resets.push_back(true);
        self
    }

    /// Shrink the incremental-persistence snapshot cadence so a fast mock run
    /// emits a [`HarnessEvent::ContextSnapshot`] per tool dispatch. The real
    /// default (10s) throttles the serialization cost on long runs.
    #[cfg(test)]
    pub(crate) fn with_snapshot_interval(mut self, interval: std::time::Duration) -> Self {
        self.snapshot_interval = interval;
        self
    }

    /// Build the exact messages array that would be sent to the LLM on the
    /// next stream call. Mirrors what `run_agent_loop` sends each iteration.
    pub(crate) fn build_messages_for_test(&mut self, input: &str) -> Vec<ChatMessage> {
        self.context_manager.build_messages(input)
    }

    /// Build the system context (instructions + correction memory) exactly as
    /// `run_agent_loop` does for each iteration.
    pub(crate) fn build_chat_context_for_test(&mut self) -> String {
        self.build_chat_context()
    }

    /// Whether the currently-active connector has native tool definitions
    /// registered. Used to prove that fallback switches drop them.
    pub(crate) fn connector_has_tools(&self) -> bool {
        self.connector.has_tools()
    }

    /// Provider name of the currently-active connector (test accessor).
    pub(crate) fn connector_provider(&self) -> Option<&'static str> {
        self.connector.provider_name()
    }

    pub(crate) fn push_tool_call(&mut self, tc: ToolCallData) {
        self.tool_issuer.push_back(tc);
    }
}
