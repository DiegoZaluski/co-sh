use super::{checkup, events};
use super::{background, guardrails, hooks, panes, queued_input};
use super::context;
#[allow(unused_imports)]
pub(crate) use loop_state::result_is_useless;
// Test-only re-export: the `harness::test` modules are siblings of `core`,
// so they cannot reach the private `guards` module to name a `ResumeCause`.
// Production code refers to it as `super::guards::ResumeCause` directly.
#[cfg(test)]
pub(crate) use guards::ResumeCause;
#[allow(unused_imports)]
use super::context::error_catalog_window;
use super::context::{ContextManager, MAX_CONTEXT_TOKENS, MapRequest, RunOutcome};
use super::correction_memory::CorrectionMemory;
use super::tools;
use super::tools::CoshTools;
use crate::mcp::{McpConfig, McpManager};
#[cfg(test)]
use cosh_sdk::connector::ChatMessage;
use cosh_sdk::connector::{
    ClaudeThinkingBlock, Connector, ToolCallMode,
};
#[cfg(test)]
use cosh_sdk::extract_action::ToolSchema;
#[cfg(test)]
use cosh_sdk::extract_action::NativeToolCall;
use cosh_sdk::extract_action::ToolCallData;
use cosh_tools::TOOL_FORMAT;
pub use types::{ManualCompactionOutcome, Mode, StreamEvent};
use cosh_tools::lsp::Lsp;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
#[cfg(test)]
use tokio::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod types;

#[cfg(test)]
#[path = "../test/checkpoint_output.rs"]
mod checkpoint_output;
#[cfg(test)]
#[path = "../test/map_reduce_test.rs"]
mod map_reduce_test;
#[path = "../summarization.rs"]
mod summarization;
#[cfg(test)]
#[path = "../test/summary_failure_loop.rs"]
mod summary_failure_loop;
mod prompts;
mod limits;
mod prefix_cache;
mod tool_catalog;
mod guards;
mod loop_state;
mod snapshots;
mod extraction;
mod stream;
mod map_reduce;
mod compaction;
mod prompt_context;
mod tool_routing;
mod build;
mod dispatch;
mod subagent;
mod agent_loop;
#[cfg(test)]
mod mock_support;

use compaction::{
    CompactionErr, MAP_CONCURRENCY, MIN_SUMMARY_OUTPUT_HEADROOM, compaction_retry_backoff,
    looks_like_argument_error, wait_for_stop_signal,
};
#[allow(unused_imports)]
use compaction::MAX_COMPACTION_RETRIES;
#[cfg(not(test))]
use compaction::discovered_context_window;
#[allow(unused_imports)]
pub(crate) use compaction::annotate_summary_tool_set;


use prefix_cache::PrefixCacheProbe;
pub use tool_catalog::HarnessTool;
use tool_catalog::{build_lsp, default_harness_tools, PromptSystem};

pub(crate) use limits::{CONTEXT_WINDOW_MARKER, INTERRUPTED_MARKER, MAX_ITERATIONS, ITERATION_RENEWALS, SUBAGENT_BLOCKED_TOOLS};
use limits::{LOOP_DETECTION_WINDOW_SIZE, MAX_TOOL_RETRIES, SUBAGENT_INTERNAL_NOTE};
pub use prompts::{
    INSTRUCTIONS_ASK, INSTRUCTIONS_BUILD, INSTRUCTIONS_BUILD_BASE, INSTRUCTIONS_BUILD_SELF_REVIEW,
    INSTRUCTIONS_SUBAGENT,
};


