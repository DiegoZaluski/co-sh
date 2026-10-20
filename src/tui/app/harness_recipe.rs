//! THE harness assembly recipe.
//!
//! One place builds every loop-bearing [`Harness`] in the TUI: the main
//! agent loop and the `/agents` cosh pane both assemble through
//! [`assemble`] — the same connector resolution, the same prompt-cache
//! headers, the same MCP boot, the same RAG wiring, the same
//! effective-context budget and the same skills — with the caller's
//! differences declared as explicit parameters (mode, instructions,
//! disabled tools, cache key, checkup, queued input), never as a
//! re-implemented subset. The assembly used to live twice (here for the
//! main loop, a hand-cut subset in `agents_dispatch::run_manager_turn`);
//! the subset had already drifted (`effective_context`, MCP, cache
//! headers, tool-call mode never reached the pane).
//!
//! This is the assembly ONLY — the loop execution and every App side
//! effect (session bookkeeping, transcript, spinner, queue resets) stay
//! with the caller, and the caller owns how an assembly failure surfaces
//! (the main loop toasts an `Error` event; the pane closes its turn with
//! an error report). One-shot non-loop users of `Harness::chat` (the
//! prompt corrector) and the nested internal sub-agent are deliberately
//! NOT assembled here: they are different use cases with their own
//! contracts, not subsets of this recipe.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use cosh::harness::Harness;
use cosh::harness::HarnessEvent;
use cosh::harness::Mode;
use cosh::harness::ContextManagerState;
use cosh::harness::queued_input::QueuedInput;
use cosh_sdk::connector::Connector;
use cosh_sdk::connector::ToolCallMode;

/// Everything one agent-loop assembly needs, decided by the caller. The
/// main loop fills it from the user's live config; the cosh pane fills it
/// from the same config plus its own session identity.
pub(crate) struct HarnessAssembly {
    /// Primary provider (the session's own route, or the auto-rotation
    /// winner when `model` is `auto`).
    pub provider: String,
    /// `Some("auto")` resolves through `fallbacks` (first connectable
    /// provider wins, the rest ride the harness's fallback chain);
    /// `None`/concrete keeps the primary route.
    pub model: Option<String>,
    /// User-chosen reasoning effort, mapped onto the closest level each
    /// model accepts (never sent where it would 400 or be ignored).
    pub reasoning: Option<String>,
    /// The user's tool-call delivery mode (native / inline) — respected
    /// for every loop assembled here.
    pub tool_call_mode: ToolCallMode,
    /// Base URL override for the PRIMARY provider only (the local-server
    /// map below covers the fallback providers).
    pub base_url: Option<String>,
    pub local_base_urls: std::collections::HashMap<String, String>,
    /// The full fallback list (provider, model) — the recipe splits it
    /// into primary + remaining for `auto`, or uses it as the harness's
    /// fallback chain for a concrete model.
    pub fallbacks: Vec<(String, String)>,
    pub summarization_models: Vec<(String, String)>,
    pub cwd: String,
    pub mode: Mode,
    pub disabled_tools: HashSet<String>,
    /// Instruction override (`None` = the mode-based default prompt). The
    /// main loop and the pane pass their role's text here.
    pub instructions: Option<&'static str>,
    // Session-level prompt-cache settings: the user's TTL/retention
    // choices (Settings screen) and the CALLER'S session id as the
    // cache-affinity key — a routing hint, never user data. The pane
    // passes ITS OWN session id: a shared key would make the main loop
    // and the pane compete for the same cache-warm backend.
    pub anthropic_ttl_1h: bool,
    pub openai_cache_retention: Option<String>,
    pub cache_key: Option<String>,
    /// Registered MCP servers (Settings screen → setup.json): booted
    /// before the header snapshot with per-server failure toasts.
    pub mcp_config: cosh::mcp::McpConfig,
    pub skills_config: cosh::setup::SkillsConfig,
    /// The effective-context budget toggle (Settings screen) — the SAME
    /// knob for every loop; a loop assembled without it silently runs on
    /// the raw window even when the user turned the budget on.
    pub effective_context: bool,
    /// The decision-model audit config, `Some` only for loops that run
    /// the audit (the main loop). `None` (the pane) assembles without the
    /// audit — a declared deviation, not an omission.
    #[cfg(feature = "onnx")]
    pub checkup: Option<cosh::setup::DecisionConfig>,
    /// RAG recall wiring (tool-doc suffix + DB registry), `Some` only when
    /// the caller has recall databases configured.
    #[cfg(feature = "embed")]
    pub recall: Option<(String, Vec<cosh::harness::tools::RecallDb>)>,
    /// The authoritative model-facing history (the session JSONL's
    /// context records). `None` starts the loop with an empty context.
    pub ctx_state: Option<ContextManagerState>,
    /// Tag this loop's `agents_call` requests as the MANAGER requester:
    /// a background pane report drains ONLY there, never into the main
    /// chat loop. The main loop stays MAIN (`false`).
    pub as_manager: bool,
    /// Bind this loop to the `/agents` pane it runs in (its registry id):
    /// strips the `agents_call` tool and the panes context block — the
    /// mechanical anti-replication invariant (a pane never coordinates).
    /// `None` for the main loop: it owns no pane and keeps the tool.
    pub own_pane_id: Option<String>,
}

