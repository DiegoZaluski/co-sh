//! Loop detection: interaction signatures, repetition windows and history
//! bookkeeping shared by the agent-loop iteration.

use std::collections::VecDeque;

use super::{Harness, limits::{LOOP_DETECTION_MAX_REPEATS, LOOP_DETECTION_WINDOW_SIZE,
    REPETITION_MAX_IDENTICAL, REPETITION_MIN_TEXT_LEN}};

impl Harness {
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

    /// Crush-style loop detection over the current tool interactions.
    ///
    /// Pushes the current interaction signature into `loop_window` and stops
    /// the loop (Toast + `Done`, returns `true`) when the same signature
    /// shows up more than [`LOOP_DETECTION_MAX_REPEATS`] times within a full
    /// [`LOOP_DETECTION_WINDOW_SIZE`] window.
    ///
    /// Called BOTH mid-iteration (the guardrail/hook denial arms, whose
    /// `continue` skips the end-of-iteration checks — without this a stuck
    /// model re-issuing an identical denied call burned the whole
    /// [`MAX_TOOL_RETRIES`] budget on one mistake, observed in the field)
    /// and once at the end of each iteration. A denied call pushes twice
    /// per iteration (deny arm + end-of-iteration), so repeated denials
    /// fill the window in half the iterations — detection still lands
    /// safely inside the retry budget.
    pub(super) fn check_loop_detection(
        &mut self,
        tool_interactions: &[String],
        loop_window: &mut VecDeque<String>,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        if tool_interactions.is_empty() {
            return false;
        }
        let sig = Self::tool_interaction_signature(tool_interactions);
        loop_window.push_back(sig.clone());
        if loop_window.len() > LOOP_DETECTION_WINDOW_SIZE {
            loop_window.pop_front();
        }
        let repeats = loop_window.iter().filter(|s| **s == sig).count();
        if loop_window.len() == LOOP_DETECTION_WINDOW_SIZE && repeats > LOOP_DETECTION_MAX_REPEATS {
            // Heuristic guard: the stop stands, but the decision model
            // audits it so an improper stop is never silent.
            let verdict = self.review_guard_stop("loop-detection");
            self.context_manager.close_loop();
            let _ = tx.send(super::events::HarnessEvent::Toast {
                message: "Agent stopped: repeated identical tool calls \
                          detected (the model appears stuck in a loop)."
                    .to_string(),
                variant: super::events::ToastVariant::Info,
            });
            let _ = tx.send(self.lsp_snapshot_event());
            let _ = tx.send(super::events::HarnessEvent::Done {
                context: self.context_manager.save_state(),
                checkup_verdict: verdict,
            });
            return true;
        }
        false
    }

    /// Textual-output repetition detection: the blind spot of the crush-style
    /// window above. That detector only sees TOOL interactions, so a model
    /// stuck repeating the same THOUGHT with no tool call (a reasoning-only
    /// `stop` stream that the natural-stop gate keeps continuing, or a final
    /// answer the checkup keeps vetoing) loops until `MAX_ITERATIONS`,
    /// re-sending the ever-growing context on every iteration.
    ///
    /// Called once per iteration, right after the record site, with the
    /// CURRENT iteration's textual output: the visible answer when one was
    /// produced, otherwise the attempt's reasoning. Whitespace is collapsed
    /// before comparison so tokenization differences between attempts cannot
    /// mask an identical thought; texts shorter than
    /// [`REPETITION_MIN_TEXT_LEN`] are skipped (too little signal, too cheap
    /// to guard). After [`REPETITION_MAX_IDENTICAL`] consecutive identical
    /// texts the loop stops (Toast + `Done`, returns `true`) with the same
    /// guard-stop audit the tool detector performs.
    ///
    /// The reset happens on ANY differing text and on tool-calling turns
    /// (the caller clears the streak when `had_tools` is set) — a turn that
    /// dispatched tools is progress, and a single different text proves the
    /// model is still moving, so only an unbroken streak of verbatim repeats
    /// counts.
    pub(super) fn check_repetition_detection(
        &mut self,
        text: &str,
        identical_text_streak: &mut usize,
        last_output_signature: &mut Option<u64>,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.len() < REPETITION_MIN_TEXT_LEN {
            *identical_text_streak = 0;
            *last_output_signature = None;
            return false;
        }
        let sig = Self::text_signature(&normalized);
        if *last_output_signature == Some(sig) {
            *identical_text_streak += 1;
        } else {
            *identical_text_streak = 1;
            *last_output_signature = Some(sig);
        }
        if *identical_text_streak > REPETITION_MAX_IDENTICAL {
            // Heuristic guard: the stop stands, but the decision model
            // audits it so an improper stop is never silent.
            let verdict = self.review_guard_stop("repetition-detection");
            self.context_manager.close_loop();
            let _ = tx.send(super::events::HarnessEvent::Toast {
                message: concat!(
                    "Agent stopped: the model repeated the same output ",
                    "verbatim across consecutive turns (it appears stuck ",
                    "in a repetition loop)."
                )
                .to_string(),
                variant: super::events::ToastVariant::Info,
            });
            let _ = tx.send(self.lsp_snapshot_event());
            let _ = tx.send(super::events::HarnessEvent::Done {
                context: self.context_manager.save_state(),
                checkup_verdict: verdict,
            });
            return true;
        }
        false
    }