#[allow(clippy::struct_field_names)]
pub struct Harness {
    connector: Connector,
    /// MCP connections of this session. Owns every client; a failing server
    /// degrades to a `Failed` snapshot and never aborts the others. Not
    /// `Send` (like the prototype's clients), so the harness stays on the
    /// agent thread (`std::thread::spawn` + `block_on` in the TUI).
    mcp: McpManager,
    /// Registered MCP servers to connect. Populated via
    /// [`Self::with_mcp_config`] from the TUI's `Setup.mcp`.
    mcp_config: McpConfig,
    header_context: String,
    system_prompts: Vec<PromptSystem>,
    harness_tools: Vec<HarnessTool>,
    cosh_tools: Option<CoshTools>,
    /// Image payload drained from the last Tier-1 dispatch (a
    /// `computer_screenshot`'s PNG, as base64 `ImageBlock`s). Consumed by
    /// [`Self::push_tool_history`] when it records that dispatch's result;
    /// dropped on any other path (hook halt, stop, MCP fallback), so the
    /// bytes can never leak onto a later tool result.
    pending_images: Vec<cosh_sdk::connector::ImageBlock>,
    mode: Mode,
    /// Override for the mode-based base instructions (see
    /// [`Self::with_instructions`]). The internal sub-agent uses its own
    /// prompt so it never inherits the main agent's review-loop mandate.
    instructions: Option<&'static str>,
    /// Shared stop signal from the TUI, checked during streaming.
    stop_signal: Option<Arc<AtomicBool>>,
    /// Event channel for streaming reasoning/thinking tokens to the TUI.
    /// Set by [`run_agent_loop`](Self::run_agent_loop); the stream loop uses
    /// it to emit [`HarnessEvent::Reasoning`] live while the model thinks.
    reasoning_tx: Option<tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>>,
    /// Claude extended-thinking blocks captured from the CURRENT stream's
    /// final chunk (the turn ended with a tool call). Taken by
    /// [`Self::push_tool_history`] and attached to the tool-call turn so the
    /// follow-up request can replay them verbatim (the Anthropic API
    /// validates the signature and rejects missing blocks with 400). Reset at
    /// the start of every stream.
    pending_thinking_blocks: Vec<ClaudeThinkingBlock>,
    /// To stop the agent loop.
    pub(crate) stop: bool,
    /// Reasoning deltas accumulated for the CURRENT stream attempt. Cleared
    /// at every stream start and by a mid-stream reset, and recorded with the
    /// attempt's context item (`add_assistant_with_reasoning`) so a restored
    /// session rehydrates the thinking blocks. DISPLAY-ONLY payload — see
    /// `ContextItem::Assistant::reasoning`.
    attempt_reasoning: String,
    tool_issuer: VecDeque<ToolCallData>,
    /// Routing identity of the call currently being dispatched: set at the
    /// top of [`Self::dispatch_next`] (the SAME per-call ref the
    /// `ToolCall` event carries), read by the sub-agent dispatch arms to
    /// stamp every stream/report event. Cleared after the dispatch returns.
    /// Parallel sub-agent calls (same CLI or different) each hold a UNIQUE
    /// ref, so the TUI can pair a window's input echo with ITS OWN stream
    /// and report — the last-Running-window guess could cross-pair them.
    active_call_ref: Option<String>,
    /// The CURRENT loop's queued-input handle (`None` outside a running
    /// loop). Set at every `run_agent_loop*` entry so a blocking dispatch
    /// arm (the `agents_call` wait) can wake when the user queues a message
    /// — the queue belongs to the LOOP, not the harness, and a harness
    /// reused across loops must never read a previous loop's queue.
    loop_queued_input: Option<queued_input::QueuedInput>,
    /// True when this harness is an INTERNAL SUB-AGENT (a nested harness
    /// built by [`Self::run_internal_subagent`]). Gates the two behaviors a
    /// nested loop must not have (Claude Code non-interactive nesting rules):
    /// background sub-agent spawns are rejected, and completion
    /// notifications are never drained — a sub-agent must not swallow a
    /// notification meant for the top-level agent's context.
    nested: bool,
    /// Which loop this harness drives for the /agents pane bridge
    /// ([`super::panes::MAIN`] for the normal chat loop,
    /// [`super::panes::MANAGER`] for the split's cosh-pane manager loop). Tags
    /// every `agents_call` request so a background report is drained ONLY
    /// by the loop that asked for it — the two loops never run
    /// concurrently (the `/agents` gate refuses to open while the main
    /// loop works), but a delivery can outlive its requester's turn.
    panes_requester: &'static str,
    /// The registry id of the /agents pane this harness itself runs in
    /// (`Some("cosh")`, `Some("cosh#2")`, …) when the loop IS a pane; `None`
    /// for the MAIN chat loop, which has no pane. `agents_call` refuses to
    /// target this id: the manager would queue a turn behind the very turn
    /// making the call, deadlocking the wait.
    panes_own_id: Option<String>,
    /// PreToolUse hooks runner (empty when no hooks are configured).
    hook_runner: Option<super::hooks::HookRunner>,

