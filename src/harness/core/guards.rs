//! Terminal-error surfacing and the review-gate stop decision.

use super::Harness;

/// WHY a report-owing turn is being resumed — the `&str` this used to be
/// invited a typo to silently select the wrong steering text, and the value
/// doubles as log wording, so a type is the only place the two can be kept
/// consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCause {
    /// The natural-stop gate's nudge budget was exhausted and the model
    /// produced no output at all.
    EmptyStop,
    /// The textual-output repetition detector fired.
    Repetition,
}

impl ResumeCause {
    /// The short name for the log line / guard audit.
    const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyStop => "empty response",
            Self::Repetition => "repetition-detection",
        }
    }
}

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

    /// Whether this loop owes a REPORT — the one class of turn that must
    /// never end on a stop nobody asked for.
    ///
    /// Two loops owe one:
    /// - the internal sub-agent (`as_nested`): its ONLY deliverable is the
    ///   final report text (`stop_agent_loop` is blocked for it, see
    ///   [`super::limits::SUBAGENT_BLOCKED_TOOLS`]);
    /// - the `/agents` cosh pane (`with_own_pane_id`): a requester is
    ///   BLOCKED on its report, and `stop_agent_loop` is blocked there too
    ///   (the TUI's `MANAGER_BLOCKED_TOOLS`).
    ///
    /// The main chat loop owes no report: its stop is final, the user reads
    /// the answer and asks again. It therefore never resumes — the reason
    /// this helper is gated on the loop's ROLE rather than on the stop shape
    /// alone.
    pub(crate) fn owes_report(&self) -> bool {
        self.nested || self.panes_own_id.is_some()
    }

    /// Continue a REPORT-OWING turn whose loop stopped without anybody
    /// asking it to — returning the steering message for the next iteration,
    /// or `None` when the stop must stand.
    ///
    /// This is the missing seam behind the field defect: a `/agents` pane
    /// whose loop ended right after a reasoning (or on an exhausted
    /// natural-stop budget) closed the turn `incomplete`, handing its
    /// requester "the pane produced no report" for work that was one
    /// attempt away from delivering one. The loop COULD have continued — a
    /// continuation is just the history plus one steering turn, exactly what
    /// every other `continue` in the loop does — nothing but the absence of
    /// this helper made the stop final.
    ///
    /// The stop STANDS when any of these holds, and each is exact rather
    /// than heuristic:
    ///
    /// 1. The loop owes no report ([`Self::owes_report`]) — a chat turn's
    ///    stop is the user's to read.
    /// 2. The USER stopped it: `self.stop` / the shared `stop_signal`, the
    ///    SAME flags `check_stop!` reads. For a pane `self.stop` cannot come
    ///    from the model (`stop_agent_loop` is blocked), so this is the
    ///    user's stop and nothing else — the single legitimate early end.
    /// 3. The resume budget is spent ([`super::limits::PANE_RESUME_BUDGET`]).
    ///    Kept per LOOP, because the inner nudges renew per ENTRY and would
    ///    otherwise make the resume unbounded.
    ///
    /// The reasoning of the cut attempt rides the steering message when there
    /// is one, the same one-shot promotion the natural-stop gate performs: the
    /// model already paid for that thinking, and re-deriving it wastes a whole
    /// attempt of a turn that is already behind.
    pub(crate) fn resume_pane_turn(
        &mut self,
        stop_signal: &std::sync::atomic::AtomicBool,
        cause: ResumeCause,
    ) -> Option<String> {
        use std::sync::atomic::Ordering;
        let reason = cause.as_str();
        if !self.owes_report() {
            return None;
        }
        if self.stop || stop_signal.load(Ordering::Relaxed) {
            log::debug!(
                target: "laya",
                "pane resume declined: the USER stopped this turn ({reason}) — the stop stands"
            );
            return None;
        }
        if self.pane_resumes_left == 0 {
            return None;
        }
        self.pane_resumes_left -= 1;

        // The note must describe what ACTUALLY happened, or it steers the
        // model wrong: an empty-stop resume follows a stream that produced
        // nothing, while a repetition resume follows one that produced text
        // and then repeated itself verbatim. Naming the wrong cause invites
        // the model to "continue" work it never stopped doing.
        let mut steering = String::from(match cause {
            ResumeCause::Repetition => {
                "[Harness note: your last few attempts repeated the same text \
                 verbatim instead of advancing, so the harness is giving you one \
                 more turn rather than ending this one. Your context is intact. Do \
                 NOT restate the same message: take the next concrete step and end \
                 with your final report.]"
            }
            ResumeCause::EmptyStop => {
                "[Harness note: your previous attempt ended with no output at all \
                 before you delivered the report this turn exists to produce. Your \
                 context is intact — nothing was lost. Continue from exactly where \
                 you were cut and finish the task, ending with your final report.]"
            }
        });
        let reasoning = std::mem::take(&mut self.attempt_reasoning);
        if !reasoning.trim().is_empty() {
            #[cfg(test)]
            self.promoted_reasoning_log.push(reasoning.clone());
            steering.push_str(&format!(
                "\n\n<your-previous-reasoning>\n{reasoning}\n</your-previous-reasoning>"
            ));
        }
        log::warn!(
            target: "laya",
            "pane resume ({reason}): this turn owes a report and NOBODY asked it to \
             stop (no user stop, no `stop_agent_loop` for a pane) — re-sending the \
             context instead of closing `incomplete` ({} resumes left)",
            self.pane_resumes_left
        );
        Some(steering)
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