    /// FNV-1a 64-bit over one normalized text — the same deterministic hash
    /// used for tool-interaction signatures above, so two byte-identical
    /// texts (post whitespace-collapsing) always compare equal.
    fn text_signature(text: &str) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Add a tool call + its result to the context manager (structural
    /// layer — structural). The native `tool_call → tool` chain is
    /// preserved 1:1: the call item renders as an `assistant` message with
    /// `tool_calls`, the result as a `tool` message with the matching id.
    pub(super) fn push_tool_history(
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
        // Harness tools are routed MID-STREAM, before the real tool calls of
        // the same response are dispatched — they must never consume the
        // stashed thinking blocks, which belong to the turn's real tool_use
        // (Anthropic validates them on the follow-up request).
        let thinking_blocks = if self.harness_tools.iter().any(|t| t.name == name) {
            Vec::new()
        } else {
            std::mem::take(&mut self.pending_thinking_blocks)
        };
        self.context_manager.add_tool_call_with_thinking(
            &tool_id,
            name,
            &args_str,
            signature,
            thinking_blocks,
        );
        // Multimodal channel: the Tier-1 dispatch (`dispatch_next`) drained
        // this dispatch's staged PNG payload into `self.pending_images`
        // right after the call returned. Consume it here so the images ride
        // on THIS result's `ChatMessage`; a payload drained by any other
        // path (hook halt, stop, MCP fallback) is dropped there instead.
        self.context_manager.add_tool_result_with_images(
            &tool_id,
            result,
            result_is_useless(name, result),
            std::mem::take(&mut self.pending_images),
        );
    }

    /// Monotonic id generator for synthetic tool_call ids (inline JSON calls
    /// without an extractor id).
    pub(super) fn context_manager_pending_id(&mut self) -> u64 {
        // Reuse the context manager's monotonic counter through a tiny wrapper:
        // every tool without an id gets a fresh, globally-unique number.
        self.tool_call_synthetic += 1;
        self.tool_call_synthetic
    }
}

/// Whether a tool RESULT should be flagged `useless` in the context manager.
///
/// Reads the tool's own declared output contract: `find_grep` marks zero-match
/// results with `"useless": true` in its JSON, and the model sees the same
/// field. Gate by tool name so no other tool's JSON is ever interpreted.
/// A successful `mask_tool_result` call is always useless: the model has
/// reacted to the masked result, so its chain is dead weight and the
/// useless-chain sweep removes it on the next turn at no extra cost.
/// `pub(crate)` for the bridge unit test in `harness::test`.
pub(crate) fn result_is_useless(name: &str, result: &str) -> bool {
    if matches!(name, "mask_tool_result") {
        return true;
    }
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