    /// Language-server tooling for passive diagnostics injection after
    /// fs_write/fs_edit. `None` when `COSH_LSP=off` or no runtime.
    lsp: Option<Arc<Lsp>>,

    /// Monotonic counter for synthetic tool_call ids (inline JSON calls that
    /// the extractor emits without an id).
    tool_call_synthetic: u64,

    /// Single owner of the whole conversation (user prompts, assistant
    /// outputs, tool calls and results) before it reaches the LLM. Fresh
    /// content is compressed asynchronously; the budget bar reflects this
    /// manager's single token budget.
    pub context_manager: ContextManager,

    /// Tools explicitly disabled by the user via the Internal Tools screen.
    /// These are excluded from both the prompt header and the extractor.
    disabled_tools: HashSet<String>,

    /// How many consecutive tool calls have failed so far.
    /// Reset to 0 on the first successful dispatch.
    tool_failure_count: usize,

    /// How many tool calls failed extraction (invalid schema) in the current stream.
    /// Reset at the start of each [`stream_chat`](Self::stream_chat).
    tool_extraction_failure_count: usize,

    /// Raw JSON of the last failed tool call attempt, for correction feedback.
    last_failed_raw: String,

    /// `finish_reason` of the most recently completed stream (e.g. `"stop"`,
    /// `"length"`). `"length"` means the provider cut the response at
    /// `max_tokens` — the loop must continue instead of treating the
    /// truncated turn as a finished one.
    last_finish_reason: Option<String>,
    /// Whether the most recently completed stream ended with a half-parsed
    /// construct still in flight (a validated tool call awaiting delivery or
    /// an unclosed JSON object). A stream in this state was CUT mid-tool-call
    /// regardless of what `finish_reason` claims — the natural-stop gate
    /// must treat the turn as truncated, never as finished. Set at stream
    /// end from the extractor, reset at every stream start.
    last_stream_unfinished: bool,

    /// Bounded, deduplicated memory of tool correction errors.
    /// Persists across agent loop iterations so the model never repeats the
    /// same mistake blindly.
    correction_memory: CorrectionMemory,
    /// Remaining natural-stop continuations granted by the deterministic
    /// nudges (reasoning-only retry, empty-response retry), per agent loop.
    /// Independent of
    /// `MAX_ITERATIONS` (which bounds ALL continuations): the nudges must
    /// stay scarce so a model that keeps insisting on stopping with open
    /// work still terminates visibly instead of looping to 100.
    stop_nudges_left: u32,

    /// Test-only override for the natural-stop nudge budget `run_agent_loop_inner`
    /// starts each ENTRY with. The budget is reset on every entry, so a test
    /// that wants to reach the "budget exhausted" branch cannot simply set
    /// `stop_nudges_left` — it seeds this instead.
    #[cfg(test)]
    stop_nudge_budget_override: Option<u32>,

    /// Remaining IMPROPER-STOP resumes granted to THIS loop when the loop
    /// owes a REPORT (a `/agents` cosh pane, or the internal sub-agent). See
    /// [`super::limits::PANE_RESUME_BUDGET`] for why such a loop continues
    /// where a chat turn does not, and why the bound is what makes the
    /// continuation safe. Consumed only by [`Self::resume_pane_turn`]; a loop
    /// that owes no report never reaches it (the helper refuses).
    pane_resumes_left: u32,

    /// Exact-prefix cache probe: the previous request's fingerprint and the
    /// running counters. See [`PrefixCacheProbe`].
    prefix_cache_probe: PrefixCacheProbe,
    /// Test-only record of every reasoning promotion the natural-stop gate
    /// performed, so tests can pin the one-turn promotion contract without
    /// intercepting the request wire. `pub(crate)` under `cfg(test)` only:
    /// the test modules are siblings of `core` and read the log directly.
    #[cfg(test)]
    pub(crate) promoted_reasoning_log: Vec<String>,

    /// Cache of permanently-allowed subagents (Allow chosen by user).
    /// Each entry is the agent name (e.g. "opencode", "claude").
    /// Once allowed, the subagent tool skips the permission dialog for
    /// the rest of the process lifetime.
    agent_permissions: HashSet<String>,

