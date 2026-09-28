use std::io;
use std::time::Duration;

use cosh_tui::core::lib::rgba::RGBA;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::style::Color;

use super::{App, AppMode};
use crate::component::spinner_highlight::HighlightSpinner;
use crate::notification;
use crate::routes::session::delete::terminated_loop_save_target;
use crate::routes::session::queue_choice::QueueTarget;
use crate::session_store::is_valid_session;
use crate::types::{Part, Session, SessionStatus};
use crate::ui::dialogs::DialogType;
use cosh::harness::HarnessEvent;
use cosh::harness::context::ContextManagerState;

/// Process-wide sequence for live-created tool parts' `tool_call_id`s. The
/// id is the identity per-box UI state (expand/collapse, LSP-notes toggle)
/// AND the spinner map key — unique per part across the WHOLE app run,
/// stable once assigned.
///
/// The counter is SEEDED above every id a restored session already carries:
/// persisted parts keep their `call-N` ids from the previous process run
/// (`ensure_tool_call_ids` only fills blanks, it does not renumber), so an
/// unseeded counter would restart at `call-0` on every app start and
/// collide with the transcript's historical ids — the new call would share
/// a spinner/expansion key with an old row, resurrecting the very
/// re-animation bug the call-id keying was introduced to fix. Seeding is
/// idempotent per run: each seed advances the counter past the observed
/// maximum, and the atomic takes the largest value seen.
pub(crate) static TOOL_CALL_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Advance [`TOOL_CALL_SEQ`] past every `call-N` id carried by the given
/// session's tool parts. Called when a persisted session becomes current
/// (and again whenever any session is loaded), so the next live `call-N`
/// allocation can never reuse an id already present in a restored
/// transcript.
pub fn seed_tool_call_seq_beyond_session(session: &Session) {
    let mut max_seen: u64 = TOOL_CALL_SEQ.load(std::sync::atomic::Ordering::Relaxed);
    for msg in &session.messages {
        for part in &msg.parts {
            if let Part::Tool(t) = part
                && let Some(id) = &t.tool_call_id
                && let Some(digits) = id.strip_prefix("call-")
                && let Ok(n) = digits.parse::<u64>()
            {
                max_seen = max_seen.max(n);
            }
        }
    }
    // `fetch_max` keeps the highest value across all seeds; the next
    // `fetch_add` then returns an id STRICTLY above every seen one.
    TOOL_CALL_SEQ.fetch_max(max_seen + 1, std::sync::atomic::Ordering::Relaxed);
}

impl App {
    pub(super) fn handle_events(&mut self) -> io::Result<bool> {
        self.handle_events_with_wait(Duration::from_millis(50))
    }