/// Build the connector this loop's requests ride: the user's tool-call
/// mode, the reasoning effort resolved per model, the local-server URL
/// and the session-level prompt-cache settings on EVERY connector the
/// loop may build (primary and fallbacks alike — each field is only read
/// by the caller that implements it, so applying unconditionally is safe
/// across fallback families).
///
/// Returns the primary connector plus the REMAINING fallback chain (the
/// entries after the one that won, for `auto`).
fn build_connector(
    a: &HarnessAssembly,
) -> Result<(Connector, Vec<(String, String)>), String> {
    let apply_cache = |c: Connector| {
        let c = c.with_prompt_cache_ttl_1h(a.anthropic_ttl_1h);
        let c = match &a.cache_key {
            Some(id) => c.with_prompt_cache_key(id),
            None => c,
        };
        let c = match &a.cache_key {
            Some(id) => c.with_session_id(super::session_affinity_id(id)),
            None => c,
        };
        match &a.openai_cache_retention {
            Some(retention) => c.with_prompt_cache_retention(retention),
            None => c,
        }
    };
    if a.model.as_deref() == Some("auto") {
        let mut last_err = String::new();
        for (i, (fb_provider, fb_model)) in a.fallbacks.iter().enumerate() {
            match Connector::new(fb_provider) {
                Ok(c) => {
                    let mut c =
                        c.with_model(fb_model).with_tool_call_mode(a.tool_call_mode);
                    // Honor a configured local server URL for this
                    // fallback provider, if any.
                    if let Some(url) = a.local_base_urls.get(fb_provider) {
                        c = c.with_base_url(url.clone());
                    }
                    // The user chose a reasoning effort for auto mode —
                    // apply it, mapped onto the closest level THIS model
                    // accepts (a model without the knob drops it). The
                    // harness re-resolves per model on every fallback
                    // switch.
                    if let Some(r) = a.reasoning.as_deref()
                        && let Some(resolved) =
                            cosh_sdk::connector::resolve_reasoning_effort(
                                fb_model,
                                r,
                                Some("cosh/cache"),
                            )
                    {
                        c = c.with_reasoning_effort(resolved);
                    }
                    let remaining = a.fallbacks[i + 1..].to_vec();
                    return Ok((apply_cache(c), remaining));
                }
                Err(e) => {
                    last_err = format!("connector for {fb_provider}: {e}");
                }
            }
        }
        return Err(format!("auto: no fallback available ({last_err})"));
    }
    let c = Connector::new(&a.provider).map_err(|e| format!("connector: {e}"))?;
    let with_model = if let Some(ref m) = a.model {
        c.with_model(m)
    } else {
        c
    };
    // Honor a configured local server URL.
    let with_model = if let Some(ref url) = a.base_url {
        with_model.with_base_url(url.clone())
    } else {
        with_model
    };
    // Apply the user's tool-call mode (native / inline) — the two
    // delivery paths are mutually exclusive at the request level.
    let with_model = with_model.with_tool_call_mode(a.tool_call_mode);
    let with_model = if let Some(ref r) = a.reasoning {
        // Map the chosen level onto the closest one the model actually
        // accepts (e.g. "medium" on a low/high-only model) — never send
        // a knob that would 400 or be silently ignored.
        let effective = a
            .model
            .as_deref()
            .and_then(|m| {
                cosh_sdk::connector::resolve_reasoning_effort(m, r, Some("cosh/cache"))
            })
            .unwrap_or_else(|| r.clone());
        with_model.with_reasoning_effort(effective)
    } else {
        with_model
    };
    Ok((apply_cache(with_model), Vec::new()))
}

