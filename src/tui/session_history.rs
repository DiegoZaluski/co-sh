//! Immutable session-history events and their derived projections.
//!
//! This module deliberately contains no filesystem writes. A JSONL history is
//! replayed into [`HistoryProjection`]; the projection is disposable and the
//! ordered [`HistoryEvent`] values remain the only source of truth.

use std::collections::{HashMap, HashSet, VecDeque};

use cosh::harness::context::{ContextItem, ContextManagerState, SplitState};
use serde::{Deserialize, Serialize};

use crate::types::{Message, MessageRole, Part, Session};

pub(crate) const HISTORY_SCHEMA_VERSION: u32 = 1;
pub(crate) const LEGACY_HEAD_EVENT_ID: u64 = 0;

/// One immutable JSONL event. IDs are monotonically increasing within the
/// physical history file and are stable reference targets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HistoryEvent {
    pub schema_version: u32,
    pub event_id: u64,
    pub branch_id: String,
    pub delta: Delta,
}

impl HistoryEvent {
    pub(crate) fn new(event_id: u64, branch_id: impl Into<String>, delta: Delta) -> Self {
        Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            event_id,
            branch_id: branch_id.into(),
            delta,
        }
    }
}

/// A stable selection of a reconstructable point in the event history.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Reference {
    pub branch_id: String,
    pub event_id: u64,
    pub selection: Selection,
}

impl Reference {
    pub(crate) fn head(branch_id: impl Into<String>, event_id: u64) -> Self {
        Self {
            branch_id: branch_id.into(),
            event_id,
            selection: Selection::AtEvent,
        }
    }
}

/// Optional refinement of an event state for display message actions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message_id", rename_all = "snake_case")]
pub(crate) enum Selection {
    AtEvent,
    BeforeMessage(String),
    ThroughMessage(String),
}

/// Immutable metadata needed to begin a root history or identify a branch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BranchMetadata {
    pub session_id: String,
    pub title: String,
    pub title_generated: bool,
    pub created_at: u64,
    pub cwd: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

impl BranchMetadata {
    pub(crate) fn from_session(session: &Session, cwd: String) -> Self {
        Self {
            session_id: session.id.clone(),
            title: session.title.clone(),
            title_generated: session.title_generated,
            created_at: session.created_at,
            cwd,
            provider: session.provider.clone(),
            model: session.model.clone(),
            reasoning: session.reasoning.clone(),
        }
    }
}