    /// How long the idle loop may block waiting for input before redrawing.
    ///
    /// Every animation in the TUI is time-based (cursor blink, toast
    /// auto-dismiss, sparkle, paste-burst window, dialog spinners), so an
    /// idle frame only needs to happen when the next one of those deadlines
    /// arrives. Blocking until then instead of the fixed 50 ms drops the
    /// idle redraw rate from ~20 fps to the animation cadence — on mid-range
    /// Windows laptops that removes a permanent CPU/thermal tax that made
    /// every other frame feel slow. Live frames (streaming, spinners,
    /// auto-scroll) never reach this path: `run()` bypasses it entirely via
    /// the frame-pacer wait.
    pub(super) fn idle_wait(&self) -> Duration {
        const DEFAULT: Duration = Duration::from_millis(200);
        const MIN: Duration = Duration::from_millis(16);

        // Toast countdown: repaint as soon as the remaining lifetime can
        // still change the visible state (its progress is not animated
        // frame-by-frame, so the expiry tick is the only deadline).
        if let Some(toast) = &self.toast_state.current {
            return MIN.max(Duration::from_millis(toast.duration_ms.saturating_sub(self.toast_state.elapsed)));
        }

        // Cursor blink: the prompt cursor is On for the first 500 ms after
        // activity, then toggles every 500 ms — wake at the next toggle.
        let now = std::time::SystemTime::now();
        let blink_deadline = |cursor: &crate::component::cursor::Cursor| -> Option<Duration> {
            let idle_ms = now.duration_since(cursor.last_input_at).ok()?.as_millis() as u64;
            if idle_ms < 500 {
                // Steady-on until the blink cycle starts.
                return Some(Duration::from_millis(500 - idle_ms));
            }
            let into_cycle = now
                .duration_since(cursor.blink_start)
                .ok()?
                .as_millis() as u64
                % 1000;
            Some(Duration::from_millis(1000 - into_cycle))
        };
        if matches!(self.mode(), AppMode::Session)
            && !self.question_dialog.visible
            && !self.permission_dialog.visible
            && !self.queue_choice_dialog.visible
            && !self.free_gateway_dialog.visible
        {
            if let Some(wait) = blink_deadline(&self.prompt_view.cursor) {
                return wait.max(MIN);
            }
        }
        // Modal dialog stack cursors blink too.
        if let Some(d) = self.dialog.current()
            && let Some(wait) = blink_deadline(&d.cursor)
        {
            return wait.max(MIN);
        }

        // RAG input and AddProvider search bar have blinking cursors as well
        // (neither route activates the live-render gate on its own).
        if let Some(wait) = blink_deadline(&self.add_provider_view.search_bar.cursor) {
            return wait.max(MIN);
        }
        #[cfg(feature = "embed")]
        if let Some(wait) = blink_deadline(&self.rag_view.url_input.cursor) {
            return wait.max(MIN);
        }

        // Paste burst in flight: the flush is due BURST_WINDOW after the
        // last absorbed key — wake exactly then so a long paste lands
        // atomically at the right moment instead of up to 200 ms late.
        if self.paste_burst_pending_flush_deadline() {
            return MIN;
        }

        // Header sparkle flourish: time-driven starfield — keep redraws at
        // animation cadence while it plays.
        if self.sparkle.is_animating() {
            return Duration::from_millis(33);
        }

        // ModelList loading spinner on top of the dialog stack: frame-counted
        // (advanced per rendered frame), so it needs regular idle frames.
        if let Some(d) = self.dialog.current()
            && matches!(d.dialog_type, DialogType::ModelList { loading: true, .. })
        {
            return Duration::from_millis(33);
        }

        DEFAULT
    }

    pub(super) fn handle_events_with_wait(&mut self, wait: Duration) -> io::Result<bool> {
        // NOTE: no toast tick here. `handle_events*()` is only reached when
        // input events are pending (during the agent loop it can be skipped
        // for whole seconds while the user watches the stream), so a tick
        // with a fixed step would let toasts outlive their programmed
        // duration. The tick runs once per frame in `run()` using the real
        // measured frame delta instead.

        // Block briefly for the FIRST event, then drain everything already
        // buffered. Input events (mouse drag during a copy selection, mouse
        // wheel, key auto-repeat) arrive in bursts far faster than one per
        // frame — consuming exactly one event per loop iteration let the
        // internal event queue back up, so the selection highlight / cursor
        // visibly lagged behind the mouse and then "normalized" once the
        // burst ended. Draining the whole buffer per call applies every
        // pending input BEFORE the next render, keeping the cursor speed
        // constant at any event rate.
        if !event::poll(wait)? {
            return Ok(false);
        }

        let mut quit = false;
        loop {
            let evt = event::read()?;
            if self.process_event(evt)? {
                quit = true;
                break;
            }
            if !event::poll(Duration::from_millis(0))? {
                break;
            }
        }
        Ok(quit)
    }

