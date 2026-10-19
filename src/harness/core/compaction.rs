//! LLM compaction: summarizer attempts, retry/backoff policy, context-window
//! discovery and the manual `/compact` funnel.
//!
//! Free items (constants, error/outcome classification, stop-signal polling
//! and window discovery) precede the `impl Harness` block.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(test))]
use std::collections::HashMap;
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};

use tokio::time::Duration;

use cosh_sdk::connector::{ChatStream, Connector, ConnectorError};
#[cfg(not(test))]
use cosh_sdk::connector::discover_context_window;

use super::{Harness, ManualCompactionOutcome, RunOutcome, StreamEvent};
#[cfg(not(test))]
use super::context::error_catalog_window;
#[cfg(test)]
use super::{CONTEXT_WINDOW_MARKER, INTERRUPTED_MARKER};

/// How many times a generic (non-context-window) summarizer error is retried
/// before the harness gives up and surfaces a TUI notification.
pub(super) const MAX_COMPACTION_RETRIES: usize = 3;

/// Maximum number of independent map requests in flight. Four gives useful
/// latency reduction without assuming a provider grants high burst capacity.
pub(super) const MAP_CONCURRENCY: usize = 4;

/// Base of the exponential backoff between summarizer retries (attempt N
/// waits `2^(N-1) * BASE`).
const COMPACTION_RETRY_BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Minimum output headroom used only for local request-fit checks when a
/// provider does not expose its default output reservation. It never becomes
/// a `max_tokens` request parameter.
pub(super) const MIN_SUMMARY_OUTPUT_HEADROOM: usize = 200;

/// Minimum time between two persistent context-overflow toasts: the user
/// must keep being reminded while the provider is stuck, but not spammed on
/// every dispatch iteration.
const OVERFLOW_TOAST_COOLDOWN: Duration = Duration::from_secs(15);

/// Handoff/tool-set consistency check: a compaction summary IS the handoff
/// to a fresh agent, and one produced from an older build's transcript (or
/// naming a tool the transcript shows being called) can prime the model into
/// calling a tool that no longer exists — the failure then compounds when
/// the model invents an argument schema for the removed tool.
///
/// Tool-name shape: an identifier with one of this project's tool-family
/// prefixes (`fs_`, `plan_`, `bash_`, ...). Argument fields (`file_hash`),
/// commands, and prose never carry those prefixes, so the false-positive
/// surface collapses to names that ARE current tools — which the membership
/// check filters out. Known gap (accepted): MCP tool names are server-defined
/// (often dotted, e.g. `acme.deploy`) and are NOT matched — a summary from a
/// session with a since-disconnected MCP server sails through; the dispatch
/// layer's unknown-tool rejection (which echoes the current tool set) is the
/// backstop for that case. Any `[Tool set notice]` paragraph already present
/// (the summary is re-serialized as `[Previous summary]` on later
/// compactions) is stripped first, so exactly one fresh notice exists.
pub(crate) fn annotate_summary_tool_set(summary: &str, current: &[String]) -> String {
    static TOOL_LIKE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tool_like = TOOL_LIKE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"\b((?:fs|plan|web|bash|lsp|find|recall|skills|subagent)_[a-z0-9_]+)\b")
            .unwrap()
    });
    // Drop notice paragraphs from earlier compactions (and the blank line
    // that separated them) so notices never accumulate across update-mode
    // re-compactions.
    let mut lines: Vec<&str> = summary
        .lines()
        .filter(|line| !line.trim_start().starts_with("[Tool set notice]:"))
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let summary = lines.join("\n");

    let mut stale: Vec<String> = Vec::new();
    for capture in tool_like.captures_iter(&summary) {
        let name = capture[1].to_string();
        if !current.iter().any(|tool| tool == &name) && !stale.contains(&name) {
            stale.push(name);
        }
    }
    if stale.is_empty() {
        return summary;
    }
    const MAX_LISTED: usize = 8;
    let more = if stale.len() > MAX_LISTED {
        format!(" (+{} more)", stale.len() - MAX_LISTED)
    } else {
        String::new()
    };
    stale.truncate(MAX_LISTED);
    format!(
        "{summary}\n\n[Tool set notice]: the notes above reference tools that are \
         no longer available in this session (removed, renamed, disabled, or \
         hidden by the current mode): {}{more}. Do not call them; use the \
         current tool set only.",
        stale.join(", ")
    )
}

/// Backoff after the `attempt`-th failed summarizer attempt (1-based).
pub(super) fn compaction_retry_backoff(attempt: usize) -> Duration {
    COMPACTION_RETRY_BACKOFF_BASE.saturating_mul(1u32 << attempt.min(4))
}

