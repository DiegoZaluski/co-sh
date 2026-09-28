//! Checkup — the decision model's audit seam.
//!
//! A `Checkup` audits decisions the harness makes. The current point of use
//! is agent-loop termination: when the loop is about to end on an ambiguous
//! signal (a text-only natural completion), the model reads the final text
//! and answers whether the task is actually done. Explicit stops (the
//! `stop_agent_loop` tool, Esc, hook halts) and infrastructure failures
//! never pass through here — there is no ambiguity to audit.
//!
//! The trait compiles **always**, so harness tests stub it without the ONNX
//! runtime; the concrete adapter lives behind `#[cfg(feature = "onnx")]`.
//!
//! **Fail-open, per point of use**: the model is an improver, never a single
//! point of failure. On load or inference failure the verdict is
//! [`TerminationDecision::Terminated`] — at the natural-completion gate that
//! preserves the current behaviour (the loop ends); at the heuristic guards
//! the stop stands regardless and the verdict is only recorded.

/// The model's responsibility: auditing harness decisions.
///
/// `Send + Sync`: the harness holds a shared `Arc<dyn Checkup>` and may
/// review from any iteration of the agent loop.
pub trait Checkup: Send + Sync {
    /// Current point of use: review the decision to end the agent loop.
    ///
    /// `final_text` is the assistant's text-only response that is about to
    /// close the loop. The verdict says whether that text actually
    /// terminates the task.
    fn review_termination(&self, final_text: &str) -> TerminationVerdict;
}

/// Whether the reviewed text terminates the agent loop.
///
/// `NotTerminated` (not "running"): the model judges whether the text
/// closes the loop — it does not observe loop state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationDecision {
    /// The task is complete: a summary of what was done, nothing left to do.
    Terminated,
    /// The loop is still going: the agent announces it will read or write
    /// files, run commands, or start the next step.
    NotTerminated,
}

/// The verdict of a termination review.
///
/// Decision-specific by design: the model audits one question, so the
/// verdict names that question. A future audited decision gets its own
/// verdict type instead of forcing this one to be generic.
#[derive(Debug, Clone, Copy)]
pub struct TerminationVerdict {
    /// The audited decision.
    pub decision: TerminationDecision,
    /// The calibrated `answer_confidence` of the model's answer
    /// (0.0 when the verdict is a fail-open fallback — no information).
    pub confidence: f64,
}

impl TerminationVerdict {
    /// The fail-open verdict: on any load/inference failure the loop
    /// behaves exactly as if the model were not there.
    #[cfg(feature = "onnx")]
    pub(crate) fn fallback() -> Self {
        Self {
            decision: TerminationDecision::Terminated,
            confidence: 0.0,
        }
    }
}

/// The ONNX adapter: builds the `loop_terminated` question (the
/// `type`/`instructions`/`criteria` shape from
/// `crates/cosh-onnx/tests/eval/loop_termination.rs`) and drives the
/// resident [`cosh_onnx::DecisionModel`].
#[cfg(feature = "onnx")]
pub use onnx_adapter::OnnxCheckup;

#[cfg(feature = "onnx")]
mod onnx_adapter {
    use std::sync::Arc;

    use serde_json::{Map, Value, json};

    use cosh_onnx::{DecisionModel, LoadOptions, ModelKind, Prediction};

    use super::{Checkup, TerminationDecision, TerminationVerdict};

    /// The question id inside the schema — the same id the loop-termination
    /// evaluation scores, so benchmark numbers map 1:1 onto this consumer.
    const QID: &str = "loop_terminated";

    /// The `loop_terminated` question, verbatim from the eval: two options
    /// because `temperature_by_options` calibrates `choice:2` explicitly.
    fn loop_questions() -> Map<String, Value> {
        let mut questions = Map::new();
        questions.insert(
            QID.to_string(),
            json!({
                "type": "choice",
                "instructions": "An agent produced the text in `message` while working on a task. Did the agent's loop finish with this text, or is the agent about to keep working?",
                "criteria": {
                    "terminated": "the task is complete and the loop ended: a summary of what was done, nothing left to do",
                    "running": "the loop is still going: the agent will read or write files, run commands, or start the next step",
                }
            }),
        );
        questions
    }

    /// A [`Checkup`] over a resident decision model.
    ///
    /// Holds the shared `Arc<dyn DecisionModel>` — the same resident the
    /// rest of the app uses; `decide` is `&self`, so a review only borrows.
    pub struct OnnxCheckup {
        model: Arc<dyn DecisionModel>,
    }

    impl OnnxCheckup {
        /// Wrap an already-loaded resident model.
        pub fn new(model: Arc<dyn DecisionModel>) -> Self {
            Self { model }
        }

        /// Load the resident model for `kind` with default options.
        /// Fail-open: returns `None` (no checkup) on load failure — the
        /// harness runs unaudited, exactly as it does without the feature.
        ///
        /// A `Custom` kind whose repo is a LOCAL DIRECTORY resolves the ONNX
        /// graph against that directory (`{repo}/laya.onnx`). Hub-downloaded
        /// snapshots need no special handling: the loader falls back to the
        /// snapshot dir when the graph is not found relative to the process
        /// CWD (the same contract the eval tests follow when they pass
        /// `onnx_path` explicitly).
        pub fn load(kind: ModelKind) -> Option<Self> {
            let mut options = LoadOptions::default();
            if let ModelKind::Custom { repo, .. } = &kind
                && std::path::Path::new(repo).is_dir()
            {
                options.onnx_path = Some(format!("{repo}/laya.onnx"));
            }
            match cosh_onnx::load(kind, &options) {
                Ok(model) => Some(Self::new(model)),
                Err(e) => {
                    log::warn!("termination checkup disabled: model load failed: {e}");
                    None
                }
            }
        }
    }

    impl Checkup for OnnxCheckup {
        fn review_termination(&self, final_text: &str) -> TerminationVerdict {
            let state = json!({"message": final_text});
            let questions = loop_questions();
            let result = match self.model.decide(&state, &questions, None, None, None) {
                Ok(result) => result,
                Err(e) => {
                    log::warn!("termination checkup failed-open: {e}");
                    return TerminationVerdict::fallback();
                }
            };
            let prediction = Prediction::from(result);
            // Missing answer fields are a broken checkpoint/runtime pairing,
            // not a decision — fail open rather than guess.
            let Some(choice) = prediction.choice(QID) else {
                log::warn!("termination checkup failed-open: no choice answer for {QID}");
                return TerminationVerdict::fallback();
            };
            let decision = if choice == "terminated" {
                TerminationDecision::Terminated
            } else {
                TerminationDecision::NotTerminated
            };
            TerminationVerdict {
                decision,
                confidence: prediction.answer_confidence(QID).unwrap_or(0.0),
            }
        }
    }
}
