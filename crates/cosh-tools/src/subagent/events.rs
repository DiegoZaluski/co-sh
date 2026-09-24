//! Typed event stream for ACP sub-agent sessions.
//!
//! The bare `String` chunk channel (Phase 1 and earlier) only carried agent
//! message text. The ACP `session/update` stream offers more — thoughts, tool
//! calls, plans, usage — and the TUI needs all of it (Phase 3 renders the
//! activity inside the sub-agent box). This module defines a self-owned event
//! enum mirroring the relevant [`SessionUpdate`] variants, deliberately
//! decoupled from the `agent-client-protocol` types so the display layer and
//! any future persistence/rehydration do not depend on a versioned protocol
//! crate shape.
//!
//! Protocol enums are `#[non_exhaustive]`: unknown variants map to the
//! mirror enums' `Unknown` fallback (serde `#[serde(other)]` keeps the same
//! behavior for persisted data), so a harness advertising a newer spec never
//! breaks deserialization or the TUI.

use agent_client_protocol::schema::v1 as schema;
use serde::{Deserialize, Serialize};

/// Category of a sub-agent tool call (mirror of ACP `ToolKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reading files or data.
    Read,
    /// Modifying files or content.
    Edit,
    /// Removing files or data.
    Delete,
    /// Moving or renaming files.
    Move,
    /// Searching for information.
    Search,
    /// Running commands or code.
    Execute,
    /// Internal reasoning or planning.
    Think,
    /// Retrieving external data.
    Fetch,
    /// Switching the current session mode.
    SwitchMode,
    /// Anything the mirror does not know yet.
    #[serde(other)]
    Unknown,
}

impl ToolKind {
    fn from_acp(kind: schema::ToolKind) -> Self {
        match kind {
            schema::ToolKind::Read => Self::Read,
            schema::ToolKind::Edit => Self::Edit,
            schema::ToolKind::Delete => Self::Delete,
            schema::ToolKind::Move => Self::Move,
            schema::ToolKind::Search => Self::Search,
            schema::ToolKind::Execute => Self::Execute,
            schema::ToolKind::Think => Self::Think,
            schema::ToolKind::Fetch => Self::Fetch,
            schema::ToolKind::SwitchMode => Self::SwitchMode,
            // `#[non_exhaustive]`: future spec kinds stay recognizable
            // without breaking this crate.
            _ => Self::Unknown,
        }
    }
}

/// Lifecycle status of a sub-agent tool call (mirror of ACP
/// `ToolCallStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Not started yet (input streaming or awaiting approval).
    Pending,
    /// Currently running.
    InProgress,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed,
    /// Anything the mirror does not know yet.
    #[serde(other)]
    Unknown,
}

impl ToolCallStatus {
    fn from_acp(status: schema::ToolCallStatus) -> Self {
        match status {
            schema::ToolCallStatus::Pending => Self::Pending,
            schema::ToolCallStatus::InProgress => Self::InProgress,
            schema::ToolCallStatus::Completed => Self::Completed,
            schema::ToolCallStatus::Failed => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// Priority of a sub-agent plan entry (mirror of ACP `PlanEntryPriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryPriority {
    /// Critical to the overall goal.
    High,
    /// Important but not critical.
    Medium,
    /// Nice to have.
    Low,
    /// Anything the mirror does not know yet.
    #[serde(other)]
    Unknown,
}

impl PlanEntryPriority {
    fn from_acp(priority: schema::PlanEntryPriority) -> Self {
        match priority {
            schema::PlanEntryPriority::High => Self::High,
            schema::PlanEntryPriority::Medium => Self::Medium,
            schema::PlanEntryPriority::Low => Self::Low,
            _ => Self::Unknown,
        }
    }
}

/// Status of a sub-agent plan entry (mirror of ACP `PlanEntryStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    /// Not started yet.
    Pending,
    /// Currently being worked on.
    InProgress,
    /// Successfully completed.
    Completed,
    /// Anything the mirror does not know yet.
    #[serde(other)]
    Unknown,
}

impl PlanEntryStatus {
    fn from_acp(status: schema::PlanEntryStatus) -> Self {
        match status {
            schema::PlanEntryStatus::Pending => Self::Pending,
            schema::PlanEntryStatus::InProgress => Self::InProgress,
            schema::PlanEntryStatus::Completed => Self::Completed,
            _ => Self::Unknown,
        }
    }
}

/// One entry of the sub-agent's execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Human-readable description of what this task aims to accomplish.
    pub content: String,
    /// Relative importance of this task.
    pub priority: PlanEntryPriority,
    /// Current execution status of this task.
    pub status: PlanEntryStatus,
}