    /// Paths permanently approved via Allow (life of the process).
    /// The harness checks this BEFORE calling `check_tool_permission`:
    /// if all paths from a tool call are already approved, the dialog
    /// is skipped entirely. Paths approved via AllowOnce are NOT stored
    /// here — they are only added transiently to the cosh-tools allowlist
    /// and removed after dispatch.
    approved_paths: HashSet<std::path::PathBuf>,

    /// Remaining fallback (provider, model) pairs to try if the current
    /// connector's API call fails.
    fallbacks: Vec<(String, String)>,
    /// The decision-model audit seam (`harness::checkup`). `None` when the
    /// binary has no ONNX runtime or the user never enabled the termination
    /// audit — the loop then behaves exactly as it always did.
    checkup: Option<Arc<dyn super::checkup::Checkup>>,
    /// The consumer floor for the termination audit: a `NotTerminated`
    /// verdict only continues the loop when its confidence reaches this
    /// threshold (from `setup.model_decision.termination.min_confidence`).
    checkup_min_confidence: f64,
    summarization_models: Vec<(String, String)>,
    summarization_connector: Option<Connector>,
    summarization_window: Option<usize>,
    compaction_interrupted: bool,
    #[cfg(test)]
    mock_compaction_models: Vec<(String, String)>,
    #[cfg(test)]
    mock_summarization_windows: std::collections::HashMap<String, usize>,

    /// Configured base URLs for local providers (provider → URL), used when
    /// building fallback connectors so they hit the user's server.
    local_base_urls: std::collections::HashMap<String, String>,

    /// Window size (tokens) parsed from the LAST context-window overflow
    /// error, stashed by [`Self::stream_chat_with_messages`] so the caller can
    /// size the hierarchical MapReduce contingency against a known window.
    /// Reset at the start of every stream.
    last_context_window: Option<usize>,

    /// Window size (tokens) of the ACTIVE model from the last successful
    /// context-window discovery. Together with [`Self::last_context_window`]
    /// it forms the KNOWN window that drives the hierarchical MapReduce
    /// contingency (see [`Self::known_checkpoint_window`]). Set at loop start and
    /// on fallback switches; `None` when discovery failed (unknown window →
    /// the split is sized against the token budget).
    discovered_window: Option<usize>,
    /// Whether discovered windows are resized to the reduced sweet-spot
    /// budget (`setup.effective_context`). `false` uses the full window.
    effective_context: bool,

    /// When the persistent context-overflow toast was last shown, for the
    /// throttling cooldown (see [`Self::notify_context_overflow`]).
    last_overflow_toast: Option<tokio::time::Instant>,

    /// Cadence of the periodic [`HarnessEvent::ContextSnapshot`] emissions
    /// (incremental persistence). A test setter shrinks it so a fast mock
    /// run still produces a snapshot per tool dispatch.
    snapshot_interval: std::time::Duration,

    /// How many consecutive generic (non-context-window) summarizer failures
    /// in the current compaction attempt — bounded by [`MAX_COMPACTION_RETRIES`].
    compaction_generic_retries: usize,

    #[cfg(test)]
    pub(crate) mock_chat_response: Option<Result<String, String>>,
    /// Test-only queue of mock CHAT (summarizer) responses, consumed one per
    /// call and falling back to [`Self::mock_chat_response`] when empty. Lets
    /// a test drive a SEQUENCE of summarizer outcomes — e.g. a single-shot
    /// compaction overflow that reports its window, followed by the split
    /// chunks' successes (the reactive fork).
    #[cfg(test)]
    pub(crate) mock_chat_queue: VecDeque<Result<String, String>>,
    /// Per-map delay used to prove ordinal-stable out-of-order completion.
    #[cfg(test)]
    pub(crate) mock_map_delay_queue: VecDeque<u64>,
    #[cfg(test)]
    pub(crate) mock_stream_queue: VecDeque<Result<Vec<String>, String>>,
    /// Test-only queue of mock REASONING chunks, consumed one queue entry per
    /// mock stream (parallel to `mock_stream_queue`): each entry's strings are
    /// emitted as `HarnessEvent::Reasoning` deltas and accumulated on
    /// `attempt_reasoning` exactly like the real stream path, so tests can
    /// drive the reasoning-persistence contract end to end.
    #[cfg(test)]
    pub(crate) mock_reasoning_queue: VecDeque<Vec<String>>,
    /// Test-only per-token pause (ms) applied by the mock stream so tests
    /// can interleave with it — e.g. set the stop signal mid-stream and
    /// assert the loop stops instead of completing the turn.
    #[cfg(test)]
    pub(crate) mock_stream_delay_ms: u64,
    /// Test-only queue of NATIVE (structured) tool calls, consumed one list
    /// per stream right after its string tokens. Lets a test drive a real
    /// provider-delivered `tool_calls`/`tool_use`/`functionCall` through the
    /// same `register_native_call` → `route_tool_call` funnel the
    /// real-stream branch uses.
    #[cfg(test)]
    pub(crate) mock_native_stream_queue: VecDeque<Vec<NativeToolCall>>,
    /// Per-stream mock `finish_reason` values, consumed one per stream so a
    /// test can simulate a single truncated response followed by a normal one.
    #[cfg(test)]
    pub(crate) mock_finish_reasons: VecDeque<Option<String>>,
    /// Per-stream mock reset markers, consumed one per stream: `true` makes
    /// the mock path emit [`StreamEvent::Reset`] before the tokens, driving
    /// the same discard-then-restart flow the SDK's mid-stream retry emits.
    #[cfg(test)]
    pub(crate) mock_stream_resets: VecDeque<bool>,
    #[cfg(test)]
    pub(crate) test_tools: Vec<ToolSchema>,
}


