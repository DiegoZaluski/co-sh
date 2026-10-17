//! Feed for the Right Panel's MAIN-AGENT mirror window.
//!
//! While the `/agents` mode is open, the Right Panel's Subagent section can
//! display the MAIN agent itself through the exact viewer the subagents use
//! (F1 expands the section; the mirror outranks every normal candidate
//! while it exists). The mirror is a display-only `PtySession` in
//! `RightPanelState` (`main_mirror` — see `open_main_mirror_window`) fed
//! EXCLUSIVELY by this module: every hook translates a main-loop
//! `HarnessEvent` into the typed `SubagentEvent` the window's timeline
//! already renders, so the mirror reads like the chat with zero new
//! rendering code.
//!
//! The translation mirrors `InternalEventBridge` and `manager_context_
//! replay_events` (the two other projections of the cosh event vocabulary)
//! with one deliberate difference: there is no STREAMING tool-output feed.
//! The live path projects a tool call's FINAL result tail (`ToolResult`/`
//! `ToolError` patch the row once, like a pane mirror's); the mid-stream
//! `ToolOutput` chunks are chat-only — the mirror shows the agent's
//! narration, thinking, tool activity, plan and usage in the
//! subagent-window visual language. The OPEN path additionally backfills
//! the timeline from the session transcript (`backfill_from_transcript`)
//! so a mirror opened after prior dialog shows that dialog too.

use cosh::harness::events::HarnessEvent;
use cosh_tools::subagent::events::{SubagentEvent, ToolCallStatus, ToolOutputBlock};

use super::App;

/// Bridge state for ONE main-agent turn: announced tool calls awaiting
/// their result, FIFO (the main loop guarantees the Nth `ToolResult`/`
/// ToolError` matches the Nth `ToolCall` — the same sequential contract the
/// internal sub-agent bridge relies on). Ids are synthesized per
/// announcement; the mirror is display-only, so ids only need to pair a
/// patch with its row.
#[derive(Default)]
pub(super) struct MirrorBridge {
    issued: usize,
    outstanding: std::collections::VecDeque<(String, String)>,
}

impl MirrorBridge {
    fn tool_call(&mut self, tool: &str, input: &serde_json::Value) -> SubagentEvent {
        let id = format!("main-mirror-{}", self.issued);
        self.issued += 1;
        self.outstanding.push_back((id.clone(), tool.to_string()));
        SubagentEvent::ToolCall {
            id,
            title: tool.to_string(),
            kind: cosh_tools::subagent::internal::tool_kind(tool),
            status: ToolCallStatus::InProgress,
            raw_input: Some(input.clone()),
            tool: Some(tool.to_string()),
        }
    }

    fn patch(&mut self, status: ToolCallStatus, tail: String) -> Option<SubagentEvent> {
        let (id, tool) = self.outstanding.pop_front()?;
        Some(SubagentEvent::ToolCallUpdate {
            id,
            status: Some(status),
            title: None,
            raw_output: None,
            content: cosh_tools::subagent::events::ToolOutputBlock {
                text: cosh_tools::subagent::internal::output_tail(&tail),
                skipped: 0,
                diff: cosh_tools::subagent::internal::diff_summary_from_result(tool, &tail),
            },
        })
    }
}

