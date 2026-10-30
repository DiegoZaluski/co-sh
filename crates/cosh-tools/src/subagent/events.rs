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
    pub(crate) fn from_acp(kind: schema::ToolKind) -> Self {
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
    /// The REAL unified diff of the edit (the chat's `DiffRenderable` paints
    /// it; the summary line above stays the compact magnitude). `None` when
    /// the source could not produce one. Serialized with a default so older
    /// persisted envelopes (summary-only) still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified: Option<String>,
}

/// One slash command the agent harness advertises through the ACP
/// `available_commands_update` session update.
///
/// The name is the AGENT's own spelling (`/models` for one harness,
/// `/model` for another) — the TUI keeps it verbatim in its slash menu and
/// only the opened box gets standardized through the client-side mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommand {
    /// Command name as the agent exposes it (e.g. `compact`, `new`).
    pub name: String,
    /// Human-readable description of what the command does.
    pub description: String,
    /// Input hint when the command takes free-form text after its name
    /// (the unstructured `hint`; `None` for argument-less commands).
    pub input_hint: Option<String>,
}

/// One offered value of a session configuration select option: the id the
/// `session/set_config_option` request takes plus its human label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigValue {
    /// Value id (what `set_config_option` selects).
    pub value: String,
    /// Human-readable label of the value, when the harness names it.
    pub name: String,
}

/// One session configuration option, mirrored from the ACP config-option
/// payloads (`session/new`, `session/resume`, `session/set_config_option`
/// and the `config_option_update` session update). This is the data behind
/// the agent-side config boxes: model, provider, thought level, mode …
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigOption {
    /// Unique identifier of the option (e.g. `model`).
    pub id: String,
    /// Human-readable label of the option.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Semantic category when the harness names one (`model`, `mode`,
    /// `thought_level`, ...); unknown or absent categories stay `None`.
    pub category: Option<String>,
    /// Offered values of a select option (empty for boolean options).
    pub values: Vec<AgentConfigValue>,
    /// Current value id of a select option (empty when not a select).
    pub current: String,
    /// Current value of a boolean option (`None` when not boolean).
    pub boolean: Option<bool>,
}

/// One session mode an agent operates in, mirrored from the ACP
/// `SessionModeState` payloads (`session/new`, `session/resume`,
/// `session/load`). The id is what the `session/set_mode` request selects;
/// the name is the human label the harness gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionMode {
    /// Stable mode id (what `session/set_mode` selects).
    pub id: String,
    /// Human-readable name of the mode, as the harness names it.
    pub name: String,
    /// Optional human-readable description of the mode.
    pub description: Option<String>,
}

/// The user's answer to a pane permission request (what travels back
/// through the pane's permission channel to the awaiting ACP handler).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PanePermissionAnswer {
    /// The user picked one of the agent's offered options (its id).
    Selected(String),
    /// The user dismissed the box: the pending request is CANCELLED per the
    /// ACP spec (a distinct semantic from an offered reject option — the
    /// turn's harness decides what a cancellation means).
    Cancel,
}

