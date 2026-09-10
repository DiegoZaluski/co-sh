#[cfg(not(test))]
use super::context::error_catalog_window;
use super::context::{ContextManager, MAX_CONTEXT_TOKENS, MapRequest, RunOutcome};
use super::correction_memory::CorrectionMemory;
use super::tools::{CoshTools, Tools, is_tool_disabled};
use crate::mcp::{McpConfig, McpManager};
use cosh_sdk::connector::{
    ChatMessage, ChatStream, ClaudeThinkingBlock, Connector, ConnectorError, ToolCallMode,
    ToolDefinition, resolve_reasoning_effort,
};
#[cfg(not(test))]
use cosh_sdk::connector::{discover_context_window, effective_context_window};
use cosh_sdk::extract_action::{
    ExtractAction, Item, NativeToolCall, StreamAction, ToolCallData, ToolSchema,
};
use cosh_tools::TOOL_FORMAT;
use cosh_tools::lsp::Lsp;
#[cfg(not(test))]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};
use tokio::time::Duration;

#[cfg(test)]
#[path = "test/map_reduce_test.rs"]
mod map_reduce_test;
#[path = "summarization.rs"]
mod summarization;

/// One event delivered to a streaming callback: a text token, or the reset
/// marker the SDK emits when it retries a mid-stream failure (the consumer
/// must drop any partial content from the failed attempt before the retried
/// response restarts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A text token.
    Token(String),
    /// The retried response is about to restart from the beginning.
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Ask,
    Yolo,
}

/// Result of the user-triggered `/compact`
/// ([`Harness::compact_on_demand`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualCompactionOutcome {
    /// The deterministic funnel ran and the LLM summary was applied.
    Compacted,
    /// Nothing to fold: the timeline holds at most a lone previous summary.
    NothingToCompact,
    /// The summarizer call failed (the automatic path already toasted).
    Failed,
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
    "  After fs_edit, the result carries the updated \u{00B6}path#TAG header — use it directly\n",
    "  for subsequent edits on the same file. Do NOT re-read a file just to get a new tag.\n",
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

/// Loop-detection window: the number of recent tool-call iterations watched
/// for a repeated identical interaction, mirroring crush's
/// `loopDetectionWindowSize`. Detection only fires once the window is full
/// (at least this many tool-calling iterations have happened).
const LOOP_DETECTION_WINDOW_SIZE: usize = 10;

/// Loop-detection threshold: an identical tool+input+result signature seen
/// more than this many times within the window stops the loop, mirroring
/// crush's `loopDetectionMaxRepeats`. Catches a model hammering the exact
/// same failing call long before it burns tokens up to `MAX_ITERATIONS`.
const LOOP_DETECTION_MAX_REPEATS: usize = 5;

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
pub(crate) const SUBAGENT_BLOCKED_TOOLS: &[&str] = &["ask_questions", "stop_agent_loop"];

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
pub(crate) const INTERRUPTED_MARKER: &str = "__cosh_interrupted__";

/// Internal marker returned by a stream function when the provider rejected
/// the request because the prompt exceeds its context window. Compared by
/// identity (constant), like [`INTERRUPTED_MARKER`] — the harness drains one
/// tool chain and retries when it sees this. `pub(crate)` so tests can
/// simulate the overflow through the mock paths.
pub(crate) const CONTEXT_WINDOW_MARKER: &str = "__cosh_context_window_exceeded__";

/// How many times a generic (non-context-window) summarizer error is retried
/// before the harness gives up and surfaces a TUI notification.
const MAX_COMPACTION_RETRIES: usize = 3;

/// Maximum number of independent map requests in flight. Four gives useful
/// latency reduction without assuming a provider grants high burst capacity.
const MAP_CONCURRENCY: usize = 4;

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

async fn wait_for_stop_signal(stop_signal: Option<Arc<AtomicBool>>) {
    let Some(stop_signal) = stop_signal else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        if stop_signal.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    // A window the user's own provider reported in a previous overflow is the
    // authoritative maximum — a local read that beats a public-catalog guess
    // and costs no network call.
    if let Some(window) = error_catalog_window(model) {
        return Some(window);
    }
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
/// can decide between retrying (generic errors), driving MapReduce
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
    /// A contingency (the hierarchical MapReduce path) brought the total down —
    /// the compaction is no longer needed.
    ResolvedByContingency,
    /// The summarizer call failed or produced nothing.
    Failed,
}

