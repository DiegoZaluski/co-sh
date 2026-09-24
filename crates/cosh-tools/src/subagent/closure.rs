//! The turn's final-message accumulator — the ACP analogue of the internal
//! harness's `ContextItem::Closure` (the item that marks WHICH text is a
//! loop's final answer).
//!
//! A sub-agent turn narrates: message, tool call, message, tool call, … and
//! the text that matters to the caller is the LAST message — the one written
//! after the final tool call (this is the market-standard contract: Claude
//! Code returns "only its final message" from the Task tool, Gemini CLI's
//! `generalist` returns "only the final result", and `codex exec` literally
//! ships an `--output-last-message` flag). Everything earlier is narration
//! that lives only in the TUI's live timeline.
//!
//! The ACP `SessionUpdate` stream carries NO native end-of-message marker
//! (v1 variants: `AgentMessageChunk`, `AgentThoughtChunk`, `ToolCall`,
//! `ToolCallUpdate`, `Plan`, …), so the boundary is derived: a `ToolCall`
//! ANNOUNCEMENT ends the current message segment, and text chunks after the
//! last announcement are the final message. (The variant list was checked
//! against the `agent-client-protocol` version pinned in Cargo.toml at the
//! time this module was written; re-verify on protocol bumps.)
//!
//! # The defensive fallback
//!
//! A harness that ends its turn immediately after a tool call produces NO
//! final message (Claude Code had exactly this bug — the parent received
//! nothing). An empty report would be worse than the last complete message,
//! so an emptied segment is remembered as the fallback and returned when the
//! turn ends without post-call text.
//!
//! Re-announcements of an ALREADY-SEEN tool call id do not start a new
//! segment (the TUI treats them as patches too): resetting on one would
//! split a message that a quirky harness interleaved with its own patch.

use std::collections::HashSet;

use super::events::SubagentEvent;

/// Tracks which text of one ACP turn is the final message.
#[derive(Debug, Default)]
pub(crate) struct TurnClosure {
    /// Text chunks since the last (new) tool-call announcement — the
    /// candidate final message.
    current: String,
    /// The last message COMPLETED by a tool-call announcement. Only read
    /// when the turn ends with an empty `current` (the fallback).
    previous: String,
    /// Tool-call ids already announced; a repeated id is a re-announcement
    /// (a patch), not a segment boundary.
    seen_calls: HashSet<String>,
}

impl TurnClosure {
    /// Feed one mapped session update. Only `Message` and `ToolCall` events
    /// affect the closure; thoughts, tool updates, plans and usage are
    /// display-only and never enter the returned text.
    pub(crate) fn observe(&mut self, event: &SubagentEvent) {
        match event {
            SubagentEvent::Message { text } => self.current.push_str(text),
            // A NEW tool call ends the current message segment (the guard's
            // first operand deduplicates: a repeated id is a re-announcement
            // — a patch, not a boundary — and falls through to the catch-all
            // arm). An empty segment is a no-op: back-to-back tool calls
            // must not wipe the fallback with an empty overwrite.
            SubagentEvent::ToolCall { id, .. }
                if self.seen_calls.insert(id.clone()) && !self.current.is_empty() =>
            {
                self.previous = std::mem::take(&mut self.current);
            }
            _ => {}
        }
    }

    /// The turn's report: the message written after the last tool call, or —
    /// when the turn ended without any post-call text — the last completed
    /// message before it.
    pub(crate) fn output(&self) -> String {
        if self.current.is_empty() {
            self.previous.clone()
        } else {
            self.current.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> SubagentEvent {
        SubagentEvent::Message {
            text: text.to_string(),
        }
    }

    fn tool_call(id: &str) -> SubagentEvent {
        SubagentEvent::ToolCall {
            id: id.to_string(),
            title: "call".to_string(),
            kind: crate::subagent::events::ToolKind::Read,
            status: crate::subagent::events::ToolCallStatus::InProgress,
            raw_input: None,
        }
    }

    #[test]
    fn the_message_after_the_last_tool_call_is_the_output() {
        let mut closure = TurnClosure::default();
        closure.observe(&message("narration one "));
        closure.observe(&tool_call("a"));
        closure.observe(&message("narration two "));
        closure.observe(&tool_call("b"));
        closure.observe(&message("final report"));
        assert_eq!(closure.output(), "final report");
    }

    #[test]
    fn a_turn_ending_right_after_a_tool_call_falls_back() {
        let mut closure = TurnClosure::default();
        closure.observe(&message("the last real message"));
        closure.observe(&tool_call("a"));
        // Turn ends here: no post-call text at all.
        assert_eq!(closure.output(), "the last real message");
    }

    #[test]
    fn back_to_back_tool_calls_keep_the_fallback() {
        let mut closure = TurnClosure::default();
        closure.observe(&message("before"));
        closure.observe(&tool_call("a"));
        closure.observe(&tool_call("b"));
        // The empty segment between a and b must NOT wipe the fallback.
        assert_eq!(closure.output(), "before");
    }

    #[test]
    fn a_reannounced_call_does_not_split_the_message() {
        let mut closure = TurnClosure::default();
        closure.observe(&tool_call("a"));
        closure.observe(&message("part one "));
        // A quirky harness re-announces the same call mid-message.
        closure.observe(&tool_call("a"));
        closure.observe(&message("part two"));
        assert_eq!(closure.output(), "part one part two");
    }

    #[test]
    fn a_reannounced_call_after_the_final_text_does_not_reset() {
        let mut closure = TurnClosure::default();
        closure.observe(&tool_call("a"));
        closure.observe(&message("the report"));
        closure.observe(&tool_call("a"));
        assert_eq!(closure.output(), "the report");
    }

    #[test]
    fn display_only_events_never_enter_the_output() {
        let mut closure = TurnClosure::default();
        closure.observe(&SubagentEvent::Thought {
            text: "thinking".to_string(),
        });
        closure.observe(&SubagentEvent::Plan {
            entries: Vec::new(),
        });
        closure.observe(&SubagentEvent::Usage {
            context_window: 1,
            tokens_in_context: 1,
        });
        assert_eq!(closure.output(), "");
    }

    #[test]
    fn a_cancelled_turn_with_no_text_is_empty() {
        let mut closure = TurnClosure::default();
        closure.observe(&tool_call("a"));
        assert_eq!(closure.output(), "");
    }
}