/// One tool-call output block: the text the TUI renders plus a compact
/// diff summary for file modifications.
///
/// Images and terminals are counted but not carried (nothing renderable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolOutputBlock {
    /// Extracted text (`ToolCallContent::Content(ContentBlock::Text)`).
    pub text: String,
    /// How many blocks were skipped because they carried no text.
    pub skipped: u32,
    /// Compact summary of the LAST diff block (`ToolCallContent::Diff`):
    /// the modified path plus added/removed line counts derived from
    /// `new_text`/`old_text`. CLI support varies (gemini confirmed sending
    /// diffs); `None` when the update carried no diff.
    pub diff: Option<ToolDiffSummary>,
}

/// Compact summary of one tool-call diff block: what changed and by how
/// much, rendered as a single line (`path +12 −3`) under the tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDiffSummary {
    /// The file path being modified.
    pub path: String,
    /// Lines present in `new_text` but not in `old_text` (all lines of a
    /// new file count as added).
    pub added: u32,
    /// Lines present in `old_text` but not in `new_text` (0 for new files).
    pub removed: u32,
}

/// A typed event produced by a running sub-agent session.
///
/// Everything the ACP `session/update` stream offers that the TUI can act
/// on. Serialized form is stable snake_case so Phase 6 can persist and
/// rehydrate it without a protocol-crate dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SubagentEvent {
    /// A chunk of the agent's response text (the stream the TUI already
    /// renders as the sub-agent's message).
    Message {
        /// The streamed text chunk.
        text: String,
    },
    /// A chunk of the agent's internal reasoning.
    Thought {
        /// The streamed reasoning chunk.
        text: String,
    },
    /// A tool call was initiated by the sub-agent.
    ToolCall {
        /// Unique id of the call within the session.
        id: String,
        /// Human-readable title describing what the tool is doing.
        title: String,
        /// Category of the tool.
        kind: ToolKind,
        /// Execution status at emission time.
        status: ToolCallStatus,
        /// Raw input parameters, when the harness reports them.
        raw_input: Option<serde_json::Value>,
    },
    /// An update to a previously announced tool call.
    ToolCallUpdate {
        /// Id of the updated call.
        id: String,
        /// New status, when the update carries one.
        status: Option<ToolCallStatus>,
        /// New title, when the update carries one.
        title: Option<String>,
        /// Raw output returned by the tool, when the update carries it.
        raw_output: Option<serde_json::Value>,
        /// Text extracted from the update's content blocks (replaces, not
        /// extends — mirroring the ACP "collections are overwritten" rule).
        content: ToolOutputBlock,
    },
    /// The agent's execution plan (complete replacement, per the ACP spec).
    Plan {
        /// The full list of plan entries in order.
        entries: Vec<PlanEntry>,
    },
    /// Context window usage snapshot.
    Usage {
        /// Total context window size in tokens.
        context_window: u64,
        /// Tokens currently in context.
        tokens_in_context: u64,
    },
    /// The session mode changed.
    Mode {
        /// The id of the new mode.
        id: String,
    },
    /// Session metadata (title) was set or changed.
    SessionInfo {
        /// The session title, when one was set.
        title: Option<String>,
    },
}

impl SubagentEvent {
    /// Map one ACP `session/update` into a display event.
    ///
    /// Returns `None` for updates with no TUI representation (user chunks,
    /// available-commands refreshes, config-option updates, unstable
    /// variants); those are logged by the caller instead of silently
    /// dropped.
    pub(crate) fn from_session_update(update: schema::SessionUpdate) -> Option<Self> {
        match update {
            schema::SessionUpdate::AgentMessageChunk(chunk) => {
                chunk_text(chunk, "agent message").map(|text| Self::Message { text })
            }
            schema::SessionUpdate::AgentThoughtChunk(chunk) => {
                chunk_text(chunk, "agent thought").map(|text| Self::Thought { text })
            }
            schema::SessionUpdate::ToolCall(call) => Some(Self::from_tool_call(call)),
            schema::SessionUpdate::ToolCallUpdate(update) => {
                Some(Self::from_tool_call_update(update))
            }
            schema::SessionUpdate::Plan(plan) => Some(Self::from_plan(plan)),
            schema::SessionUpdate::UsageUpdate(update) => Some(Self::from_usage(update)),
            schema::SessionUpdate::CurrentModeUpdate(schema::CurrentModeUpdate {
                current_mode_id,
                ..
            }) => Some(Self::Mode {
                id: current_mode_id.0.to_string(),
            }),
            schema::SessionUpdate::SessionInfoUpdate(update) => {
                session_info_title(&update).map(|title| Self::SessionInfo { title: Some(title) })
            }
            // No display representation (yet).
            _ => None,
        }
    }

    fn from_tool_call(call: schema::ToolCall) -> Self {
        Self::ToolCall {
            id: call.tool_call_id.0.to_string(),
            title: call.title,
            kind: ToolKind::from_acp(call.kind),
            status: ToolCallStatus::from_acp(call.status),
            raw_input: call.raw_input,
        }
    }

