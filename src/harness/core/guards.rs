//! Terminal-error surfacing and the review-gate stop decision.

use super::Harness;

impl Harness {
    /// Emit the terminal `Error` event of an agent loop. The failure is first
    /// recorded as a DISPLAY-ONLY context item (`ContextManager::add_error`)
    /// and its snapshot travels with the event: the transcript restores the
    /// styled error line after a restart, while the model never sees a
    /// provider failure as assistant output.
    ///
    /// `checkup_verdict`: the decision model's audit of the stop, from
    /// [`Self::review_guard_stop`] — `None` for explicit stops (hook halt)
    /// and infrastructure failures (provider error, context window).
    pub(super) fn emit_terminal_error(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        msg: String,
        checkup_verdict: Option<String>,
    ) {
        self.context_manager.add_error(&format!("Error: {msg}"));
        let _ = tx.send(super::events::HarnessEvent::Error {
            message: msg,
            context: Some(self.context_manager.save_state()),
            checkup_verdict,
        });
    }

    /// Audit a heuristic guard stop (MAX_TOOL_RETRIES, MAX_ITERATIONS, loop
    /// detection) with the decision model. The stop itself always stands —
    /// the guards are cost/safety nets — but the verdict is recorded on the
    /// terminal event and toasted when the model disagrees, so an improper
    /// stop is never silent. Returns `None` when no checkup is attached.
    ///
    /// The review runs over the most recent assistant text in the context
    /// manager (the last thing the model said before the guard fired).
    pub(super) fn review_guard_stop(&self, guard: &str) -> Option<String> {
        let checkup = self.checkup.as_ref()?;
        let last_text = self
            .context_manager
            .last_assistant_text()
            .unwrap_or_default();
        let verdict = checkup.review_termination(&last_text);
        use super::checkup::TerminationDecision;
        let label = match verdict.decision {
            TerminationDecision::Terminated => "terminated",
            TerminationDecision::NotTerminated => "not-terminated",
        };
        let summary = format!(
            "termination checkup @ {guard}: {label} (confidence {:.2})",
            verdict.confidence
        );
        // A guard firing while the model believes the task is NOT done is a
        // suspicious stop — surface it instead of only recording it.
        if verdict.decision == TerminationDecision::NotTerminated {
            log::warn!(target: "laya", "{summary}");
            log::warn!("{summary}");
        } else {
            log::debug!(target: "laya", "{summary}");
            log::debug!("{summary}");
        }
        Some(summary)
    }
}
