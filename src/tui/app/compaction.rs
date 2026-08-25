use super::App;
use cosh::harness::context_manager::CompactionEvent;

impl App {
    /// Append a one-shot compaction notice line (phases 2-4) to the current
    /// session chat.
    pub(super) fn push_compaction_line(
        session: &mut crate::types::Session,
        phase: crate::types::CompactionPhase,
    ) {
        use crate::types::{CompactionPart, Message, MessageRole, Part};
        session.messages.push(Message {
            id: format!("msg-ctx-{}", session.messages.len()),
            role: MessageRole::Assistant,
            parts: vec![Part::Compaction(CompactionPart::done(phase))],
            created_at: 0,
            agent: None,
            model: None,
        });
    }

    /// Handle a compaction-phase notification from the harness: start the
    /// pipeline stopwatch line, finalize it (freezing the elapsed time), or
    /// append a one-shot phase notice.
    ///
    /// NOTE on the stopwatch: it measures from when THIS function processes
    /// `PipelineStarted` to when it processes `PipelineFinished`, so channel
    /// queue delay is included. That is the intended design ("the TUI times
    /// it, the context manager only notifies") — do not "fix" it to use
    /// harness-side timestamps.
    pub(super) fn handle_compaction_event(&mut self, event: CompactionEvent) {
        use crate::types::{CompactionPart, CompactionPhase, Message, MessageRole, Part};
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        match event {
            CompactionEvent::PipelineStarted => {
                session.messages.push(Message {
                    id: format!("msg-ctx-{}", session.messages.len()),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Compaction(CompactionPart::running(
                        CompactionPhase::Pipeline,
                    ))],
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    agent: None,
                    model: None,
                });
            }
            CompactionEvent::PipelineFinished => {
                Self::finalize_compaction_line(session, CompactionPhase::Pipeline);
            }
            CompactionEvent::DraftsEvicted => {
                Self::push_compaction_line(session, CompactionPhase::Drafts);
            }
        }
    }

    /// Freeze the still-running stopwatch line of the given phase. The
    /// pipeline and the LLM compaction share this lifecycle: `Started` opens
    /// a running line, its terminal event freezes it.
    pub(super) fn finalize_compaction_line(
        session: &mut crate::types::Session,
        phase: crate::types::CompactionPhase,
    ) {
        use crate::types::Part;
        let now = crate::types::now_ms();
        let running = session.messages.iter_mut().rev().find(|m| {
            matches!(&m.parts[..], [Part::Compaction(c)]
                if c.phase == phase && c.is_running())
        });
        if let Some(msg) = running
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.elapsed_ms = Some(now.saturating_sub(c.started_at));
        }
    }

    /// Handle an LLM-compaction lifecycle event (phase 3, the last-resort
    /// fallback driven by the harness): `Started` opens the running
    /// "Summarizing" box in the chat, `Finished`/`Failed` freezes it.
    pub(super) fn handle_llm_compaction_event(
        &mut self,
        event: cosh::harness::events::LlmCompactionEvent,
    ) {
        use crate::types::{CompactionPart, CompactionPhase, Message, MessageRole, Part};
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        match event {
            cosh::harness::events::LlmCompactionEvent::Started => {
                session.messages.push(Message {
                    id: format!("msg-ctx-{}", session.messages.len()),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Compaction(CompactionPart::running(
                        CompactionPhase::Llm,
                    ))],
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    agent: None,
                    model: None,
                });
            }
            cosh::harness::events::LlmCompactionEvent::Finished
            | cosh::harness::events::LlmCompactionEvent::Failed => {
                Self::finalize_compaction_line(session, CompactionPhase::Llm);
            }
        }
    }

    /// Accumulate a streamed summary token into the running "Summarizing"
    /// box (the newest running LLM-compaction line in the current session).
    /// Tokens append to the box's text so the user watches the summary being
    /// written live, exactly like the main agent's tokens stream into the
    /// chat.
    pub(super) fn handle_llm_compaction_token(&mut self, text: &str) {
        use crate::types::{CompactionPhase, Part};
        if text.is_empty() {
            return;
        }
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let target = session.messages.iter_mut().rev().find(|m| {
            matches!(&m.parts[..], [Part::Compaction(c)]
                if c.phase == CompactionPhase::Llm && c.is_running())
        });
        if let Some(msg) = target
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.text.push_str(text);
        }
    }

    /// Close any still-running compaction lines in the current session so they
    /// never tick forever (e.g. a session restored from disk with a line that
    /// was mid-pipeline when the process died, or a loop interrupted right
    /// after `PipelineStarted`).
    pub(super) fn finalize_stale_compaction_lines(&mut self) {
        let now = crate::types::now_ms();
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        for msg in &mut session.messages {
            if let Some(crate::types::Part::Compaction(c)) = msg.parts.last_mut()
                && c.is_running()
            {
                c.elapsed_ms = Some(now.saturating_sub(c.started_at));
            }
        }
    }

    /// True when the current session has a still-running compaction line — the
    /// render loop must stay live so the stopwatch ticks every frame.
    pub(super) fn has_running_compaction(&self) -> bool {
        self.state.current_session().is_some_and(|s| {
            s.messages.iter().any(|m| {
                m.parts
                    .iter()
                    .any(|p| matches!(p, crate::types::Part::Compaction(c) if c.is_running()))
            })
        })
    }
}
