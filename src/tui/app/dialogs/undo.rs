use super::super::App;
use crossterm::event::KeyCode;

use crate::routes::session::agents_split::MANAGER_LABEL;
use crate::routes::session::right_panel::types::SubagentTimelineEntry;
use crate::ui::dialogs::DialogType;

use crate::ui::toast::{ToastOptions, ToastVariant};

/// How many of the most recent ACP checkpoints the `/undo` box lists. The
/// native list applies the same kind of cap (the store's `UNDO_VERSIONS_CAP`):
/// the box draws one row per entry and does not scroll.
const PANE_UNDO_CAP: usize = 10;

impl App {
    pub(in crate::app) fn is_undo_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::UndoList { .. }))
    }

    /// `/undo`: open the undo references for what the user is talking to.
    ///
    /// - a focused ACP pane of the live split answers with ITS checkpoints
    ///   ([`Self::open_pane_undo_dialog`]) — `/undo` is a Global-scoped
    ///   command, so with an external agent focused the App still handles it,
    ///   and the only meaningful target is that agent's own conversation;
    /// - otherwise the native, store-backed list is unchanged: the current
    ///   session's pre-revert references (a manager pane included — its
    ///   conversation IS a cosh session).
    pub(in crate::app) fn open_undo_dialog(&mut self) {
        if let Some(pane) = self.focused_external_pane() {
            self.open_pane_undo_dialog(pane);
            return;
        }
        let Some(session_id) = self.state.current_session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let versions = self.session_store.undo_versions(&session_id);
        if versions.is_empty() {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "No revert points yet.".into(),
                variant: ToastVariant::Info,
                duration_ms: 4000,
            });
            return;
        }
        self.dialog.show(DialogType::UndoList {
            session_id,
            versions,
            pane: None,
        });
    }

    /// The focused ACP pane of the LIVE split, if any. A MANAGER pane is not
    /// one: its turns run on a cosh session in the store, so the native list
    /// already answers for it.
    fn focused_external_pane(&self) -> Option<usize> {
        let split = self.agents_split.as_ref()?;
        let focused = split.focused?;
        split
            .panes
            .get(focused)
            .filter(|pane| pane.agent != MANAGER_LABEL)
            .map(|_| focused)
    }

    /// `/undo` on an ACP pane: the pane's transcript is the checkpoint log.
    ///
    /// ACP has no history truncation, and the agent's own session log is
    /// append-only, so every user prompt the pane shows is a point the
    /// conversation can be rewound TO — the entry order the ACP `session/load`
    /// replay rebuilds after a restart. The box lists them oldest first (like
    /// the native list), with the selection parked on the NEWEST one so Enter
    /// undoes the last exchange.
    fn open_pane_undo_dialog(&mut self, pane: usize) {
        let Some(pane_state) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
        else {
            return;
        };
        if pane_state.running || pane_state.rehydrating {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "The agent is working — wait for it to finish.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        let mut versions = pane_checkpoints(&pane_state.activity.timeline);
        // Keep the NEWEST checkpoints (labels stay attached to their prompt,
        // so a capped list starts at `v3`, never renumbered).
        let skip = versions.len().saturating_sub(PANE_UNDO_CAP);
        versions.drain(..skip);
        if versions.is_empty() {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "No checkpoints yet.".into(),
                variant: ToastVariant::Info,
                duration_ms: 4000,
            });
            return;
        }
        let newest = versions.len() - 1;
        self.dialog.show(DialogType::UndoList {
            session_id: String::new(),
            versions,
            pane: Some(pane),
        });
        if let Some(dialog) = self.dialog.current_mut() {
            dialog.selected = newest;
        }
    }

    pub(in crate::app) fn handle_undo_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_undo_dialog_visible() {
            return false;
        }

        let versions = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::UndoList { versions, .. } => versions.clone(),
                _ => return false,
            },
            None => return false,
        };
        let max = versions.len();
        if max == 0 {
            return false;
        }

        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 {
                        max - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % max;
                }
                true
            }
            KeyCode::Enter => {
                let (session_id, pane, selected) = match self.dialog.current() {
                    Some(d) => match &d.dialog_type {
                        DialogType::UndoList {
                            session_id,
                            versions,
                            pane,
                        } => (
                            session_id.clone(),
                            *pane,
                            d.selected.min(versions.len().saturating_sub(1)),
                        ),
                        _ => return false,
                    },
                    None => return false,
                };
                self.dialog.pop();
                let (label, _) = &versions[selected];
                match pane {
                    Some(pane) => self.restore_pane_checkpoint(pane, label),
                    None => self.restore_undo_version(&session_id, label),
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Append a rollback reference, then reload the derived branch view.
    pub(in crate::app) fn restore_undo_version(&mut self, session_id: &str, version: &str) {
        if self.state.status == crate::types::SessionStatus::Working {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "The agent is working — wait for it to finish.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        if !self.session_store.rollback_session(session_id, version) {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: format!("Could not restore {version}."),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        // The cached projection is stale now — force a replay and jump the
        // view to the bottom of the selected state.
        self.state.session_cache.pop(session_id);
        self.state
            .ensure_session_cached(session_id, &self.session_store);
        self.state.session_summaries = self.session_store.list_sessions();
        self.session_view.scroll_to_bottom();
        self.toast_state.show(ToastOptions {
            title: Some("Undo".into()),
            message: format!("Session restored to {version}."),
            variant: ToastVariant::Success,
            duration_ms: 4000,
        });
    }

    /// Rewind the ACP pane `pane` to checkpoint `version` (`v1` = the pane's
    /// first prompt): the identical rewind the Message Actions Revert runs —
    /// the transcript is truncated before that prompt, the prompt comes back
    /// for editing, and the agent session is abandoned so the next turn opens
    /// a fresh one primed with the kept prefix.
    pub(in crate::app) fn restore_pane_checkpoint(&mut self, pane: usize, version: &str) {
        let Some(timeline) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .map(|p| p.activity.timeline.clone())
        else {
            return;
        };
        // Resolve by POSITION, not by a stale index: the label names the
        // prompt's ordinal in the transcript, so a turn that streamed while
        // the box was open cannot shift the target.
        let Some(index) = checkpoint_index(&timeline, version) else {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "That checkpoint is no longer in the pane's history.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        // No success toast: the pane is its own receipt (the transcript
        // shrinks, the prompt comes back). Only failures toast.
        self.rewind_pane_timeline(pane, index, "Undo");
    }
}

/// The pane transcript's checkpoints: one per USER prompt, oldest first, as
/// `(undo_label, prompt_preview)` pairs — the shape the `/undo` box takes.
/// `v{n}` counts the prompts from the start of the pane's conversation.
fn pane_checkpoints(timeline: &[SubagentTimelineEntry]) -> Vec<(String, String)> {
    let mut checkpoints = Vec::new();
    for entry in timeline {
        if let SubagentTimelineEntry::UserMessage { text } = entry {
            let label = format!("v{}", checkpoints.len() + 1);
            checkpoints.push((label, super::super::dialog_preview_text(text)));
        }
    }
    checkpoints
}

/// The transcript index of the `v{n}` checkpoint — the n-th USER prompt —
/// or `None` when the pane no longer carries it.
fn checkpoint_index(timeline: &[SubagentTimelineEntry], version: &str) -> Option<usize> {
    let wanted: usize = version.strip_prefix('v')?.parse().ok()?;
    let mut seen = 0;
    for (index, entry) in timeline.iter().enumerate() {
        if matches!(entry, SubagentTimelineEntry::UserMessage { .. }) {
            seen += 1;
            if seen == wanted {
                return Some(index);
            }
        }
    }
    None
}