    /// Dispatch a single input event. Returns `Ok(true)` when the app should
    /// quit. Extracted from `handle_events` so a whole buffered batch can be
    /// drained with the same per-event semantics.
    fn process_event(&mut self, event: Event) -> io::Result<bool> {
        match event {
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
                // A registration panel (hook, MCP server) owns the
                // keyboard while open: paste into its active field.
                if self.is_registration_form_open() {
                    self.paste_registration_form(&text);
                } else if self.is_text_input_visible() {
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ApiKeyInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::LocalUrlInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::CacheTtlInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::RenameSession {
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
                HarnessEvent::ContextItemRecorded {
                    item_id,
                    user,
                    call_item_id,
                    tool_name,
                    compaction,
                } => {
                    let owner = self
                        .active_loop_session_id
                        .as_ref()
                        .or(self.state.current_session_id.as_ref())
                        .cloned();
                    let stream_message = self
                        .stream_msg_id
                        .as_ref()
                        .filter(|(sid, _)| Some(sid) == owner.as_ref())
                        .map(|(_, mid)| mid.clone());
                    if let Some(owner) = owner
                        && let Some(session) = self.state.session_cache.get_mut(&owner)
                    {
                        let message_id = if let Some(call_id) = call_item_id {
                            session
                                .ctx_ids
                                .iter()
                                .find(|(_, ids)| ids.contains(&call_id))
                                .map(|(id, _)| id.clone())
                        } else if let Some(tool_name) = tool_name {
                            // Heuristic fallback: with parallel calls of the SAME
                            // tool this can pick the wrong message. Harmless for
                            // the model (semantic identity comes from the native
                            // call_id groups); ctx_ids here is display bookkeeping.
                            session.messages.iter().rev().find(|m| m.parts.iter().any(|p| matches!(p, Part::Tool(t) if t.tool == tool_name && t.status == ToolStatus::Running))).map(|m| m.id.clone())
                        } else if compaction {
                            session
                                .messages
                                .iter()
                                .rev()
                                .find(|m| {
                                    m.parts
                                        .iter()
                                        .any(|p| matches!(p, Part::Compaction(c) if c.is_running()))
                                })
                                .map(|m| m.id.clone())
                        } else if user {
                            session
                                .messages
                                .iter()
                                .rev()
                                .find(|m| m.role == MessageRole::User)
                                .map(|m| m.id.clone())
                        } else {
                            stream_message.or_else(|| {
                                session
                                    .messages
                                    .iter()
                                    .rev()
                                    .find(|m| m.role == MessageRole::Assistant)
                                    .map(|m| m.id.clone())
                            })
                        };
                        if let Some(message_id) = message_id {
                            let ids = session.ctx_ids.entry(message_id).or_default();
                            if !ids.contains(&item_id) {
                                ids.push(item_id);
                            }
                        }
                    }
                }
                HarnessEvent::BeginAssistant => {
                    // A new streaming attempt starts: what follows belongs to
                    // THIS attempt. ClearAssistant may later discard only the
                    // message opened here — never older transcript content.
                    self.stream_msg_id = None;
                }
                HarnessEvent::ClearAssistant => {
                    // The SDK retried a mid-stream failure: drop the partial
                    // assistant message rendered from the failed attempt (it
                    // would otherwise concatenate with the retried response).
                    // Scoped to the message the attempt opened (BeginAssistant)
                    // AND to the session the attempt belongs to — a session
                    // switch during a background loop must never let a late
                    // reset reach another session's transcript.
                    if let Some((sid, mid)) = self.stream_msg_id.take()
                        && self.state.current_session_id.as_deref() == Some(&sid)
                        && let Some(session) = self.state.current_session_mut()
                        && let Some(pos) = session.messages.iter().position(|m| m.id == mid)
                    {
                        session.messages.remove(pos);
                    }
                }
                HarnessEvent::Token { text } => {
                    // Streaming text means the model is generating a response:
                    // the default spinner state (also restores it after a
                    // reasoning segment).
                    self.set_agent_activity(crate::types::AgentActivity::Working);
                    // Whitespace-only deltas are significant MID-STREAM when
                    // they carry a newline (paragraph/line breaks): Gemini's
                    // streamGenerateContent and several OpenAI-compatible
                    // tokenizers emit standalone `"\n\n"` chunks, and
                    // dropping them glues together text that should start
                    // on a new line (the "missing line break" that only
                    // some providers show). Whitespace WITHOUT a newline
                    // stays noise (it renders identically bundled with the
                    // words around it), and a whitespace-only delta never
                    // OPENS a bubble either.
                    let whitespace_only = text.trim().is_empty();
                    if whitespace_only && !text.contains('\n') {
                        continue;
                    }
                    let cur_session = self.state.current_session_id.clone();
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    // Append to the CURRENT attempt's message only — a fresh
                    // attempt never bleeds into the previous iteration's
                    // message (its tool parts would be at risk on a reset).
                    let taken_id = self.stream_msg_id.take();
                    let target = taken_id
                        .as_ref()
                        .filter(|(sid, _)| cur_session.as_deref() == Some(sid.as_str()))
                        .and_then(|(_, mid)| session.messages.iter_mut().find(|m| m.id == *mid));
                    if whitespace_only && target.is_none() {
                        // Leading whitespace before any text must never open
                        // a blank bubble — and the stream id goes back
                        // untouched: a break chunk must not disturb the
                        // attempt's bookkeeping.
                        self.stream_msg_id = taken_id;
                        continue;
                    }
                    match target {
                        Some(msg) => {
                            self.stream_msg_id = cur_session.map(|sid| (sid, msg.id.clone()));
                            match msg.parts.last_mut() {
                                Some(Part::Text(tp)) => tp.text.push_str(&text),
                                _ => msg.parts.push(Part::Text(TextPart {
                                    text: text.clone(),
                                    synthetic: false,
                                })),
                            }
                        }
                        None => {
                            let id = format!("msg-{}", session.messages.len());
                            self.stream_msg_id = cur_session.map(|sid| (sid, id.clone()));
                            session.messages.push(Message {
                                id,
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
                            });
                        }
                    }
                }

                HarnessEvent::ToolCall { tool, input } => {
                    // Telemetry aggregate: allowlisted/normalized tool name.
                    self.telemetry_tool_call(&tool);
                    // The spinner label follows the tool now running; the
                    // matching ToolResult/ToolError restores the default.
                    self.set_agent_activity(crate::types::AgentActivity::for_tool(&tool));
                    // The attempt ended — its message is now transcript
                    // history (tool parts attach below); a future reset must
                    // never reach it.
                    self.stream_msg_id = None;
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
                        // The echo is ALWAYS one line (multi-line prompts are
                        // collapsed — the full text lives in the chat's tool
                        // part); otherwise a long prompt would bury the live
                        // mini-chat stream under a static multi-row block.
                        if let Some(line) =
                            crate::routes::session::right_panel::types::subagent_input_line(msg)
                        {
                            self.state.right_panel.update_last_pty(line);
                        }
                        self.state.right_panel.scroll_to_bottom();
                    }

                    // Save a clone of input before it moves into the ToolPart
                    let input_clone = input.clone();

                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    let tool_call_id = format!(
                        "call-{}",
                        TOOL_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    let part = Part::Tool(ToolPart {
                        tool: tool.clone(),
                        input,
                        output: None,
                        status: ToolStatus::Running,
                        // Unique per-part identity: the key that per-box
                        // expand/collapse state is keyed by. Without it all
                        // boxes of a tool type shared one key and toggled
                        // together (live parts used to be created with None).
                        tool_call_id: Some(tool_call_id.clone()),
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                        lsp_notes: None,
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
                    // Session borrow dropped; pre-create the spinner for the
                    // new tool call so the beam is visible even if ToolResult
                    // arrives before the next render (fast tools like read).
                    // Pre-create ONLY under the key the part's renderer will
                    // actually use (render_generic-managed tools pre-create
                    // under the generic key; spinner-less renderers skip — a
                    // pre-created entry under a key no renderer reads leaks
                    // one forever-Active spinner per call).
                    let display = crate::routes::session::tool_render::tool_display(&tool);
                    let part_idx = self
                        .state
                        .current_session()
                        .and_then(|s| s.messages.last())
                        .map(|m| m.parts.len().saturating_sub(1))
                        .unwrap_or(0);
                    let tool_id = crate::routes::session::tool_render::renderer_spinner_key(
                        display,
                        Some(tool_call_id.as_str()),
                        part_idx as u16,
                    );
                    if let Some(tool_id) = tool_id {
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
                                lsp_notes: None,
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
                    // The tool is done — back to the default working state
                    // (the model's next response or tool call re-narrows it).
                    // Before the session borrow: `current_session_mut` below
                    // holds `self.state` for the rest of the block.
                    self.set_agent_activity(crate::types::AgentActivity::Working);
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

                    // Update the right panel TODO list when a plan write
                    // completed (applied below: `session` still borrows here).
                    let todo_update = if completed_tool_name.as_deref() == Some("plan_todo_write") {
                        crate::routes::session::right_panel::types::parse_todo_output(&output)
                    } else {
                        None
                    };

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

                    if let Some(todos) = todo_update {
                        self.state.right_panel.set_todos(todos);
                    }
                }

                HarnessEvent::ToolError { error } => {
                    // Telemetry aggregate: fingerprint-only representation,
                    // raw message discarded inside the accumulator.
                    self.telemetry_tool_error(&error);
                    // The failed tool is done — back to the default state.
                    // Before the session borrow (like ToolResult above): the
                    // reset must also run when the session is gone
                    // (`current_session_mut` → None) — a deleted session must
                    // not leave the label stuck on the failed tool's state.
                    self.set_agent_activity(crate::types::AgentActivity::Working);
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

                // Typed sub-agent activity (message text, thoughts, tool
                // calls, plans, usage) feeds the chronological mini-chat
                // timeline of the running subagent window (Phase 3a/3b) and
                // auto-follows the scroll like any other streamed output —
                // the box behaves as a small chat that climbs as content
                // arrives, unless the user scrolled away. NOTE: the `tool`
                // field is always "subagent_call" today and is intentionally
                // ignored; typed events carry no session id, so concurrent
                // subagents of the SAME agent CLI interleave into one window
                // (a pre-existing harness limitation).
                HarnessEvent::SubagentEvent { event, .. } => {
                    if self.state.right_panel.update_subagent_activity(&event)
                        && !self.state.right_panel.is_scrolled_up()
                    {
                        self.state.right_panel.scroll_to_bottom();
                    }
                }

                HarnessEvent::ToolDiagnostics { tool, notes } => {
                    // Emitted right before the matching ToolResult, while the
                    // fs tool part is still Running: attach the findings so
                    // the renderer can draw them below the output block.
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                            && tp.tool == tool
                        {
                            tp.lsp_notes = Some(notes);
                            break;
                        }
                    }
                }

                HarnessEvent::Reasoning { text } => {
                    // A reasoning block is streaming: the model is thinking.
                    self.set_agent_activity(crate::types::AgentActivity::Pondering);
                    if text.trim().is_empty() {
                        continue;
                    }
                    let cur_session = self.state.current_session_id.clone();
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    // Same attempt-scoping as the Token handler: reasoning
                    // belongs to the CURRENT attempt's message.
                    let target = self
                        .stream_msg_id
                        .take()
                        .filter(|(sid, _)| cur_session.as_deref() == Some(sid.as_str()))
                        .and_then(|(_, mid)| session.messages.iter_mut().find(|m| m.id == mid));
                    match target {
                        Some(msg) => {
                            self.stream_msg_id = cur_session.map(|sid| (sid, msg.id.clone()));
                            msg.push_reasoning(&text);
                        }
                        None => {
                            let id = format!("msg-{}", session.messages.len());
                            self.stream_msg_id = cur_session.map(|sid| (sid, id.clone()));
                            session.messages.push(Message {
                                id,
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
                            });
                        }
                    }
                }

                HarnessEvent::Done {
                    context,
                    checkup_verdict,
                } => {
                    // The decision model's audit of a heuristic guard stop:
                    // an improper stop is never silent.
                    // Record-everywhere, toast-on-disagreement: the verdict
                    // is on the terminal event for observability, but a
                    // guard stop the model AGREES with needs no toast.
                    if let Some(verdict) =
                        checkup_verdict.filter(|v| v.contains("not-terminated"))
                    {
                        self.toast_state.show(crate::ui::toast::ToastOptions {
                            title: Some("Termination checkup".into()),
                            message: verdict,
                            variant: crate::ui::toast::ToastVariant::Info,
                            duration_ms: 6000,
                        });
                    }
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner_bass = None;
                    self.stream_msg_id = None;
                    // opencode model: the LSP engine is session-scoped (the
                    // process-wide singleton keeps servers alive between
                    // turns), so the footer keeps showing the last snapshot.
                    // It is refreshed by the next turn's opening snapshot.
                    // Safety net: a "Summarizing" line interrupted at its start
                    // must not stay running (the stopwatch would tick forever).
                    self.finalize_stale_compaction_lines();

                    // Persist the OWNING session (TUI-A), never the viewed
                    // one. Owner gone (deleted mid-run) → skip save/title.
                    let done_target = match self.active_loop_session_id.clone() {
                        Some(owner) => terminated_loop_save_target(Some(owner.as_str()), |id| {
                            self.state.session_cache.contains(id)
                        }),
                        None => self.state.current_session_id.clone(),
                    };
                    // Persist session to disk if it has valid dialog
                    if let Some(id) = done_target
                        && let Some(session) = self.state.session_cache.get_mut(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);

                        // Trigger async title generation for the first response.
                        // The session title starts as a timestamp; the LLM produces
                        // a semantic title from the first user message.
                        // Command-mode turns are LLM work and are NEVER
                        // generated here (the TUI is a plain terminal there);
                        // the flag is per-turn, so a later real agent turn in
                        // the same session still gets its semantic title.
                        if !self.title_generated && !self.command_mode_turn {
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
                                    // Cache-affinity routing: the title call
                                    // belongs to this session, so it carries
                                    // the same opaque affinity hash as the
                                    // main loop (pins it to the warm backend).
                                    connector = connector
                                        .with_session_id(super::session_affinity_id(&session_id));
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
                        // Send a desktop notification if the terminal
                        // window is not focused (user switched away).
                        notification::notify_done(self.terminal_focused);
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
                    self.agent_spinner_bass = None;
                    self.stream_msg_id = None;
                    // Session-scoped servers (see the Done handler): keep the
                    // last LSP snapshot; the next turn refreshes it.
                    self.finalize_stale_compaction_lines();
                    // Route the save by loop owner, never by the viewed
                    // session (TUI-A): deleting background owner A while
                    // viewing B must not write A's context into B. When the
                    // owner is gone (deleted mid-run) skip save + toast.
                    // `None` owner = legacy path without owner tracking:
                    // fall back to the current session to preserve behavior.
                    let save_target = match self.active_loop_session_id.clone() {
                        Some(owner) => terminated_loop_save_target(Some(owner.as_str()), |id| {
                            self.state.session_cache.contains(id)
                        }),
                        None => self.state.current_session_id.clone(),
                    };
                    if save_target.is_some() {
                        self.toast_state.show(ToastOptions {
                            title: Some("Interrupted".into()),
                            message: "Agent loop was stopped.".into(),
                            variant: ToastVariant::Warning,
                            duration_ms: 3000,
                        });
                    }

                    // Persist session to disk even when stopped (partial dialog is still valuable)
                    if let Some(id) = save_target
                        && let Some(session) = self.state.session_cache.get_mut(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);
                        self.state.ensure_session_summary(&id);
                    }

                    let started_new_loop = self.handle_loop_end(true);
                    if !started_new_loop {
                        // Send a desktop notification if the terminal
                        // window is not focused (user switched away).
                        notification::notify_stopped(self.terminal_focused);
                    }
                }
                HarnessEvent::ContextInfo { info } => {
                    self.context_info = Some(info);
                }

                HarnessEvent::LspServers { available, servers } => {
                    // Snapshot of the LSP status for the prompt footer.
                    // Session-scoped (opencode model): the list persists
                    // across turns until a newer snapshot, the user disables
                    // LSP, or the process ends — it is never cleared by loop
                    // lifecycle events.
                    self.state.lsp_available = available;
                    self.state.lsp_servers = servers;
                }

                HarnessEvent::McpStatus { servers } => {
                    // Same session-scoped model as LSP: the footer counts
                    // persist across turns until a newer snapshot arrives.
                    self.state.mcp_count = cosh::mcp::ready_count(&servers);
                    self.state.mcp_errors = cosh::mcp::failed_count(&servers);
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

                HarnessEvent::PromptCorrection { original, result } => {
                    // A cancelled correction never applies: Esc drops the
                    // flag (and aborts the task) BEFORE this event can be
                    // drained, so any response that raced the cancel is
                    // dropped here instead of overwriting the draft.
                    if !self.prompt_correction_active {
                        continue;
                    }
                    self.prompt_correction_active = false;
                    self.prompt_correction_spinner = None;
                    // The task finished on its own: drop its handle so the
                    // field keeps meaning "in-flight task" (not last task).
                    self.prompt_correction_task = None;
                    match result {
                        Ok(corrected) if corrected.trim().is_empty() => {
                            self.toast_state.show(ToastOptions {
                                title: Some("Prompt correction".into()),
                                message:
                                    "The model returned an empty correction; the prompt was kept."
                                        .into(),
                                variant: ToastVariant::Error,
                                duration_ms: 5000,
                            });
                        }
                        Ok(corrected) if self.prompt_view.input == original => {
                            self.prompt_view.replace_with_correction(corrected);
                            self.slash_menu.update(&self.prompt_view.input);
                        }
                        Ok(_) => {
                            // The user edited or sent the draft while the
                            // request was in flight. Never overwrite newer
                            // input with an obsolete correction.
                        }
                        Err(error) => {
                            self.toast_state.show(ToastOptions {
                                title: Some("Prompt correction".into()),
                                message: format!("Could not correct the prompt: {error}"),
                                variant: ToastVariant::Error,
                                duration_ms: 8000,
                            });
                        }
                    }
                }

                HarnessEvent::Error {
                    message,
                    context,
                    checkup_verdict,
                } => {
                    // The decision model's audit of a heuristic guard stop:
                    // an improper stop is never silent.
                    // Record-everywhere, toast-on-disagreement: the verdict
                    // is on the terminal event for observability, but a
                    // guard stop the model AGREES with needs no toast.
                    if let Some(verdict) =
                        checkup_verdict.filter(|v| v.contains("not-terminated"))
                    {
                        self.toast_state.show(crate::ui::toast::ToastOptions {
                            title: Some("Termination checkup".into()),
                            message: verdict,
                            variant: crate::ui::toast::ToastVariant::Info,
                            duration_ms: 6000,
                        });
                    }
                    self.state.status = SessionStatus::Retry {
                        message: message.clone(),
                        action: None,
                    };
                    self.agent_spinner_bass = None;
                    self.stream_msg_id = None;

                    // Push error as an assistant message so it appears inline in the chat
                    let error_text = format!("Error: {message}");
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
                        if let Some(cosh::harness::context::ContextItem::Error { id, .. }) =
                            context.as_ref().and_then(|ctx| ctx.items.back())
                            && let Some(message) = session.messages.last()
                        {
                            session.ctx_ids.insert(message.id.clone(), vec![*id]);
                        }
                    }

                    // Persist the styled error line NOW: the loop is over, no
                    // `Done` snapshot follows, so without this save the error
                    // would vanish after a restart. Harness paths carry the
                    // snapshot with the display-only Error item already
                    // recorded (the model-facing timeline stays clean); a
                    // TUI-side failure (thread runtime, connector
                    // construction, panic) has nothing to persist into the
                    // context — the display JSONL alone carries the styled
                    // error line and the context records on disk stay
                    // untouched. Gated on `is_valid_session` exactly like
                    // Done/Stopped/snapshot: a prompt that never got a real
                    // answer is NOT dialog and must not pollute the sidebar
                    // with empty sessions.
                    if let Some(id) = self.state.current_session_id.clone() {
                        let should_save = self
                            .state
                            .session_cache
                            .get(&id)
                            .is_some_and(crate::session_store::is_valid_session);
                        if should_save && let Some(session) = self.state.session_cache.get_mut(&id)
                        {
                            match context {
                                Some(ctx) => self
                                    .session_store
                                    .save_session_async_with_context(session, ctx),
                                None => self.session_store.save_session_async(session),
                            }
                        }
                    }

                    // The run failed: leave the queues parked (promote leftover
                    // next-request messages to next-loop semantics but never
                    // auto-start — the user decides when to resend).
                    self.handle_loop_end(false);

                    // First failure of the keyless OpenCode path (missing key
                    // or auth rejection): offer the one-time free-gateway
                    // recommendation. Write-once — never offered again once answered.
                    self.maybe_offer_gateway_on_error(&message);
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
                    if self.summarization_model_edit.is_some() {
                        models_with_auto
                            .retain(|entry| entry.model != "auto" && !entry.provider.is_empty());
                    }

                    let auto_current = if current == "auto" {
                        current
                    } else {
                        String::new()
                    };

                    // Update the dialog with the loaded models (and stop
                    // the loading spinner, even when the fetch found
                    // nothing, so the empty list renders as before). Look
                    // past the top of the stack: the user may have pushed
                    // the reasoning sub-dialog on top of the ModelList
                    // while the fetch was in flight. `replace()` clears
                    // the stack before showing the picker, so a single
                    // ModelList can exist: take it and move the payload in
                    // without cloning it.
                    if let Some(d) = self
                        .dialog
                        .stack
                        .iter_mut()
                        .find(|d| matches!(d.dialog_type, DialogType::ModelList { .. }))
                        && let DialogType::ModelList {
                            models: dialog_models,
                            current: dialog_current,
                            loading: dialog_loading,
                            ..
                        } = &mut d.dialog_type
                    {
                        *dialog_models = models_with_auto;
                        *dialog_current = auto_current;
                        *dialog_loading = false;
                    }
                }

                HarnessEvent::QuestionRequest { questions } => {
                    // Show the question dialog with real questions from the harness
                    self.question_dialog.show_questions(questions);
                    // Blur the prompt when questions appear (like OpenCode hides the prompt)
                    self.prompt_view.blur();
                    // Send a desktop notification if the terminal
                    // window is not focused (user switched away): the
                    // dialog blocks the loop until answered.
                    notification::notify_question(self.terminal_focused);
                }

                HarnessEvent::PermissionRequest {
                    tool,
                    description,
                    args,
                } => {
                    // Send a desktop notification if the terminal
                    // window is not focused (user switched away): the
                    // dialog blocks the loop until answered.
                    notification::notify_permission(self.terminal_focused, &tool, &description);
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
                HarnessEvent::Usage {
                    usage,
                    provider,
                    model,
                    reported_cost,
                    reported_cost_credits,
                    remaining_credits,
                } => {
                    self.record_usage(
                        usage,
                        &provider,
                        &model,
                        reported_cost,
                        reported_cost_credits,
                    );
                    // Telemetry aggregate: hashed model, allowlisted
                    // provider, saturating token counters, real cost.
                    self.telemetry_llm_usage(&provider, &model, &usage, reported_cost);
                    // The provider's remaining prepaid balance (Charm
                    // Hypercredits) — the user-facing ◆ figure in the
                    // session header. Latest request wins. Its PRESENCE is
                    // what switches the header to credits mode (only Charm
                    // reports it), so no provider-name matching happens.
                    if remaining_credits.is_some() {
                        self.hypercredit_balance = remaining_credits;
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
        if self.state.status != SessionStatus::Working && !self.manual_compaction_active {
            return;
        }
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(session) = self.state.session_cache.get_mut(&id)
            && is_valid_session(session)
        {
            self.session_store
                .save_session_async_with_context(session, context);
            self.state.ensure_session_summary(&id);
        }
    }
}
