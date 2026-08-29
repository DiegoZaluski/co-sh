use std::io;
use std::time::Duration;

use cosh_tui::core::lib::rgba::RGBA;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::style::Color;

use super::{App, AppMode};
use crate::component::spinner_highlight::HighlightSpinner;
use crate::routes::session::queue_choice::QueueTarget;
use crate::session_store::is_valid_session;
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogType;
use cosh::harness::context_manager::ContextManagerState;
use cosh::harness::HarnessEvent;

impl App {
    pub(super) fn handle_events(&mut self) -> io::Result<bool> {
        self.toast_state.tick(50);

        if !event::poll(Duration::from_millis(50))? {
            return Ok(false);
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    return self.process_key_event(key);
                }
            }
            Event::FocusGained => {
                self.terminal_focused = true;
            }
            Event::FocusLost => {
                self.terminal_focused = false;
            }
            Event::Resize(_w, _h) => {}
            Event::Paste(text) => {
                // If a text input dialog is visible, paste into the dialog input
                if self.is_text_input_visible() {
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ApiKeyInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::LocalUrlInput {
                            input, cursor_pos, ..
                        } = &mut d.dialog_type
                    {
                        let cleaned: String =
                            text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                        input.insert_str(*cursor_pos, &cleaned);
                        *cursor_pos += cleaned.len();
                        d.cursor.note_activity();
                    }
                } else if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                    // Paste into the inline question dialog's text answer.
                    self.question_dialog.handle_paste(&text);
                } else if self.is_rag_mode() {
                    self.handle_rag_paste(&text);
                } else {
                    self.prompt_view.note_activity();
                    self.prompt_view.handle_paste(&text);
                    self.slash_menu.update(&self.prompt_view.input);
                }
            }
            Event::Mouse(crossterm_mouse) => {
                self.handle_mouse_event(crossterm_mouse)?;
            }
        }

        Ok(false)
    }

    pub(super) fn poll_events(&mut self) {
        use crate::types::{
            Message, MessageRole, Part, ReasoningPart, SessionStatus, TextPart, ToolPart,
            ToolStatus,
        };
        use crate::ui::toast::{ToastOptions, ToastVariant};

        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                HarnessEvent::ClearAssistant => {
                    // The SDK retried a mid-stream failure and is about to
                    // re-stream the response from the beginning: drop the
                    // partial assistant message rendered from the failed
                    // attempt (it would otherwise concatenate with the
                    // retried response).
                    if let Some(session) = self.state.current_session_mut()
                        && let Some(msg) = session.messages.last_mut()
                        && msg.role == MessageRole::Assistant
                    {
                        session.messages.pop();
                    }
                }
                HarnessEvent::Token { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => {
                            match msg.parts.last_mut() {
                                Some(Part::Text(tp)) => tp.text.push_str(&text),
                                _ => msg.parts.push(Part::Text(TextPart {
                                    text: text.clone(),
                                    synthetic: false,
                                })),
                            }
                        }
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Text(TextPart {
                                text: text.clone(),
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        }),
                    }
                }

                HarnessEvent::ToolCall { tool, input } => {
                    // Track plan_todo_write calls for right panel TODO list
                    if tool == "plan_todo_write" {
                        self.state.right_panel.pending_todo_update_count += 1;
                    }
                    // Also track plan_todo_cross_off for checkmarks
                    if tool == "plan_todo_cross_off" {
                        self.state.right_panel.pending_todo_update_count += 1;
                    }
                    // Start PTY tracking for bash calls
                    if tool == "bash_run" {
                        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        self.state.right_panel.start_pty(command.to_string(), None);
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Start PTY tracking for subagent calls
                    if tool == "subagent_call" {
                        let agent = input.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = input.get("input").and_then(|v| v.as_str()).unwrap_or("");
                        // A new session of the same agent CLI supersedes the
                        // queue's previous entries inside `start_pty` (they
                        // leave the default display but stay reachable via
                        // ← navigation). Different agents coexist.
                        let cmd = format!("subagent: {agent}");
                        self.state.right_panel.start_pty(cmd, None);
                        // Show the input message as the first line of the dialogue,
                        // visually prefixed to indicate it came from the main agent.
                        if !msg.is_empty() {
                            self.state
                                .right_panel
                                .update_last_pty(format!("→ cosh: {msg}\n"));
                        }
                        self.state.right_panel.scroll_to_bottom();
                    }

                    // Save a clone of input before it moves into the ToolPart
                    let input_clone = input.clone();

                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    let part = Part::Tool(ToolPart {
                        tool: tool.clone(),
                        input,
                        output: None,
                        status: ToolStatus::Running,
                        tool_call_id: None,
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                    });
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => msg.parts.push(part),
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![part],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        }),
                    }
                    // Session borrow dropped; create spinner for the new tool call.
                    // This ensures the beam is visible even if ToolResult arrives
                    // before the next render (fast tools like read).
                    {
                        let display = crate::routes::session::tool_render::tool_display(&tool);
                        let part_idx = self
                            .state
                            .current_session()
                            .and_then(|s| s.messages.last())
                            .map(|m| m.parts.len().saturating_sub(1))
                            .unwrap_or(0);
                        let tool_id = format!("{}_{}", display, part_idx);
                        let text = {
                            let temp_part = ToolPart {
                                tool: tool.clone(),
                                input: input_clone,
                                output: None,
                                status: ToolStatus::Running,
                                tool_call_id: None,
                                is_start: false,
                                is_streaming: false,
                                cached_line_count: None,
                            };
                            crate::routes::session::tool_render::tool_inline_text(&temp_part)
                        };
                        let highlight = crate::routes::session::tool_render::tool_color(display)
                            .map(|c| {
                                let (r, g, b) = match c {
                                    Color::Rgb(r, g, b) => (r, g, b),
                                    _ => (128, 128, 128),
                                };
                                RGBA::from_ints(r, g, b, 255)
                            })
                            .unwrap_or(RGBA::from_ints(128, 128, 128, 255));
                        let base = self.theme.text;
                        let mut spinner = HighlightSpinner::new(&text, highlight, base);
                        spinner.set_beam_pos(0.0);
                        self.session_view
                            .tool_state
                            .tool_spinners
                            .insert(tool_id, spinner);
                    }
                }

                HarnessEvent::ToolResult { output } => {
                    // Try to parse as TODO output to update right panel
                    if self.state.right_panel.pending_todo_update_count > 0 {
                        self.state.right_panel.pending_todo_update_count -= 1;
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output)
                            && let Some(groups) = val
                                .get("list")
                                .and_then(|l| l.get("groups"))
                                .and_then(|g| g.as_array())
                        {
                            let todos: Vec<_> = groups
                                .iter()
                                .flat_map(|g| {
                                    g.get("items")
                                        .and_then(|items| items.as_array())
                                        .into_iter()
                                        .flatten()
                                })
                                .map(|item| {
                                    let status = item
                                        .get("status")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("Pending");
                                    let description = item
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    crate::routes::session::right_panel::types::TodoItem {
                                        status: match status {
                                            "InProgress" => "in_progress",
                                            "Completed" => "completed",
                                            "Cancelled" => "cancelled",
                                            _ => "pending",
                                        }
                                        .to_string(),
                                        content: description.to_string(),
                                    }
                                })
                                .collect();
                            self.state.right_panel.set_todos(todos);
                        }
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };

                    // Find and complete the running tool part, capture its name
                    let mut completed_tool_name: Option<String> = None;
                    'find_running: for msg in session.messages.iter_mut().rev() {
                        for part in msg.parts.iter_mut().rev() {
                            if let Part::Tool(tp) = part
                                && tp.status == ToolStatus::Running
                            {
                                completed_tool_name = Some(tp.tool.clone());
                                tp.status = ToolStatus::Completed;
                                tp.output = Some(output.clone());
                                break 'find_running;
                            }
                        }
                    }

                    // Deduplicate plan_todo_write: only the LAST completed one keeps its output.
                    // Previous completed plan_todo_write parts get cleared so they render
                    // inline ("☰ TODO Write") instead of as full block TODOs.
                    if completed_tool_name.as_deref() == Some("plan_todo_write") {
                        let mut found_current = false;
                        for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.tool == "plan_todo_write"
                                {
                                    if !found_current {
                                        // Skip the current (latest) plan_todo_write
                                        found_current = true;
                                    } else if tp.status == ToolStatus::Completed {
                                        // Clear output of previous completed plan_todo_write
                                        tp.output = None;
                                    }
                                }
                            }
                            // Only search the current assistant message
                            if msg.role == crate::types::MessageRole::User {
                                break;
                            }
                        }
                    }

                    if !self.state.right_panel.is_scrolled_up() {
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Harmless for subagent_call (PTY already Completed via finished:true,
                    // complete_last_pty is a no-op for non-Running sessions).
                    // Required for bash_run which only completes via ToolResult.
                    self.state.right_panel.complete_last_pty(output.clone());
                }

                HarnessEvent::ToolError { error } => {
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                        {
                            tp.status = ToolStatus::Failed(error.clone());
                            break;
                        }
                    }
                    self.state.right_panel.fail_last_pty(error.clone());
                }
                HarnessEvent::ToolOutput {
                    tool,
                    output,
                    finished,
                } => {
                    // Streaming find results (glob/grep matches) are appended to
                    // the running tool part so the chat shows a live counter.
                    // Everything else (bash, subagent) streams into the right
                    // panel PTY.
                    if matches!(tool.as_str(), "find_glob" | "find_grep") {
                        let Some(session) = self.state.current_session_mut() else {
                            continue;
                        };
                        let mut appended = false;
                        'find_part: for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.status == ToolStatus::Running
                                    && tp.tool == tool
                                {
                                    let out = tp.output.get_or_insert_with(String::new);
                                    let added_lines = output.lines().count() as u32;
                                    out.push_str(&output);
                                    // Update cached line count for efficient display
                                    tp.cached_line_count = Some(
                                        tp.cached_line_count
                                            .unwrap_or(0)
                                            .saturating_add(added_lines),
                                    );
                                    appended = true;
                                    break 'find_part;
                                }
                            }
                        }
                        if !appended {
                            log::debug!(
                                "ToolOutput for {tool} with no running part; dropping chunk"
                            );
                        }
                        continue;
                    }
                    // Update right panel PTY with streaming output
                    self.state.right_panel.update_last_pty(output.clone());
                    // Auto-follow if user is at the bottom
                    if !self.state.right_panel.is_scrolled_up() {
                        self.state.right_panel.scroll_to_bottom();
                    }
                    if finished {
                        self.state.right_panel.complete_last_pty(output.clone());
                    }
                }

                HarnessEvent::Reasoning { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => {
                            msg.push_reasoning(&text);
                        }
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Reasoning(ReasoningPart {
                                text: text.clone(),
                                collapsed: true,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        }),
                    }
                }

                HarnessEvent::Done { context } => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;
                    // Safety net: a "Summarizing" line interrupted at its start
                    // must not stay running (the stopwatch would tick forever).
                    self.finalize_stale_compaction_lines();

                    // Persist session to disk if it has valid dialog
                    if let Some(id) = self.state.current_session_id.clone()
                        && let Some(session) = self.state.session_cache.get(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);

                        // Trigger async title generation for the first response.
                        // The session title starts as a timestamp; the LLM produces
                        // a semantic title from the first user message.
                        if !self.title_generated {
                            // Extract the first user message text.
                            let first_user = session.messages.iter().find_map(|m| {
                                if m.role == MessageRole::User {
                                    let text: String = m
                                        .parts
                                        .iter()
                                        .filter_map(|p| match p {
                                            Part::Text(t) => Some(t.text.as_str()),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    if text.is_empty() { None } else { Some(text) }
                                } else {
                                    None
                                }
                            });
                            if let Some(user_prompt) = first_user {
                                let provider = self.llm_config.provider.clone();
                                let model = self.llm_config.model.clone();
                                let base_url = self.base_url_for(&provider);
                                let session_id = id.clone();
                                let session_store = self.session_store.clone();
                                let event_tx = self.event_tx.clone();
                                self.tokio_handle.spawn(async move {
                                    let Ok(mut connector) =
                                        cosh_sdk::connector::Connector::new(&provider)
                                    else {
                                        return;
                                    };
                                    if let Some(ref m) = model {
                                        connector = connector.with_model(m);
                                    }
                                    if let Some(ref url) = base_url {
                                        connector = connector.with_base_url(url.clone());
                                    }
                                    // Disable tools and retry for the title call —
                                    // it is a simple chat completion.
                                    connector = connector
                                        .with_tool_call_mode(
                                            cosh_sdk::connector::ToolCallMode::Native,
                                        )
                                        .with_retry(false);
                                    if let Some(title) =
                                        cosh::harness::generate_title(&connector, &user_prompt)
                                            .await
                                    {
                                        // Persist the updated title to disk.
                                        session_store.update_title(&session_id, &title);
                                        // Update the session title in memory.
                                        let _ = event_tx.send(HarnessEvent::TitleGenerated {
                                            session_id,
                                            title,
                                        });
                                    }
                                });
                                self.title_generated = true;
                            }
                        }
                        // Ensure the sidebar shows the session (must happen
                        // after the immutable borrow of session is released).
                        self.state.ensure_session_summary(&id);
                    }

                    // The loop ended: leftover "next request" messages become
                    // "next agent loop" candidates, and the first one starts a
                    // fresh loop (FIFO). If nothing is queued, the work is
                    // complete — ring the terminal bell to call the user back.
                    let started_new_loop = self.handle_loop_end(true);
                    if !started_new_loop {
                        self.trigger_bell();
                    }
                }

                HarnessEvent::UserMessageInjected { text } => {
                    // The running loop consumed a "next request" message:
                    // retire the in-flight marker and pop the deque head
                    // (FIFO — the harness drains the channel in order).
                    // Popping only on acknowledgment guarantees a message
                    // already handed to the loop can never be lost.
                    //
                    // The ack always belongs to the OWNING session's queue —
                    // pop from there, never from whichever session is being
                    // viewed after a mid-run switch.
                    if self.next_request_in_flight {
                        self.next_request_in_flight = false;
                        let owner = self.active_loop_session_id.clone();
                        let queues = match &owner {
                            Some(id) => self.state.pending_queues.get_mut(id),
                            None => self.state.current_pending_queues_mut(),
                        };
                        if let Some(queues) = queues {
                            queues.next_request.pop_front();
                        }
                        // A delivered head shifts the edited message's home
                        // position one slot closer to the front.
                        if let Some(hint) = &mut self.edit_requeue_hint
                            && hint.queue == QueueTarget::NextRequest
                            && self.active_loop_session_id.as_deref()
                                == Some(hint.session_id.as_str())
                            && hint.index > 0
                        {
                            hint.index -= 1;
                        }
                    }
                    // Mirror the message into the owning session's history —
                    // never into whichever session happens to be selected
                    // after a mid-run switch.
                    let owner_matches = self
                        .active_loop_session_id
                        .as_ref()
                        .is_none_or(|owner| self.state.current_session_id.as_ref() == Some(owner));
                    if owner_matches && let Some(session) = self.state.current_session_mut() {
                        session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::User,
                            parts: vec![Part::Text(TextPart {
                                text,
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        });
                        self.session_view.scroll_to_bottom();
                    }
                }

                HarnessEvent::Stopped { context } => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;
                    self.finalize_stale_compaction_lines();
                    self.toast_state.show(ToastOptions {
                        title: Some("Interrupted".into()),
                        message: "Agent loop was stopped.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 3000,
                    });

                    // Persist session to disk even when stopped (partial dialog is still valuable)
                    if let Some(id) = self.state.current_session_id.clone()
                        && let Some(session) = self.state.session_cache.get(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);
                        self.state.ensure_session_summary(&id);
                    }

                    self.handle_loop_end(true);
                }
                HarnessEvent::ContextInfo { info } => {
                    self.context_info = Some(info);
                }

                HarnessEvent::ContextSnapshot { context } => {
                    // Incremental persistence: the harness emits a throttled
                    // context snapshot mid-run so a crash/restart does not lose
                    // the in-flight run. Persist the session log here, mirroring
                    // the Done/Stopped handlers.
                    self.persist_incrementally(context);
                }

                HarnessEvent::LlmCompaction { event } => {
                    self.handle_llm_compaction_event(event);
                }

                HarnessEvent::LlmCompactionToken { text } => {
                    self.handle_llm_compaction_token(&text);
                }

                HarnessEvent::CompactOnDemand { outcome } => {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    use cosh::harness::ManualCompactionOutcome as Outcome;
                    self.manual_compaction_active = false;
                    let (title, message, variant) = match outcome {
                        Outcome::Compacted => (
                            "Compacted",
                            "Session context summarized.".into(),
                            ToastVariant::Success,
                        ),
                        Outcome::NothingToCompact => (
                            "Compact",
                            "Nothing to compact yet.".into(),
                            ToastVariant::Info,
                        ),
                        Outcome::Failed => (
                            "Compact failed",
                            "The summarization call did not complete.".into(),
                            ToastVariant::Error,
                        ),
                    };
                    self.toast_state.show(ToastOptions {
                        title: Some(title.into()),
                        message,
                        variant,
                        duration_ms: 4000,
                    });
                }

                HarnessEvent::Toast { message, variant } => {
                    // Route harness notifications (context-window overflow,
                    // exhausted retries) through the existing toast system.
                    use crate::ui::toast::{ToastOptions, ToastVariant as TuiToastVariant};
                    use cosh::harness::events::ToastVariant as HarnessToastVariant;
                    let variant = match variant {
                        HarnessToastVariant::Info => TuiToastVariant::Info,
                        HarnessToastVariant::Success => TuiToastVariant::Success,
                        HarnessToastVariant::Warning => TuiToastVariant::Warning,
                        HarnessToastVariant::Error => TuiToastVariant::Error,
                    };
                    self.toast_state.show(ToastOptions {
                        title: None,
                        message,
                        variant,
                        duration_ms: 8000,
                    });
                }

                HarnessEvent::Error(msg) => {
                    self.state.status = SessionStatus::Retry {
                        message: msg.clone(),
                        action: None,
                    };
                    self.agent_spinner = None;

                    // Push error as an assistant message so it appears inline in the chat
                    let error_text = format!("Error: {msg}");
                    if let Some(session) = self.state.current_session_mut() {
                        session.messages.push(Message {
                            id: format!("msg-err-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Text(TextPart {
                                text: error_text,
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: self.llm_config.model.clone(),
                        });
                    }

                    // The run failed: leave the queues parked (promote leftover
                    // next-request messages to next-loop semantics but never
                    // auto-start — the user decides when to resend).
                    self.handle_loop_end(false);

                    // First failure of the keyless OpenCode path (missing key
                    // or auth rejection): offer the one-time free-gateway
                    // recommendation. Write-once — never offered again once answered.
                    self.maybe_offer_gateway_on_error(&msg);
                }

                HarnessEvent::ModelsLoaded { models, current } => {
                    // Update cache with the freshly fetched models
                    self.update_model_cache(&models);

                    // Prepend "auto" entry
                    let auto_entry = cosh::ModelEntry {
                        provider: String::new(),
                        model: "auto".to_string(),
                    };
                    let mut models_with_auto = vec![auto_entry];
                    models_with_auto.extend(models);

                    let auto_current = if current == "auto" {
                        current
                    } else {
                        String::new()
                    };

                    // Update the dialog with the loaded models
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ModelList {
                            models: dialog_models,
                            current: dialog_current,
                            ..
                        } = &mut d.dialog_type
                    {
                        *dialog_models = models_with_auto;
                        *dialog_current = auto_current;
                    }
                }

                HarnessEvent::QuestionRequest { questions } => {
                    // Show the question dialog with real questions from the harness
                    self.question_dialog.show_questions(questions);
                    // Blur the prompt when questions appear (like OpenCode hides the prompt)
                    self.prompt_view.blur();
                }

                HarnessEvent::PermissionRequest {
                    tool,
                    description,
                    args,
                } => {
                    // Show the permission dialog with details from the harness
                    self.permission_dialog.request =
                        Some(crate::routes::session::permission::PermissionRequest {
                            tool,
                            description,
                            args,
                        });
                    self.permission_dialog.visible = true;
                    // Default to "Allow Once" (index 1)
                    self.permission_dialog.selected = 1;
                }

                HarnessEvent::TitleGenerated { session_id, title } => {
                    // Update the session title in memory.
                    if let Some(session) = self.state.session_cache.get_mut(&session_id) {
                        session.title = title.clone();
                        session.title_generated = true;
                    }
                    // Update the sidebar summary.
                    if let Some(summary) = self
                        .state
                        .session_summaries
                        .iter_mut()
                        .find(|s| s.session_id == session_id)
                    {
                        summary.title = title;
                        summary.title_generated = true;
                    }
                }
            }
        }
    }

    /// Incremental persistence: called on each throttled `ContextSnapshot`
    /// event so a crash/restart mid-run resumes from the latest context
    /// instead of the session-start state. Mirrors the Done/Stopped save
    /// logic and skips sessions with no valid dialog yet.
    pub(super) fn persist_incrementally(&mut self, context: ContextManagerState) {
        if self.state.status != SessionStatus::Working {
            return;
        }
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(session) = self.state.session_cache.get(&id)
            && is_valid_session(session)
        {
            self.session_store
                .save_session_async_with_context(session, context);
            self.state.ensure_session_summary(&id);
        }
    }
}