struct ParallelMapResult {
    ordinal: usize,
    result: Result<String, CompactionErr>,
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

/// Build the language-server wrapper bound to `cwd`.
///
/// Returns `None` when `COSH_LSP=off|0|false`, the config-driven LSP switch is
/// off, no tokio runtime is active, or under `cfg(test)` (unit tests must not
/// spawn real language servers; LSP behavior is covered by the SDK/tools
/// suites). The engine itself is the process-wide singleton in
/// [`crate::harness::lsp::global_lsp`].
fn build_lsp(cwd: &str) -> Option<Arc<Lsp>> {
    #[cfg(test)]
    {
        let _ = cwd;
        None
    }
    #[cfg(not(test))]
    {
        if !crate::harness::lsp::lsp_enabled()
            || matches!(
                std::env::var("COSH_LSP").as_deref(),
                Ok("off" | "0" | "false")
            )
        {
            return None;
        }
        crate::harness::lsp::global_lsp(cwd)
    }
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
            mcp: McpManager::new(),
            mcp_config: McpConfig::default(),
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
                    transport
                        .take()
                        .ok_or_else(|| crate::mcp::McpError::Connect(name.to_string(), "test transport already consumed".to_string()))
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

    /// Emit the terminal `Error` event of an agent loop. The failure is first
    /// recorded as a DISPLAY-ONLY context item (`ContextManager::add_error`)
    /// and its snapshot travels with the event: the transcript restores the
    /// styled error line after a restart, while the model never sees a
    /// provider failure as assistant output.
    fn emit_terminal_error(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        msg: String,
    ) {
        self.context_manager.add_error(&format!("Error: {msg}"));
        let _ = tx.send(super::events::HarnessEvent::Error {
            message: msg,
            context: Some(self.context_manager.save_state()),
        });
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

        self.header_context = out;
        &self.header_context
    }

    /// Build an extractor with all registered MCP, cosh, and internal tools.
    fn build_extractor(&self) -> ExtractAction {
        use crate::mcp::tool_to_schema;

        log::debug!("build_extractor: mcp_tools={}", self.mcp.all_tools().len());
        let mut extractor = ExtractAction::new();
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if self.disabled_tools.contains(tool.name.as_ref()) {
                    continue;
                }
                extractor.add_tool(tool_to_schema(tool));
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

    /// Run the LLM compaction (the last-resort fallback): build the
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
    async fn llm_compact_selected(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent, ToastVariant};
        // The stuck state is keyed by MODEL: the constraint is the model's
        // context window, not the provider's API — a model switch inside the
        // same provider must get a fresh chance.
        let model = self.compaction_model_key();
        // The provider's window already overflowed and the split could not fit
        // the context: the summarizer call is doomed — skip it and re-surface
        // the notification (throttled) instead of burning a paid call per
        // dispatch.
        if self.context_manager.overflow_stuck(&model) {
            self.notify_context_overflow(tx);
            return false;
        }
        if self.context_manager.compaction_staging_active() {
            return self.checkpoint_context(tx).await;
        }
        // Defensive — the harness only calls this after `NeedsLlmCompaction`,
        // so there is normally something to compact.
        let request = match self.context_manager.llm_compaction_request() {
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
                    match chunk {
                        StreamEvent::Reset => {
                            summary.clear();
                            let _ = tx.send(HarnessEvent::LlmCompaction {
                                event: LlmCompactionEvent::OutputStarted,
                            });
                        }
                        StreamEvent::Token(text) => {
                            summary.push_str(&text);
                            let _ = tx.send(HarnessEvent::LlmCompactionToken { text });
                        }
                    }
                })
                .await
            {
                Ok(()) => break CompactionOutcome::Applied,
                Err(CompactionErr::Interrupted) => {
                    self.compaction_interrupted = true;
                    self.compaction_generic_retries = 0;
                    break CompactionOutcome::Failed;
                }
                Err(CompactionErr::ContextWindow { window_tokens }) => {
                    // The single-shot transcript overflowed the provider. When
                    // the provider reported its window, remember it; then drive
                    // the hierarchical MapReduce contingency (sized against the
                    // known window or the current budget as a fallback) — it
                    // shrinks the WHOLE timeline (every item included).
                    if let Some(w) = window_tokens {
                        self.remember_error_window(w);
                    }
                    // Re-run the deterministic funnel against the provider's
                    // newly-known effective window before paying for chunked
                    // summarization. Old tool results may now be maskable
                    // while the recent raw window remains untouched.
                    if matches!(self.context_manager.run(), RunOutcome::Resolved) {
                        self.compaction_generic_retries = 0;
                        break CompactionOutcome::ResolvedByContingency;
                    }
                    if self.checkpoint_context(tx).await {
                        self.compaction_generic_retries = 0;
                        break CompactionOutcome::ResolvedByContingency;
                    }
                    // The contingency classifies its own failure. A generic
                    // provider failure or cancellation is not a stuck window.
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
                    tokio::select! {
                        () = wait_for_stop_signal(self.stop_signal.clone()) => break CompactionOutcome::Failed,
                        () = tokio::time::sleep(compaction_retry_backoff(self.compaction_generic_retries)) => {},
                    }
                    continue;
                }
            }
        };
        let ok = match outcome {
            CompactionOutcome::Applied if !summary.trim().is_empty() => self
                .context_manager
                .apply_llm_summary(summary.trim().to_string()),
            // The contingency resolved the overflow — a successful pass.
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

    /// The ACTIVE model's known context window for the hierarchical MapReduce
    /// contingency: the window reported by the LAST context-window error (the
    /// most precise) or the last successful discovery. `None` = unknown — the
    /// split is sized against the current token budget instead.
    fn known_checkpoint_window(&self) -> Option<usize> {
        if self.summarization_connector.is_some() {
            return self.summarization_window;
        }
        self.last_context_window.or(self.discovered_window)
    }

    /// Record a context window reported by a context-window overflow error.
    ///
    /// The RAW window is stashed for the hierarchical MapReduce contingency; the
    /// budget re-sizing (to the effective value) and the persistence to the
    /// provider-error catalog are delegated to the context manager, which owns
    /// that business logic.
    fn remember_error_window(&mut self, window: usize) {
        if self.summarization_connector.is_some() {
            self.summarization_window = Some(window);
            return;
        }
        self.last_context_window = Some(window);
        let model = self.connector.effective_model();
        self.context_manager.record_provider_window(model, window);
    }

    /// The user-triggered `/compact`: run the deterministic funnel across
    /// EVERY segment (the trigger floored at zero for this pass) and then the
    /// LLM summary of whatever remains — the same phases as the automatic 80%
    /// compaction, started early and unconditionally by explicit request.
    /// All lifecycle events (phase lines, the "Summarizing" box, streamed
    /// summary tokens, toasts on failure) reuse the automatic path's events,
    /// so the TUI needs no dedicated rendering.
    pub async fn compact_on_demand(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        stop_signal: Arc<AtomicBool>,
    ) -> ManualCompactionOutcome {
        use super::context::RunOutcome;
        use super::events::HarnessEvent;

        if !self.context_manager.has_compactable_content() {
            return ManualCompactionOutcome::NothingToCompact;
        }
        // Same wiring as a loop start: the shared stop flag reaches the
        // summarizer's stream loop and the TUI sees the summary streamed live.
        self.stop_signal = Some(stop_signal);
        self.reasoning_tx = Some(tx.clone());
        self.context_manager
            .set_model(self.connector.effective_model());

        self.context_manager.begin_manual_compaction();
        let outcome = self.context_manager.run();
        let ok = if matches!(outcome, RunOutcome::NeedsLlmCompaction) {
            self.llm_compact(tx).await
        } else {
            true
        };
        self.context_manager.end_manual_compaction();

        // Refresh the budget display and persist the compacted context via
        // the TUI's normal paths (the ContextSnapshot handler writes both the
        // session JSONL, display and context records alike).
        let _ = tx.send(HarnessEvent::ContextInfo {
            info: self.context_manager.display_info(),
        });
        if ok {
            let _ = tx.send(HarnessEvent::ContextSnapshot {
                context: self.context_manager.save_state(),
            });
        }
        if ok {
            ManualCompactionOutcome::Compacted
        } else {
            ManualCompactionOutcome::Failed
        }
    }

    /// Drive the active long-context contingency to completion. New work uses
    /// hierarchical MapReduce; persisted legacy split state keeps its original
    /// execution path so upgrades never strand an in-progress session.
    async fn checkpoint_context_selected(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        if self.context_manager.split_active() {
            self.legacy_split_context(tx).await
        } else {
            self.map_reduce_context(tx).await
        }
    }

    /// Build a checkpoint from independent map summaries, recursively reduce
    /// the complete ordered set, then audit the candidate before committing.
    /// Accepted staging remains range-addressed and resumable on any failure;
    /// the committed model view is unchanged until validation succeeds.
    async fn map_reduce_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent, ToastVariant};