    fn from_tool_call_update(update: schema::ToolCallUpdate) -> Self {
        let fields = update.fields;
        Self::ToolCallUpdate {
            id: update.tool_call_id.0.to_string(),
            status: fields.status.map(ToolCallStatus::from_acp),
            title: fields.title,
            raw_output: fields.raw_output,
            content: extract_content_text(fields.content.unwrap_or_default()),
        }
    }

    fn from_plan(plan: schema::Plan) -> Self {
        Self::Plan {
            entries: plan
                .entries
                .into_iter()
                .map(
                    |schema::PlanEntry {
                         content,
                         priority,
                         status,
                         ..
                     }| PlanEntry {
                        content,
                        priority: PlanEntryPriority::from_acp(priority),
                        status: PlanEntryStatus::from_acp(status),
                    },
                )
                .collect(),
        }
    }

    fn from_usage(schema::UsageUpdate { used, size, .. }: schema::UsageUpdate) -> Self {
        Self::Usage {
            context_window: size,
            tokens_in_context: used,
        }
    }
}

/// The text of a content chunk, when it is a text block.
///
/// Non-text blocks (images, audio, resources) yield `None` — they have no
/// place in the event stream yet — and are logged here with the update kind
/// (`what`) so triage can distinguish "unmapped update variant" from
/// "unrenderable content block inside a mapped variant".
fn chunk_text(
    schema::ContentChunk { content, .. }: schema::ContentChunk,
    what: &str,
) -> Option<String> {
    match content {
        schema::ContentBlock::Text(text) => Some(text.text),
        other => {
            log::debug!("sub-agent sent a non-text {what} chunk: {other:?}");
            None
        }
    }
}

/// The session title, when the update sets one (not undefined, not null —
/// a `null` title is a *clear*, which yields no event).
fn session_info_title(update: &schema::SessionInfoUpdate) -> Option<String> {
    update.title.value().cloned()
}

/// Flatten tool-call content blocks into the text the TUI renders.
pub(crate) fn extract_content_text(content: Vec<schema::ToolCallContent>) -> ToolOutputBlock {
    let mut text = String::new();
    let mut skipped = 0_u32;
    let mut diff = None;
    for block in content {
        match block {
            schema::ToolCallContent::Content(inner) => match inner.content {
                schema::ContentBlock::Text(t) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&t.text);
                }
                _ => skipped += 1,
            },
            schema::ToolCallContent::Diff(d) => {
                // The LAST diff block wins — one file edit per tool call is
                // the norm, and a follow-up diff supersedes the previous.
                diff = Some(diff_summary(
                    &d.path.to_string_lossy(),
                    d.old_text.as_deref(),
                    &d.new_text,
                ));
            }
            // Terminals are not rendered in this phase.
            _ => skipped += 1,
        }
    }
    ToolOutputBlock {
        text,
        skipped,
        diff,
    }
}

/// Line-count budget of [`diff_summary`]: beyond this many lines a text side
/// is treated as unbounded and the set difference stops being meaningful —
/// the counts degrade to the raw line-count delta instead of an O(n) scan
/// over megabytes of generated code.
pub(crate) const DIFF_SUMMARY_LINE_CAP: usize = 5_000;

/// Compact `(+added −removed)` summary of one ACP diff block.
///
/// Added = lines present in `new_text` but not in `old_text`; removed the
/// reverse. Line-set difference (not a true LCS diff) on purpose: the box
/// only needs a magnitude, and set semantics stay correct for the common
/// shapes (new file = everything added; rewrite = large on both sides).
/// Past the line cap the counts degrade to the simple length delta.
pub(crate) fn diff_summary(path: &str, old_text: Option<&str>, new_text: &str) -> ToolDiffSummary {
    let old_lines = old_text.unwrap_or("").lines().count();
    let new_lines = new_text.lines().count();
    let (added, removed) = match (old_text, old_lines, new_lines) {
        // New file: everything is an addition.
        (None, _, _) => (new_lines, 0),
        // Either side beyond the cap: approximate with the length delta.
        (_, o, n) if o > DIFF_SUMMARY_LINE_CAP || n > DIFF_SUMMARY_LINE_CAP => {
            (n.saturating_sub(o), o.saturating_sub(n))
        }
        _ => {
            use std::collections::HashSet;
            let old_set: HashSet<&str> = old_text.unwrap_or("").lines().collect();
            let new_set: HashSet<&str> = new_text.lines().collect();
            (
                new_set.difference(&old_set).count(),
                old_set.difference(&new_set).count(),
            )
        }
    };
    ToolDiffSummary {
        path: path.to_string(),
        added: u32::try_from(added).unwrap_or(u32::MAX),
        removed: u32::try_from(removed).unwrap_or(u32::MAX),
    }
}