/// Assemble the loop's harness through the ONE recipe: connector →
/// harness → (audit) → MCP boot → context restore → RAG → header. Every
/// failure here is RETURNED (the caller decides how its surface reports
/// it); non-fatal degradation (MCP server failures, audit fail-open)
/// toasts through `event_tx` and assembles anyway.
pub(crate) async fn assemble(
    // `mut` serves the embed-gated `recall: None` → disabled-tools insert
    // below; other feature sets never mutate it.
    #[cfg_attr(not(feature = "embed"), allow(unused_mut))] mut a: HarnessAssembly,
    event_tx: &mpsc::UnboundedSender<HarnessEvent>,
) -> Result<Harness, String> {
    // A loop without recall databases hides the `recall_search` tool
    // entirely (the main loop's `maybe_disable_recall_tool` contract): a
    // caller that passes `recall: None` gets the same behavior, not a
    // tool whose description promises databases that do not exist.
    #[cfg(feature = "embed")]
    if a.recall.is_none() {
        a.disabled_tools.insert("recall_search".into());
    }
    let (connector, remaining) = build_connector(&a)?;

    let mut harness = Harness::new(connector, &a.cwd, a.disabled_tools)
        .with_mode(a.mode)
        .with_fallbacks(remaining)
        .with_summarization_models(a.summarization_models)
        .with_local_base_urls(a.local_base_urls)
        .with_skills(a.skills_config.to_skills())
        .with_mcp_config(a.mcp_config)
        .with_effective_context(a.effective_context);
    if let Some(instructions) = a.instructions {
        harness = harness.with_instructions(instructions);
    }
    if a.as_manager {
        harness = harness.as_manager();
    }
    if let Some(ref pane_id) = a.own_pane_id {
        harness = harness.with_own_pane_id(Some(pane_id.clone()));
    }

    // The decision-model audit seam (`harness::checkup`), attached over
    // the SHARED decision daemon: the model (graph + ORT session) is
    // resident in one daemon process every cosh instance talks to,
    // instead of one per-process resident per open app. The daemon loads
    // the model on the first decide (with its own retry window), so
    // assembly never blocks on a load; any daemon failure is a
    // per-review fail-open, and the loop behaves exactly as without the
    // model. The harness never chooses which model to load; a future
    // consumer of the engine passes its own kind the same way.
    // The audit's add_assistant explanation, produced only on the onnx
    // path below (unused on other paths).
    #[cfg(feature = "onnx")]
    let mut weights_explanation: Option<String> = None;
    #[cfg(feature = "onnx")]
    if let Some(checkup_config) = a.checkup
        && checkup_config.termination.enabled
    {
        use cosh::daemon::decision::{IpcCheckup, Kind};
        use cosh::harness::events::ToastVariant;

        // The audit's kind resolves through the WEIGHTS MIRROR: the
        // installer fills the shared hf-hub cache (cache-first, lazy —
        // only when this turn's checkup is enabled and the checkpoint is
        // not cached yet), and the daemon's loader probes that snapshot.
        // The daemon kind is `Custom` over the mirror checkpoint, NOT the
        // configured kind — the named kinds name the MIRROR's subfolders
        // now.
        let attach_kind = |kind: Kind, harness: Harness| -> Harness {
            let checkup = IpcCheckup::new(kind);
            harness
                .with_checkup(std::sync::Arc::new(checkup))
                .with_checkup_min_confidence(checkup_config.termination.min_confidence)
        };
        let invalid_setup = || {
            // Invalid config is a broken SETUP, not a turn failure: toast
            // and assemble WITHOUT the audit (fail-open) — never abort
            // the turn.
            let _ = event_tx.send(HarnessEvent::Toast {
                message: "Termination checkup enabled but the model config \
                          is invalid; the audit is disabled."
                    .to_string(),
                variant: ToastVariant::Warning,
            });
        };
        match crate::weights::resolve_target(&checkup_config.model, &checkup_config.hub) {
            Some(target) => {
                match crate::weights::ensure_installed(target.clone(), event_tx.clone()).await {
                    // Installed (or already cached): the daemon loads the
                    // mirror snapshot the installer filled.
                    Ok(explanation) => {
                        weights_explanation = explanation;
                        harness = attach_kind(target.to_kind(), harness);
                    }
                    // Download failed: fail-open — toast (the Failed
                    // event already toasted) and assemble WITHOUT the
                    // audit.
                    Err(_) => {}
                }
            }
            // No mirror target (a local checkpoint, or a config the
            // resolver can't turn into one): keep the plain serde
            // conversion so a local Custom kind still reaches the daemon.
            None => {
                let wire_kind: Option<Kind> = serde_json::to_value(&checkup_config.model)
                    .ok()
                    .and_then(|v| serde_json::from_value(v).ok());
                match wire_kind {
                    Some(kind) => harness = attach_kind(kind, harness),
                    None => invalid_setup(),
                }
            }
        }
    }

    // Connect MCP servers before the header snapshot: the first
    // extractor/native-tools view must see them.
    if let Err(err) = harness.connect_mcp().await {
        use cosh::harness::events::ToastVariant;
        let _ = event_tx.send(HarnessEvent::Toast {
            message: format!("MCP config invalid: {err}"),
            variant: ToastVariant::Warning,
        });
    }
    // Per-server connection failures are isolated by the manager —
    // surface them: a failed server used to show only as a silent footer
    // count with no explanation. Detail (including which credential
    // failed and why) lives in the snapshot's `last_error`; this toast is
    // the pointer, one per failing server.
    let failed: Vec<cosh::mcp::ServerSnapshot> = harness
        .mcp_snapshots()
        .into_iter()
        .filter(|s| matches!(s.status, cosh::mcp::ServerStatus::Failed))
        .collect();
    if !failed.is_empty() {
        use cosh::harness::events::ToastVariant;
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

    // Restore the authoritative context from the session file's context
    // records. No context — no history; the display transcript is never
    // fed to the model.
    if let Some(ref state) = a.ctx_state {
        harness.context_manager.restore_state(state);
    }
    // The weights installer's explanation: the TUI already rendered it as
    // a real assistant message — record the SAME text here so the model's
    // history agrees with the transcript. NOT closable: it is context,
    // not a turn that may close the loop.
    #[cfg(feature = "onnx")]
    if let Some(explanation) = weights_explanation.as_deref() {
        harness.context_manager.add_assistant(explanation, false);
    }
    #[cfg(feature = "embed")]
    if let Some((suffix, dbs)) = a.recall {
        harness.set_recall_context(suffix);
        harness.set_recall_dbs(dbs);
    }
    harness.format_header_context();
    Ok(harness)
}

/// The shared process-wide runtime accessor: every loop-bearing assembly
/// runs on THIS runtime (see [`super::agent_loop::AGENT_RUNTIME`] for
/// why — LSP and MCP transports must outlive individual turns).
pub(crate) fn agent_runtime() -> &'static tokio::runtime::Runtime {
    super::agent_loop::AGENT_RUNTIME
        .get_or_init(|| tokio::runtime::Runtime::new().expect("shared agent runtime"))
}

/// Run the assembled loop with the caller's queued-input choice: the main
/// loop shares its queue with the TUI (`Some`), the pane runs one-input-
/// per-turn (`None` — nobody pumps a pane queue, so a shared queue there
/// would only strand messages).
pub(crate) async fn run_assembled_loop(
    harness: &mut Harness,
    input: &str,
    event_tx: mpsc::UnboundedSender<HarnessEvent>,
    answer_rx: mpsc::UnboundedReceiver<
        Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
    >,
    perm_rx: mpsc::UnboundedReceiver<cosh::harness::PermissionAction>,
    stop_signal: Arc<AtomicBool>,
    queued_input: Option<QueuedInput>,
) {
    match queued_input {
        Some(queue) => {
            harness
                .run_agent_loop_with_queued_input(
                    input, event_tx, answer_rx, perm_rx, stop_signal, queue,
                )
                .await;
        }
        None => {
            harness
                .run_agent_loop(input, event_tx, answer_rx, perm_rx, stop_signal)
                .await;
        }
    }
}
