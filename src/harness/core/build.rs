//! Construction and configuration: the `Harness` constructor, the
//! `with_*`/`set_*` builder chain and the MCP/test-transport wiring.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::mcp::{McpConfig, McpManager};

use cosh_sdk::connector::Connector;

use crate::harness::tools::CoshTools;

use super::{
    ContextManager, CorrectionMemory, Harness, MAX_CONTEXT_TOKENS, Mode, PrefixCacheProbe,
    PromptSystem, SUBAGENT_INTERNAL_NOTE, build_lsp, default_harness_tools,
    limits::{PANE_RESUME_BUDGET, STOP_NUDGE_BUDGET},
};

impl Harness {
    #[must_use]
    pub fn new(connector: Connector, cwd: &str, disabled_tools: HashSet<String>) -> Self {
        let mut cosh_tools = CoshTools::new(cwd);
        // Interpolate the internal-sub-agent note into the `subagent_call`
        // tool description BEFORE the header is built: omitting `agent`
        // routes the call to an internal agent instead of an external CLI.
        cosh_tools.set_subagent_note(SUBAGENT_INTERNAL_NOTE);
        let mut harness = Self {
            connector,
            mcp: McpManager::new(),
            mcp_config: McpConfig::default(),
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: Some(cosh_tools),
            pending_images: Vec::new(),
            attempt_reasoning: String::new(),
            #[cfg(test)]
            mock_reasoning_queue: VecDeque::new(),
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
            disabled_tools,
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            last_finish_reason: None,
            last_stream_unfinished: false,
            stop_nudges_left: STOP_NUDGE_BUDGET,
            pane_resumes_left: PANE_RESUME_BUDGET,
            #[cfg(test)]
            stop_nudge_budget_override: None,
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
            #[cfg(test)]
            mock_compaction_models: Vec::new(),
            #[cfg(test)]
            mock_summarization_windows: std::collections::HashMap::new(),
            local_base_urls: std::collections::HashMap::new(),
            last_context_window: None,
            discovered_window: None,
            effective_context: false,
            hook_runner: None,
            lsp: build_lsp(cwd),
            last_overflow_toast: None,
            compaction_generic_retries: 0,
            snapshot_interval: std::time::Duration::from_secs(10),
            #[cfg(test)]
            mock_chat_response: None,
            #[cfg(test)]
            mock_chat_queue: VecDeque::new(),
            #[cfg(test)]
            mock_map_delay_queue: VecDeque::new(),
            #[cfg(test)]
            mock_stream_queue: VecDeque::new(),
            #[cfg(test)]
            mock_stream_delay_ms: 0,
            #[cfg(test)]
            mock_native_stream_queue: VecDeque::new(),
            #[cfg(test)]
            mock_finish_reasons: VecDeque::new(),
            #[cfg(test)]
            mock_stream_resets: VecDeque::new(),
            #[cfg(test)]
            test_tools: Vec::new(),
        };
        harness.install_summary_annotator();
        // The /agents pane tool is registered ONLY when panes exist: with
        // no agents bound to the session the harness — and its header —
        // stays byte-identical with the pre-feature one. No tool, no
        // context block, no interpolated noise for something that does
        // not exist.
        if super::panes::has_panes() {
            harness.harness_tools.push(super::panes::agents_call_tool());
            harness
                .harness_tools
                .push(super::panes::agents_worktree_tool());
        }
        harness
    }

    /// Load previous conversation turns into the context manager (the single
    /// owner of the conversation) so the assistant sees context when it starts.
    #[must_use]
    pub fn with_history(mut self, turns: &[(String, String)]) -> Self {
        for (role, text) in turns {
            if role == "user" {
                self.context_manager.add_user(text);
            } else {
                // Loaded assistant turns are prose and closable.
                self.context_manager.add_assistant(text, true);
            }
        }
        self
    }

    /// Provide fallback (provider, model) pairs for automatic retry when
    /// the current connector's API call fails (e.g. MissingApiKey, HttpError).
    /// The harness pops and tries each fallback in order; only when all are
    /// exhausted is the error forwarded to the TUI.
    #[must_use]
    pub fn with_fallbacks(mut self, fallbacks: Vec<(String, String)>) -> Self {
        self.fallbacks = fallbacks;
        self
    }

    /// Attach the decision-model audit seam (`harness::checkup`). The
    /// harness never chooses which model to load — the app layer hands over
    /// the ready, shared resident; without one the loop is unaudited.
    #[must_use]
    pub fn with_checkup(mut self, checkup: Arc<dyn super::checkup::Checkup>) -> Self {
        self.checkup = Some(checkup);
        self
    }

