use super::context_manager::{ContextManager, MAX_CONTEXT_TOKENS, RunOutcome};
use super::correction_memory::CorrectionMemory;
use super::tools::{CoshTools, Tools};
use cosh_sdk::connector::{
    ChatMessage, ClaudeThinkingBlock, Connector, ConnectorError, ToolDefinition,
};
#[cfg(not(test))]
use cosh_sdk::connector::{discover_context_window, effective_context_window};
use cosh_sdk::extract_action::{ExtractAction, Item, StreamAction, ToolCallData, ToolSchema};
use cosh_tools::TOOL_FORMAT;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
#[cfg(not(test))]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};
use tokio::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Ask,
    Yolo,
}

pub struct ServerSession {
    pub name_server: String,
    pub tools: Vec<Tool>,
    pub client: RunningService<RoleClient, ()>,
}

pub const INSTRUCTIONS_BUILD: &str = concat!(
    "## Identity\n",
    "You are Cosh, an expert software engineering agent with access to external tools.\n\n",
    "## Capabilities\n",
    "- You have access to external tools for file operations, code execution, web search, and more.\n",
    "- Tool invocation is defined entirely by each tool specification.\n",
    "- The available tools and their specifications are listed below.\n\n",
    "## Constraints\n",
    "- Respond directly to the user's request.\n",
    "- Do not introduce yourself unless the user explicitly asks who you are.\n",
    "- Do not volunteer information about your internal capabilities or available tools.\n",
    "- Use a tool only when it is required to produce or verify the requested result.\n",
    "- If the request can be completed correctly without using any tool, answer directly.\n",
    "- Avoid unnecessary narration or descriptions of obvious actions.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- When the current user request has been fully completed and no further action is required, invoke `stop_agent_loop`.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your response — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n\n",
    "## Self-Review Loop\n",
    "- After completing any code changes, you MUST call a subagent for code review using `subagent_call`.\n",
    "- The subagent review should focus on: correctness, security, performance, and maintainability.\n",
    "- If the review identifies critical issues (bugs, security vulnerabilities, broken functionality), fix them immediately.\n",
    "- After fixing issues, call the subagent again to review the corrected code.\n",
    "- Repeat this review-fix loop until the subagent reports only cosmetic/minor issues (style, formatting, optional improvements).\n",
    "- Only invoke `stop_agent_loop` when the review confirms no critical issues remain.\n",
    "- This self-correction loop ensures code quality before considering a task complete.\n"
);

pub const INSTRUCTIONS_ASK: &str = concat!(
    "## Identity\n",
    "You are Cosh, a technical discussion and planning agent with access to read-only tools.\n\n",
    "## Capabilities\n",
    "- You have access to read-only tools for code inspection, documentation, and information gathering.\n",
    "- Tool invocation is defined entirely by each tool specification.\n",
    "- The available tools and their specifications are listed below.\n\n",
    "## Constraints\n",
    "- Respond directly to the user's request.\n",
    "- Do not introduce yourself unless the user explicitly asks who you are.\n",
    "- Your purpose is to discuss, explore, and plan technical work.\n",
    "- Help the user understand the codebase, clarify requirements, evaluate alternatives, and outline implementation strategies.\n",
    "- Do not modify code or perform write operations.\n",
    "- Ask clarifying questions whenever the user's intent is ambiguous or required information is missing.\n",
    "- Use a tool only when it is required to inspect code, documentation, or other relevant information needed to answer correctly.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- When the discussion has naturally concluded and no further exploration is required, invoke `stop_agent_loop`.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your discussion — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n"
);

/// Base instructions for the INTERNAL sub-agent (the merged
/// `subagent_call` internal path): a nested harness that runs the task
/// with an empty context in Yolo mode. Deliberately does NOT mention
/// sub-agents or `subagent_call` at all — the main agent's instructions
/// mandate a code-review sub-agent loop, and exposing that here would
/// make the sub-agent nest itself indefinitely. The sub-agent calls
/// other sub-agents only if it decides to on its own.
///
/// `stop_agent_loop` is NOT available to the sub-agent (see
/// [`SUBAGENT_BLOCKED_TOOLS`]): its only deliverable is the final text
/// report, so the loop must end with a written answer — never a silent
/// stop.
pub const INSTRUCTIONS_SUBAGENT: &str = concat!(
    "## Identity\n",
    "You are Cosh, an expert software engineering agent executing a task delegated to you.\n\n",
    "## Capabilities\n",
    "- You have access to external tools for file operations, code execution, web search, and more.\n",
    "- Tool invocation is defined entirely by each tool specification.\n",
    "- The available tools and their specifications are listed below.\n\n",
    "## Constraints\n",
    "- Complete the delegated task directly and efficiently.\n",
    "- Do not introduce yourself or volunteer information about your internal capabilities or available tools.\n",
    "- Use a tool only when it is required to produce or verify the requested result.\n",
    "- If the request can be completed correctly without using any tool, answer directly.\n",
    "- Avoid unnecessary narration or descriptions of obvious actions.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- Your ONLY deliverable is your final text response: the caller receives nothing but that text, so ALWAYS end by writing the complete answer/report of what you did as a normal text message.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your response — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n"
);

/// Maximum consecutive tool-call failures before aborting the agent loop.
const MAX_TOOL_RETRIES: usize = 3;

/// Maximum total agent-loop iterations (tool calls + responses) before
/// the harness stops the loop as a safety net against runaway tool-calling.
///
/// 20 was too low for real multi-step work (editing files, running tests,
/// searching) — the loop force-stopped mid-task with a misleading `Done`.
pub(crate) const MAX_ITERATIONS: u64 = 100;

/// Tools disabled inside the internal sub-agent harness (the merged
/// `subagent_call` internal path). The tools are removed from the
/// sub-agent's schema AND extractor, so the model never even sees them:
/// - `ask_questions` — needs a human in the loop; the sub-agent is headless
///   and would hang waiting for an answer;
/// - `stop_agent_loop` — the sub-agent's ONLY deliverable is its final text
///   report. If it could stop the loop directly, a model might call it as
///   its last action without writing the report (the caller would receive
///   nothing). Blocked so the loop can only end with a written answer.
///
/// Add future blocklist entries here.
pub(crate) const SUBAGENT_BLOCKED_TOOLS: &[&str] =
    &["ask_questions", "stop_agent_loop"];

/// Chunk interpolated into the `subagent_call` tool description (via
/// [`SubAgent::set_note`]) so the model learns — naturally, inside the
/// description prose — that omitting `agent` routes the call to an
/// internal agent instead of an external CLI.
const SUBAGENT_INTERNAL_NOTE: &str = "When the `agent` argument is omitted or empty, an internal agent runs \
     the task instead — it starts with an empty context, runs in auto-approve \
     mode, persists nothing, and returns only its final report.";

/// Internal marker returned by stream functions when the user presses Esc
/// to stop the current generation. Compared by identity (constant), not by
/// string value — prevents the fallback retry from misinterpreting a user
/// cancellation as a connector error.
const INTERRUPTED_MARKER: &str = "__cosh_interrupted__";

/// Internal marker returned by a stream function when the provider rejected
/// the request because the prompt exceeds its context window. Compared by
/// identity (constant), like [`INTERRUPTED_MARKER`] — the harness drains one
/// tool chain and retries when it sees this. `pub(crate)` so tests can
/// simulate the overflow through the mock paths.
pub(crate) const CONTEXT_WINDOW_MARKER: &str = "__cosh_context_window_exceeded__";

/// How many times a generic (non-context-window) summarizer error is retried
/// before the harness gives up and surfaces a TUI notification.
const MAX_COMPACTION_RETRIES: usize = 3;

/// Base of the exponential backoff between summarizer retries (attempt N
/// waits `2^(N-1) * BASE`).
const COMPACTION_RETRY_BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Minimum time between two persistent context-overflow toasts: the user
/// must keep being reminded while the provider is stuck, but not spammed on
/// every dispatch iteration.
const OVERFLOW_TOAST_COOLDOWN: Duration = Duration::from_secs(15);

/// Backoff after the `attempt`-th failed summarizer attempt (1-based).
fn compaction_retry_backoff(attempt: usize) -> Duration {
    COMPACTION_RETRY_BACKOFF_BASE.saturating_mul(1u32 << attempt.min(4))
}

/// Timeout for one context-window discovery call: a hanging network request
/// must never stall the agent loop start.
#[cfg(not(test))]
const CONTEXT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Process-global cache of discovered context windows, keyed by model name.
/// Discovery hits public APIs (OpenRouter/Anthropic), so it must happen at
/// most once per model per process — never once per user message. A FAILED
/// discovery is cached as `None` too: an unknown model must not be re-queried
/// (and re-logged as a warning) on every loop start.
#[cfg(not(test))]
static DISCOVERED_WINDOW_CACHE: OnceLock<Mutex<HashMap<String, Option<usize>>>> = OnceLock::new();