/// Whether an error string smells like an argument-validation failure
/// (missing/ill-typed arguments, schema corrections) rather than a runtime
/// failure — the trigger for echoing the tool's argument schema back.
/// The patterns are deliberately narrow so runtime and content errors do
/// not match: "must be a" (never bare "must", which content rules like
/// `old_string must be unique` use), "missing field" (serde) / "missing '"
/// (hand-written arms) but never bare "missing" (would catch "Missing
/// hashline snapshot tag", a runtime staleness error), "invalid type" /
/// "invalid `" (serde type mismatches and shape corrections) but never bare
/// "invalid" (remote web bodies can contain it), "expected a"/"expected one
/// of" (serde), "was provided", "failed to parse", "did not match".
/// Self-teaching correction prompts (which already say what to do) and
/// runtime failures (file not found, permission denied, timeouts) match
/// none of these.
pub(super) fn looks_like_argument_error(err: &str) -> bool {
    let lowered = err.to_ascii_lowercase();
    [
        "missing field",
        "missing '",
        "invalid type",
        "invalid `",
        "expected a",
        "expected one of",
        "must be a",
        "was provided",
        "failed to parse",
        "did not match",
    ]
    .iter()
    .any(|pattern| lowered.contains(pattern))
}

pub(super) async fn wait_for_stop_signal(stop_signal: Option<Arc<AtomicBool>>) {
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
pub(super) async fn discovered_context_window(model: Option<&str>) -> Option<usize> {
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
pub(super) enum CompactionErr {
    /// The user interrupted the generation.
    Interrupted,
    /// The provider rejected the prompt as larger than its context window.
    ContextWindow { window_tokens: Option<usize> },
    /// Generation stopped before a natural completion. Retrying the same
    /// request cannot change a deterministic truncation result.
    Incomplete(String),
    /// Any other connector error.
    Other(String),
}

/// Outcome of a single [`Harness::llm_compact`] run.
pub(super) enum CompactionOutcome {
    /// A summary was produced and is ready to be applied.
    Applied,
    /// A contingency (the hierarchical MapReduce path) brought the total down —
    /// the compaction is no longer needed.
    ResolvedByContingency,
    /// The summarizer call failed or produced nothing.
    Failed,
}

impl Harness {
    /// Run the LLM compaction (the last-resort fallback): build the
    /// prompt from the context manager's serialized context, ask the model
    /// for the continuation summary, and apply it. The summarizer is a
    /// SEPARATE agent: it streams with its OWN system prompt from the context
    /// manager (never the agent loop's fixed system prompt), and each token is
    /// forwarded to the TUI so the user watches the "Summarizing" box fill
    /// live. Returns whether the summary was applied.
    ///
    /// MapReduce is used when the selected summarizer cannot fit the complete
    /// request, or when verified contingency progress is being resumed.
    /// Generic errors use bounded retries with backoff and a failure toast.
    pub(super) async fn llm_compact_selected(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent};
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
        // Compaction accounting (file log, target `cosh::harness::context`):
        // the compaction request's own size — a summary call that re-sends
        // most of the session is itself a large bill. Logged before the
        // first summarizer attempt so trigger loops are visible.
        log::info!(
            target: "cosh::harness::context",
            "llm compaction: prompt_bytes={} system_bytes={}",
            request.prompt.len(),
            request.system.len(),
        );
        if self
            .summarization_request_overflow(&request.system, &request.prompt)
            .is_some()
        {
            return self.checkpoint_context(tx).await;
        }
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        });
        // This call starts a fresh retry budget — a previous call (entry or
        // mid-loop) must not shorten it.
        self.compaction_generic_retries = 0;
        let mut summary = String::new();
        let outcome = loop {
            // A retry starts a new response in both the candidate and the
            // TUI. Concatenating failed partial responses makes the visible
            // text look like one long, completed summary.
            if self.compaction_generic_retries > 0 {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::OutputStarted,
                });
            }
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
                Err(CompactionErr::Incomplete(_reason)) => {
                    // No toast: the failure is visible in the chat — the
                    // "Summarizing" line freezes on Failed and the loop's
                    // error path renders the provider error inline.
                    break CompactionOutcome::Failed;
                }
                Err(CompactionErr::Other(_e)) => {
                    self.compaction_generic_retries += 1;
                    if self.compaction_generic_retries >= MAX_COMPACTION_RETRIES {
                        self.compaction_generic_retries = 0;
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
            CompactionOutcome::Applied => {
                let summary = summary.trim();
                let ok = !summary.is_empty()
                    && self.context_manager.apply_llm_summary(summary.to_string());
                // No toast on failure. Note the budget case emits no error
                // event at all: the frozen "Summarizing" line is the only
                // signal in the chat — a toast here would be the sole copy
                // of the failure, not a duplicate.
                ok
            }
            // The contingency resolved the overflow — a successful pass.
            CompactionOutcome::ResolvedByContingency => true,
            CompactionOutcome::Failed => false,
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

    /// Install the summary annotator on the context manager so EVERY
    /// compaction commit path (single-shot, MapReduce contingency, legacy
    /// split) gets the same handoff/tool-set consistency check: the closure
    /// captures a snapshot of the current tool set — compaction runs are
    /// short-lived and the harness re-installs the annotator on
    /// construction, so the snapshot is fresh for every handoff.
    pub(super) fn install_summary_annotator(&mut self) {
        let current = self.effective_tool_names();
        self.context_manager
            .set_summary_annotator(Box::new(move |summary| {
                annotate_summary_tool_set(summary, &current)
            }));
    }

    /// The ACTIVE model's known context window for the hierarchical MapReduce
    /// contingency: the window reported by the LAST context-window error (the
    /// most precise) or the last successful discovery. `None` = unknown — the
    /// split is sized against the current token budget instead.
    pub(super) fn known_checkpoint_window(&self) -> Option<usize> {
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
    pub(super) fn remember_error_window(&mut self, window: usize) {
        if self.summarization_connector.is_some() {
            self.summarization_window = Some(window);
            return;
        }
        self.last_context_window = Some(window);
        let model = self.connector.effective_model();
        let effective = self.effective_context;
        self.context_manager
            .record_provider_window(model, window, effective);
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
        //
        // Clear the flag BEFORE adopting it: every summarizer entry point
        // races it against the connect future BEFORE connecting, so a flag
        // still set from an earlier interaction (ESC/Interrupt) aborts the
        // manual pass with `CompactionErr::Interrupted` before a single byte
        // leaves the process — and retrying `/compact` can never succeed.
        // The flag belongs to the PREVIOUS turn; a compaction that is being
        // started right now must begin from a clean slate. This is the
        // harness-side half of the same contract `start_agent_loop` honours;
        // the TUI clears its copy in `start_manual_compaction`, and this
        // clears whatever arrives through the API, keeping the fix
        // self-contained instead of relying on every caller remembering.
        let _ = stop_signal.swap(false, std::sync::atomic::Ordering::Relaxed);
        self.stop_signal = Some(stop_signal);
        self.reasoning_tx = Some(tx.clone());
        self.context_manager
            .set_model(self.connector.effective_model());

        self.context_manager.begin_manual_compaction();
        self.context_manager.set_binding_sender(tx.clone());
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
    pub(super) async fn checkpoint_context_selected(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        if self.context_manager.split_active() {
            self.legacy_split_context(tx).await
        } else {
            self.map_reduce_context(tx).await
        }
    }
    pub(super) async fn summarize_map_with_connector(
        connector: Connector,
        system: String,
        prompt: String,
        stop_signal: Option<Arc<AtomicBool>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        window: usize,
    ) -> Result<String, CompactionErr> {
        Self::summarize_map_with_connector_mode(
            connector,
            system,
            prompt,
            stop_signal,
            event_tx,
            window,
            false,
        )
        .await
    }

    pub(super) async fn summarize_map_with_connector_mode(
        connector: Connector,
        system: String,
        prompt: String,
        stop_signal: Option<Arc<AtomicBool>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        window: usize,
        full_output: bool,
    ) -> Result<String, CompactionErr> {
        use tokio_stream::StreamExt;

        let encoding = crate::util::TokenEncoding::for_model(connector.effective_model());
        let input_tokens = encoding
            .estimate(&system)
            .saturating_add(encoding.estimate(&prompt))
            .saturating_add(64);
        let (connector, output_tokens) = if full_output {
            (
                connector.clone(),
                connector.effective_max_tokens().map(|value| value as usize),
            )
        } else {
            let target = (window / 10).clamp(64, 2_000);
            let connector = connector.with_max_tokens(target as u32);
            let output = connector.effective_max_tokens().unwrap_or(target as u32) as usize;
            (connector, Some(output))
        };
        let output_headroom = output_tokens.unwrap_or(MIN_SUMMARY_OUTPUT_HEADROOM);
        if input_tokens.saturating_add(output_headroom) >= window {
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
            let reported_cost_credits = stream.reported_cost_credits().await;
            let remaining_credits = stream.remaining_credits().await;
            let _ = event_tx.send(super::events::HarnessEvent::Usage {
                usage,
                provider: connector.provider_name().unwrap_or("unknown").to_owned(),
                model: connector.model().unwrap_or("").to_owned(),
                reported_cost,
                reported_cost_credits,
                remaining_credits,
            });
        }
        Self::validate_summary_completion(finish_reason.as_deref())?;
        (!summary.trim().is_empty())
            .then_some(summary)
            .ok_or_else(|| CompactionErr::Other("summarizer returned an empty response".into()))
    }
    #[cfg(test)]
    pub(super) async fn summarize_checkpoint_request(
        &mut self,
        system: &str,
        prompt: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        stream_output: bool,
    ) -> Result<String, CompactionErr> {
        self.summarize_checkpoint_request_mode(system, prompt, tx, stream_output, false)
            .await
    }

    pub(super) async fn summarize_checkpoint_request_mode(
        &mut self,
        system: &str,
        prompt: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        stream_output: bool,
        full_output: bool,
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
                Self::summarize_map_with_connector_mode(
                    self.compaction_connector().clone(),
                    system.to_string(),
                    prompt.to_string(),
                    self.stop_signal.clone(),
                    tx.clone(),
                    self.context_manager.map_reduce_window(),
                    full_output,
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
                Err(CompactionErr::Incomplete(error)) => {
                    return Err(CompactionErr::Incomplete(error));
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
    pub(super) async fn legacy_split_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent};
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
                Err(CompactionErr::Incomplete(_e)) => {
                    if self.summarization_connector.is_none() {
                        self.context_manager.abort_split();
                    }
                    // No toast: the frozen "Summarizing" line (Failed)
                    // already reports it in the chat.
                    break false;
                }
                Err(CompactionErr::Other(_e)) => {
                    generic_retries += 1;
                    if generic_retries >= MAX_COMPACTION_RETRIES {
                        if self.summarization_connector.is_none() {
                            self.context_manager.abort_split();
                        }
                        // No toast: the frozen "Summarizing" line (Failed)
                        // already reports it in the chat.
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
    pub(super) fn notify_context_overflow(
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
    pub(super) async fn emit_usage(&self, stream: &mut ChatStream) {
        let Some(usage) = stream.usage().await else {
            return;
        };
        // The stream is fully drained by now, so this never blocks.
        let reported_cost = stream.reported_cost().await;
        let reported_cost_credits = stream.reported_cost_credits().await;
        let mut remaining_credits = stream.remaining_credits().await;
        // Fallback (mirrors Crush's FetchCredits): the Charm gateway's
        // streaming usage frames do NOT always carry
        // `usage.remaining.hypercredits` (observed live on glm models via
        // the Prism router), so when the stream reported none, fetch the
        // balance straight from the account endpoint. Best-effort: a
        // failure leaves the balance as None and the header stays in USD
        // mode for this turn.
        if remaining_credits.is_none() && self.connector.provider_name() == Some("charm") {
            remaining_credits = self.connector.hyper_credits_balance().await;
        }
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
                reported_cost_credits,
                remaining_credits,
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
    pub(super) async fn stream_summarize_for_compaction(
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
                            if other.starts_with("summarizer response is incomplete") {
                                return Err(CompactionErr::Incomplete(other.to_string()));
                            }
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
        // Normal summarization preserves the selected connector's output
        // settings. MapReduce owns its segment limits; the trigger budget is
        // not a generation limit for the complete handoff.
        let connector = self.compaction_connector().clone();
        if let Some(window) = self.summarization_request_overflow(system, prompt) {
            return Err(CompactionErr::ContextWindow {
                window_tokens: Some(window),
            });
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
            let reported_cost = stream.reported_cost().await;
            let reported_cost_credits = stream.reported_cost_credits().await;
            let remaining_credits = stream.remaining_credits().await;
            let _ = tx.send(super::events::HarnessEvent::Usage {
                usage,
                provider: connector.provider_name().unwrap_or("unknown").to_string(),
                model: connector.effective_model().unwrap_or("").to_string(),
                reported_cost,
                reported_cost_credits,
                remaining_credits,
            });
        }
        Self::validate_summary_completion(finish_reason.as_deref())
    }

    /// Only natural text completion can become checkpoint evidence. EOF or
    /// a provider limit is not proof that the summary finished successfully.
    pub(super) fn validate_summary_completion(reason: Option<&str>) -> Result<(), CompactionErr> {
        match reason {
            Some("stop" | "STOP" | "end_turn") => Ok(()),
            other => Err(CompactionErr::Incomplete(format!(
                "summarizer response is incomplete (finish reason: {})",
                other.unwrap_or("missing")
            ))),
        }
    }
}