/// A session state transition. Every variant is applied only during replay;
/// none of them mutates an earlier event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Delta {
    Genesis {
        metadata: BranchMetadata,
    },
    Metadata {
        change: MetadataDelta,
    },
    Message {
        change: MessageDelta,
    },
    Context {
        change: ContextDelta,
    },
    /// A named reference marker. It carries no projected state and is always
    /// optional/rebuildable metadata rather than an authoritative snapshot.
    Snapshot {
        name: String,
        target: Reference,
    },
    Revert {
        target: Reference,
        undo_label: String,
    },
    Rollback {
        target: Reference,
        undo_label: String,
    },
    Fork {
        parent: Reference,
        metadata: BranchMetadata,
    },
    Tombstone,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub(crate) enum MetadataDelta {
    Title {
        title: String,
        title_generated: bool,
    },
    Model {
        provider: Option<String>,
        model: Option<String>,
        reasoning: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum MessageDelta {
    Upsert {
        message: Message,
        #[serde(default)]
        context_item_ids: Vec<u64>,
    },
    Remove {
        message_id: String,
    },
}

/// Fine-grained persisted transitions for the context-manager projection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub(crate) enum ContextDelta {
    Initialize {
        next_id: u64,
        max_tokens: usize,
        overflow_model: Option<String>,
        split: Option<SplitState>,
        visible_from: Option<u64>,
        hidden: HashSet<u64>,
        #[serde(default)]
        masked: HashSet<u64>,
    },
    ItemUpsert {
        item: ContextItem,
    },
    ItemRemove {
        item_id: u64,
    },
    NextId {
        value: u64,
    },
    MaxTokens {
        value: usize,
    },
    OverflowModel {
        value: Option<String>,
    },
    Split {
        value: Option<SplitState>,
    },
    VisibleFrom {
        value: Option<u64>,
    },
    Hidden {
        item_ids: Vec<u64>,
        hidden: bool,
    },
    Masked {
        item_ids: Vec<u64>,
        masked: bool,
    },
}

/// Current state of one logical branch, derived by replay.
#[derive(Clone, Debug)]
pub(crate) struct BranchProjection {
    pub session: Session,
    pub cwd: String,
    pub context: Option<ContextManagerState>,
    pub head_event_id: u64,
    pub deleted: bool,
}

impl BranchProjection {
    fn from_metadata(metadata: BranchMetadata, head_event_id: u64) -> Self {
        Self {
            session: Session {
                id: metadata.session_id,
                title: metadata.title,
                messages: Vec::new(),
                created_at: metadata.created_at,
                title_generated: metadata.title_generated,
                provider: metadata.provider,
                model: metadata.model,
                reasoning: metadata.reasoning,
                ctx_ids: HashMap::new(),
            },
            cwd: metadata.cwd,
            context: None,
            head_event_id,
            deleted: false,
        }
    }

    fn apply_selection(&mut self, selection: &Selection) -> Result<(), ReplayError> {
        let (message_id, keep_through) = match selection {
            Selection::AtEvent => return Ok(()),
            Selection::BeforeMessage(message_id) => (message_id, false),
            Selection::ThroughMessage(message_id) => (message_id, true),
        };
        let Some(index) = self
            .session
            .messages
            .iter()
            .position(|message| message.id == *message_id)
        else {
            return Err(ReplayError::MissingMessage(message_id.clone()));
        };

        let split = index + usize::from(keep_through);
        let kept_ids: HashSet<u64> = self.session.messages[..split]
            .iter()
            .flat_map(|message| {
                self.session
                    .ctx_ids
                    .get(&message.id)
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect();
        let removed_ids: HashSet<u64> = self.session.messages[split..]
            .iter()
            .flat_map(|message| {
                self.session
                    .ctx_ids
                    .get(&message.id)
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect();
        self.session.messages.truncate(split);
        let retained_messages: HashSet<&str> = self
            .session
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        self.session
            .ctx_ids
            .retain(|message_id, _| retained_messages.contains(message_id.as_str()));

        if let Some(context) = self.context.as_mut() {
            if keep_through {
                context.items.retain(|item| kept_ids.contains(&item.id()));
            } else {
                context
                    .items
                    .retain(|item| !removed_ids.contains(&item.id()));
            }
            repair_context_visibility(context);
        }
        Ok(())
    }
}

/// All live branch heads plus the event-point states needed to resolve future
/// references. Both maps are derived and may be discarded at any time.
#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryProjection {
    pub branches: HashMap<String, BranchProjection>,
    event_states: HashMap<(String, u64), BranchProjection>,
    reference_targets: HashSet<(String, u64)>,
    pub snapshots: Vec<(String, String, Reference)>,
    pub reverts: Vec<RevertRecord>,
    pub last_event_id: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RevertRecord {
    pub branch_id: String,
    pub undo_label: String,
    pub previous: Reference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReplayError {
    UnsupportedVersion(u32),
    NonMonotonicEvent { previous: u64, next: u64 },
    DuplicateBranch(String),
    MissingBranch(String),
    MissingReference(Reference),
    MissingMessage(String),
    CrossBranchReference { branch: String, target: String },
    InvalidGenesisBranch { envelope: String, metadata: String },
}

impl HistoryProjection {
    pub(crate) fn replay(
        legacy: Option<BranchProjection>,
        events: &[HistoryEvent],
    ) -> Result<Self, ReplayError> {
        let mut projection = Self {
            reference_targets: events
                .iter()
                .filter_map(|event| match &event.delta {
                    Delta::Snapshot { target, .. }
                    | Delta::Revert { target, .. }
                    | Delta::Rollback { target, .. } => Some(target),
                    Delta::Fork { parent, .. } => Some(parent),
                    _ => None,
                })
                .map(|reference| (reference.branch_id.clone(), reference.event_id))
                .collect(),
            ..Self::default()
        };
        if let Some(mut branch) = legacy {
            branch.head_event_id = LEGACY_HEAD_EVENT_ID;
            let branch_id = branch.session.id.clone();
            if projection
                .reference_targets
                .contains(&(branch_id.clone(), LEGACY_HEAD_EVENT_ID))
            {
                projection
                    .event_states
                    .insert((branch_id.clone(), LEGACY_HEAD_EVENT_ID), branch.clone());
            }
            projection.branches.insert(branch_id, branch);
        }

        for event in events {
            projection.apply_event(event)?;
        }
        for branch in projection.branches.values_mut() {
            if let Some(context) = branch.context.as_mut() {
                repair_context_visibility(context);
            }
        }
        Ok(projection)
    }

    fn apply_event(&mut self, event: &HistoryEvent) -> Result<(), ReplayError> {
        if event.schema_version != HISTORY_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedVersion(event.schema_version));
        }
        if event.event_id <= self.last_event_id && event.event_id != LEGACY_HEAD_EVENT_ID {
            return Err(ReplayError::NonMonotonicEvent {
                previous: self.last_event_id,
                next: event.event_id,
            });
        }

        let mut state = match &event.delta {
            Delta::Genesis { metadata } => {
                if event.branch_id != metadata.session_id {
                    return Err(ReplayError::InvalidGenesisBranch {
                        envelope: event.branch_id.clone(),
                        metadata: metadata.session_id.clone(),
                    });
                }
                if self.branches.contains_key(&event.branch_id) {
                    return Err(ReplayError::DuplicateBranch(event.branch_id.clone()));
                }
                BranchProjection::from_metadata(metadata.clone(), event.event_id)
            }
            Delta::Fork { parent, metadata } => {
                if self.branches.contains_key(&event.branch_id) {
                    return Err(ReplayError::DuplicateBranch(event.branch_id.clone()));
                }
                let mut branch = self.resolve(parent)?;
                branch.session.id = metadata.session_id.clone();
                branch.session.title = metadata.title.clone();
                branch.session.title_generated = metadata.title_generated;
                branch.session.created_at = metadata.created_at;
                branch.session.provider = metadata.provider.clone();
                branch.session.model = metadata.model.clone();
                branch.session.reasoning = metadata.reasoning.clone();
                branch.cwd = metadata.cwd.clone();
                branch.deleted = false;
                branch
            }
            Delta::Revert { target, undo_label } => {
                if target.branch_id != event.branch_id {
                    return Err(ReplayError::CrossBranchReference {
                        branch: event.branch_id.clone(),
                        target: target.branch_id.clone(),
                    });
                }
                let previous = Reference::head(
                    event.branch_id.clone(),
                    self.branches
                        .get(&event.branch_id)
                        .ok_or_else(|| ReplayError::MissingBranch(event.branch_id.clone()))?
                        .head_event_id,
                );
                self.reverts.push(RevertRecord {
                    branch_id: event.branch_id.clone(),
                    undo_label: undo_label.clone(),
                    previous,
                });
                self.resolve(target)?
            }
            Delta::Rollback { target, .. } => {
                if target.branch_id != event.branch_id {
                    return Err(ReplayError::CrossBranchReference {
                        branch: event.branch_id.clone(),
                        target: target.branch_id.clone(),
                    });
                }
                self.resolve(target)?
            }
            _ => self
                .branches
                .get(&event.branch_id)
                .cloned()
                .ok_or_else(|| ReplayError::MissingBranch(event.branch_id.clone()))?,
        };

        match &event.delta {
            Delta::Genesis { .. }
            | Delta::Fork { .. }
            | Delta::Revert { .. }
            | Delta::Rollback { .. } => {}
            Delta::Metadata { change } => apply_metadata(&mut state, change),
            Delta::Message { change } => apply_message(&mut state, change),
            Delta::Context { change } => apply_context(&mut state, change),
            Delta::Snapshot { name, target } => {
                self.snapshots
                    .push((event.branch_id.clone(), name.clone(), target.clone()))
            }
            Delta::Tombstone => state.deleted = true,
        }
        state.head_event_id = event.event_id;
        self.last_event_id = event.event_id;
        if self
            .reference_targets
            .contains(&(event.branch_id.clone(), event.event_id))
        {
            self.event_states
                .insert((event.branch_id.clone(), event.event_id), state.clone());
        }
        self.branches.insert(event.branch_id.clone(), state);
        Ok(())
    }

    pub(crate) fn resolve(&self, reference: &Reference) -> Result<BranchProjection, ReplayError> {
        let mut state = self
            .event_states
            .get(&(reference.branch_id.clone(), reference.event_id))
            .cloned()
            .ok_or_else(|| ReplayError::MissingReference(reference.clone()))?;
        if let Some(context) = state.context.as_mut() {
            repair_context_visibility(context);
        }
        state.apply_selection(&reference.selection)?;
        Ok(state)
    }
}

fn apply_metadata(state: &mut BranchProjection, change: &MetadataDelta) {
    match change {
        MetadataDelta::Title {
            title,
            title_generated,
        } => {
            state.session.title = title.clone();
            state.session.title_generated = *title_generated;
        }
        MetadataDelta::Model {
            provider,
            model,
            reasoning,
        } => {
            state.session.provider = provider.clone();
            state.session.model = model.clone();
            state.session.reasoning = reasoning.clone();
        }
    }
}

fn apply_message(state: &mut BranchProjection, change: &MessageDelta) {
    match change {
        MessageDelta::Upsert {
            message,
            context_item_ids,
        } => {
            match state
                .session
                .messages
                .iter()
                .position(|existing| existing.id == message.id)
            {
                Some(index) => state.session.messages[index] = message.clone(),
                None => state.session.messages.push(message.clone()),
            }
            if context_item_ids.is_empty() {
                state.session.ctx_ids.remove(&message.id);
            } else {
                state
                    .session
                    .ctx_ids
                    .insert(message.id.clone(), context_item_ids.clone());
            }
        }
        MessageDelta::Remove { message_id } => {
            state
                .session
                .messages
                .retain(|message| message.id != *message_id);
            state.session.ctx_ids.remove(message_id);
        }
    }
}

fn context_or_default(state: &mut BranchProjection) -> &mut ContextManagerState {
    state
        .context
        .get_or_insert_with(ContextManagerState::default)
}

fn apply_context(state: &mut BranchProjection, change: &ContextDelta) {
    match change {
        ContextDelta::Initialize {
            next_id,
            max_tokens,
            overflow_model,
            split,
            visible_from,
            hidden,
            masked,
        } => {
            state.context = Some(ContextManagerState {
                items: VecDeque::new(),
                next_id: *next_id,
                max_tokens: *max_tokens,
                overflow_model: overflow_model.clone(),
                split: split.clone(),
                visible_from: *visible_from,
                hidden: hidden.clone(),
                masked: masked.clone(),
            });
        }
        ContextDelta::ItemUpsert { item } => {
            let context = context_or_default(state);
            match context
                .items
                .iter()
                .position(|existing| existing.id() == item.id())
            {
                Some(index) => context.items[index] = item.clone(),
                None => context.items.push_back(item.clone()),
            }
        }
        ContextDelta::ItemRemove { item_id } => {
            let context = context_or_default(state);
            context.items.retain(|item| item.id() != *item_id);
            repair_context_visibility(context);
        }
        ContextDelta::NextId { value } => context_or_default(state).next_id = *value,
        ContextDelta::MaxTokens { value } => context_or_default(state).max_tokens = *value,
        ContextDelta::OverflowModel { value } => {
            context_or_default(state).overflow_model = value.clone();
        }
        ContextDelta::Split { value } => context_or_default(state).split = value.clone(),
        ContextDelta::VisibleFrom { value } => context_or_default(state).visible_from = *value,
        ContextDelta::Hidden { item_ids, hidden } => {
            let context = context_or_default(state);
            for id in item_ids {
                if *hidden {
                    context.hidden.insert(*id);
                } else {
                    context.hidden.remove(id);
                }
            }
        }
        ContextDelta::Masked { item_ids, masked } => {
            let context = context_or_default(state);
            for id in item_ids {
                if *masked {
                    context.masked.insert(*id);
                } else {
                    context.masked.remove(id);
                }
            }
        }
    }
}

fn repair_context_visibility(context: &mut ContextManagerState) {
    if let Some(boundary) = context.visible_from
        && !context.items.iter().any(|item| item.id() == boundary)
    {
        context.visible_from = None;
    }
    let live: HashSet<u64> = context.items.iter().map(ContextItem::id).collect();
    let live_results: HashSet<u64> = context
        .items
        .iter()
        .filter_map(|item| match item {
            ContextItem::ToolResult { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    context.hidden.retain(|id| live.contains(id));
    context.masked.retain(|id| live_results.contains(id));
}

/// Result of decoding a physical JSONL file. Legacy records become one
/// synthetic event-zero projection; appended modern events remain ordinary
/// immutable deltas.
#[derive(Default)]
pub(crate) struct ParsedHistory {
    pub legacy: Option<BranchProjection>,
    pub events: Vec<HistoryEvent>,
    pub corrupt_lines: Vec<usize>,
}

pub(crate) fn parse_jsonl(session_id: &str, contents: &str) -> ParsedHistory {
    let mut parsed = ParsedHistory::default();
    let mut legacy_header: Option<LegacyHeader> = None;
    let mut legacy_messages = Vec::new();
    let mut legacy_items = VecDeque::new();

    for (index, raw) in contents.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<HistoryEvent>(line) {
            parsed.events.push(event);
            continue;
        }
        if index == 0
            && let Ok(header) = serde_json::from_str::<LegacyHeader>(line)
        {
            legacy_header = Some(header);
            continue;
        }
        match serde_json::from_str::<LegacyRecord>(line) {
            Ok(LegacyRecord::Message(message)) => legacy_messages.push(message),
            Ok(LegacyRecord::Item(item)) => legacy_items.push_back(item),
            Err(_) => parsed.corrupt_lines.push(index + 1),
        }
    }

    if let Some(header) = legacy_header {
        let mut session = Session {
            id: session_id.to_string(),
            title: header.title,
            messages: Vec::with_capacity(legacy_messages.len()),
            created_at: header.created_at,
            title_generated: header.title_generated,
            provider: header.provider,
            model: header.model,
            reasoning: header.reasoning,
            ctx_ids: HashMap::new(),
        };
        for stored in legacy_messages {
            if !stored.ctx_ids.is_empty() {
                session
                    .ctx_ids
                    .insert(stored.id.clone(), stored.ctx_ids.clone());
            }
            session.messages.push(stored.into_message());
        }
        let context = header.context.map(|bookkeeping| {
            let mut context = bookkeeping.into_state(legacy_items);
            repair_context_visibility(&mut context);
            context
        });
        parsed.legacy = Some(BranchProjection {
            session,
            cwd: header.cwd,
            context,
            head_event_id: LEGACY_HEAD_EVENT_ID,
            deleted: false,
        });
    }
    parsed
}

#[derive(Deserialize)]
struct LegacyHeader {
    title: String,
    title_generated: bool,
    created_at: u64,
    cwd: String,
    provider: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    #[serde(default, deserialize_with = "deserialize_legacy_context")]
    context: Option<LegacyContextBookkeeping>,
}

fn deserialize_legacy_context<'de, D>(
    deserializer: D,
) -> Result<Option<LegacyContextBookkeeping>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

#[derive(Deserialize)]
struct LegacyContextBookkeeping {
    next_id: u64,
    max_tokens: usize,
    overflow_model: Option<String>,
    split: Option<SplitState>,
    visible_from: Option<u64>,
    hidden: HashSet<u64>,
    #[serde(default)]
    masked: HashSet<u64>,
}

impl LegacyContextBookkeeping {
    fn into_state(self, items: VecDeque<ContextItem>) -> ContextManagerState {
        ContextManagerState {
            items,
            next_id: self.next_id,
            max_tokens: self.max_tokens,
            overflow_model: self.overflow_model,
            split: self.split,
            visible_from: self.visible_from,
            hidden: self.hidden,
            masked: self.masked,
        }
    }
}

#[derive(Deserialize)]
enum LegacyRecord {
    Message(LegacyStoredMessage),
    Item(ContextItem),
}

#[derive(Deserialize)]
struct LegacyStoredMessage {
    id: String,
    role: String,
    parts: Vec<Part>,
    created_at: u64,
    agent: Option<String>,
    model: Option<String>,
    #[serde(default)]
    ctx_ids: Vec<u64>,
}

impl LegacyStoredMessage {
    fn into_message(self) -> Message {
        Message {
            id: self.id,
            role: if self.role == "assistant" {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            parts: self.parts,
            created_at: self.created_at,
            agent: self.agent,
            model: self.model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TextPart;

    fn metadata(id: &str) -> BranchMetadata {
        BranchMetadata {
            session_id: id.into(),
            title: "Session".into(),
            title_generated: false,
            created_at: 10,
            cwd: "/work".into(),
            provider: None,
            model: None,
            reasoning: None,
        }
    }

    fn message(id: &str, role: MessageRole, text: &str) -> Message {
        Message {
            id: id.into(),
            role,
            parts: vec![Part::Text(TextPart {
                text: text.into(),
                synthetic: false,
            })],
            created_at: 10,
            agent: None,
            model: None,
        }
    }

    fn user_item(id: u64, text: &str) -> ContextItem {
        ContextItem::User {
            id,
            original: text.into(),
        }
    }

    #[test]
    fn genesis_and_mutations_replay_into_a_projection() {
        let events = vec![
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
            HistoryEvent::new(
                2,
                "root",
                Delta::Message {
                    change: MessageDelta::Upsert {
                        message: message("m1", MessageRole::User, "hello"),
                        context_item_ids: vec![1],
                    },
                },
            ),
            HistoryEvent::new(
                3,
                "root",
                Delta::Context {
                    change: ContextDelta::Initialize {
                        next_id: 2,
                        max_tokens: 100,
                        overflow_model: None,
                        split: None,
                        visible_from: None,
                        hidden: HashSet::new(),
                        masked: HashSet::new(),
                    },
                },
            ),
            HistoryEvent::new(
                4,
                "root",
                Delta::Context {
                    change: ContextDelta::ItemUpsert {
                        item: user_item(1, "hello"),
                    },
                },
            ),
            HistoryEvent::new(
                5,
                "root",
                Delta::Metadata {
                    change: MetadataDelta::Title {
                        title: "Renamed".into(),
                        title_generated: true,
                    },
                },
            ),
        ];
        let projection = HistoryProjection::replay(None, &events).unwrap();
        let root = &projection.branches["root"];
        assert_eq!(root.session.title, "Renamed");
        assert_eq!(root.session.messages.len(), 1);
        assert_eq!(root.session.ctx_ids["m1"], vec![1]);
        assert_eq!(root.context.as_ref().unwrap().items.len(), 1);
        assert_eq!(root.head_event_id, 5);
    }

    #[test]
    fn revert_rollback_and_fork_resolve_references_without_losing_history() {
        let events = vec![
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
            HistoryEvent::new(
                2,
                "root",
                Delta::Message {
                    change: MessageDelta::Upsert {
                        message: message("m1", MessageRole::User, "one"),
                        context_item_ids: vec![1],
                    },
                },
            ),
            HistoryEvent::new(
                3,
                "root",
                Delta::Message {
                    change: MessageDelta::Upsert {
                        message: message("m2", MessageRole::Assistant, "two"),
                        context_item_ids: vec![2],
                    },
                },
            ),
            HistoryEvent::new(
                4,
                "root",
                Delta::Message {
                    change: MessageDelta::Upsert {
                        message: message("m3", MessageRole::User, "three"),
                        context_item_ids: vec![3],
                    },
                },
            ),
            HistoryEvent::new(
                5,
                "root",
                Delta::Revert {
                    target: Reference {
                        branch_id: "root".into(),
                        event_id: 4,
                        selection: Selection::BeforeMessage("m3".into()),
                    },
                    undo_label: "v1".into(),
                },
            ),
            HistoryEvent::new(
                6,
                "fork",
                Delta::Fork {
                    parent: Reference {
                        branch_id: "root".into(),
                        event_id: 4,
                        selection: Selection::ThroughMessage("m1".into()),
                    },
                    metadata: BranchMetadata {
                        session_id: "fork".into(),
                        title: "Fork".into(),
                        ..metadata("root")
                    },
                },
            ),
            HistoryEvent::new(
                7,
                "root",
                Delta::Rollback {
                    target: Reference::head("root", 4),
                    undo_label: "v1".into(),
                },
            ),
        ];
        let projection = HistoryProjection::replay(None, &events).unwrap();
        assert_eq!(projection.branches["root"].session.messages.len(), 3);
        assert_eq!(projection.branches["fork"].session.messages.len(), 1);
        assert_eq!(projection.branches["fork"].session.title, "Fork");
        assert_eq!(projection.reverts.len(), 1);
        assert_eq!(projection.reverts[0].previous, Reference::head("root", 4));
    }

    #[test]
    fn prefix_selection_repairs_context_visibility() {
        let mut legacy = BranchProjection::from_metadata(metadata("root"), 0);
        legacy.session.messages = vec![
            message("m1", MessageRole::User, "one"),
            message("m2", MessageRole::Assistant, "summary"),
        ];
        legacy.session.ctx_ids.insert("m1".into(), vec![1]);
        legacy.session.ctx_ids.insert("m2".into(), vec![2]);
        legacy.context = Some(ContextManagerState {
            items: VecDeque::from([
                user_item(1, "one"),
                ContextItem::Compaction {
                    id: 2,
                    summary: "summary".into(),
                    covered_ranges: Vec::new(),
                },
            ]),
            next_id: 3,
            max_tokens: 100,
            overflow_model: None,
            split: None,
            visible_from: Some(2),
            hidden: HashSet::from([1]),
            masked: HashSet::new(),
        });
        let projection = HistoryProjection::replay(
            Some(legacy),
            &[HistoryEvent::new(
                1,
                "root",
                Delta::Revert {
                    target: Reference {
                        branch_id: "root".into(),
                        event_id: 0,
                        selection: Selection::BeforeMessage("m2".into()),
                    },
                    undo_label: "v1".into(),
                },
            )],
        )
        .unwrap();
        let context = projection.branches["root"].context.as_ref().unwrap();
        assert_eq!(context.items.len(), 1);
        assert_eq!(context.visible_from, None);
        assert_eq!(context.hidden, HashSet::from([1]));
    }

    #[test]
    fn revert_and_fork_apply_their_distinct_context_prefix_rules() {
        let mut legacy = BranchProjection::from_metadata(metadata("root"), 0);
        legacy.session.messages = vec![
            message("m1", MessageRole::User, "one"),
            message("m2", MessageRole::Assistant, "two"),
        ];
        legacy.session.ctx_ids.insert("m1".into(), vec![1]);
        legacy.session.ctx_ids.insert("m2".into(), vec![2]);
        legacy.context = Some(ContextManagerState {
            items: VecDeque::from([
                user_item(1, "one"),
                user_item(99, "unmapped"),
                user_item(2, "two"),
            ]),
            next_id: 100,
            max_tokens: 100,
            overflow_model: None,
            split: None,
            visible_from: None,
            hidden: HashSet::new(),
            masked: HashSet::new(),
        });
        let events = [
            HistoryEvent::new(
                1,
                "root",
                Delta::Revert {
                    target: Reference {
                        branch_id: "root".into(),
                        event_id: 0,
                        selection: Selection::BeforeMessage("m2".into()),
                    },
                    undo_label: "v1".into(),
                },
            ),
            HistoryEvent::new(
                2,
                "fork",
                Delta::Fork {
                    parent: Reference {
                        branch_id: "root".into(),
                        event_id: 0,
                        selection: Selection::ThroughMessage("m1".into()),
                    },
                    metadata: metadata("fork"),
                },
            ),
        ];
        let projection = HistoryProjection::replay(Some(legacy), &events).unwrap();
        let reverted_ids: Vec<u64> = projection.branches["root"]
            .context
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(ContextItem::id)
            .collect();
        let forked_ids: Vec<u64> = projection.branches["fork"]
            .context
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(ContextItem::id)
            .collect();
        assert_eq!(reverted_ids, vec![1, 99]);
        assert_eq!(forked_ids, vec![1]);
    }

    #[test]
    fn replay_caches_only_states_named_by_references() {
        let mut events = vec![HistoryEvent::new(
            1,
            "root",
            Delta::Genesis {
                metadata: metadata("root"),
            },
        )];
        for event_id in 2..=100 {
            events.push(HistoryEvent::new(
                event_id,
                "root",
                Delta::Metadata {
                    change: MetadataDelta::Title {
                        title: format!("title-{event_id}"),
                        title_generated: false,
                    },
                },
            ));
        }
        events.push(HistoryEvent::new(
            101,
            "root",
            Delta::Snapshot {
                name: "middle".into(),
                target: Reference::head("root", 50),
            },
        ));

        let projection = HistoryProjection::replay(None, &events).unwrap();
        assert_eq!(projection.event_states.len(), 1);
        assert_eq!(
            projection
                .resolve(&Reference::head("root", 50))
                .unwrap()
                .session
                .title,
            "title-50"
        );
        assert_eq!(projection.branches["root"].session.title, "title-100");
    }

    #[test]
    fn tombstone_hides_only_the_selected_branch_projection() {
        let events = vec![
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
            HistoryEvent::new(2, "root", Delta::Tombstone),
        ];
        let projection = HistoryProjection::replay(None, &events).unwrap();
        assert!(projection.branches["root"].deleted);
        assert_eq!(projection.branches["root"].head_event_id, 2);
    }

    #[test]
    fn snapshot_is_a_reference_marker_not_a_state_source() {
        let events = vec![
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
            HistoryEvent::new(
                2,
                "root",
                Delta::Snapshot {
                    name: "checkpoint".into(),
                    target: Reference::head("root", 1),
                },
            ),
        ];
        let projection = HistoryProjection::replay(None, &events).unwrap();
        assert_eq!(projection.snapshots.len(), 1);
        assert_eq!(projection.snapshots[0].1, "checkpoint");
        assert!(projection.branches["root"].session.messages.is_empty());
    }

    #[test]
    fn corrupt_lines_are_reported_while_legacy_state_and_events_survive() {
        let legacy = r#"{"title":"Old","title_generated":false,"created_at":10,"cwd":"/work","provider":null,"model":null,"reasoning":null,"context":null}
{"Message":{"id":"m1","role":"user","parts":[{"type":"Text","text":"hi","synthetic":false}],"created_at":10,"agent":null,"model":null,"ctx_ids":[]}}
not-json"#;
        let event = serde_json::to_string(&HistoryEvent::new(
            1,
            "old-id",
            Delta::Metadata {
                change: MetadataDelta::Title {
                    title: "New".into(),
                    title_generated: true,
                },
            },
        ))
        .unwrap();
        let parsed = parse_jsonl("old-id", &format!("{legacy}\n{event}\n"));
        assert_eq!(parsed.corrupt_lines, vec![3]);
        let projection = HistoryProjection::replay(parsed.legacy, &parsed.events).unwrap();
        assert_eq!(projection.branches["old-id"].session.title, "New");
        assert_eq!(projection.branches["old-id"].session.messages.len(), 1);
    }

    #[test]
    fn replay_rejects_forward_or_missing_references() {
        let events = vec![
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
            HistoryEvent::new(
                2,
                "fork",
                Delta::Fork {
                    parent: Reference::head("root", 99),
                    metadata: metadata("fork"),
                },
            ),
        ];
        assert!(matches!(
            HistoryProjection::replay(None, &events),
            Err(ReplayError::MissingReference(_))
        ));
    }
}