/// Map a main-loop event to the typed events the mirror window renders.
/// `None` = nothing worth projecting (bookkeeping, LSP/MCP footers,
/// question/permission dialogs — the chat shows those differently).
fn mirror_events(bridge: &mut MirrorBridge, event: &HarnessEvent) -> Vec<SubagentEvent> {
    match event {
        HarnessEvent::Token { text } => {
            vec![SubagentEvent::Message { text: text.clone() }]
        }
        HarnessEvent::Reasoning { text } => {
            vec![SubagentEvent::Thought { text: text.clone() }]
        }
        HarnessEvent::ToolCall { tool, input, .. } => {
            let mut events = vec![bridge.tool_call(tool, input)];
            if tool == "plan_todo_write"
                && let Some(plan) =
                    cosh_tools::subagent::internal::InternalEventBridge::plan_from_todo_write(input)
            {
                events.push(plan);
            }
            events
        }
        HarnessEvent::ToolResult { output, .. } => bridge
            .patch(ToolCallStatus::Completed, output.clone())
            .into_iter()
            .collect(),
        HarnessEvent::ToolError { error, .. } => bridge
            .patch(ToolCallStatus::Failed, error.clone())
            .into_iter()
            .collect(),
        HarnessEvent::ContextInfo { info } => {
            vec![cosh_tools::subagent::internal::InternalEventBridge::usage(
                info.total_tokens,
                info.max_tokens,
            )]
        }
        _ => Vec::new(),
    }
}

/// Backfill the mirror from the session's chat transcript: the conversation
/// the user ALREADY had with the main agent before opening the panel. The
/// mirror opens EMPTY otherwise — a user who interacted first would see no
/// trace of that dialog (the reported defect). Projected through the same
/// typed-event vocabulary, newest last, with the mirror bridge left alone
/// (backfill and live stream never interleave: the bridge is fresh at open).
fn backfill_from_transcript(session: &crate::types::Session) -> Vec<SubagentEvent> {
    let mut events = Vec::new();
    for message in &session.messages {
        match message.role {
            crate::types::MessageRole::User => {
                for part in &message.parts {
                    if let crate::types::Part::Text(text) = part {
                        events.push(SubagentEvent::UserMessage {
                            text: text.text.clone(),
                        });
                    }
                }
            }
            crate::types::MessageRole::Assistant => {
                for part in &message.parts {
                    match part {
                        crate::types::Part::Text(text) => {
                            events.push(SubagentEvent::Message {
                                text: text.text.clone(),
                            });
                        }
                        crate::types::Part::Reasoning(reasoning) => {
                            if !reasoning.text.is_empty() {
                                events.push(SubagentEvent::Thought {
                                    text: reasoning.text.clone(),
                                });
                            }
                        }
                        crate::types::Part::Tool(tool) => {
                            let status = match &tool.status {
                                crate::types::ToolStatus::Running => ToolCallStatus::InProgress,
                                crate::types::ToolStatus::Completed => ToolCallStatus::Completed,
                                crate::types::ToolStatus::Failed(_) => ToolCallStatus::Failed,
                            };
                            let output_block = tool.output.as_ref().map(|tail| ToolOutputBlock {
                                text: cosh_tools::subagent::internal::output_tail(tail),
                                skipped: 0,
                                diff: cosh_tools::subagent::internal::diff_summary_from_result(
                                    tool.tool.clone(), tail,
                                ),
                            });
                            // ONE id for the call AND its update: a fallback
                            // id computed per-event would drift (events.len()
                            // grows between the two pushes) and the patch
                            // would never pair with its row.
                            let id = tool
                                .tool_call_id
                                .clone()
                                .unwrap_or_else(|| format!("backfill-{}", events.len()));
                            events.push(SubagentEvent::ToolCall {
                                id: id.clone(),
                                title: tool.tool.clone(),
                                kind: cosh_tools::subagent::internal::tool_kind(&tool.tool),
                                status,
                                raw_input: Some(tool.input.clone()),
                                tool: Some(tool.tool.clone()),
                            });
                            if let Some(block) = output_block {
                                events.push(SubagentEvent::ToolCallUpdate {
                                    id,
                                    status: Some(status),
                                    title: None,
                                    raw_output: None,
                                    content: block,
                                });
                            }
                        }
                        crate::types::Part::File(_)
                        | crate::types::Part::Compaction(_)
                        | crate::types::Part::Install(_) => {}
                    }
                }
            }
        }
    }
    events
}