    /// The consumer floor for the termination audit (from
    /// `setup.model_decision.termination.min_confidence`): a `NotTerminated`
    /// verdict only vetoes a natural completion when its calibrated
    /// confidence reaches this threshold.
    ///
    /// The floor is clamped to `[0.0, 1.0]`: a user-set value above 1.0
    /// would silently disable the veto entirely, and a negative one would
    /// make even a ~0.0-confidence verdict continue the loop.
    #[must_use]
    pub fn with_checkup_min_confidence(mut self, min_confidence: f64) -> Self {
        self.checkup_min_confidence = min_confidence.clamp(0.0, 1.0);
        self
    }

    /// Map of local provider name → base URL configured by the user (from
    /// setup.json). Applied when a fallback connector is built for a local
    /// provider so it hits the user's server instead of the default port.
    #[must_use]
    pub fn with_local_base_urls(mut self, urls: std::collections::HashMap<String, String>) -> Self {
        self.local_base_urls = urls.clone();
        #[cfg(feature = "embed")]
        if let Some(cosh_tools) = self.cosh_tools.take() {
            self.cosh_tools = Some(cosh_tools.with_local_base_urls(urls));
        }
        self
    }

    /// Configure skill discovery from a prebuilt skills wrapper (built by
    /// the TUI from the setup.json `skills` section; the plain
    /// `CoshTools::new` default already covers `~/.skills`). Nested
    /// sub-agent harnesses inherit the parent's wrapper.
    #[must_use]
    pub fn with_skills(mut self, skills: cosh_tools::skills::Skills) -> Self {
        if let Some(tools) = self.cosh_tools.as_mut() {
            tools.set_skills(skills);
        }
        self
    }

    /// Set lifecycle hooks from config entries: `pre` fires before a tool
    /// call, `post` after its successful execution.
    pub fn with_hooks(
        mut self,
        pre: &[super::hooks::HookConfig],
        post: &[super::hooks::HookConfig],
        cwd: &str,
    ) -> Self {
        if !pre.is_empty() || !post.is_empty() {
            self.hook_runner = Some(super::hooks::HookRunner::new(pre, post, cwd));
        }
        self
    }

    /// Registered MCP servers to connect, populated from `Setup.mcp` by the
    /// TUI (see [`Self::with_mcp_config`]) and booted via
    /// [`Self::connect_mcp`].
    #[must_use]
    pub fn with_mcp_config(mut self, config: McpConfig) -> Self {
        self.mcp_config = config;
        self
    }

    /// Whether discovered windows use the reduced sweet-spot budget,
    /// populated from `Setup.effective_context` by the TUI.
    #[must_use]
    pub fn with_effective_context(mut self, effective: bool) -> Self {
        self.effective_context = effective;
        self
    }

    /// Connect every enabled server from [`Self::with_mcp_config`].
    /// Per-server failures are isolated by the manager and never abort the
    /// loop; the TUI reads them back via status snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error only when the whole section is invalid (e.g.
    /// duplicate server names).
    pub async fn connect_mcp(&mut self) -> Result<(), crate::mcp::McpError> {
        let config = std::mem::take(&mut self.mcp_config);
        let outcome = self.mcp.connect_all(&config).await;
        self.mcp_config = config;
        outcome
    }

