use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

use crate::ui::toast::{ToastOptions, ToastVariant};

impl App {
    pub(in crate::app) fn is_undo_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::UndoList { .. }))
    }

    /// `/undo`: open the version list for the CURRENT session. No versions on
    /// tmp (nothing was reverted yet, or the machine rebooted) → informative
    /// toast instead of an empty list.
    pub(in crate::app) fn open_undo_dialog(&mut self) {
        let Some(session_id) = self.state.current_session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let versions = self.undo_store.versions(&session_id);
        if versions.is_empty() {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: "No snapshots yet — they are created on each revert.".into(),
                variant: ToastVariant::Info,
                duration_ms: 4000,
            });
            return;
        }
        self.dialog.show(DialogType::UndoList {
            session_id,
            versions,
        });
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
                let (session_id, selected) = match self.dialog.current() {
                    Some(d) => match &d.dialog_type {
                        DialogType::UndoList {
                            session_id,
                            versions,
                        } => (
                            session_id.clone(),
                            d.selected.min(versions.len().saturating_sub(1)),
                        ),
                        _ => return false,
                    },
                    None => return false,
                };
                self.dialog.pop();
                self.restore_undo_version(&session_id, &versions[selected]);
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Roll the session files back to a snapshot: copy the stored JSONL +
    /// `.ctx` over the live ones, drop the session from the cache so the
    /// next access reloads from disk, and refresh the sidebar summaries.
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
        let (jsonl, ctx) = self.session_store.session_paths(session_id);
        if !self.undo_store.restore(session_id, version, &jsonl, &ctx) {
            self.toast_state.show(ToastOptions {
                title: Some("Undo".into()),
                message: format!("Could not restore {version}."),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        // The cached session (and its ctx_ids mapping) is stale now — force a
        // reload from the restored files, and jump the view to the bottom of
        // the restored transcript.
        self.state.session_cache.pop(session_id);
        self.state
            .ensure_session_cached(session_id, &self.session_store);
        self.state.session_summaries = self.session_store.list_sessions();
        self.session_view.scroll_to_bottom();
        // Re-persist the restored state through the FIFO writer thread: any
        // queued-but-unexecuted save job for this session lands BEFORE this
        // re-save, so the queue's final word is the restored content (the
        // direct copy above is only the fast path).
        if let Some(mut restored) = self.state.session_cache.get_mut(session_id).cloned() {
            match self.session_store.load_context(session_id) {
                Some(context) => {
                    self.session_store
                        .save_session_async_with_context(&mut restored, context);
                }
                // The restored snapshot had NO `.ctx` (display-only era):
                // re-save the transcript anyway so a queued-but-unexecuted
                // job still lands before the restored content.
                None => self.session_store.save_session_async(&restored),
            }
        }
        self.toast_state.show(ToastOptions {
            title: Some("Undo".into()),
            message: format!("Session restored to {version}."),
            variant: ToastVariant::Success,
            duration_ms: 4000,
        });
    }
}