impl App {
    /// Whether the main-agent mirror owns the Subagent section right now.
    pub(super) fn main_mirror_open(&self) -> bool {
        self.state.right_panel.main_mirror_index().is_some()
    }

    /// Open the main-agent mirror. Idempotent; the bridge is reset on EVERY
    /// open (a close→reopen mid-turn must not patch the new window against
    /// ids its fresh timeline has never seen — the old bridge's outstanding
    /// deque would feed patches for tool rows that no longer exist, dropped
    /// silently).
    ///
    /// NO list-mode handling here: `subagent_list_active` is false whenever
    /// the mirror exists (the sole-owner guard in `RightPanelState`), so the
    /// section renders exactly the mirror window expanded. The list-mode /
    /// focus state the user left behind is left UNTOUCHED — it reasserts
    /// itself the moment the mirror closes, unchanged.
    ///
    /// Idempotent for real: an already-open mirror is left exactly as it is
    /// (no second backfill stacking a duplicate transcript into the window).
    pub(super) fn open_main_mirror(&mut self) {
        if self.main_mirror_open() {
            return;
        }
        self.main_mirror_bridge = MirrorBridge::default();
        self.state.right_panel.open_main_mirror_window();
        // BACKFILL: project the conversation the user ALREADY had with the
        // main agent into the fresh window (a user who interacted first
        // must still see that dialog — the panel opens showing the main
        // agent's history, not an empty pane). The bridge stays untouched:
        // backfill and live stream never interleave (it was just reset).
        let session = self.state.current_session().cloned();
        if let Some(session) = session {
            let running = self.state.status == crate::types::SessionStatus::Working;
            for event in backfill_from_transcript(&session) {
                self.state.right_panel.sync_main_mirror_event(&event, running);
            }
        }
    }

    /// Close the mirror for good: the timeline resets, the next open starts
    /// a fresh projection of the then-current conversation.
    pub(super) fn close_main_mirror(&mut self) {
        self.state.right_panel.close_main_mirror_window(true);
    }

    /// Feed ONE main-loop event into the mirror (a no-op while closed).
    /// The mirror is only projected while `/agents` is open — a turn run
    /// outside the mode builds no projection (the transcript is the view).
    pub(super) fn mirror_main_event(&mut self, event: &HarnessEvent, running: bool) {
        if !self.agents_split.is_some() || !self.main_mirror_open() {
            return;
        }
        for sub in mirror_events(&mut self.main_mirror_bridge, event) {
            self.state.right_panel.sync_main_mirror_event(&sub, running);
        }
    }

    /// Echo the just-submitted prompt as the mirror's turn input (a no-op
    /// while closed). Called on the same submit path `start_agent_loop`
    /// serves.
    pub(super) fn mirror_main_input(&mut self, input: &str) {
        if !self.agents_split.is_some() || !self.main_mirror_open() {
            return;
        }
        self.main_mirror_bridge = MirrorBridge::default();
        self.state.right_panel.set_main_mirror_input_echo(Some(input));
    }

    /// Append a free-form line (a loop error, an interrupted-turn notice)
    /// to the mirror's transcript — the same `Message` entry a subagent
    /// window renders for its failure text.
    pub(super) fn mirror_main_message(&mut self, text: &str) {
        if !self.agents_split.is_some() || !self.main_mirror_open() {
            return;
        }
        self.main_mirror_bridge = MirrorBridge::default();
        self.state
            .right_panel
            .sync_main_mirror_event(&SubagentEvent::Message { text: text.to_string() }, false);
    }

    /// Seal the mirror's terminal marker for the turn that just ended
    /// (`failed` = Stopped/Error — the same rule `close_turn` applies).
    pub(super) fn mirror_main_done(&mut self, failed: bool) {
        if !self.agents_split.is_some() || !self.main_mirror_open() {
            return;
        }
        self.main_mirror_bridge = MirrorBridge::default();
        self.state.right_panel.finish_main_mirror(failed);
    }
}

