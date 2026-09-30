//! The daemon-side client of the decision protocol: [`IpcCheckup`], the
//! `Checkup` implementation the harness attaches INSTEAD of a resident
//! model.
//!
//! The seam is unchanged and still sync: a review serializes the same
//! `state`/`questions` the ONNX adapter built, sends `decision/decide`,
//! and interprets the model's raw result — the interpretation logic
//! (`choice`/`answer_confidence` → verdict) moved here verbatim from the
//! in-process adapter. What changed is only where the model runs.
//!
//! Fail-open, at every layer, per the approved protocol: daemon absent or
//! unactivatable, handshake rejected, request timeout, remote error of any
//! code — all collapse to [`TerminationVerdict::fallback`], and the loop
//! behaves exactly as without the model. The daemon's only failure mode
//! that reaches the user is nothing.

use serde_json::json;

use crate::daemon::client::DaemonClient;
use crate::daemon::ipc_error::Ipc;

use super::protocol::{self, DecideParams, DecideResult, Kind, code, method};

use crate::harness::checkup::{Checkup, TerminationDecision, TerminationVerdict};

/// The question id inside the schema — the same id the loop-termination
/// evaluation scores, carried over from the in-process adapter unchanged.
const QID: &str = "loop_terminated";

/// The `loop_terminated` question, verbatim from the eval: two options
/// because `temperature_by_options` calibrates `choice:2` explicitly.
fn loop_questions() -> serde_json::Map<String, serde_json::Value> {
    let mut questions = serde_json::Map::new();
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

/// A `Checkup` that decides through the shared daemon.
pub struct IpcCheckup {
    client: DaemonClient,
    kind: Kind,
}

/// The whole-review budget (M3): one review = activation (≤2s) + handshake
/// + decide. The first decide of a daemon also loads the model (a multi-GB
/// mmap + ORT session build takes seconds; the daemon's own retry window
/// prevents repeat attempts). 45s bounds the WORST case — a wedged daemon
/// fails the review well under a minute instead of composing per-read
/// timeouts into minutes.
const DECIDE_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

impl IpcCheckup {
    /// Attach the daemon-backed checkup for `kind`.
    ///
    /// Never fails: unlike the in-process adapter's `load` (which returned
    /// `None` when the model could not load), the model is NOT loaded
    /// here — the daemon loads it on the first `decide`, throttled by its
    /// own retry window. A broken setup surfaces as per-review fail-opens,
    /// not as a missing checkup at assembly time.
    pub fn new(kind: Kind) -> Self {
        Self {
            client: DaemonClient::new(protocol::NAME, protocol::PROTOCOL),
            kind,
        }
    }

    /// One decide round trip; any failure is `None` (fail open).
    ///
    /// Every outcome is logged under `target: "laya"` (mirrored into the
    /// dedicated `laya.log`): the review that is about to run, the verdict
    /// that came back, and the fail-open reason when it did not. The model
    /// is otherwise invisible — a silent fail-open is indistinguishable
    /// from a working model, which is exactly how improper stops go
    /// undiagnosed.
    fn decide(&self, state: &serde_json::Value) -> Option<serde_json::Value> {
        log::info!(target: "laya", "checkup: decide request (kind={})", self.kind.name());
        let result = self.client.call_with_budget(
            method::DECIDE,
            decide_params(&self.kind, state)?,
            DECIDE_BUDGET,
        );
        match result {
            Ok(result) => {
                let decided: DecideResult = ipc_from_value(result)?;
                log::info!(target: "laya", "checkup: decide ok");
                Some(decided.result)
            }
            Err(e) => {
                // BUSY gets one short-backoff retry (the approved policy);
                // everything else fails open immediately.
                if Self::remote_code(&e) == Some(code::BUSY) {
                    log::warn!(target: "laya", "checkup: daemon BUSY, one backoff retry");
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    match self.client.call_with_budget(
                        method::DECIDE,
                        decide_params(&self.kind, state)?,
                        DECIDE_BUDGET,
                    ) {
                        Ok(result) => {
                            let decided: DecideResult = ipc_from_value(result)?;
                            log::info!(target: "laya", "checkup: decide ok (after retry)");
                            Some(decided.result)
                        }
                        Err(e) => {
                            log::warn!(target: "laya", "checkup: failed-open after retry: {e}");
                            log::warn!("decision daemon failed-open: {e}");
                            None
                        }
                    }
                } else {
                    log::warn!(target: "laya", "checkup: failed-open: {e}");
                    log::warn!("decision daemon failed-open: {e}");
                    None
                }
            }
        }
    }

    fn remote_code(e: &Ipc) -> Option<i32> {
        DaemonClient::remote_error(e).map(|error| error.code)
    }
}

impl Checkup for IpcCheckup {
    fn review_termination(&self, final_text: &str) -> TerminationVerdict {
        let Some(result) = self.decide(&json!({ "message": final_text })) else {
            log::warn!(target: "laya", "checkup: verdict FAIL-OPEN (no daemon answer)");
            return TerminationVerdict::fallback();
        };
        // The interpretation the in-process adapter performed on
        // `Prediction` — now on the raw wire value. Missing answer fields
        // are a broken checkpoint/runtime pairing: fail open, not guess.
        let answers = result
            .get("answers")
            .and_then(serde_json::Value::as_object)
            .and_then(|answers| answers.get(QID))
            .cloned();
        let Some(answer) = answers else {
            log::warn!(target: "laya", "checkup: verdict FAIL-OPEN: no choice answer for {QID}");
            log::warn!("decision daemon failed-open: no choice answer for {QID}");
            return TerminationVerdict::fallback();
        };
        let choice = answer.get("choice").and_then(serde_json::Value::as_str);
        let Some(choice) = choice else {
            log::warn!(target: "laya", "checkup: verdict FAIL-OPEN: no choice answer for {QID}");
            log::warn!("decision daemon failed-open: no choice answer for {QID}");
            return TerminationVerdict::fallback();
        };
        let decision = if choice == "terminated" {
            TerminationDecision::Terminated
        } else {
            TerminationDecision::NotTerminated
        };
        let confidence = answer
            .get("answer_confidence")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        log::info!(
            target: "laya",
            "checkup: verdict {decision:?} (confidence {confidence:.2}) — text: {:?}",
            truncate_for_log(final_text)
        );
        TerminationVerdict {
            decision,
            confidence,
        }
    }
}

/// A bounded preview of the reviewed text for the log: the first line,
/// capped, so `laya.log` shows WHAT the model judged without dumping the
/// whole response.
fn truncate_for_log(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let mut preview: String = first.chars().take(120).collect();
    if first.chars().count() > 120 {
        preview.push('…');
    }
    preview
}

/// Typed params → wire value (`InvalidParams` cannot happen on serialize;
/// a failure is a bug, fail open).
fn decide_params(kind: &Kind, state: &serde_json::Value) -> Option<serde_json::Value> {
    crate::daemon::ipc::to_value(&DecideParams {
        kind: kind.clone(),
        state: state.clone(),
        questions: loop_questions(),
    })
    .ok()
}

fn ipc_from_value<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Option<T> {
    crate::daemon::ipc::from_value(value).ok()
}