impl Harness {
    /// Builds the system header for the LLM.
    ///
    /// Concatenates mode-specific instructions, system prompts, harness tools, system tools,
    /// and MCP server tools — all rendered inline with full name, description, and input schema.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        let instructions = match self.instructions {
            Some(ins) => ins,
            None => match self.mode {
                Mode::Build | Mode::Yolo | Mode::Command => INSTRUCTIONS_BUILD,
                Mode::Ask => INSTRUCTIONS_ASK,
            },
        };
        let _ = write!(out, "{instructions}");
        // The tool-call delivery mode decides BOTH the prompt instruction
        // and the header schemas — the two paths are mutually exclusive (see
        // `ToolCallMode`):
        // - `Native` (default, like crush): the model uses the platform's
        //   structured function calling. The prompt forbids JSON-in-text, and
        //   the schemas live ONLY in the request's native `tools` array —
        //   re-sending them in the system prompt would duplicate every
        //   schema on each request (~4.3k tokens of the measured header).
        // - `Inline`: the model writes `{"name", "arguments"}` JSON into its
        //   text response (the extractor parses it). The full `Schema: {...}`
        //   dump stays in the header, and the request carries no native
        //   `tools` (see the caller gates) so the API can never produce
        //   structured tool calls — the two paths never cross.
        let include_inline_schemas = self.connector.tool_call_mode() == ToolCallMode::Inline;
        let tool_format = if include_inline_schemas {
            TOOL_FORMAT
        } else {
            cosh_tools::TOOL_FORMAT_NATIVE
        };
        let _ = write!(out, "## Tool Format\n{tool_format}");
        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
        }

        // The plan tools are only exposed in Build/Yolo modes. Teach the
        // agent the structured TODO workflow: create the full plan in a
        // single `plan_todo_write` call, then drive it by rewriting the
        // list with updated statuses during execution.
        if !matches!(self.mode, Mode::Ask) {
            let _ = write!(
                out,
                "## System: Plan\n\
                 Plan the full TODO list up front: create it with a single \
                 `plan_todo_write` call (flat list, one task per entry; use the \
                 optional per-item `key` so sibling tasks can reference each \
                 other in `depends_on` within the same call). During execution, \
                 change a task's status (start, complete, cancel) by rewriting \
                 the full list with the updated status; exactly one task may be \
                 in-progress at a time.\n\n"
            );
        }

        let _ = write!(out, "## Tools\n\n");
        let _ = write!(out, "### Harness Tools\n\n");
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            if include_inline_schemas {
                let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
                let _ = write!(
                    out,
                    "### {}\n{}\nSchema: {}\n\n",
                    tool.name, tool.description, schema,
                );
            } else {
                let _ = write!(out, "### {}\n{}\n\n", tool.name, tool.description);
            }
        }
        if let Some(ref cosh) = self.cosh_tools {
            let _ = write!(out, "### System Tools\n\n");
            match self.mode {
                // Yolo/Command dispatch without an interactive approval dialog,
                // and Build asks per-call (the guardrail denies coordinate
                // forms and approves the tree-grounded ones), so every mode
                // sees the full toolset.
                Mode::Build => cosh.write_tool_descriptions_enabled(
                    &mut out,
                    &self.disabled_tools,
                    include_inline_schemas,
                ),
                Mode::Yolo | Mode::Command => cosh.write_tool_descriptions_enabled(
                    &mut out,
                    &self.disabled_tools,
                    include_inline_schemas,
                ),
                Mode::Ask => cosh.write_tool_descriptions_filtered(
                    &mut out,
                    &self.disabled_tools,
                    include_inline_schemas,
                ),
            }
        }

        // MCP tools stay hidden in Ask mode: the read-only planning agent
        // must not see tools whose safety is unknown.
        if !matches!(self.mode, Mode::Ask) {
            for (server, tools) in self.mcp.tools_by_server() {
                let visible: Vec<_> = tools
                    .into_iter()
                    .filter(|tool| !self.disabled_tools.contains(tool.name.as_ref()))
                    .collect();
                if visible.is_empty() {
                    continue;
                }
                let _ = write!(out, "### MCP Server: {server}\n\n");
                for tool in visible {
                    let desc = tool.description.as_deref().unwrap_or_default();
                    if include_inline_schemas {
                        let schema =
                            serde_json::to_string_pretty(&*tool.input_schema).unwrap_or_default();
                        let _ = write!(
                            out,
                            "- **{name}**: {desc}\n  Schema: {schema}\n",
                            name = tool.name
                        );
                    } else {
                        let _ = writeln!(out, "- **{name}**: {desc}", name = tool.name);
                    }
                }
            }
        }

        // Notify the model when the effective tool set changed since its last
        // turn (mode switch, disabled-tool change, or MCP server drift): it
        // would otherwise keep calling tools from its previous repertoire
        // and accumulate confusing failures.
        let current = self.effective_tool_names();
        if let Some(prior) = self.context_manager.last_tool_set().map(<[String]>::to_vec) {
            let removed: Vec<&String> = prior.iter().filter(|n| !current.contains(n)).collect();
            let added: Vec<&String> = current.iter().filter(|n| !prior.contains(n)).collect();
            if !removed.is_empty() || !added.is_empty() {
                let _ = writeln!(out, "\n## Tool Availability Changed\n");
                if !removed.is_empty() {
                    let list = removed
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(out, "- No longer available; do not call: {list}.");
                }
                if !added.is_empty() {
                    let list = added
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(out, "- Now available: {list}.");
                }
            }
        }
        // NOTE: the /agents panes listing does NOT live in the header
        // anymore. It moved to the context manager's TAIL (see
        // `ContextManager::set_panes_block`): the system prompt is prefix
        // position 0, so a pane binding/unbinding re-rendered this block
        // mid-session and invalidated the provider's exact-prefix cache
        // for the WHOLE conversation on every such change. The tail costs
        // only itself — the same contract the corrections and TODO blocks
        // already follow.

        self.context_manager.set_last_tool_set(current);

        self.header_context = out;
        &self.header_context
    }
    /// Send a chat completion and return the full response as a single string.
    ///
    /// Use this when streaming is not enabled — the model's reply is collected
    /// entirely and returned as `Result<String, String>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the connector call fails.
    pub async fn chat(&mut self, input: &str) -> Result<String, String> {
        #[cfg(test)]
        if let Some(response) = self.next_mock_chat() {
            let raw = response?;
            let mut extractor = self.build_extractor();
            return Ok(self.process_extraction(&raw, &mut extractor));
        }

        let context = self.build_chat_context();

        let out = self
            .connector
            .chat_with_system(input, &context)
            .await
            .map_err(|e| e.to_string())?;

        // Inline mode: parse tool calls out of the model's text. Native mode
        // (default): the text is the answer — never parsed, matching the
        // crush contract (tool calls arrive only as structured parts, which
        // the non-streaming `chat` path does not surface).
        if self.connector.tool_call_mode() == ToolCallMode::Inline {
            let mut extractor = self.build_extractor();
            let result = self.process_extraction(out.message(), &mut extractor);
            self.last_failed_raw = extractor.take_last_failed_raw();
            Ok(result)
        } else {
            Ok(out.message().to_string())
        }
    }
}
