use super::App;

impl App {
    /// Freeze the still-running "Summarizing" stopwatch line. The LLM
    /// compaction uses this lifecycle: `Started` opens a running line, its
    /// terminal event freezes it.
    pub(super) fn finalize_compaction_line(session: &mut crate::types::Session) {
        use crate::types::Part;
        let now = crate::types::now_ms();
        let running = session
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(&m.parts[..], [Part::Compaction(c)] if c.is_running()));
        if let Some(msg) = running
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.elapsed_ms = Some(now.saturating_sub(c.started_at));
        }
    }

    /// Handle an LLM-compaction lifecycle event (the last-resort fallback
    /// driven by the harness): `Started` opens the running "Summarizing" box
    /// in the chat, `Finished`/`Failed` freezes it.
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
                if self.telemetry.enabled() {
                    self.session_telemetry.record_message();
                }
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
            cosh::harness::events::LlmCompactionEvent::Progress {
                phase,
                completed,
                total,
            } => {
                let label = match phase {
                    cosh::harness::events::LlmCompactionPhase::Mapping => "Mapping segments",
                    cosh::harness::events::LlmCompactionPhase::SequentialFallback => {
                        "Mapping sequentially"
                    }
                    cosh::harness::events::LlmCompactionPhase::Reducing => "Reducing summaries",
                    cosh::harness::events::LlmCompactionPhase::Validating => {
                        "Validating checkpoint"
                    }
                    cosh::harness::events::LlmCompactionPhase::Correcting => {
                        "Correcting checkpoint"
                    }
                };
                if let Some(crate::types::Part::Compaction(part)) = session
                    .messages
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.parts.iter_mut())
                    .find(
                        |part| matches!(part, crate::types::Part::Compaction(c) if c.is_running()),
                    )
                {
                    part.text = if total == 0 {
                        label.to_string()
                    } else {
                        format!("{label}: {completed}/{total}")
                    };
                }
            }
            cosh::harness::events::LlmCompactionEvent::OutputStarted => {
                if let Some(crate::types::Part::Compaction(part)) = session
                    .messages
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.parts.iter_mut())
                    .find(
                        |part| matches!(part, crate::types::Part::Compaction(c) if c.is_running()),
                    )
                {
                    part.text.clear();
                }
            }
            cosh::harness::events::LlmCompactionEvent::Finished
            | cosh::harness::events::LlmCompactionEvent::Failed => {
                Self::finalize_compaction_line(session);
            }
        }
    }

    /// Accumulate a streamed summary token into the running "Summarizing"
    /// box (the newest running LLM-compaction line in the current session).
    /// Tokens append to the box's text so the user watches the summary being
    /// written live, exactly like the main agent's tokens stream into the
    /// chat.
    pub(super) fn handle_llm_compaction_token(&mut self, text: &str) {
        use crate::types::Part;
        if text.is_empty() {
            return;
        }
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let target = session
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(&m.parts[..], [Part::Compaction(c)] if c.is_running()));
        if let Some(msg) = target
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.text.push_str(text);
        }
    }

    /// Close any still-running compaction lines in the current session so they
    /// never tick forever (e.g. a loop interrupted right after the "Summarizing"
    /// box opened).
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