/// One option of a permission request: the id the
/// `session/request_permission` response selects plus its human label and
/// semantic kind (the box colors allow/reject choices by it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPermissionOption {
    /// Stable option id (what the response selects).
    pub id: String,
    /// Human-readable label of the option (e.g. "Allow once").
    pub name: String,
    /// Semantic kind, mirrored from the ACP enum: `allow_once`,
    /// `allow_always`, `reject_once`, `reject_always` (unknown kinds map
    /// to `other`).
    pub kind: String,
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
    /// A chunk of the USER's own message text. Live v1 turns rarely echo
    /// the prompt back; history REPLAY (`session/load`, see
    /// [`acp::replay`](crate::subagent::acp::replay)) streams the retained
    /// user messages through the same chunk variant, so a rehydrated
    /// timeline shows both sides of the conversation instead of an
    /// agent-only monologue.
    UserMessage {
        /// The streamed text chunk.
        text: String,
    },
    /// A chunk of an AGENT's input to the pane — a message another agent
    /// sent through `agents_call` (the main agent's harness or the manager
    /// pane), not the human user. Live-only: the TUI applies it when it
    /// dispatches the pane turn, and the pane renders it with a distinct
    /// rail so the user sees WHO is speaking (history replay never
    /// produces this variant — the ACP session/load stream only carries
    /// real user chunks).
    AgentMessage {
        /// The agent's input text.
        text: String,
    },
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
        /// The tool's REAL name (`fs_edit`, `find_grep`, …) when the source
        /// carries one — the internal sub-agent does; an ACP harness does
        /// not (its protocol has only the kind + a free-form title). The
        /// TUI labels the row with THIS name, exactly like the main chat's
        /// compact renderer, instead of the kind's generic short form.
        /// `#[serde(default)]`: persisted envelopes from before the field
        /// existed still load (they render by kind, as they always did);
        /// `skip_serializing_if` keeps `None` out of the wire form (an ACP
        /// harness never sees a `tool: null` it does not understand).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
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
    /// The session's serving triple, read from the ACP session config
    /// options once per turn (and per replay pass) — the data behind the
    /// `/agents` prompt badge. Emitted by the caller, never by
    /// `session/update` mapping: it is pane chrome, not timeline content,
    /// so the TUI stores it on the pane instead of applying it. Anything
    /// the harness does not advertise stays `None` and its segment drops
    /// out of the badge.
    SessionModel {
        /// Effective model id (after the preferred-model selection).
        model: Option<String>,
        /// Human label of the model value, when the harness names it.
        model_label: Option<String>,
        /// Serving provider id, when the harness advertises a `provider`
        /// option (cline does; kilo/opencode do not).
        provider: Option<String>,
        /// Human label of the provider value, when named.
        provider_label: Option<String>,
        /// Reasoning/thought level display, when the harness offers a real
        /// choice (a single offered value is a fixed marker, not a level,
        /// and stays empty).
        reasoning: Option<String>,
    },
    /// The session mode changed.
    Mode {
        /// The id of the new mode.
        id: String,
    },
    /// A tool call AWAITS the user's permission decision. Emitted by the
    /// ACP `session/request_permission` request when the client runs a
    /// non-Yolo mode (the split's Yolo contract auto-approves instead —
    /// see [`crate::subagent::acp`]'s permission handler). The TUI shows
    /// its permission box for THIS pane; the user's answer flows back
    /// through the pane's permission channel and selects the response
    /// option. Timeline-visible like a tool call: the row shows the
    /// awaiting state until the user answers.
    PermissionRequest {
        /// The awaiting tool call's id (patches it on the answer).
        call_id: String,
        /// The tool's title as the harness describes it.
        title: String,
        /// The tool's kind (the row's icon).
        kind: ToolKind,
        /// The raw input JSON (the TUI's detail rows), when sent.
        raw_input: Option<serde_json::Value>,
        /// The options the agent offers, in wire order: `(option id, label,
        /// kind string)` — the client shows them as the box's rows and the
        /// user's pick sends `option id` back.
        options: Vec<AgentPermissionOption>,
    },
    /// The session's MODES were (re)published — the current mode plus the
    /// full list of modes the agent can operate in. Pane chrome like
    /// `SessionModel`/`Commands`: stored on the pane, never streamed into
    /// the timeline. The client maps its own harness mode onto one of these
    /// ids (a similarity match — no ACP contract fixes the spellings) and
    /// binds it through `session/set_mode`; an agent that advertises no
    /// modes (`None`/empty list, like gemini) simply gets none.
    Modes {
        /// The full mode list, in the harness's published order.
        modes: Vec<AgentSessionMode>,
        /// The id of the mode the session starts in.
        current: String,
    },
    /// Session metadata (title) was set or changed.
    SessionInfo {
        /// The session title, when one was set.
        title: Option<String>,
    },
    /// The agent's available slash commands were (re)published. Emitted by
    /// a live turn whenever the harness refreshes its command list — the
    /// TUI caches it per pane and feeds the focused pane's slash menu.
    /// Also produced by the on-demand `session_snapshot` probe. NOT timeline
    /// content: the TUI stores it on the pane like `SessionModel`.
    Commands {
        /// The full command list, in the harness's published order.
        commands: Vec<AgentCommand>,
    },
    /// The session's config options were (re)published (the full set, not a
    /// delta — ACP always carries the complete list). Pane chrome like
    /// `SessionModel`/`Commands`: stored on the pane, never streamed into
    /// the timeline.
    ConfigOptions {
        /// The full option list.
        options: Vec<AgentConfigOption>,
    },
}

/// Whether a config VALUE must never reach the TUI's pickers: the
/// `/agents` split runs YOLO by contract, so permission-escalation modes
/// some harnesses publish as plain session-mode values (claude's
/// `bypassPermissions`, devin's `bypass`) must be stripped at the mirror —
/// the single choke point every config-options path flows through (the
/// snapshot probe, live `config_option_update`s, and `set_config_option`
/// responses alike).
fn value_exposed(value: &str) -> bool {
    let normalized = value.trim().to_lowercase().replace(['-', ' ', '.'], "_");
    !(normalized.contains("bypass")
        || normalized.contains("permission")
        || normalized.contains("yolo")
        // The permission-softening spellings no stem above can catch —
        // each harness's own docs publish them as plain mode/mode-config
        // VALUES: claude's `acceptEdits` and devin's `accept-edits` (both
        // switch edits to auto-accept — the YOLO contract decides that),
        // codex's `agent-full-access` (the everything-runs toggle).
        || normalized.contains("accept")
        // camelCase publications (`fullAccess`) fold to `fullaccess` — no
        // underscore — so this stem is checked in BOTH forms (claude's
        // `acceptEdits` precedent shows camelCase value spellings exist).
        || normalized.contains("full_access")
        || normalized.contains("fullaccess"))
}