/// Discover the ACTIVE model's real context window (the RAW advertised
/// window, cached per model and bounded by [`CONTEXT_DISCOVERY_TIMEOUT`]).
/// Returns `None` (keeping the current budget) when the model is unknown or
/// discovery fails.
///
/// The compaction trigger is 80% of the context manager's budget: with the
/// hardcoded 100k default and a 300k-window model, the LLM compaction would
/// fire at 80k — far below the model's actual capacity — and re-trigger on
/// every overflow (the "infinite summarization" loop). Sizing the budget to
/// the model's EFFECTIVE window (a conservative fraction of the raw one, see
/// `effective_context_window`) is what keeps the trigger sane.
#[cfg(not(test))]
async fn discovered_context_window(model: Option<&str>) -> Option<usize> {
    let model = model?;
    let cache = DISCOVERED_WINDOW_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(window) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(model)
    {
        return *window;
    }
    // On-disk catalog caching under `~/.local/share/cosh/cache` (see `cosh-sdk`'s
    // `discover_context_window`), so repeated launches reuse the downloaded
    // models.dev / OpenRouter catalogs instead of re-fetching them.
    let window = tokio::time::timeout(
        CONTEXT_DISCOVERY_TIMEOUT,
        discover_context_window(model, Some("cosh/cache")),
    )
    .await
    .ok()
    .flatten();
    log::debug!("context window discovery for {model}: {window:?}");
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(model.to_string(), window);
    window
}

/// The classified result of a single summarizer attempt, so [`Harness::llm_compact`]
/// can decide between retrying (generic errors), draining tool chains
/// (context-window overflow) or giving up.
#[derive(Debug)]
enum CompactionErr {
    /// The user interrupted the generation.
    Interrupted,
    /// The provider rejected the prompt as larger than its context window.
    ContextWindow { window_tokens: Option<usize> },
    /// Any other connector error.
    Other(String),
}

/// Outcome of a single [`Harness::llm_compact`] run.
enum CompactionOutcome {
    /// A summary was produced and is ready to be applied.
    Applied,
    /// A contingency (chain draining or the split-and-concatenate path)
    /// brought the total down — the compaction is no longer needed.
    ResolvedByContingency,
    /// The summarizer call failed or produced nothing.
    Failed,
}

pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct HarnessTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

fn default_harness_tools() -> Vec<HarnessTool> {
    vec![HarnessTool {
        name: "stop_agent_loop".into(),
        description: "Stop running the agent loop".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }]
}

#[allow(clippy::struct_field_names)]
pub struct Harness {
    connector: Connector,
    sessions: Vec<ServerSession>,
    protocol: Option<String>,
    header_context: String,
    system_prompts: Vec<PromptSystem>,
    harness_tools: Vec<HarnessTool>,
    cosh_tools: Option<CoshTools>,
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
    tool_issuer: VecDeque<ToolCallData>,

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

    /// Bounded, deduplicated memory of tool correction errors.
    /// Persists across agent loop iterations so the model never repeats the
    /// same mistake blindly.
    correction_memory: CorrectionMemory,

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

    /// Window size (tokens) parsed from the LAST context-window overflow
    /// error, stashed by [`Self::stream_chat_with_messages`] so the retry
    /// loop can drain tool chains locally (without an HTTP round trip per
    /// chain) until the estimate fits. Reset at the start of every stream.
    last_context_window: Option<usize>,

    /// Window size (tokens) of the ACTIVE model from the last successful
    /// context-window discovery. Together with [`Self::last_context_window`]
    /// it forms the KNOWN window that drives the split-and-concatenate
    /// contingency (see [`Self::known_split_window`]). Set at loop start and
    /// on fallback switches; `None` when discovery failed (unknown window →
    /// legacy drain path).
    discovered_window: Option<usize>,

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
    #[cfg(test)]
    pub(crate) mock_stream_queue: VecDeque<Result<Vec<String>, String>>,
    /// Per-stream mock `finish_reason` values, consumed one per stream so a
    /// test can simulate a single truncated response followed by a normal one.
    #[cfg(test)]
    pub(crate) mock_finish_reasons: VecDeque<Option<String>>,
    #[cfg(test)]
    pub(crate) test_tools: Vec<ToolSchema>,
}