    /// Attach an in-memory MCP transport for tests (duplex servers built by
    /// `harness::test`). Production code connects via [`Self::connect_mcp`].
    #[cfg(test)]
    pub(crate) async fn attach_test_transport<T, E, A>(
        &mut self,
        name: &str,
        transport: T,
    ) -> Result<(), String>
    where
        T: rmcp::transport::IntoTransport<rmcp::service::RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        use crate::mcp::{McpServerEntry, McpTransport, StdioTransport};
        // Mirror `connect_one` hygiene: drop a stale client before the new
        // handshake so re-attaching a name never leaks the old service.
        self.mcp.disconnect(name).await;
        self.mcp.insert_test_entry(McpServerEntry {
            name: name.to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "test".to_string(),
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        });
        // A duplex transport serves exactly one dial, so hand it out once.
        // A second dial (era flip/retry) fails here with
        // `Connect("test transport already consumed")`, which carries no
        // era evidence and replaces the original `EraStale` — test-only
        // masking, never hit in production where factories spawn fresh
        // transports per dial.
        let mut transport = Some(transport);
        self.mcp
            .register_with_retry(
                name,
                || {
                    transport.take().ok_or_else(|| {
                        crate::mcp::McpError::Connect(
                            name.to_string(),
                            "test transport already consumed".to_string(),
                        )
                    })
                },
                std::time::Duration::from_millis(200),
                crate::mcp::manager::ProbePolicy::STDIO,
            )
            .await
            .map_err(|err| err.to_string())
    }

    #[must_use]
    pub fn with_system_prompt(mut self, title: impl Into<String>, text: impl Into<String>) -> Self {
        self.system_prompts.push(PromptSystem {
            title: title.into(),
            text: text.into(),
        });
        self
    }

    #[must_use]
    pub const fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Override the mode-based base instructions (e.g. the internal
    /// sub-agent prompt). When set,
    /// [`format_header_context`](Self::format_header_context) uses this text
    /// instead of `INSTRUCTIONS_BUILD`/`INSTRUCTIONS_ASK`.
    #[must_use]
    pub const fn with_instructions(mut self, instructions: &'static str) -> Self {
        self.instructions = Some(instructions);
        self
    }

    /// Mark this harness as an INTERNAL SUB-AGENT (a nested harness).
    ///
    /// Set by [`Self::run_internal_subagent`] on the nested harness. A
    /// nested loop (a) rejects background `subagent_call` spawns — matching
    /// Claude Code's non-interactive nesting rule, where a headless sub-agent
    /// cannot outlive its parent to deliver a notification — and (b) never
    /// drains background completion notifications, so a notification meant
    /// for the top-level agent's context is never swallowed by a sub-agent.
    #[must_use]
    pub const fn as_nested(mut self) -> Self {
        self.nested = true;
        self
    }

    /// Mark this harness as the MANAGER loop (the `/agents` split's `cosh`
    /// pane, at any position):
    /// its `agents_call` requests are tagged [`super::panes::MANAGER`] so a
    /// background pane report is drained only by this loop — the main
    /// chat loop must never inject a report the manager asked for, and
    /// vice versa. Default is [`super::panes::MAIN`] (the normal loop).
    #[must_use]
    pub const fn as_manager(mut self) -> Self {
        self.panes_requester = super::panes::MANAGER;
        self
    }

    /// Bind this harness to the `/agents` pane it runs in (the pane's
    /// registry id: `"cosh"`, `"cosh#2"`, …). A pane loop NEVER coordinates:
    /// binding the id strips the `agents_call` tool (registered in `new`
    /// before the id was known) and the header's panes block (skipped in
    /// `format_header_context`). The reported replication bug lived exactly
    /// here: a cosh pane ran the manager instructions + the tool, so one
    /// delegation from the main agent made the pane's own manager loop
    /// re-delegate to EVERY sibling — the call propagated to all the cosh
    /// agents instead of staying in the addressed one. Coordination belongs
    /// to the TOP-LEVEL agent alone; a pane does its work and reports back.
    /// `None` for the MAIN loop, which owns no pane and keeps the tool.
    #[must_use]
    pub fn with_own_pane_id(mut self, id: Option<String>) -> Self {
        self.panes_own_id = id;
        if self.panes_own_id.is_some() {
            self.harness_tools.retain(|tool| tool.name != "agents_call");
            // The worktree ask is coordination tooling: a pane never
            // rebinds itself or a peer (same supremacy invariant as the
            // stripped `agents_call`).
            self.harness_tools
                .retain(|tool| tool.name != "agents_worktree");
            // `stop_agent_loop` goes too, and NOT only as tidiness: the
            // pane's whole report contract rests on "the loop cannot stop
            // itself", which is what makes a stop it did take the USER's
            // stop (see `Harness::resume_pane_turn`). The TUI's
            // `MANAGER_BLOCKED_TOOLS` enforced that by disabling the tool
            // per pane, but a pane harness built anywhere else would inherit
            // the model's ability to end its own turn with no report — and
            // the resume logic would then read that self-stop as a user stop
            // and correctly refuse, closing the turn with nothing. Stripping
            // it HERE binds the invariant to the pane ROLE, where it belongs.
            self.harness_tools
                .retain(|tool| tool.name != "stop_agent_loop");
        }
        self
    }

    /// Inject the RAG database context into the `recall_search` tool description.
    ///
    /// The `suffix` describes which knowledge bases are available so the
    /// agent sees a tailored description. Must be called before
    /// [`format_header_context`](Self::format_header_context).
    #[cfg(feature = "embed")]
    pub fn set_recall_context(&mut self, suffix: String) {
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_recall_context(suffix);
        }
    }

    /// Set the RAG database registry for the `recall_search` dispatch.
    ///
    /// Each entry holds the connection URI, table name, and embedder config
    /// needed to embed a query and search the vector DB. Must be called
    /// before [`format_header_context`](Self::format_header_context).
    #[cfg(feature = "embed")]
    pub fn set_recall_dbs(&mut self, dbs: Vec<super::tools::RecallDb>) {
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_recall_dbs(dbs);
        }
    }

    /// Check whether there are pending tool calls awaiting dispatch.
    #[must_use]
    pub fn has_pending_tools(&self) -> bool {
        !self.tool_issuer.is_empty()
    }
}