        let model = self.compaction_model_key();
        if self.context_manager.overflow_stuck(&model) {
            self.notify_context_overflow(tx);
            return false;
        }
        let window = self
            .known_checkpoint_window()
            .unwrap_or_else(|| self.context_manager.max_tokens());
        if !self.context_manager.begin_map_reduce(window) {
            return false;
        }

        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        });
        self.context_manager
            .prepare_map_reduce_model(&model, window);
        let result = self.drive_map_reduce(tx).await;
        self.emit_compaction_snapshot(tx);
        self.compaction_generic_retries = 0;
        match &result {
            Err(CompactionErr::Other(message)) => {
                let _ = tx.send(HarnessEvent::Toast {
                    message: format!("LLM compaction failed: {message}"),
                    variant: ToastVariant::Error,
                });
            }
            Err(CompactionErr::ContextWindow { window_tokens }) => {
                if let Some(window) = window_tokens {
                    self.remember_error_window(*window);
                }
                self.context_manager.mark_overflow(&model);
                self.notify_context_overflow(tx);
            }
            Err(CompactionErr::Interrupted) => self.compaction_interrupted = true,
            Ok(_) => {}
        }
        let ok = result.is_ok_and(|committed| committed);
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: if ok {
                LlmCompactionEvent::Finished
            } else {
                LlmCompactionEvent::Failed
            },
        });
        ok
    }

    async fn drive_map_reduce(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> Result<bool, CompactionErr> {
        use super::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};

        loop {
            if self
                .stop_signal
                .as_ref()
                .is_some_and(|signal| signal.load(Ordering::Relaxed))
            {
                return Err(CompactionErr::Interrupted);
            }
            let pending = self.context_manager.pending_map_requests();
            if !pending.is_empty() && self.context_manager.parallel_mapping_enabled() {
                self.run_parallel_maps(pending, tx).await?;
                continue;
            }
            if let Some(request) = pending.into_iter().next() {
                let (completed, total) = self.context_manager.map_progress();
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::SequentialFallback,
                        completed,
                        total,
                    },
                });
                let result = self
                    .summarize_checkpoint_request(&request.system, &request.prompt, tx, false)
                    .await;
                let summary = match result {
                    Err(CompactionErr::ContextWindow { window_tokens })
                        if self
                            .context_manager
                            .repartition_incomplete_maps(window_tokens) =>
                    {
                        self.emit_compaction_snapshot(tx);
                        continue;
                    }
                    result => result?,
                };
                if !self
                    .context_manager
                    .accept_map_summary(request.ordinal, &summary)
                {
                    return Err(CompactionErr::Other(
                        "could not accept a MapReduce segment summary".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.next_reduce_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Reducing,
                        completed: request.start,
                        total: request.total,
                    },
                });
                if request.final_group {
                    let _ = tx.send(HarnessEvent::LlmCompaction {
                        event: LlmCompactionEvent::OutputStarted,
                    });
                }
                let mut summary = self
                    .summarize_checkpoint_request(
                        &request.system,
                        &request.prompt,
                        tx,
                        request.final_group,
                    )
                    .await?;
                let reread = self.context_manager.parse_reread_request(&summary);
                if summary.trim().starts_with("REREAD:") && reread.is_empty() {
                    return Err(CompactionErr::Other(
                        "reducer requested an invalid source range".into(),
                    ));
                }
                if !reread.is_empty() {
                    let conflict = self
                        .context_manager
                        .conflict_reduce_request(&request, &reread)
                        .ok_or_else(|| {
                            CompactionErr::Other("reducer requested an invalid source range".into())
                        })?;
                    summary = self
                        .summarize_checkpoint_request(
                            &conflict.system,
                            &conflict.prompt,
                            tx,
                            request.final_group,
                        )
                        .await?;
                    if summary.trim().starts_with("REREAD:") {
                        return Err(CompactionErr::Other(
                            "reducer could not resolve a conflict after raw reread".into(),
                        ));
                    }
                }
                if !self
                    .context_manager
                    .accept_reduce_summary(&request, &summary)
                {
                    return Err(CompactionErr::Other(
                        "could not accept a MapReduce reduction".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.next_validation_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Validating,
                        completed: request.start,
                        total: request.total,
                    },
                });
                let audit = self
                    .summarize_checkpoint_request(&request.system, &request.prompt, tx, false)
                    .await?;
                if !self.context_manager.accept_validation(&request, &audit) {
                    return Err(CompactionErr::Other(
                        "could not accept a checkpoint validation result".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.correction_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Correcting,
                        completed: 0,
                        total: 0,
                    },
                });
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::OutputStarted,
                });
                let corrected = self
                    .summarize_checkpoint_request(&request.system, &request.prompt, tx, true)
                    .await?;
                if !self.context_manager.accept_correction(&corrected) {
                    return Err(CompactionErr::Other(
                        "could not accept a corrected checkpoint".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            let committed = self.context_manager.commit_map_reduce();
            if committed {
                let summary = self
                    .context_manager
                    .save_state()
                    .items
                    .back()
                    .and_then(|item| {
                        if let super::context::ContextItem::Compaction { summary, .. } = item {
                            Some(summary.clone())
                        } else {
                            None
                        }
                    });
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::OutputStarted,
                });
                if let Some(text) = summary {
                    let _ = tx.send(HarnessEvent::LlmCompactionToken { text });
                }
            }
            return if committed {
                Ok(true)
            } else {
                Err(CompactionErr::Other(
                    "checkpoint could not be committed: source changed, output exceeds the budget, or reduction/validation limits were exhausted; accepted progress is preserved".into(),
                ))
            };
        }
    }

    fn emit_compaction_snapshot(
        &self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        let _ = tx.send(super::events::HarnessEvent::ContextSnapshot {
            context: self.context_manager.save_state(),
        });
    }

    async fn run_parallel_maps(
        &mut self,
        requests: Vec<MapRequest>,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> Result<(), CompactionErr> {
        use super::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};

        let (completed, total) = self.context_manager.map_progress();
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Progress {
                phase: LlmCompactionPhase::Mapping,
                completed,
                total,
            },
        });

        let mut requests = requests.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..MAP_CONCURRENCY {
            let Some(request) = requests.next() else {
                break;
            };
            self.spawn_parallel_map(&mut tasks, request, tx);
        }

        let mut generic_failure = false;
        let mut overflow_windows = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(ParallelMapResult {
                    ordinal,
                    result: Ok(summary),
                }) => {
                    if !self.context_manager.accept_map_summary(ordinal, &summary) {
                        tasks.abort_all();
                        return Err(CompactionErr::Other(
                            "could not accept a parallel MapReduce segment summary".into(),
                        ));
                    }
                    self.emit_compaction_snapshot(tx);
                    let (completed, total) = self.context_manager.map_progress();
                    let _ = tx.send(HarnessEvent::LlmCompaction {
                        event: LlmCompactionEvent::Progress {
                            phase: LlmCompactionPhase::Mapping,
                            completed,
                            total,
                        },
                    });
                }
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::Interrupted),
                    ..
                }) => {
                    tasks.abort_all();
                    return Err(CompactionErr::Interrupted);
                }
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::ContextWindow { window_tokens }),
                    ..
                }) => overflow_windows.push(window_tokens),
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::Other(_)),
                    ..
                })
                | Err(_) => generic_failure = true,
            }

            if !generic_failure
                && overflow_windows.is_empty()
                && let Some(request) = requests.next()
            {
                self.spawn_parallel_map(&mut tasks, request, tx);
            }
        }

        if !overflow_windows.is_empty() {
            let reported_window = overflow_windows.into_iter().flatten().min();
            if !self
                .context_manager
                .repartition_incomplete_maps(reported_window)
            {
                return Err(CompactionErr::ContextWindow {
                    window_tokens: reported_window,
                });
            }
            if generic_failure {
                self.context_manager.disable_parallel_mapping();
            }
            self.emit_compaction_snapshot(tx);
            return Ok(());
        }

        if generic_failure {
            self.context_manager.disable_parallel_mapping();
            self.emit_compaction_snapshot(tx);
            tokio::select! {
                () = tokio::time::sleep(compaction_retry_backoff(1)) => {},
                () = wait_for_stop_signal(self.stop_signal.clone()) => {
                    return Err(CompactionErr::Interrupted);
                }
            }
        }
        Ok(())
    }

    fn spawn_parallel_map(
        &mut self,
        tasks: &mut tokio::task::JoinSet<ParallelMapResult>,
        request: MapRequest,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        let connector = self.compaction_connector().clone();
        let stop_signal = self.stop_signal.clone();
        let event_tx = tx.clone();
        let window = self.context_manager.map_reduce_window();
        #[cfg(test)]
        let mock_response = self.next_mock_chat();
        #[cfg(test)]
        let mock_delay = self.mock_map_delay_queue.pop_front().unwrap_or(0);
        tasks.spawn(async move {
            let ordinal = request.ordinal;
            #[cfg(test)]
            if let Some(response) = mock_response {
                if mock_delay > 0 {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(mock_delay)) => {},
                        () = wait_for_stop_signal(stop_signal.clone()) => {
                            return ParallelMapResult { ordinal, result: Err(CompactionErr::Interrupted) };
                        }
                    }
                }
                let result = response
                    .map_err(Self::classify_mock_compaction_error)
                    .and_then(|summary| {
                        (!summary.trim().is_empty())
                            .then_some(summary)
                            .ok_or_else(|| {
                                CompactionErr::Other("summarizer returned an empty response".into())
                            })
                    });
                return ParallelMapResult { ordinal, result };
            }
            let result = Self::summarize_map_with_connector(
                connector,
                request.system,
                request.prompt,
                stop_signal,
                event_tx,
                window,
            )
            .await;
            ParallelMapResult { ordinal, result }
        });
    }

    async fn summarize_map_with_connector(
        connector: Connector,
        system: String,
        prompt: String,
        stop_signal: Option<Arc<AtomicBool>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        window: usize,
    ) -> Result<String, CompactionErr> {
        use tokio_stream::StreamExt;

        let encoding = crate::util::TokenEncoding::for_model(connector.effective_model());
        let input_tokens = encoding
            .estimate(&system)
            .saturating_add(encoding.estimate(&prompt))
            .saturating_add(64);
        let output_tokens = (window / 10).clamp(64, 2_000);
        let connector = connector.with_max_tokens(output_tokens as u32);
        let output_tokens = connector.effective_max_tokens().unwrap_or(output_tokens as u32) as usize;
        if input_tokens.saturating_add(output_tokens) >= window {
            return Err(CompactionErr::ContextWindow {
                window_tokens: Some(window),
            });
        }
        let mut stream = tokio::select! {
            result = connector.stream_chat_with_system_no_tools(&prompt, &system) => {
                result.map_err(|error| match error {
                    ConnectorError::ContextWindowExceeded { window_tokens, .. } => {
                        CompactionErr::ContextWindow { window_tokens }
                    }
                    other => CompactionErr::Other(other.to_string()),
                })?
            }
            () = wait_for_stop_signal(stop_signal.clone()) => {
                return Err(CompactionErr::Interrupted);
            }
        };
        let mut summary = String::new();
        let mut finish_reason = None;
        loop {
            let chunk = tokio::select! {
                chunk = stream.next() => chunk,
                () = wait_for_stop_signal(stop_signal.clone()) => {
                    return Err(CompactionErr::Interrupted);
                }
            };
            match chunk {
                Some(Ok(chunk)) if chunk.is_reset() => {
                    summary.clear();
                    finish_reason = None;
                }
                Some(Ok(chunk)) => {
                    if let Some(reason) = chunk.finish_reason() {
                        finish_reason = Some(reason.to_owned());
                    }
                    summary.push_str(chunk.token());
                }
                Some(Err(ConnectorError::ContextWindowExceeded { window_tokens, .. })) => {
                    return Err(CompactionErr::ContextWindow { window_tokens });
                }
                Some(Err(error)) => return Err(CompactionErr::Other(error.to_string())),
                None => break,
            }
        }
        if let Some(usage) = stream.usage().await {
            let reported_cost = stream.reported_cost().await;
            let _ = event_tx.send(super::events::HarnessEvent::Usage {
                usage,
                provider: connector.provider_name().unwrap_or("unknown").to_owned(),
                model: connector.model().unwrap_or("").to_owned(),
                reported_cost,
            });
        }
        Self::validate_summary_completion(finish_reason.as_deref())?;
        (!summary.trim().is_empty())
            .then_some(summary)
            .ok_or_else(|| CompactionErr::Other("summarizer returned an empty response".into()))
    }

    #[cfg(test)]
    fn classify_mock_compaction_error(message: String) -> CompactionErr {
        match message.as_str() {
            INTERRUPTED_MARKER => CompactionErr::Interrupted,
            CONTEXT_WINDOW_MARKER => CompactionErr::ContextWindow {
                window_tokens: None,
            },
            other => other
                .strip_prefix(CONTEXT_WINDOW_MARKER)
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|window| window.parse::<usize>().ok())
                .map_or_else(
                    || CompactionErr::Other(message),
                    |window_tokens| CompactionErr::ContextWindow {
                        window_tokens: Some(window_tokens),
                    },
                ),
        }
    }

    async fn summarize_checkpoint_request(
        &mut self,
        system: &str,
        prompt: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        stream_output: bool,
    ) -> Result<String, CompactionErr> {
        use super::events::HarnessEvent;

        let mut retries = 0usize;
        loop {
            #[cfg(test)]
            let mock = self.next_mock_chat();
            #[cfg(not(test))]
            let mock: Option<Result<String, String>> = None;
            let result = if let Some(response) = mock {
                #[cfg(test)]
                {
                    response.map_err(Self::classify_mock_compaction_error)
                }
                #[cfg(not(test))]
                {
                    response.map_err(CompactionErr::Other)
                }
            } else {
                Self::summarize_map_with_connector(
                    self.compaction_connector().clone(),
                    system.to_string(),
                    prompt.to_string(),
                    self.stop_signal.clone(),
                    tx.clone(),
                    self.context_manager.map_reduce_window(),
                )
                .await
            };
            match result {
                Ok(summary) if !summary.trim().is_empty() => {
                    if stream_output {
                        let _ = tx.send(HarnessEvent::LlmCompaction {
                            event: super::events::LlmCompactionEvent::OutputStarted,
                        });
                        let _ = tx.send(HarnessEvent::LlmCompactionToken {
                            text: summary.clone(),
                        });
                    }
                    return Ok(summary);
                }
                Ok(_) => {
                    return Err(CompactionErr::Other(
                        "summarizer returned an empty response".into(),
                    ));
                }
                Err(CompactionErr::Other(error)) => {
                    retries += 1;
                    if retries >= MAX_COMPACTION_RETRIES {
                        return Err(CompactionErr::Other(error));
                    }
                    tokio::select! {
                        () = tokio::time::sleep(compaction_retry_backoff(retries)) => {},
                        () = wait_for_stop_signal(self.stop_signal.clone()) => {
                            return Err(CompactionErr::Interrupted);
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Drive a persisted legacy split-and-concatenate contingency to
    /// completion.
    ///
    /// The timeline is summarized in sequential chunks (whole items, in
    /// historical order) by the split summarizer; each returned summary is
    /// appended to the staging buffer; when EVERYTHING is consumed the buffer
    /// is committed as the new single anchor. The timeline is untouched until
    /// that final commit, and a failure at any point aborts (everything
    /// stays). Persisted staging is resumed exactly where it stopped.
    ///
    /// Returns whether the split was committed. `false` means the context is
    /// unchanged — the caller marks the overflow stuck and notifies the user.
    async fn legacy_split_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent, ToastVariant};
        let model = self.compaction_model_key();
        // Resume an in-progress split without needing a fresh window (the
        // window is persisted in the staging). A fresh split sizes against the
        // known window, falling back to the current token budget when the
        // window is unknown.
        if self.summarization_connector.is_some() && self.context_manager.split_active() {
            let window = self
                .known_checkpoint_window()
                .unwrap_or_else(|| self.context_manager.max_tokens());
            self.context_manager.begin_split(window);
        }
        if !self.context_manager.split_active() {
            let window = self
                .known_checkpoint_window()
                .unwrap_or_else(|| self.context_manager.max_tokens());
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
                    match chunk {
                        StreamEvent::Reset => {
                            summary.clear();
                            let _ = tx.send(HarnessEvent::LlmCompaction {
                                event: LlmCompactionEvent::OutputStarted,
                            });
                        }
                        StreamEvent::Token(text) => {
                            summary.push_str(&text);
                            let _ = tx.send(HarnessEvent::LlmCompactionToken { text });
                        }
                    }
                })
                .await
            {
                Ok(()) => {
                    // All-or-nothing: an empty chunk summary would silently
                    // drop that chunk's content from the final anchor (the
                    // cursor still advances) — treat it as a failure and
                    // abort, keeping the timeline exactly as it was.
                    if summary.trim().is_empty() {
                        if self.summarization_connector.is_none() {
                            self.context_manager.abort_split();
                        }
                        break false;
                    }
                    generic_retries = 0;
                    self.context_manager
                        .advance_split(&summary, request.chunk_end);
                }
                Err(CompactionErr::Interrupted) => {
                    self.compaction_interrupted = true;
                    // Atomicity: abort — the timeline stays exactly as it was.
                    if self.summarization_connector.is_none() {
                        self.context_manager.abort_split();
                    }
                    break false;
                }
                Err(CompactionErr::ContextWindow { .. }) => {
                    // A chunk sized to the window should never overflow; the
                    // window guess was wrong — abort and mark the overflow
                    // stuck so the caller notifies the user.
                    if self.summarization_connector.is_none() {
                        self.context_manager.abort_split();
                    }
                    self.context_manager.mark_overflow(&model);
                    self.notify_context_overflow(tx);
                    break false;
                }
                Err(CompactionErr::Other(e)) => {
                    generic_retries += 1;
                    if generic_retries >= MAX_COMPACTION_RETRIES {
                        if self.summarization_connector.is_none() {
                            self.context_manager.abort_split();
                        }
                        let _ = tx.send(HarnessEvent::Toast {
                            message: format!("LLM compaction failed: {e}"),
                            variant: ToastVariant::Error,
                        });
                        break false;
                    }
                    tokio::select! {
                        () = wait_for_stop_signal(self.stop_signal.clone()) => break false,
                        () = tokio::time::sleep(compaction_retry_backoff(generic_retries)) => {},
                    }
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
        if self.summarization_connector.is_some() {
            // The explicit chain owns its fallback/exhaustion notification.
            return;
        }
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

    /// Emit a [`HarnessEvent::Usage`] for a just-completed stream, if the
    /// provider reported usage and the TUI is listening. Drains the stream
    /// (its accumulated usage) first. Used by every LLM request so session
    /// token/cost tracking reflects real API usage. Also carries the REAL
    /// cost reported by the provider (`usage.cost`), when it reports one.
    async fn emit_usage(&self, stream: &mut ChatStream) {
        let Some(usage) = stream.usage().await else {
            return;
        };
        // The stream is fully drained by now, so this never blocks.
        let reported_cost = stream.reported_cost().await;
        if let Some(ref tx) = self.reasoning_tx {
            let _ = tx.send(super::events::HarnessEvent::Usage {
                usage,
                provider: self
                    .connector
                    .provider_name()
                    .unwrap_or("unknown")
                    .to_owned(),
                model: self.connector.model().unwrap_or("").to_owned(),
                reported_cost,
            });
        }
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
    /// distinguish a context-window overflow — the one error the split
    /// contingency is meant to fix — from generic failures and user
    /// interruptions.
    async fn stream_summarize_for_compaction(
        &mut self,
        system: &str,
        prompt: &str,
        mut on_token: impl FnMut(StreamEvent),
    ) -> Result<(), CompactionErr> {
        #[cfg(test)]
        if let Some(response) = self.next_mock_chat() {
            match response {
                Ok(text) => {
                    on_token(StreamEvent::Token(text));
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
        let mut connector = self.compaction_connector().clone();
        if let Some(window) = self.known_checkpoint_window() {
            let encoding = crate::util::TokenEncoding::for_model(connector.effective_model());
            let output = (window / 10).clamp(64, 2_000);
            connector = connector.with_max_tokens(output as u32);
            let output = connector.effective_max_tokens().unwrap_or(output as u32) as usize;
            if encoding
                .estimate(system)
                .saturating_add(encoding.estimate(prompt))
                .saturating_add(64)
                .saturating_add(output)
                >= window
            {
                return Err(CompactionErr::ContextWindow {
                    window_tokens: Some(window),
                });
            }
        }
        let mut stream = tokio::select! {
            result = connector.stream_chat_with_system_no_tools(prompt, system) => {
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
        let mut finish_reason = None;
        loop {
            // Poll the stream with a periodic stop check (like the main loop's
            // streams), so a stalled provider stays interruptible by the user.
            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk,
                    () = wait_for_stop_signal(self.stop_signal.clone()) => {
                        return Err(CompactionErr::Interrupted);
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(ConnectorError::ContextWindowExceeded { window_tokens, .. })) => {
                        return Err(CompactionErr::ContextWindow { window_tokens });
                    }
                    Some(Err(e)) => return Err(CompactionErr::Other(e.to_string())),
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
            if chunk.is_reset() {
                finish_reason = None;
                on_token(StreamEvent::Reset);
                continue;
            }
            if let Some(reason) = chunk.finish_reason() {
                finish_reason = Some(reason.to_owned());
            }
            let token = chunk.token();
            if !token.is_empty() {
                on_token(StreamEvent::Token(token.to_string()));
            }
        }
        if let Some(usage) = stream.usage().await
            && let Some(tx) = &self.reasoning_tx
        {
            let _ = tx.send(super::events::HarnessEvent::Usage {
                usage,
                provider: connector.provider_name().unwrap_or("unknown").to_string(),
                model: connector.effective_model().unwrap_or("").to_string(),
                reported_cost: stream.reported_cost().await,
            });
        }
        Self::validate_summary_completion(finish_reason.as_deref())
    }

    /// Only natural text completion can become checkpoint evidence. EOF or
    /// a provider limit is not proof that the summary finished successfully.
    fn validate_summary_completion(reason: Option<&str>) -> Result<(), CompactionErr> {
        match reason {
            Some("stop" | "STOP" | "end_turn") => Ok(()),
            other => Err(CompactionErr::Other(format!(
                "summarizer response is incomplete (finish reason: {})",
                other.unwrap_or("missing")
            ))),
        }
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
        mut on_event: impl FnMut(StreamEvent),
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
                    // Simulate the SDK's mid-stream retry marker: the consumer
                    // must discard any partial content and restart.
                    if self.mock_stream_resets.pop_front() == Some(true) {
                        self.handle_mid_stream_reset(&mut extractor);
                        on_event(StreamEvent::Reset);
                    }
                    for token in &tokens {
                        // Mid-stream stop: an interrupted mock turn must
                        // surface the INTERRUPTED_MARKER error exactly like
                        // the real stream path — never a silent `Ok("done")`
                        // that makes the loop treat the truncation as a
                        // completed turn.
                        if self
                            .stop_signal
                            .as_ref()
                            .is_some_and(|s| s.load(Ordering::Relaxed))
                        {
                            return Err(INTERRUPTED_MARKER.to_string());
                        }
                        if self.mock_stream_delay_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(self.mock_stream_delay_ms))
                                .await;
                        }
                        self.dispatch_chunk(token, None, &mut extractor, &mut on_event);
                    }
                    // Test-only: route structured native tool calls through the
                    // same funnel the real-stream branch uses (a provider
                    // emitting `tool_calls`/`tool_use`/`functionCall` parts).
                    if let Some(calls) = self.mock_native_stream_queue.pop_front() {
                        for call in calls {
                            self.process_native_tool_call(&call, &mut extractor, &mut on_event);
                        }
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
                                if let Some(window) = window_tokens {
                                    self.remember_error_window(window);
                                }
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
                // log::debug!("stream_chat_with_messages STOPPED during connect");
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
                    // Surface the interruption as the marker error — NOT as a
                    // completed turn. Falling through to `Ok("done")` here
                    // made the agent loop treat the truncated response as a
                    // finished assistant turn and CONTINUE (Done → queued
                    // loop auto-restart) instead of stopping.
                    return Err(INTERRUPTED_MARKER.to_string());
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        // log::debug!("stream_chat_with_messages STREAM_ERR={e}");
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
            // The SDK retried a mid-stream failure: drop the partial content
            // of the failed attempt — the extractor buffer, the thinking
            // blocks stashed from it, and any finish reason it carried — and
            // tell the consumer (agent loop / TUI) to discard what it
            // rendered, so the retried response restarts clean.
            if chunk.is_reset() {
                log::debug!("stream_chat_with_messages RESET after retry");
                self.handle_mid_stream_reset(&mut extractor);
                on_event(StreamEvent::Reset);
                continue;
            }
            self.dispatch_chunk(token, chunk.tool_call(), &mut extractor, &mut on_event);
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

        // log::debug!("stream_chat_with_messages DONE total_tokens={token_count}");
        self.emit_usage(&mut stream).await;
        Ok("done".into())
    }

    /// Dispatch one stream chunk to the correct tool-call path, honoring the
    /// active [`ToolCallMode`]:
    /// - a native (structured) chunk always routes to the extractor's native
    ///   validation funnel ([`Self::process_native_tool_call`]), in both
    ///   modes;
    /// - text chunks are parsed for inline-JSON tool calls ONLY in
    ///   [`ToolCallMode::Inline`] — in `Native` mode (default) the text is
    ///   the model's answer and is emitted verbatim, never parsed (the crush
    ///   contract: tool calls arrive only as structured parts).
    fn dispatch_chunk(
        &mut self,
        token: &str,
        native: Option<&NativeToolCall>,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        if let Some(native) = native {
            self.process_native_tool_call(native, extractor, on_event);
        } else if self.connector.tool_call_mode() == ToolCallMode::Inline {
            self.process_stream_chunk(token, extractor, on_event);
        } else {
            on_event(StreamEvent::Token(token.to_string()));
        }
    }

    /// Process a stream chunk through the extractor, routing tool calls and
    /// yielding text via the callback.
    fn process_stream_chunk(
        &mut self,
        token: &str,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        let action = extractor.extract_stream(token);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_event(StreamEvent::Token(text));
            }
            StreamAction::ToolCall(tc) => self.route_tool_call(tc),
            StreamAction::Pending => {}
        }
    }

    /// Route a validated tool call: consume it if it is a harness tool,
    /// otherwise queue it for dispatch. Shared by the inline-JSON path
    /// ([`Self::process_stream_chunk`]) and the native structured path
    /// ([`Self::process_native_tool_call`]).
    fn route_tool_call(&mut self, tc: ToolCallData) {
        match self.handle_harness_tool(&tc) {
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
        }
    }

    /// Drop every trace of a failed stream attempt when the SDK emits a
    /// mid-stream retry marker: the extractor's half-parsed buffer, stashed
    /// thinking blocks, any tool calls already queued from the failed
    /// attempt, and the finish reason it carried. The retried response
    /// restarts from the beginning, so the failed attempt's tool calls must
    /// not be dispatched (or duplicated) alongside it.
    fn handle_mid_stream_reset(&mut self, extractor: &mut ExtractAction) {
        extractor.reset_stream_state();
        self.pending_thinking_blocks.clear();
        self.tool_issuer.clear();
        self.last_finish_reason = None;
    }

    /// Route a tool call the provider delivered NATIVELY (structured
    /// `tool_calls`/`tool_use`/`functionCall` parts) through the extractor's
    /// native validation funnel — the same `StreamAction` dispatch as the
    /// inline-JSON path, minus the text parsing. Validation failures are
    /// counted and fed to the correction memory exactly like a failed inline
    /// call (the tool-failure message streams as text).
    fn process_native_tool_call(
        &mut self,
        native: &NativeToolCall,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        let action = extractor.register_native_call(native);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_event(StreamEvent::Token(text));
            }
            StreamAction::ToolCall(tc) => self.route_tool_call(tc),
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
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<String, String> {
        // Reset the truncation signal for this stream before anything else.
        self.last_finish_reason = None;

        #[cfg(test)]
        if let Some(response) = self.mock_stream_queue.pop_front() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.dispatch_chunk(token, None, &mut extractor, &mut on_event);
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
                        // log::debug!("stream_chat CONNECTOR_ERR={e}");
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
                // log::debug!("stream_chat STOPPED during connect");
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
                    // log::debug!("stream_chat STOPPED by signal");
                    // Same contract as [`Self::stream_chat_with_messages`]:
                    // an interrupted stream must surface the marker error —
                    // the compaction caller maps it to
                    // `CompactionErr::Interrupted` (a silent `Ok` would make
                    // the truncation look like a finished summary).
                    return Err(INTERRUPTED_MARKER.to_string());
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        // log::debug!("stream_chat STREAM_ERR={e}");
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
            // The SDK retried a mid-stream failure: drop the partial content
            // of the failed attempt and tell the consumer to do the same, so
            // the retried response restarts clean (see
            // [`Self::stream_chat_with_messages`]).
            if chunk.is_reset() {
                self.handle_mid_stream_reset(&mut extractor);
                on_event(StreamEvent::Reset);
                continue;
            }
            self.dispatch_chunk(token, chunk.tool_call(), &mut extractor, &mut on_event);
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

        // log::debug!("stream_chat DONE total_tokens={token_count}");
        self.emit_usage(&mut stream).await;
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
                            !is_tool_disabled(
                                desc["name"].as_str().unwrap_or_default(),
                                &self.disabled_tools,
                            )
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

        // MCP server tools (hidden in Ask mode, like the header).
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if self.disabled_tools.contains(tool.name.as_ref()) {
                    continue;
                }
                defs.push(crate::mcp::tool_to_definition(tool));
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

    /// Hash one iteration's tool interactions (name, arguments, result — in
    /// dispatch order) into a stable signature for loop detection, mirroring
    /// crush's `getToolInteractionSignature`. Two iterations whose tool calls
    /// and results are byte-identical hash equal, so a model stuck repeating
    /// the exact same failing action is detected (see
    /// [`LOOP_DETECTION_WINDOW_SIZE`]/[`LOOP_DETECTION_MAX_REPEATS`]).
    ///
    /// FNV-1a 64-bit — deterministic within a run, no external deps.
    fn tool_interaction_signature(interactions: &[String]) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for part in interactions {
            for b in part.as_bytes() {
                hash ^= u64::from(*b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            // Separator so "a" + "b" and "ab" cannot collide.
            hash ^= 0xff;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// Add a tool call + its result to the context manager (structural
    /// layer — structural). The native `tool_call → tool` chain is
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

    /// Catalog names of every language server currently alive (Starting or
    /// Ready) across both managers this harness drives — its own passive
    /// diagnostics manager and the one inside [`CoshTools`] used by the
    /// `lsp_*` tools. Deduplicated, order not guaranteed. An empty slice means
    /// no server is running right now.
    fn active_lsp_server_names(&self) -> Vec<String> {
        use std::collections::HashSet;

        let mut servers = HashSet::new();
        let mut collect = |lsp: Option<&Arc<Lsp>>| {
            if let Some(lsp) = lsp {
                for (key, lifecycle) in lsp.manager().states() {
                    if matches!(
                        lifecycle,
                        cosh_sdk::lsp::ClientLifecycle::Starting
                            | cosh_sdk::lsp::ClientLifecycle::Ready
                    ) {
                        servers.insert(key.server);
                    }
                }
            }
        };
        collect(self.lsp.as_ref());
        collect(self.cosh_tools.as_ref().and_then(|c| c.lsp()));
        servers.into_iter().collect()
    }

    /// Whether LSP is enabled at all (`COSH_LSP` not `off`): true when either
    /// manager built a wrapper. Distinct from how many servers run right now.
    fn lsp_available(&self) -> bool {
        self.lsp.is_some() || self.cosh_tools.as_ref().and_then(|c| c.lsp()).is_some()
    }

    /// Snapshot of the current LSP engine state for the TUI's footer/tags.
    fn lsp_snapshot_event(&self) -> super::events::HarnessEvent {
        super::events::HarnessEvent::LspServers {
            available: self.lsp_available(),
            servers: self.active_lsp_server_names(),
        }
    }

    /// Current MCP snapshots for the TUI's prompt footer.
    pub fn mcp_snapshots(&self) -> Vec<crate::mcp::ServerSnapshot> {
        self.mcp.status_snapshots()
    }

    /// Snapshot of the current MCP state for the TUI's footer.
    fn mcp_snapshot_event(&self) -> super::events::HarnessEvent {
        super::events::HarnessEvent::McpStatus {
            servers: self.mcp_snapshots(),
        }
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
        self.drain_mcp().await;
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
        use super::events::{HarnessEvent, ToastVariant};
        use super::guardrails::{PermissionCheck, check_tool_permission};
        use cosh_tools::question::types::{QuestionInput, QuestionOutput};

        // The input lives in the context manager; `current_input` only carries
        // the harness's per-iteration steering message (empty for the first
        // iteration so the user prompt is not sent twice).
        let mut current_input = String::new();
        let mut iteration = 0u64;

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
                // Size the budget to the model's EFFECTIVE window, not the raw
                // advertised one — the advertised capacity is a poor estimate
                // of what the model can actually reason over (the "sweet
                // spot" sizing, see `effective_context_window`). The RAW
                // window is kept above in `discovered_window`, where it still
                // drives the hierarchical MapReduce contingency.
                self.context_manager
                    .set_max_tokens(effective_context_window(window));
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

        // Apply the 80% compaction before the first LLM request, so the
        // initial context is already within budget. When the useless-chain
        // sweep still leaves the total over the trigger, the LLM compaction
        // (or the hierarchical MapReduce contingency) runs; the in-flight input
        // was folded into the summary, so it is re-added for the model to see
        // the task verbatim.
        if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction)
            || self.context_manager.compaction_staging_active()
        {
            // Known-window contingency: when the ACTIVE model's window is
            // known and the remaining context still exceeds it, the
            // single-shot compaction transcript would itself overflow the
            // provider — drive the hierarchical MapReduce path instead (a
            // restored in-progress split is always resumed).
            if self.context_manager.compaction_staging_active()
                || self
                    .known_checkpoint_window()
                    .is_some_and(|w| self.context_manager.display_info().total_tokens > w)
            {
                self.checkpoint_context(&tx).await;
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
            let mut overflow_recoveries = 0usize;
            let result = loop {
                // Attempt boundary: the TUI scopes its discard-on-reset to the
                // message(s) opened after this point — a reset can then never
                // eat the PREVIOUS iteration's transcript.
                let _ = tx.send(HarnessEvent::BeginAssistant);
                let messages = self.context_manager.build_messages(&current_input);
                let tokens_before_attempt = self.context_manager.display_info().total_tokens;
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
                    if self.context_manager.compaction_staging_active()
                        || self
                            .known_checkpoint_window()
                            .is_some_and(|w| self.context_manager.display_info().total_tokens > w)
                    {
                        self.checkpoint_context(&tx).await;
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
                self.emit_terminal_error(&tx, user_msg);
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
            // calls are recorded separately as structural items during dispatch;
            // an output that carried a tool call stays structural too — only
            // pure text is closable.
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
                    let _ = tx.send(self.lsp_snapshot_event());
                    let _ = tx.send(HarnessEvent::Done {
                        context: self.context_manager.save_state(),
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
                                    let _ = tx.send(HarnessEvent::ToolError { error: e });
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
                                        // Loop-detection signature part.
                                        tool_interactions
                                            .push(format!("{name}\u{0}{args}\u{0}{json}"));
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
                        self.correction_memory.push(&reason);
                        let _ = tx.send(HarnessEvent::ToolError { error: reason });
                        // Halt stops the entire turn
                        if hr.halt {
                            let msg = "Turn halted by hook".to_string();
                            self.emit_terminal_error(&tx, msg);
                            terminal_sent = true;
                            break;
                        }
                        if self.tool_failure_count >= MAX_TOOL_RETRIES {
                            let msg = format!(
                                "{MAX_TOOL_RETRIES} consecutive tool call failures. Agent loop interrupted."
                            );
                            self.emit_terminal_error(&tx, msg);
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
                                self.emit_terminal_error(&tx, msg);
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
                                        // log::debug!("run_agent_loop PERM_CHANNEL_CLOSED");
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
                                    self.emit_terminal_error(&tx, reason);
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
                                // Loop-detection signature part.
                                tool_interactions.push(format!("{name}\u{0}{args}\u{0}{output}"));
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
                                // Loop-detection signature part.
                                tool_interactions.push(format!("{name}\u{0}{args}\u{0}{e}"));
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
                                self.emit_terminal_error(&tx, msg);
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
            if !tool_interactions.is_empty() {
                let sig = Self::tool_interaction_signature(&tool_interactions);
                loop_window.push_back(sig.clone());
                if loop_window.len() > LOOP_DETECTION_WINDOW_SIZE {
                    loop_window.pop_front();
                }
                let repeats = loop_window.iter().filter(|s| **s == sig).count();
                if loop_window.len() == LOOP_DETECTION_WINDOW_SIZE
                    && repeats > LOOP_DETECTION_MAX_REPEATS
                {
                    // log::debug!("run_agent_loop LOOP_DETECTED repeats={repeats}");
                    self.context_manager.close_loop();
                    let _ = tx.send(HarnessEvent::Toast {
                        message: "Agent stopped: repeated identical tool calls \
                                  detected (the model appears stuck in a loop)."
                            .to_string(),
                        variant: ToastVariant::Info,
                    });
                    let _ = tx.send(self.lsp_snapshot_event());
                    let _ = tx.send(HarnessEvent::Done {
                        context: self.context_manager.save_state(),
                    });
                    break;
                }
            }

            if !had_tools {
                if extraction_failures > 0 {
                    // Model tried but all tool calls failed validation.
                    // Give it another chance with the correction prompt.
                    if iteration >= MAX_ITERATIONS {
                        // Safety net — emit a terminal event so the TUI does
                        // not stay in a "running" state.
                        self.context_manager.close_loop();
                        let _ = tx.send(self.lsp_snapshot_event());
                        let _ = tx.send(HarnessEvent::Done {
                            context: self.context_manager.save_state(),
                        });
                        break;
                    }
                    // log::debug!("run_agent_loop RETRY (extraction failures)");
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
                        let _ = tx.send(self.lsp_snapshot_event());
                        let _ = tx.send(HarnessEvent::Done {
                            context: self.context_manager.save_state(),
                        });
                        break;
                    }
                    // log::debug!("run_agent_loop TRUNCATED (length) — continuing");
                    current_input =
                        "Your previous response was cut off by the output token limit. \
                         Please continue exactly where you left off."
                            .to_string();
                    continue;
                }

                // No tools and no extraction failures — conversation is complete.
                // The final text response becomes the loop's Closure.
                // log::debug!("run_agent_loop DONE (no tools)");
                self.context_manager.close_loop();
                // Final LSP snapshot: this break skips the per-cycle emission
                // below, so send the freshest state here.
                let _ = tx.send(self.lsp_snapshot_event());
                let _ = tx.send(HarnessEvent::Done {
                    context: self.context_manager.save_state(),
                });
                break;
            }

            // log::debug!("run_agent_loop RESTARTING with tool results");
            // The tool results have already been recorded in the context
            // manager via `push_tool_history` during dispatch. Just set the
            // continuation prompt for the next iteration.
            current_input =
                "Please continue with your response based on the information above.".to_string();

            // Compact before the next request. The LLM compaction runs when
            // the total is still over the trigger — the harness performs the
            // model call.
            if matches!(self.context_manager.run(), RunOutcome::NeedsLlmCompaction) {
                self.llm_compact(&tx).await;
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

        // A tool the user disabled on the Internal Tools screen is never
        // dispatched, even if the model hallucinates a call (its schema was
        // already hidden). Group prefixes (e.g. `lsp`) expand to their tools.
        if is_tool_disabled(&tool_name, &self.disabled_tools) {
            self.tool_issuer.pop_front();
            return Err(format!("tool '{tool_name}' is disabled"));
        }

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

        // Test-only tools have no real dispatch implementation; return a
        // canned, deterministic success so tests (e.g. loop detection) can
        // drive repeated identical (tool, args, result) interactions.
        #[cfg(test)]
        if let Some(schema) = self.test_tools.iter().find(|t| t.name == tool_name) {
            self.tool_issuer.pop_front();
            return Ok(format!("ok:{}", schema.name));
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

        // Tier 2: MCP servers (managed). Hidden in Ask mode, like the
        // header: the planning agent must not reach tools of unknown safety.
        // The harness-level "no server found" message is kept stable.
        if !matches!(self.mode, Mode::Ask) && self.mcp.owner_of(&tool_name).is_some() {
            self.tool_issuer.pop_front();
            if self.disabled_tools.contains(&tool_name) {
                return Err(format!("tool '{tool_name}' is disabled"));
            }
            return match self.mcp.call_tool(&tool_name, args_map).await {
                Ok(result) => Ok(crate::mcp::result_to_text(&result)),
                Err(crate::mcp::McpError::UnknownTool(_)) => {
                    Err(format!("no server found for tool '{tool_name}'"))
                }
                Err(err) => Err(err.to_string()),
            };
        }

        self.tool_issuer.pop_front();
        Err(format!("no server found for tool '{tool_name}'"))
    }

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
        let summarization_models = self.summarization_models.clone();
        let local_base_urls = self.local_base_urls.clone();
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
                        .with_fallbacks(fallbacks)
                        .with_summarization_models(summarization_models)
                        .with_local_base_urls(local_base_urls);
                    // The header (instructions + tool list) is NOT built by
                    // run_agent_loop itself — the TUI does it before every
                    // loop. Build it here so the sub-agent sees its own
                    // prompt and its own (blocklisted) tool set.
                    nested.format_header_context();

                    // Bridge: forward the sub-agent's text as ToolOutput,
                    // usage, and explicit summarization routing notices.
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
                            if Harness::forward_nested_accounting(&event, bridge_tx.as_ref()) {
                                continue;
                            }
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
                                super::events::HarnessEvent::Error { message, .. } => {
                                    *err_capture.lock().unwrap() = Some(message);
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
                    // report is the Closure content.
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
            summarization_models: Vec::new(),
            summarization_connector: None,
            summarization_window: None,
            compaction_interrupted: false,
            mock_compaction_models: Vec::new(),
            mock_summarization_windows: std::collections::HashMap::new(),
            local_base_urls: std::collections::HashMap::new(),
            last_context_window: None,
            discovered_window: None,
            last_overflow_toast: None,
            hook_runner: None,
            lsp: None,
            compaction_generic_retries: 0,
            snapshot_interval: std::time::Duration::from_secs(10),
            mock_chat_response: None,
            mock_chat_queue: VecDeque::new(),
            mock_map_delay_queue: VecDeque::new(),
            mock_stream_queue: VecDeque::new(),
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
    fn next_mock_chat(&mut self) -> Option<Result<String, String>> {
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