/// Whether a tool RESULT should be flagged `useless` in the context manager.
///
/// Reads the tool's own declared output contract: `find_grep` marks zero-match
/// results with `"useless": true` in its JSON, and the model sees the same
/// field. Gate by tool name so no other tool's JSON is ever interpreted.
/// `pub(crate)` for the bridge unit test in `harness::test`.
pub(crate) fn result_is_useless(name: &str, result: &str) -> bool {
    if !matches!(name, "find_grep" | "find_glob") {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(result) {
        Ok(value) => value
            .get("useless")
            .and_then(|field| field.as_bool())
            .unwrap_or(false),
        Err(_) => false,
    }
}

impl Harness {
    #[must_use]
    pub fn new(connector: Connector, cwd: &str, disabled_tools: HashSet<String>) -> Self {
        let mut cosh_tools = CoshTools::new(cwd);
        // Interpolate the internal-sub-agent note into the `subagent_call`
        // tool description BEFORE the header is built: omitting `agent`
        // routes the call to an internal agent instead of an external CLI.
        cosh_tools.set_subagent_note(SUBAGENT_INTERNAL_NOTE);
        Self {
            connector,
            sessions: Vec::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: Some(cosh_tools),
            mode: Mode::Build,
            instructions: None,
            stop_signal: None,
            reasoning_tx: None,
            pending_thinking_blocks: Vec::new(),
            stop: false,
            tool_issuer: VecDeque::new(),
            tool_call_synthetic: 0,
            context_manager: ContextManager::new(MAX_CONTEXT_TOKENS),
            disabled_tools,
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            last_finish_reason: None,
            correction_memory: CorrectionMemory::new(5),
            agent_permissions: HashSet::new(),
            approved_paths: HashSet::new(),
            fallbacks: Vec::new(),
            last_context_window: None,
            discovered_window: None,
            last_overflow_toast: None,
            compaction_generic_retries: 0,
            snapshot_interval: std::time::Duration::from_secs(10),
            #[cfg(test)]
            mock_chat_response: None,
            #[cfg(test)]
            mock_chat_queue: VecDeque::new(),
            #[cfg(test)]
            mock_stream_queue: VecDeque::new(),
            #[cfg(test)]
            mock_finish_reasons: VecDeque::new(),
            #[cfg(test)]
            test_tools: Vec::new(),
        }
    }

    /// Load previous conversation turns into the context manager (the single
    /// owner of the conversation) so the assistant sees context when it starts.
    #[must_use]
    pub fn with_history(mut self, turns: &[(String, String)]) -> Self {
        for (role, text) in turns {
            if role == "user" {
                self.context_manager.add_user(text);
            } else {
                // Loaded assistant turns are prose and compressible.
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

    /// # Errors
    ///
    /// Returns an error if the transport cannot be created,
    /// the MCP handshake fails, or the protocol is unsupported.
    pub async fn connect(
        &mut self,
        server: &str,
        protocol: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match protocol {
            "stdio" => {
                let mut cmd = tokio::process::Command::new("npx");
                cmd.arg("-y").arg(server);
                let transport = TokioChildProcess::new(cmd)?;
                ().serve(transport).await?
            }

            "http" => {
                let transport = StreamableHttpClientTransport::from_uri(server);
                ().serve(transport).await?
            }

            other => return Err(format!("unsupported protocol: {other}").into()),
        };

        let name_server = client
            .peer_info()
            .map(|i| i.server_info.name.clone())
            .unwrap_or_default();

        let tools = client.list_all_tools().await?;

        self.sessions.push(ServerSession {
            name_server,
            tools,
            client,
        });
        Ok(())
    }

    pub fn set_protocol(&mut self, protocol: impl Into<Option<String>>) -> &mut Self {
        self.protocol = protocol.into();
        self
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

    /// Builds the system header for the LLM.
    ///
    /// Concatenates mode-specific instructions, system prompts, harness tools, system tools,
    /// and MCP server tools — all rendered inline with full name, description, and input schema.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        let instructions = match self.instructions {
            Some(ins) => ins,
            None => match self.mode {
                Mode::Build | Mode::Yolo => INSTRUCTIONS_BUILD,
                Mode::Ask => INSTRUCTIONS_ASK,
            },
        };
        let _ = write!(out, "{instructions}");
        // All providers officially recommend native function calling and warn
        // that inline-JSON instructions in the prompt conflict with it (the
        // Gemini API rejects the turn with MALFORMED_FUNCTION_CALL when a
        // model obeys the inline format over toolConfig AUTO). Cloud providers
        // therefore get the NATIVE instruction. Only local model servers
        // (ollama/lmstudio/vllm/llamacpp) may lack reliable native function
        // calling and keep the legacy inline-JSON TOOL_FORMAT as a fallback
        // the extractor can capture.
        //
        // `include_inline_schemas` follows the SAME gate: local providers
        // keep the full `Schema: {...}` dump in the header (the model emits
        // inline JSON the extractor parses), while cloud providers omit it —
        // they hold the schemas in the native `tools` array of the request,
        // so re-sending them in the system prompt would duplicate every
        // schema on each request (~4.3k tokens of the measured header).
        let include_inline_schemas = self.connector.is_local();
        let tool_format = if include_inline_schemas {
            TOOL_FORMAT
        } else {
            cosh_tools::TOOL_FORMAT_NATIVE
        };
        let _ = write!(out, "## Tool Format\n{tool_format}");
        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
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
                Mode::Build | Mode::Yolo => cosh.write_tool_descriptions_enabled(
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

        for session in &self.sessions {
            let _ = write!(out, "### MCP Server: {}\n\n", session.name_server);
            for tool in &session.tools {
                let desc = tool.description.as_deref().unwrap_or_default();
                if include_inline_schemas {
                    let schema = serde_json::to_string_pretty(&*tool.input_schema).unwrap_or_default();
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

        self.header_context = out;
        &self.header_context
    }

    /// Build an extractor with all registered MCP, cosh, and internal tools.
    fn build_extractor(&self) -> ExtractAction {
        log::debug!("build_extractor: sessions={}", self.sessions.len());
        let mut extractor = ExtractAction::new();
        for session in &self.sessions {
            for tool in &session.tools {
                extractor.add_tool(ToolSchema {
                    name: tool.name.to_string(),
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
                });
            }
        }
        if let Some(ref cosh) = self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build | Mode::Yolo => cosh.schemas_enabled(&self.disabled_tools),
                Mode::Ask => cosh.schemas_filtered(&self.disabled_tools),
            };
            log::debug!(
                "build_extractor: cosh.schemas() returned {} tools",
                schemas.len()
            );
            for schema in schemas {
                extractor.add_tool(schema);
            }
        }
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            extractor.add_tool(ToolSchema {
                name: tool.name.clone(),
                input_schema: tool.input_schema.clone(),
            });
        }
        #[cfg(test)]
        for ts in &self.test_tools {
            extractor.add_tool(ts.clone());
        }
        extractor
    }

    /// Handle a harness tool call immediately, returning `Some` when the call
    /// was consumed by the harness, or `None` when it must be dispatched
    /// externally.
    pub(crate) fn handle_harness_tool(&mut self, tc: &ToolCallData) -> Option<String> {
        let _tool = self.harness_tools.iter().find(|t| t.name == tc.name)?;
        match tc.name.as_str() {
            "stop_agent_loop" => {
                self.stop = true;
                Some(String::new())
            }
            _ => Some(String::new()),
        }
    }

    /// Process extracted items into a text response, routing tool calls.
    fn process_extraction(&mut self, raw: &str, extractor: &mut ExtractAction) -> String {
        let result = extractor.extract_batch(raw);
        let mut output = String::new();
        for item in result.items {
            match item {
                Item::Text(t) => output.push_str(&t),
                Item::ToolCall(tc) => match self.handle_harness_tool(&tc) {
                    None => {
                        self.tool_issuer.push_back(tc);
                    }
                    Some(result) if !result.is_empty() => {
                        self.push_tool_history(
                            &tc.id,
                            &tc.name,
                            &tc.arguments,
                            &tc.thought_signature,
                            &result,
                        );
                    }
                    _ => {}
                },
            }
        }
        let extraction_failures = extractor.take_tool_failures();
        self.tool_extraction_failure_count += extraction_failures;
        output
    }

    /// Build the system context for the LLM.
    ///
    /// Returns the header context (instructions + tool definitions) plus the
    /// correction memory. The conversation itself lives entirely in the
    /// [`ContextManager`] and is delivered as structured messages via
    /// [`stream_chat_with_messages`](Self::stream_chat_with_messages) — never
    /// duplicated into the system prompt.
    fn build_chat_context(&mut self) -> String {
        let mut out = self.header_context.clone();
        let correction = self.correction_memory.format();
        out.push_str(&correction);
        out
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

        let mut extractor = self.build_extractor();
        let result = self.process_extraction(out.message(), &mut extractor);
        self.last_failed_raw = extractor.take_last_failed_raw();
        Ok(result)
    }

    /// Run the LLM compaction (phase 3, the last-resort fallback): build the
    /// prompt from the context manager's serialized context, ask the model
    /// for the continuation summary, and apply it. The summarizer is a
    /// SEPARATE agent: it streams with its OWN system prompt from the context
    /// manager (never the agent loop's fixed system prompt), and each token is
    /// forwarded to the TUI so the user watches the "Summarizing" box fill
    /// live. Returns whether the summary was applied.
    ///
    /// Overflow recovery (the accepted design): when the provider rejects the
    /// prompt as larger than its context window, ONE tool chain is drained
    /// per attempt and the call is retried ("1 por vez"); when the provider
    /// reported its window, further chains are drained locally with the token
    /// estimate so no HTTP round trip is paid per chain. The prompt is
    /// REBUILT before every attempt, so a retry always serializes the
    /// current (post-drain) timeline — never a stale prompt that still
    /// contains the removed chains. Once every chain is gone the overflow is
    /// marked stuck for this provider and the user is notified. Generic
    /// errors are retried [`MAX_COMPACTION_RETRIES`] times with exponential
    /// backoff, then a TUI notification is surfaced.
    async fn llm_compact(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent, ToastVariant};
        let provider = self.connector.provider_name().unwrap_or("?");
        // The provider's window already overflowed with every tool chain
        // drained: the summarizer call is doomed — skip it and re-surface the
        // notification (throttled) instead of burning a paid call per dispatch.
        if self.context_manager.overflow_stuck(provider) {
            self.notify_context_overflow(tx);
            return false;
        }
        // Defensive — the harness only calls this after `NeedsLlmCompaction`,
        // so there is normally something to compact.
        let mut request = match self.context_manager.llm_compaction_request() {
            Some(request) => request,
            None => return false,
        };
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        });
        // This call starts a fresh retry budget — a previous call (entry or
        // mid-loop) must not shorten it.
        self.compaction_generic_retries = 0;
        let mut summary = String::new();
        let outcome = loop {
            // A failed attempt's partial tokens must not pollute the summary
            // applied from a later successful attempt (the TUI box keeps its
            // already-streamed text, but the APPLIED value stays clean).
            summary.clear();
            match self
                .stream_summarize_for_compaction(&request.system, &request.prompt, |chunk| {
                    summary.push_str(chunk);
                    let _ = tx.send(HarnessEvent::LlmCompactionToken {
                        text: chunk.to_string(),
                    });
                })
                .await
            {
                Ok(()) => break CompactionOutcome::Applied,
                Err(CompactionErr::Interrupted) => {
                    self.compaction_generic_retries = 0;
                    break CompactionOutcome::Failed;
                }
                Err(CompactionErr::ContextWindow { window_tokens }) => {
                    // Known-window contingency: the single-shot transcript
                    // overflowed the provider. When the provider reported its
                    // window, drive the split instead of draining — it
                    // shrinks the whole timeline (protected items included).
                    if let Some(w) = window_tokens {
                        self.last_context_window = Some(w);
                    }
                    if window_tokens.is_some() && self.split_context(tx).await {
                        self.compaction_generic_retries = 0;
                        break CompactionOutcome::ResolvedByContingency;
                    }
                    if self.context_manager.evict_tool_chain_for_overflow() {
                        // One chain per attempt. When the provider reported
                        // its window, drain LOCALLY (no HTTP round trip per
                        // chain) until the estimate fits, then retry once.
                        if let Some(window) = window_tokens {
                            while self.context_manager.display_info().total_tokens > window
                                && self.context_manager.evict_tool_chain_for_overflow()
                            {
                            }
                        }
                        // REBUILD the request from the SHRUNK timeline: a
                        // stale prompt (serialized before the drain) still
                        // contains the removed chains, so retrying it is
                        // doomed to overflow again. When the drain already
                        // brought the total below the trigger, the overflow
                        // is resolved and there is nothing left to summarize.
                        match self.context_manager.llm_compaction_request() {
                            Some(rebuilt) => request = rebuilt,
                            None => break CompactionOutcome::ResolvedByContingency,
                        }
                        continue;
                    }
                    // No tool chain left to relieve the overflow — stuck.
                    self.context_manager.mark_overflow(provider);
                    self.notify_context_overflow(tx);
                    self.compaction_generic_retries = 0;
                    break CompactionOutcome::Failed;
                }
                Err(CompactionErr::Other(e)) => {
                    self.compaction_generic_retries += 1;
                    if self.compaction_generic_retries >= MAX_COMPACTION_RETRIES {
                        self.compaction_generic_retries = 0;
                        let _ = tx.send(HarnessEvent::Toast {
                            message: format!("LLM compaction failed: {e}"),
                            variant: ToastVariant::Error,
                        });
                        break CompactionOutcome::Failed;
                    }
                    tokio::time::sleep(compaction_retry_backoff(self.compaction_generic_retries))
                        .await;
                    continue;
                }
            }
        };
        let ok = match outcome {
            CompactionOutcome::Applied if !summary.trim().is_empty() => self
                .context_manager
                .apply_llm_summary(summary.trim().to_string()),
            // A contingency (drain or split) resolved the overflow — a
            // successful pass.
            CompactionOutcome::ResolvedByContingency => true,
            CompactionOutcome::Applied | CompactionOutcome::Failed => false,
        };
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: if ok {
                LlmCompactionEvent::Finished
            } else {
                LlmCompactionEvent::Failed
            },
        });
        ok
    }

    /// The ACTIVE model's known context window for the split-and-concatenate
    /// contingency: the window reported by the LAST context-window error (the
    /// most precise) or the last successful discovery. `None` = unknown — the
    /// legacy drain path applies instead.
    fn known_split_window(&self) -> Option<usize> {
        self.last_context_window.or(self.discovered_window)
    }

    /// Drive the split-and-concatenate contingency to completion.
    ///
    /// The timeline is summarized in sequential chunks (whole items, in
    /// historical order) by the split summarizer; each returned summary is
    /// appended to the staging buffer; when EVERYTHING is consumed the buffer
    /// is committed as the new single anchor. The timeline is untouched until
    /// that final commit, and a failure at any point aborts (everything
    /// stays). Persisted staging is resumed exactly where it stopped.
    ///
    /// Returns whether the split was committed. `false` means the context is
    /// unchanged — the caller falls back to the legacy behavior (chain drain,
    /// stuck overflow).
    async fn split_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent, ToastVariant};
        // Resume an in-progress split without needing a fresh window (the
        // window is persisted in the staging). A fresh split needs a KNOWN
        // window and a context that actually exceeds it.
        if !self.context_manager.split_active() {
            let Some(window) = self.known_split_window() else {
                return false;
            };
            if self.context_manager.display_info().total_tokens <= window {
                return false;
            }
            self.context_manager.begin_split(window);
        }
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        });
        let mut generic_retries = 0usize;
        let ok = loop {
            if self.context_manager.split_all_consumed() {
                break self.context_manager.commit_split();
            }
            let Some(request) = self.context_manager.split_next_chunk() else {
                break self.context_manager.commit_split();
            };
            let mut summary = String::new();
            match self
                .stream_summarize_for_compaction(&request.system, &request.prompt, |chunk| {
                    summary.push_str(chunk);
                    let _ = tx.send(HarnessEvent::LlmCompactionToken {
                        text: chunk.to_string(),
                    });
                })
                .await
            {
                Ok(()) => {
                    // All-or-nothing: an empty chunk summary would silently
                    // drop that chunk's content from the final anchor (the
                    // cursor still advances) — treat it as a failure and
                    // abort, keeping the timeline exactly as it was.
                    if summary.trim().is_empty() {
                        self.context_manager.abort_split();
                        break false;
                    }
                    generic_retries = 0;
                    self.context_manager
                        .advance_split(&summary, request.chunk_end);
                }
                Err(CompactionErr::Interrupted) => {
                    // Atomicity: abort — the timeline stays exactly as it was.
                    self.context_manager.abort_split();
                    break false;
                }
                Err(CompactionErr::ContextWindow { .. }) => {
                    // A chunk sized to a KNOWN window should never overflow;
                    // the window guess was wrong — abort and let the caller
                    // fall back to the legacy drain.
                    self.context_manager.abort_split();
                    break false;
                }
                Err(CompactionErr::Other(e)) => {
                    generic_retries += 1;
                    if generic_retries >= MAX_COMPACTION_RETRIES {
                        self.context_manager.abort_split();
                        let _ = tx.send(HarnessEvent::Toast {
                            message: format!("LLM compaction failed: {e}"),
                            variant: ToastVariant::Error,
                        });
                        break false;
                    }
                    tokio::time::sleep(compaction_retry_backoff(generic_retries)).await;
                    continue;
                }
            }
        };
        self.compaction_generic_retries = 0;
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: if ok {
                LlmCompactionEvent::Finished
            } else {
                LlmCompactionEvent::Failed
            },
        });
        ok
    }

    /// Surface the context-window overflow to the user through the TUI toast
    /// system, throttled by [`OVERFLOW_TOAST_COOLDOWN`] so a stuck provider
    /// keeps reminding the user ("switch the model / start a new session")
    /// without spamming a toast on every dispatch iteration.
    fn notify_context_overflow(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        use super::events::{HarnessEvent, ToastVariant};
        let now = tokio::time::Instant::now();
        let cooldown_ok = match self.last_overflow_toast {
            Some(last) => now.duration_since(last) >= OVERFLOW_TOAST_COOLDOWN,
            None => true,
        };
        if !cooldown_ok {
            return;
        }
        self.last_overflow_toast = Some(now);
        let provider = self.connector.provider_name().unwrap_or("?");
        let _ = tx.send(HarnessEvent::Toast {
            message: format!(
                "Context window exceeded ({provider}). Switch model or start new session."
            ),
            variant: ToastVariant::Warning,
        });
    }

    /// Ask the summarizer model for the compaction summary, STREAMING the
    /// tokens through `on_token` (which forwards them to the TUI as
    /// [`HarnessEvent::LlmCompactionToken`]). The summarizer is a dedicated
    /// agent: it uses the context manager's OWN system prompt (`system`), not
    /// the agent loop's fixed system prompt — it never sees the tool
    /// definitions or the harness instructions. Mockable in tests exactly
    /// like [`Self::chat`].
    ///
    /// Errors are CLASSIFIED (not flattened to strings) so the caller can
    /// distinguish a context-window overflow — the one error that draining
    /// tool chains can fix — from generic failures and user interruptions.
    async fn stream_summarize_for_compaction(
        &mut self,
        system: &str,
        prompt: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<(), CompactionErr> {
        #[cfg(test)]
        if let Some(response) = self.next_mock_chat() {
            match response {
                Ok(text) => {
                    on_token(&text);
                    return Ok(());
                }
                Err(msg) => {
                    return Err(match msg.as_str() {
                        INTERRUPTED_MARKER => CompactionErr::Interrupted,
                        CONTEXT_WINDOW_MARKER => CompactionErr::ContextWindow {
                            window_tokens: None,
                        },
                        other => {
                            // Test-only: "{CONTEXT_WINDOW_MARKER}:{window}"
                            // reports the window along with the overflow, so
                            // the mock can drive the REACTIVE split fork
                            // inside `llm_compact` (an overflow error WITH a
                            // reported window).
                            if let Some(window) = other
                                .strip_prefix(CONTEXT_WINDOW_MARKER)
                                .and_then(|rest| rest.strip_prefix(':'))
                                .and_then(|n| n.parse::<usize>().ok())
                            {
                                CompactionErr::ContextWindow {
                                    window_tokens: Some(window),
                                }
                            } else {
                                CompactionErr::Other(other.to_string())
                            }
                        }
                    });
                }
            }
        }

        use tokio_stream::StreamExt;
        // The summarizer is a SEPARATE agent: its request carries NO tool
        // definitions (a dedicated connector call that clones the params with
        // tools cleared), so the model answers with prose, never a tool call.
        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_system_no_tools(prompt, system) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_summarize CONNECTOR_ERR={e}");
                        return Err(match e {
                            ConnectorError::ContextWindowExceeded {
                                window_tokens, ..
                            } => CompactionErr::ContextWindow { window_tokens },
                            other => CompactionErr::Other(other.to_string()),
                        });
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                log::debug!("stream_summarize STOPPED during connect");
                return Err(CompactionErr::Interrupted);
            }
        };
        loop {
            // Poll the stream with a periodic stop check (like the main loop's
            // streams), so a stalled provider stays interruptible by the user.
            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        log::debug!("stream_summarize STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(CompactionErr::Other(e)),
                    None => break,
                }
            };
            let stop = self
                .stop_signal
                .as_ref()
                .is_some_and(|s| s.load(Ordering::Relaxed));
            if stop {
                log::debug!("stream_summarize STOPPED by signal");
                // Interrupted mid-summary: do NOT half-apply a truncated
                // summary — surface the interruption so `llm_compact` skips
                // the compaction this round (the context stays as it was).
                return Err(CompactionErr::Interrupted);
            }
            let token = chunk.token();
            if !token.is_empty() {
                on_token(token);
            }
        }
        Ok(())
    }

    /// Stream a chat completion using a proper messages array (native tool-call
    /// format). This is the replacement for the old text-based
    /// [`stream_chat`](Self::stream_chat) when using the native tool API.
    ///
    /// The `system` parameter is the system context (instructions + tool
    /// definitions, no history). The `messages` array contains the
    /// conversation history with roles (user, assistant with `tool_calls`,
    /// tool with `tool_call_id`).
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start.
    pub async fn stream_chat_with_messages(
        &mut self,
        system: &str,
        messages: &[ChatMessage],
        mut on_token: impl FnMut(&str),
    ) -> Result<String, String> {
        // Reset the truncation signal and the thinking-block stash for this
        // stream before anything else.
        self.last_finish_reason = None;
        self.pending_thinking_blocks.clear();

        #[cfg(test)]
        if let Some(response) = self.mock_stream_queue.pop_front() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.process_stream_chunk(token, &mut extractor, &mut on_token);
                    }
                    // Tests can simulate a provider-side truncation by queueing
                    // a finish_reason per stream (consumed one at a time).
                    self.last_finish_reason = self.mock_finish_reasons.pop_front().flatten();
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        // A fresh real stream: forget any stashed context-window size from a
        // previous attempt (the mock path returns before this and sets its
        // own expectations).
        self.last_context_window = None;

        log::debug!("stream_chat_with_messages messages={}", messages.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_messages(system, messages) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_chat_with_messages CONNECTOR_ERR={e}");
                        // A context-window overflow must reach the retry loop
                        // as the identity marker (with the window size stashed)
                        // so it is never confused with a generic error.
                        return Err(match e {
                            ConnectorError::ContextWindowExceeded {
                                window_tokens, ..
                            } => {
                                self.last_context_window = window_tokens;
                                CONTEXT_WINDOW_MARKER.to_string()
                            }
                            other => other.to_string(),
                        });
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                log::debug!("stream_chat_with_messages STOPPED during connect");
                return Err(INTERRUPTED_MARKER.to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    log::debug!("stream_chat_with_messages STOPPED by signal");
                    break;
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        log::debug!("stream_chat_with_messages STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            // Capture the stream's terminal finish_reason so run_agent_loop can
            // detect max-token truncation ("length") and continue the loop
            // instead of treating the cut-off turn as finished.
            if let Some(f) = fr {
                self.last_finish_reason = Some(f.to_owned());
            }
            // Log only the first few tokens and every 100th — logging every
            // token costs a mutex + file write per token in debug builds.
            if token_count <= 5 || token_count.is_multiple_of(100) || fr.is_some() {
                log::debug!(
                    "stream_chat_with_messages token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            self.process_stream_chunk(token, &mut extractor, &mut on_token);
            // Stream reasoning/thinking deltas to the TUI so the user sees the
            // model "think" while it works (never echoed back to the model).
            let reasoning = chunk.reasoning();
            if !reasoning.is_empty()
                && let Some(ref rtx) = self.reasoning_tx
            {
                let _ = rtx.send(super::events::HarnessEvent::Reasoning {
                    text: reasoning.to_string(),
                });
            }
            // Stash Claude thinking blocks (emitted at turn end when the turn
            // ended with a tool call) so `push_tool_history` can replay them
            // verbatim on the follow-up request.
            if let Some(blocks) = chunk.thinking_blocks() {
                self.pending_thinking_blocks = blocks.to_vec();
            }
        }

        self.last_failed_raw = extractor.take_last_failed_raw();

        log::debug!("stream_chat_with_messages DONE total_tokens={token_count}");
        Ok("done".into())
    }

    /// Process a stream chunk through the extractor, routing tool calls and
    /// yielding text via the callback.
    fn process_stream_chunk(
        &mut self,
        token: &str,
        extractor: &mut ExtractAction,
        on_token: &mut dyn FnMut(&str),
    ) {
        let action = extractor.extract_stream(token);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_token(&text);
            }
            StreamAction::ToolCall(tc) => match self.handle_harness_tool(&tc) {
                None => {
                    self.tool_issuer.push_back(tc);
                }
                Some(result) if !result.is_empty() => {
                    self.push_tool_history(
                        &tc.id,
                        &tc.name,
                        &tc.arguments,
                        &tc.thought_signature,
                        &result,
                    );
                }
                _ => {}
            },
            StreamAction::Pending => {}
        }
    }

    /// Stream a chat completion, calling `on_token` with each text delta.
    ///
    /// Use this when streaming is enabled — tokens are delivered in real time
    /// via the callback. Returns `Ok("done".into())` when the stream finishes.
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start or a chunk is malformed.
    pub async fn stream_chat(
        &mut self,
        input: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<String, String> {
        // Reset the truncation signal for this stream before anything else.
        self.last_finish_reason = None;

        #[cfg(test)]
        if let Some(response) = self.mock_stream_queue.pop_front() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.process_stream_chunk(token, &mut extractor, &mut on_token);
                    }
                    // Tests can simulate a provider-side truncation by queueing
                    // a finish_reason per stream (consumed one at a time).
                    self.last_finish_reason = self.mock_finish_reasons.pop_front().flatten();
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        let context = self.build_chat_context();
        log::debug!("stream_chat context_len={}", context.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_system(input, &context) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_chat CONNECTOR_ERR={e}");
                        return Err(e.to_string());
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                log::debug!("stream_chat STOPPED during connect");
                return Err(INTERRUPTED_MARKER.to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    log::debug!("stream_chat STOPPED by signal");
                    break;
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        log::debug!("stream_chat STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            // Capture the stream's terminal finish_reason so run_agent_loop can
            // detect max-token truncation ("length") and continue the loop
            // instead of treating the cut-off turn as finished.
            if let Some(f) = fr {
                self.last_finish_reason = Some(f.to_owned());
            }
            // Log only the first few tokens and every 100th — logging every
            // token costs a mutex + file write per token in debug builds.
            if token_count <= 5 || token_count.is_multiple_of(100) || fr.is_some() {
                log::debug!(
                    "stream_chat token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            self.process_stream_chunk(token, &mut extractor, &mut on_token);
            // Stream reasoning/thinking deltas to the TUI so the user sees the
            // model "think" while it works (never echoed back to the model).
            let reasoning = chunk.reasoning();
            if !reasoning.is_empty()
                && let Some(ref rtx) = self.reasoning_tx
            {
                let _ = rtx.send(super::events::HarnessEvent::Reasoning {
                    text: reasoning.to_string(),
                });
            }
        }

        // Capture the last failed tool call raw JSON for correction feedback
        self.last_failed_raw = extractor.take_last_failed_raw();

        log::debug!("stream_chat DONE total_tokens={token_count}");
        Ok("done".into())
    }

    // Populate the connector's native tool definitions once.
    fn init_native_tools(&mut self) {
        use cosh_sdk::connector::ToolFunction as TFunc;

        let mut defs: Vec<ToolDefinition> = Vec::new();

        // Harness tools (e.g. stop_agent_loop)
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            defs.push(ToolDefinition::new(
                TFunc::new(&tool.name)
                    .with_description(&tool.description)
                    .with_parameters(tool.input_schema.clone()),
            ));
        }

        // Cosh tools — filtered by mode so the native API only exposes
        // tools the model is allowed to call in the current mode.
        if let Some(ref cosh) = self.cosh_tools {
            let descriptions: Vec<serde_json::Value> = match self.mode {
                Mode::Build | Mode::Yolo => {
                    // All tools minus disabled
                    cosh.tool_descriptions()
                        .into_iter()
                        .filter(|desc| {
                            !self
                                .disabled_tools
                                .contains(desc["name"].as_str().unwrap_or_default())
                        })
                        .collect()
                }
                Mode::Ask => {
                    // Only read-only / planning tools
                    cosh.tool_descriptions_filtered(&self.disabled_tools)
                }
            };

            for desc in descriptions {
                let name = desc["name"].as_str().unwrap_or_default();
                let description = desc["description"].as_str().unwrap_or_default();
                if let Some(input_schema) = desc.get("inputSchema").cloned() {
                    defs.push(ToolDefinition::new(
                        TFunc::new(name)
                            .with_description(description)
                            .with_parameters(input_schema),
                    ));
                }
            }
        }

        // MCP server tools
        for session in &self.sessions {
            for tool in &session.tools {
                let name: &str = tool.name.as_ref();
                if self.disabled_tools.contains(name) {
                    continue;
                }
                let description = tool.description.as_deref().unwrap_or_default();
                let input_schema = (*tool.input_schema).clone();
                defs.push(ToolDefinition::new(
                    TFunc::new(name)
                        .with_description(description)
                        .with_parameters(serde_json::Value::Object(input_schema)),
                ));
            }
        }

        self.connector.set_tools(defs);
    }

    /// Mirror the tools' current TODO list into the dedicated protected TODO
    /// context block (see `context_manager::todo_ctxt`). Called at loop start
    /// and after every `plan_*` dispatch so the block always reflects the
    /// authoritative `Plan` state.
    fn sync_todo_context(&mut self) {
        if let Some(cosh) = &self.cosh_tools {
            let list = cosh.todo_list();
            self.context_manager.set_todo_list(list);
        }
    }

    /// Add a tool call + its result to the context manager (structural
    /// layer — never prose-compressed). The native `tool_call → tool` chain is
    /// preserved 1:1: the call item renders as an `assistant` message with
    /// `tool_calls`, the result as a `tool` message with the matching id.
    fn push_tool_history(
        &mut self,
        id: &str,
        name: &str,
        args: &serde_json::Value,
        signature: &str,
        result: &str,
    ) {
        let args_str = serde_json::to_string(args).unwrap_or_default();
        // Providers require a non-empty tool_call_id; synthesize one when the
        // extractor did not attach an id (inline JSON calls).
        let tool_id = if id.is_empty() {
            format!("call_{:016x}", self.context_manager_pending_id())
        } else {
            id.to_string()
        };
        // `signature` is the Gemini 3.x thought signature (empty for every
        // other path) — carried so the follow-up request can replay the
        // native functionCall with its sibling thoughtSignature.
        // `thinking_blocks` are the Claude extended-thinking blocks that
        // preceded this turn's tool call (empty for every other path) —
        // replayed verbatim, signature included, on the follow-up request.
        self.context_manager.add_tool_call_with_thinking(
            &tool_id,
            name,
            &args_str,
            signature,
            std::mem::take(&mut self.pending_thinking_blocks),
        );
        self.context_manager.add_tool_result_flagged(
            &tool_id,
            result,
            result_is_useless(name, result),
        );
    }

    /// Monotonic id generator for synthetic tool_call ids (inline JSON calls
    /// without an extractor id).
    fn context_manager_pending_id(&mut self) -> u64 {
        // Reuse the context manager's monotonic counter through a tiny wrapper:
        // every tool without an id gets a fresh, globally-unique number.
        self.tool_call_synthetic += 1;
        self.tool_call_synthetic
    }

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
        // mid-loop messages): a disconnected receiver that drains nothing.
        let (_never, queued_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.run_agent_loop_inner(input, tx, answer_rx, perm_rx, stop_signal, queued_rx)
            .await;
    }

    /// Like [`run_agent_loop`](Self::run_agent_loop) but with an additional
    /// channel the TUI can push user messages into WHILE the loop is running:
    /// every message is recorded as a regular user turn and delivered to the
    /// model before the NEXT request (the "next request" queue), and the
    /// TUI is acknowledged via [`HarnessEvent::UserMessageInjected`].
    /// Messages still queued when the loop ends are never injected.
    pub async fn run_agent_loop_with_queued_input(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        perm_rx: tokio::sync::mpsc::UnboundedReceiver<super::guardrails::PermissionAction>,
        stop_signal: Arc<AtomicBool>,
        queued_input_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        self.run_agent_loop_inner(input, tx, answer_rx, perm_rx, stop_signal, queued_input_rx)
            .await;
    }

    /// Shared implementation of [`run_agent_loop`](Self::run_agent_loop) and
    /// [`run_agent_loop_with_queued_input`](Self::run_agent_loop_with_queued_input).
    /// The `queued_input_rx` channel is drained before every request; its
    /// messages are injected into the model context (see the docs of the
    /// public wrappers).
    async fn run_agent_loop_inner(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        mut answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        mut perm_rx: tokio::sync::mpsc::UnboundedReceiver<super::guardrails::PermissionAction>,
        stop_signal: Arc<AtomicBool>,
        mut queued_input_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        use super::events::HarnessEvent;
        use super::guardrails::{PermissionCheck, check_tool_permission};
        use cosh_tools::question::types::{QuestionInput, QuestionOutput};

        // The input lives in the context manager; `current_input` only carries
        // the harness's per-iteration steering message (empty for the first
        // iteration so the user prompt is not sent twice).
        let mut current_input = String::new();
        let mut iteration = 0u64;

        // Throttle the periodic context snapshots so the TUI can persist the
        // `.ctx` companion file incrementally without serializing the whole
        // context on every tool dispatch. The snapshot itself is emitted on
        // the agent thread (the expensive clone + bincode pass never touches
        // the UI thread).
        let mut last_snapshot = std::time::Instant::now();

        // Store the stop signal so stream_chat can check it mid-stream.
        self.stop_signal = Some(stop_signal.clone());

        // Route reasoning/thinking tokens to the TUI as they stream in.
        self.reasoning_tx = Some(tx.clone());

        // Route compaction-phase notifications to the TUI so it can show the
        // pipeline stopwatch and the per-phase lines live in the chat. The
        // pipeline runs synchronously on THIS thread, so the TUI (a separate
        // thread) receives `PipelineStarted` before the compression work and
        // `PipelineFinished` after it — exactly what the stopwatch needs.
        self.context_manager.set_compaction_observer({
            let tx = tx.clone();
            move |event| {
                let _ = tx.send(HarnessEvent::Compaction { event });
            }
        });

        // Pass the event tx to CoshTools for streaming tool output (e.g. bash)
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_event_tx(tx.clone());
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
            // drives the split-and-concatenate contingency when the held
            // context exceeds it (see `known_split_window`). A failed
            // discovery leaves it `None` — unknown window → legacy path.
            self.discovered_window = None;
            if let Some(window) = discovered_context_window(self.connector.effective_model()).await
            {
                self.discovered_window = Some(window);
                // Size the budget to the model's EFFECTIVE window, not the raw
                // advertised one — the advertised capacity is a poor estimate
                // of what the model can actually reason over (the "sweet
                // spot" sizing, see `effective_context_window`). The RAW
                // window is kept above in `discovered_window`, where it still
                // drives the split-and-concatenate contingency.
                self.context_manager
                    .set_max_tokens(effective_context_window(window));
            }
        }

        // A model/provider switch clears a previously recorded stuck
        // context-window overflow — the new provider gets a fresh chance.
        self.context_manager
            .sync_provider(self.connector.provider_name().unwrap_or("?"));
        self.compaction_generic_retries = 0;

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

        // Apply the 80% compaction (the synchronous pipeline + eviction
        // phases) before the first LLM request, so the initial context is
        // already within budget. When the deterministic phases exhaust every
        // draft and the total is still over the trigger, the LLM compaction
        // (phase 3, the last-resort fallback) runs; the in-flight input was
        // folded into the summary, so it is re-added for the model to see the
        // task verbatim.
        if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction) {
            // Known-window contingency: when the ACTIVE model's window is
            // known and the remaining context still exceeds it, the
            // single-shot compaction transcript would itself overflow the
            // provider — drive the split-and-concatenate path instead (a
            // restored in-progress split is always resumed).
            if self.context_manager.split_active()
                || self
                    .known_split_window()
                    .is_some_and(|w| self.context_manager.display_info().total_tokens > w)
            {
                self.split_context(&tx).await;
            } else {
                self.llm_compact(&tx).await;
            }
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
                    self.context_manager.close_loop();
                    let cs =
                        bincode::serialize(&self.context_manager.save_state()).unwrap_or_default();
                    let _ = tx.send(HarnessEvent::Stopped { context_state: cs });
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

            // Inject user messages queued for the NEXT REQUEST (the TUI's
            // "next request" queue) into the model context before this
            // request is built. Each is a regular protected user turn and is
            // acknowledged via `UserMessageInjected` so the TUI can move it
            // out of its pending area into the normal history. An injected
            // message is the newest instruction: it supersedes the
            // per-iteration steering prompt.
            let mut injected_any = false;
            while let Ok(text) = queued_input_rx.try_recv() {
                injected_any = true;
                self.context_manager.add_user(&text);
                let _ = tx.send(HarnessEvent::UserMessageInjected { text });
            }
            if injected_any {
                current_input.clear();
            }

            // Phase 1: stream the LLM response using native tool-call format.
            // The messages are rebuilt on EVERY attempt: when a context-window
            // overflow drains tool chains below, the retry must not re-send
            // the removed call/result pairs.
            log::debug!("run_agent_loop PHASE1_START iteration={iteration}");
            let system_context = self.build_chat_context();
            let mut assistant_response = String::new();
            let result = loop {
                let messages = self.context_manager.build_messages(&current_input);
                let attempt = self
                    .stream_chat_with_messages(&system_context, &messages, |token| {
                        assistant_response.push_str(token);
                        let _ = tx.send(HarnessEvent::Token {
                            text: token.to_string(),
                        });
                    })
                    .await;
                if let Err(ref e) = attempt
                    && e == CONTEXT_WINDOW_MARKER
                {
                    // Known-window contingency: prefer the split path — it
                    // shrinks the WHOLE timeline (protected items included)
                    // into a fitting anchor, while draining only removes tool
                    // chains. On success retry immediately; on failure fall
                    // through to the legacy drain.
                    if self.split_context(&tx).await {
                        continue;
                    }
                    let window = self.last_context_window.take();
                    if self.context_manager.evict_tool_chain_for_overflow() {
                        // One chain per attempt; when the provider
                        // reported its window, drain locally (no HTTP
                        // round trip per chain) until the estimate fits.
                        // NOTE: the local estimate counts COMPRESSED drafts
                        // while the request sends originals (and the
                        // compaction transcript passes tool results verbatim,
                        // untruncated), so it is
                        // approximate in both directions — only a hint to
                        // skip round trips, never a hard guarantee.
                        if let Some(window) = window {
                            while self.context_manager.display_info().total_tokens > window
                                && self.context_manager.evict_tool_chain_for_overflow()
                            {
                            }
                        }
                        // Recoverable: still draining — no toast yet (the
                        // retry may succeed).
                        continue;
                    }
                    // No chain left: the overflow is stuck. Surface the
                    // (throttled) warning and fall through to the normal
                    // error handling with a HUMAN-readable message.
                    self.context_manager
                        .mark_overflow(self.connector.provider_name().unwrap_or("?"));
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
                    self.context_manager.close_loop();
                    let _ = tx.send(HarnessEvent::Stopped {
                        context_state: bincode::serialize(&self.context_manager.save_state())
                            .unwrap_or_default(),
                    });
                    break;
                }

                let mut switched = false;
                while !self.fallbacks.is_empty() {
                    let (provider, model) = self.fallbacks.remove(0);
                    match Connector::new(&provider) {
                        Ok(c) => {
                            self.connector = c.with_model(&model);
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
                                    self.context_manager
                                        .set_max_tokens(effective_context_window(window));
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
                    // The fallback model may be smaller than the context we
                    // hold: when its window is known and the estimate exceeds
                    // it, drive the split contingency BEFORE retrying the
                    // request (an in-progress split is always resumed).
                    if self.context_manager.split_active()
                        || self
                            .known_split_window()
                            .is_some_and(|w| self.context_manager.display_info().total_tokens > w)
                    {
                        self.split_context(&tx).await;
                    }
                    continue;
                }

                // Generic terminal error: also surface a TUI notification, as
                // the user asked (the context-window case is notified
                // separately and throttled — do not double-toast it here).
                // The CONTEXT_WINDOW_MARKER is internal — never show it to
                // the user: translate it to a human message first.
                let user_msg = if e == CONTEXT_WINDOW_MARKER {
                    format!(
                        "Context window exceeded ({}). Switch model or start new session.",
                        self.connector.provider_name().unwrap_or("?")
                    )
                } else {
                    e.clone()
                };
                let _ = tx.send(HarnessEvent::Error(user_msg));
                if e != CONTEXT_WINDOW_MARKER {
                    use super::events::ToastVariant;
                    let _ = tx.send(HarnessEvent::Toast {
                        message: e,
                        variant: ToastVariant::Error,
                    });
                }
                break;
            }

            // Record the assistant's text response in the context manager so it
            // is delivered via the messages array on the next iteration. Tool
            // calls are recorded separately as structural items during dispatch
            // (never prose-compressed); an output that carried a tool call stays
            // structural too — only pure text is compressible.
            let had_tools = self.has_pending_tools();
            if !assistant_response.is_empty() {
                self.context_manager
                    .add_assistant(&assistant_response, !had_tools);
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
                    self.context_manager.close_loop();
                    let _ = tx.send(HarnessEvent::Done {
                        context_state: bincode::serialize(&self.context_manager.save_state())
                            .unwrap_or_default(),
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
                let info = self
                    .tool_issuer
                    .front()
                    .map(|tc| (tc.id.clone(), tc.name.clone(), tc.arguments.clone()));
                let info_sig = self
                    .tool_issuer
                    .front()
                    .map(|tc| tc.thought_signature.clone());

                if let Some((ref _call_id, ref name, ref args)) = info {
                    // log::debug!("run_agent_loop DISPATCH tool={name}");
                    let _ = tx.send(HarnessEvent::ToolCall {
                        tool: name.clone(),
                        input: args.clone(),
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
                            let _ = tx.send(HarnessEvent::QuestionRequest {
                                questions: q_input.questions.clone(),
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
                                    let output = QuestionOutput {
                                        questions: q_input.questions,
                                        answers,
                                    };
                                    let json = serde_json::to_string(&output)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    // Save to structured history (native tool format)
                                    if let Some((ref call_id, ref name, ref args)) = info {
                                        let tool_id = if call_id.is_empty() {
                                            format!("call_{:016x}", iteration)
                                        } else {
                                            call_id.clone()
                                        };
                                        self.push_tool_history(
                                            &tool_id,
                                            name,
                                            args,
                                            info_sig.as_deref().unwrap_or_default(),
                                            &json,
                                        );
                                    }
                                    let _ = tx.send(HarnessEvent::ToolResult { output: json });
                                }
                                Some(Err(e)) => {
                                    let _ = tx.send(HarnessEvent::ToolError { error: e });
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
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                        }
                    }
                } else {
                    // Normal dispatch for all other tools
                    // log::debug!("run_agent_loop dispatch_next start");

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
                            // Blocked by mode restrictions (Ask mode) — skip dispatch entirely
                            log::debug!(
                                "run_agent_loop PERM_DENIED tool={:?} reason={reason}",
                                info.as_ref().map(|(_, n, _)| n)
                            );
                            self.tool_issuer.pop_front();
                            self.tool_failure_count += 1;
                            self.correction_memory.push(&reason);
                            let _ = tx.send(HarnessEvent::ToolError { error: reason });
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call failures. Agent loop interrupted."
                                );
                                let _ = tx.send(HarnessEvent::Error(msg));
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
                                                if let Some((ref call_id, ref name, ref args)) =
                                                    info
                                                {
                                                    let tool_id = if call_id.is_empty() {
                                                        format!("call_{:016x}", iteration)
                                                    } else {
                                                        call_id.clone()
                                                    };
                                                    self.push_tool_history(
                                                        &tool_id,
                                                        name,
                                                        args,
                                                        info_sig.as_deref().unwrap_or_default(),
                                                        &msg,
                                                    );
                                                }
                                                self.correction_memory.push(&msg);
                                                let _ =
                                                    tx.send(HarnessEvent::ToolError { error: msg });
                                                continue;
                                            }
                                        }
                                    }
                                    None => {
                                        // Channel closed or stop requested
                                        log::debug!("run_agent_loop PERM_CHANNEL_CLOSED");
                                        self.tool_issuer.pop_front();
                                        let _ = tx.send(HarnessEvent::ToolError {
                                            error: "Internal error: permission channel closed"
                                                .to_string(),
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

                    // If this was AllowOnce, remove the approved paths from the
                    // allowlist in BOTH success and error cases — we never want
                    // AllowOnce paths to leak into future calls.
                    if !allow_once_paths.is_empty()
                        && let Some(cosh) = self.cosh_tools.as_mut()
                    {
                        for p in &allow_once_paths {
                            match allow_once_tool.as_str() {
                                "fs_read" | "fs_write" | "fs_edit" | "fs_rollback" => {
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
                        DispatchOut::Ok(output) => {
                            self.tool_failure_count = 0;
                            log::debug!("run_agent_loop dispatch_next OK len={}", output.len());
                            // Record the tool call + result in native history
                            if let Some((ref call_id, ref name, ref args)) = info {
                                let tool_id = if call_id.is_empty() {
                                    // Generate a synthetic ID for inline tool calls
                                    format!("call_{:016x}", iteration)
                                } else {
                                    call_id.clone()
                                };
                                self.push_tool_history(
                                    &tool_id,
                                    name,
                                    args,
                                    info_sig.as_deref().unwrap_or_default(),
                                    &output,
                                );
                            }
                            let _ = tx.send(HarnessEvent::ToolResult { output });
                        }
                        DispatchOut::Err(e) => {
                            // Record the error in history so the model sees
                            // its tool call + the error as a proper turn.
                            if let Some((ref call_id, ref name, ref args)) = info {
                                let tool_id = if call_id.is_empty() {
                                    format!("call_{:016x}", iteration)
                                } else {
                                    call_id.clone()
                                };
                                self.push_tool_history(
                                    &tool_id,
                                    name,
                                    args,
                                    info_sig.as_deref().unwrap_or_default(),
                                    &e,
                                );
                            }
                            self.tool_failure_count += 1;
                            log::debug!(
                                "run_agent_loop dispatch_next ERR={e} \
                                 (failure #{}/{MAX_TOOL_RETRIES})",
                                self.tool_failure_count,
                            );
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call \
                                     failures. Agent loop interrupted."
                                );
                                let _ = tx.send(HarnessEvent::Error(msg));
                                terminal_sent = true;
                                break;
                            }
                        }
                        DispatchOut::Stopped => {
                            log::debug!("run_agent_loop dispatch_next STOPPED by user");
                            self.stop = true;
                            self.context_manager.close_loop();
                            let _ = tx.send(HarnessEvent::Stopped {
                                context_state: bincode::serialize(
                                    &self.context_manager.save_state(),
                                )
                                .unwrap_or_default(),
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

            if !had_tools {
                if extraction_failures > 0 {
                    // Model tried but all tool calls failed validation.
                    // Give it another chance with the correction prompt.
                    if iteration >= MAX_ITERATIONS {
                        // Safety net — emit a terminal event so the TUI does
                        // not stay in a "running" state.
                        self.context_manager.close_loop();
                        let _ = tx.send(HarnessEvent::Done {
                            context_state: bincode::serialize(&self.context_manager.save_state())
                                .unwrap_or_default(),
                        });
                        break;
                    }
                    log::debug!("run_agent_loop RETRY (extraction failures)");
                    current_input.clear();
                    continue;
                }
                // The provider cut the response at max_tokens (finish_reason
                // "length") with no tool calls: the turn is truncated, not
                // finished. Continue the loop so the model can complete the
                // answer instead of emitting a silent `Done` mid-sentence.
                // Bounded by MAX_ITERATIONS above.
                if self.last_finish_reason.as_deref() == Some("length") {
                    if iteration >= MAX_ITERATIONS {
                        // Safety net — emit a terminal event so the TUI does
                        // not stay in a "running" state.
                        self.context_manager.close_loop();
                        let _ = tx.send(HarnessEvent::Done {
                            context_state: bincode::serialize(&self.context_manager.save_state())
                                .unwrap_or_default(),
                        });
                        break;
                    }
                    log::debug!("run_agent_loop TRUNCATED (length) — continuing");
                    current_input =
                        "Your previous response was cut off by the output token limit. \
                         Please continue exactly where you left off."
                            .to_string();
                    continue;
                }
                // No tools and no extraction failures — conversation is complete.
                // The final text response becomes the loop's LoopClosure.
                log::debug!("run_agent_loop DONE (no tools)");
                self.context_manager.close_loop();
                let _ = tx.send(HarnessEvent::Done {
                    context_state: bincode::serialize(&self.context_manager.save_state())
                        .unwrap_or_default(),
                });
                break;
            }

            log::debug!("run_agent_loop RESTARTING with tool results");
            // The tool results have already been recorded in the context
            // manager via `push_tool_history` during dispatch. Just set the
            // continuation prompt for the next iteration.
            current_input =
                "Please continue with your response based on the information above.".to_string();

            // Compact before the next request. The LLM compaction (phase 3,
            // the last-resort fallback) runs when the deterministic phases
            // have nothing left to compress or evict and the total is still
            // over the trigger — the harness performs the model call.
            if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction) {
                self.llm_compact(&tx).await;
            }
            let _ = tx.send(HarnessEvent::ContextInfo {
                info: self.context_manager.display_info(),
            });

            // Incremental persistence: hand the TUI a serialized snapshot of
            // the running context at a throttled cadence so a crash/restart
            // mid-run does not lose the in-flight run's context.
            if last_snapshot.elapsed() >= self.snapshot_interval {
                last_snapshot = std::time::Instant::now();
                let _ = tx.send(HarnessEvent::ContextSnapshot {
                    context_state: bincode::serialize(&self.context_manager.save_state())
                        .unwrap_or_default(),
                });
            }
        }
        log::debug!("run_agent_loop EXIT");

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
                    "fs_read" | "fs_write" | "fs_edit" | "fs_rollback" => {
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

    /// Dequeue and dispatch the next pending tool call through the
    /// three-tier dispatch: cosh tools → MCP servers.
    ///
    /// Returns the text content of the tool response on success.
    /// Returns an error if the queue is empty, the tool is not found,
    /// arguments are malformed, or the execution fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue is empty, the tool is unknown,
    /// arguments are not a JSON object, or the execution fails.
    pub async fn dispatch_next(&mut self) -> Result<String, String> {
        let (tool_name, args_map) = {
            let tc = self
                .tool_issuer
                .front()
                .ok_or_else(|| "no pending tool calls".to_string())?;

            let serde_json::Value::Object(ref args_map) = tc.arguments else {
                self.tool_issuer.pop_front();
                return Err("tool arguments must be a JSON object".to_string());
            };

            (tc.name.clone(), args_map.clone())
        };

        // The merged `subagent_call` tool has TWO dispatch paths behind one
        // visible definition: `agent` present → external CLI (the CoshTools
        // arm below); `agent` omitted or empty → INTERNAL sub-agent (a nested
        // harness that reuses this connector, starts with an empty context,
        // runs in Yolo mode, persists nothing, and reports only its final
        // answer). The internal path needs this harness's connector and stop
        // signal, so it is routed here, at harness level, before Tier 1.
        if tool_name == "subagent_call" {
            let agent_empty = args_map
                .get("agent")
                .and_then(|v| v.as_str())
                .is_none_or(|a| a.trim().is_empty());
            if agent_empty {
                // Pop BEFORE any fallible step: dispatch_next consumes the
                // item on both success and error, so the caller never
                // re-dispatches a failed internal call.
                self.tool_issuer.pop_front();
                let input = args_map
                    .get("input")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let call_input = self
                    .cosh_tools
                    .as_ref()
                    .ok_or_else(|| "internal sub-agent unavailable".to_string())?
                    .resolve_subagent_input(input)?;
                return self.run_internal_subagent(call_input).await;
            }
        }

        // Tier 1: cosh tools
        let mut dispatched: Option<String> = None;
        if let Some(ref cosh) = self.cosh_tools {
            let args = serde_json::Value::Object(args_map.clone());
            match cosh.dispatch(&tool_name, args).await {
                Ok(result) => {
                    self.tool_issuer.pop_front();
                    dispatched = Some(result);
                }
                Err(err) if err.starts_with("unknown cosh tool") => {}
                Err(err) => {
                    self.tool_issuer.pop_front();
                    return Err(err);
                }
            }
        }
        if let Some(result) = dispatched {
            // Any plan tool may have changed the TODO list; mirror the
            // authoritative Plan state into the protected TODO block so the
            // model always sees the up-to-date plan.
            if tool_name.starts_with("plan_") {
                self.sync_todo_context();
            }
            return Ok(result);
        }

        // Tier 2: MCP sessions
        let idx = self
            .sessions
            .iter()
            .position(|s| s.tools.iter().any(|t| t.name == tool_name));

        let Some(idx) = idx else {
            self.tool_issuer.pop_front();
            return Err(format!("no server found for tool '{tool_name}'"));
        };

        self.tool_issuer.pop_front();
        let params = CallToolRequestParams::new(tool_name).with_arguments(args_map);

        let result = self.sessions[idx]
            .client
            .call_tool(params)
            .await
            .map_err(|e| e.to_string())?;

        let text: Vec<String> = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect();

        let text = text.join("\n");

        Ok(text)
    }

    /// Run the INTERNAL sub-agent path of the merged `subagent_call` tool:
    /// a nested [`Harness`] that reuses this connector (same provider/model),
    /// starts with an EMPTY context (no history, no `.ctx`), runs in
    /// [`Mode::Yolo`] (approval was already asked at the parent tool gate),
    /// and persists nothing. It runs on its OWN THREAD with a dedicated
    /// current-thread runtime (the same pattern the TUI uses): a nested
    /// harness is itself a full agent loop, and awaiting it inline would
    /// make `run_agent_loop`'s future recursive (an infinitely sized type),
    /// while the harness type is not `Send` (it owns the MCP client). Its
    /// events are bridged to the TUI as a plain `ToolOutput` stream — the
    /// tools it calls and any snapshot/summary events are dropped, so the
    /// main agent never sees its intermediate work and nothing is written
    /// to disk. Only the final report (the nested loop's `LoopClosure`) is
    /// returned as the tool result.
    ///
    /// # Errors
    ///
    /// Returns an error when the nested harness ends WITHOUT a final text
    /// report (connector error, user stop, model never answering), when its
    /// thread panicked, or when the channel closed — the real reason is
    /// surfaced instead of a silent empty result.
    #[allow(clippy::unwrap_used)]
    async fn run_internal_subagent(&mut self, input: String) -> Result<String, String> {
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

        std::thread::spawn(move || {
            use std::panic::AssertUnwindSafe;

            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = result_tx.send(Err(format!("internal sub-agent runtime: {e}")));
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
                        .with_fallbacks(fallbacks);
                    // The header (instructions + tool list) is NOT built by
                    // run_agent_loop itself — the TUI does it before every
                    // loop. Build it here so the sub-agent sees its own
                    // prompt and its own (blocklisted) tool set.
                    nested.format_header_context();

                    // Bridge: forward ONLY the sub-agent's text stream to
                    // the TUI (as `ToolOutput` under the shared tool name).
                    // Tool calls/results, snapshots, compaction and Done are
                    // dropped; the loop's fatal Error (if any) is captured
                    // so a failure is surfaced instead of masked.
                    let (nested_tx, mut nested_rx) =
                        tokio::sync::mpsc::unbounded_channel::<super::events::HarnessEvent>();
                    let bridge_tx = parent_tx.clone();
                    let last_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                    let err_capture = last_error.clone();
                    let bridge = tokio::spawn(async move {
                        while let Some(event) = nested_rx.recv().await {
                            match event {
                                super::events::HarnessEvent::Token { text }
                                | super::events::HarnessEvent::Reasoning { text } => {
                                    if let Some(tx) = &bridge_tx {
                                        let _ = tx.send(super::events::HarnessEvent::ToolOutput {
                                            tool: "subagent_call".to_string(),
                                            output: text,
                                            finished: false,
                                        });
                                    }
                                }
                                super::events::HarnessEvent::Error(e) => {
                                    *err_capture.lock().unwrap() = Some(e);
                                }
                                _ => {}
                            }
                        }
                    });

                    nested
                        .run_agent_loop(&input, nested_tx, answer_rx, perm_rx, stop_signal)
                        .await;

                    // The nested loop already closes its own context on every
                    // normal exit; the defensive call is idempotent (a no-op
                    // when the last item is not an assistant draft). The
                    // report is the LoopClosure content.
                    nested.context_manager.close_loop();
                    let final_answer = nested.context_manager.final_answer();
                    // Release the nested harness (and its event-tx clones) so
                    // the bridge drains every buffered event, then join it.
                    drop(nested);
                    let _ = bridge.await;

                    match final_answer {
                        Some(report) => {
                            if let Some(tx) = &parent_tx {
                                let _ = tx.send(super::events::HarnessEvent::ToolOutput {
                                    tool: "subagent_call".to_string(),
                                    output: report.clone(),
                                    finished: true,
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
        });

        let report = result_rx
            .await
            .map_err(|_| "internal sub-agent thread ended without a result".to_string())??;
        Ok(report)
    }
}

#[cfg(test)]
impl Harness {
    /// Create a harness for testing without a real connector.
    pub(crate) fn new_test() -> Self {
        let connector = Connector::new("openai").unwrap();
        Self {
            connector,
            sessions: Vec::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: None,
            mode: Mode::Build,
            instructions: None,
            stop_signal: None,
            reasoning_tx: None,
            pending_thinking_blocks: Vec::new(),
            stop: false,
            tool_issuer: VecDeque::new(),
            tool_call_synthetic: 0,
            context_manager: ContextManager::new(MAX_CONTEXT_TOKENS),
            disabled_tools: HashSet::new(),
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            last_finish_reason: None,
            correction_memory: CorrectionMemory::new(5),
            agent_permissions: HashSet::new(),
            approved_paths: HashSet::new(),
            fallbacks: Vec::new(),
            last_context_window: None,
            discovered_window: None,
            last_overflow_toast: None,
            compaction_generic_retries: 0,
            snapshot_interval: std::time::Duration::from_secs(10),
            mock_chat_response: None,
            mock_chat_queue: VecDeque::new(),
            mock_stream_queue: VecDeque::new(),
            mock_finish_reasons: VecDeque::new(),
            test_tools: Vec::new(),
        }
    }

    /// Register a tool schema for testing extraction without needing a real MCP session.
    pub(crate) fn with_test_tool(mut self, name: &str, schema: serde_json::Value) -> Self {
        self.test_tools.push(ToolSchema {
            name: name.to_string(),
            input_schema: schema,
        });
        self
    }

    /// Set a mock response for `chat()`. `Ok(text)` simulates a successful reply;
    /// `Err(msg)` simulates a connector failure.
    pub(crate) fn with_mock_chat(mut self, response: Result<&str, &str>) -> Self {
        self.mock_chat_response = Some(response.map(|s| s.to_string()).map_err(|s| s.to_string()));
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

    /// Resolve the next mock CHAT (summarizer) response: the queued one when
    /// present, else the single repeated [`Self::mock_chat_response`].
    #[cfg(test)]
    fn next_mock_chat(&mut self) -> Option<Result<String, String>> {
        if let Some(r) = self.mock_chat_queue.pop_front() {
            return Some(r);
        }
        self.mock_chat_response.clone()
    }

    /// Set the KNOWN context window of the active model, as if discovery had
    /// succeeded — lets tests drive the split-and-concatenate contingency (a
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

    /// Queue a mock `finish_reason` for the NEXT stream (consumed one per
    /// stream). Call once per stream to simulate truncation on just the first
    /// response and a normal completion on the following one.
    #[cfg(test)]
    pub(crate) fn with_mock_finish_reason(mut self, reason: Option<&str>) -> Self {
        self.mock_finish_reasons.push_back(reason.map(String::from));
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

    pub(crate) fn push_session(&mut self, session: ServerSession) {
        self.sessions.push(session);
    }

    pub(crate) fn push_tool_call(&mut self, tc: ToolCallData) {
        self.tool_issuer.push_back(tc);
    }
}