/// Mirror one ACP [`SessionConfigOption`] into the protocol-free shape.
pub(crate) fn mirror_config_option(option: &schema::SessionConfigOption) -> AgentConfigOption {
    let (values, current, boolean) = match &option.kind {
        schema::SessionConfigKind::Select(select) => {
            let values = match &select.options {
                schema::SessionConfigSelectOptions::Ungrouped(options) => options
                    .iter()
                    .filter(|o| value_exposed(&o.value.0))
                    .map(|o| AgentConfigValue {
                        value: o.value.0.to_string(),
                        name: o.name.clone(),
                    })
                    .collect(),
                schema::SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|g| g.options.iter())
                    .filter(|o| value_exposed(&o.value.0))
                    .map(|o| AgentConfigValue {
                        value: o.value.0.to_string(),
                        name: o.name.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (values, select.current_value.0.to_string(), None)
        }
        schema::SessionConfigKind::Boolean(flag) => (Vec::new(), String::new(), Some(flag.current_value)),
        _ => (Vec::new(), String::new(), None),
    };
    AgentConfigOption {
        id: option.id.0.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
        category: option.category.as_ref().map(|c| match c {
            schema::SessionConfigOptionCategory::Model => "model".to_string(),
            schema::SessionConfigOptionCategory::Mode => "mode".to_string(),
            schema::SessionConfigOptionCategory::ModelConfig => "model_config".to_string(),
            schema::SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
            schema::SessionConfigOptionCategory::Other(other) => other.clone(),
            // `#[non_exhaustive]`: a future spec category carries no string
            // of its own here — the option still mirrors, just uncategorized.
            _ => String::new(),
        }),
        values,
        current,
        boolean,
    }
}

impl SubagentEvent {
    /// Whether THIS event is timeline/report CONTENT — the conversation
    /// material itself: message, thought, tool and plan data.
    ///
    /// Pane chrome (usage, mode, session info, commands, config options) is
    /// NOT content: chrome reflects the session's CURRENT state, so replaying
    /// it is harmless (it overwrites the same pane slot). Content is
    /// chronological history: replaying it APPENDS old turns into a new one.
    ///
    /// The live-turn path (`acp::run_session`) uses this to drop the content
    /// half of a `session/load` history replay — the v1 ACP contract makes a
    /// `load`-resume stream the ENTIRE retained conversation as ordinary
    /// `session/update` notifications BEFORE answering (measured on
    /// `goose acp`: the `user_message_chunk` / `agent_thought_chunk` /
    /// `agent_message_chunk` of EVERY previous turn arrive during the load
    /// window). Unfiltered, that replay lands in the live pane's timeline
    /// and in the turn's closure (`TurnClosure`), mixing entries from
    /// different moments — the reported "pane mixes inputs from different
    /// moments" symptom. The dedicated rehydrate path (`acp::replay`) is the
    /// only consumer that WANTS the full content stream.
    pub(crate) fn is_content(&self) -> bool {
        matches!(
            self,
            Self::UserMessage { .. }
                | Self::AgentMessage { .. }
                | Self::Message { .. }
                | Self::Thought { .. }
                | Self::ToolCall { .. }
                | Self::ToolCallUpdate { .. }
                | Self::Plan { .. }
        )
    }

    /// Map one ACP `session/update` into a display event.
    ///
    /// Returns `None` for updates with no TUI representation (available-
    /// commands refreshes, config-option updates, unstable variants); those
    /// are logged by the caller instead of silently dropped. User message
    /// chunks ARE mapped (the replayed history needs both sides), though a
    /// live turn's own prompt echo is suppressed by the caller-side
    /// `turn_only` filter in [`acp::replay`](super::acp).
    pub(crate) fn from_session_update(update: schema::SessionUpdate) -> Option<Self> {
        match update {
            schema::SessionUpdate::UserMessageChunk(chunk) => {
                chunk_text(chunk, "user message").map(|text| Self::UserMessage { text })
            }
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
            schema::SessionUpdate::AvailableCommandsUpdate(update) => {
                Some(Self::Commands {
                    commands: update
                        .available_commands
                        .into_iter()
                        .map(|cmd| AgentCommand {
                            name: cmd.name,
                            description: cmd.description,
                            input_hint: match cmd.input {
                                Some(schema::AvailableCommandInput::Unstructured(input)) => {
                                    Some(input.hint)
                                }
                                _ => None,
                            },
                        })
                        .collect(),
                })
            }
            schema::SessionUpdate::ConfigOptionUpdate(update) => Some(Self::ConfigOptions {
                options: update.config_options.iter().map(mirror_config_option).collect(),
            }),
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
            // The ACP protocol carries no tool name — the free-form title is
            // all a harness gives us. `None`: the row labels itself by kind,
            // as it always did.
            tool: None,
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

/// Context lines around each hunk of the unified diff the TUI renders from
/// an ACP diff block (`ToolDiffSummary::unified`). Three lines — the same
/// default git uses, keeping the agent-view diffs and the chat's edit boxes
/// visually identical.
pub(crate) const UNIFIED_DIFF_CONTEXT: usize = 3;

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
        unified: old_text.map(|old| {
            cosh_sdk::hashline::diff::structured_patch(old, new_text, UNIFIED_DIFF_CONTEXT)
                .to_unified_diff(path, path)
        }),
    }
}
