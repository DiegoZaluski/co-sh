use super::super::{App, PendingSessionDelete};
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

impl App {
    pub(in crate::app) fn is_confirm_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::Confirm { .. })
            )
    }

    pub(in crate::app) fn handle_confirm_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_confirm_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Left | KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected ^= 1;
                }
                true
            }
            KeyCode::Enter => {
                self.resolve_confirm_dialog();
                true
            }
            KeyCode::Esc => {
                // A pending agents_worktree confirmation Esc-dismisses as a
                // DECLINE: the asking loop still gets its outcome delivery
                // (the pane keeps its binding), the drain resumes later.
                if self.pending_worktree_confirmation.is_some() {
                    self.resolve_pending_worktree_confirmation(false);
                    return true;
                }
                self.pending_delete = None;
                self.clear_rag_pending_state();
                self.dialog.pop();
                true
            }
            // Sovereign modal: every other key is swallowed while the user
            // is deciding — nothing else in the TUI may react to it.
            _ => true,
        }
    }

    /// Apply the currently selected Confirm option: "Yes" resolves the
    /// pending destructive action (session delete, RAG delete, quit);
    /// "No" cancels. Mirrors the keymap's generic Confirm path.
    fn resolve_confirm_dialog(&mut self) {
        let selected = self.dialog.current().map(|d| d.selected).unwrap_or(1);
        if selected == 0 {
            // A pending agents_worktree confirmation RESOLVES FIRST —
            // while it is parked, this dialog belongs to it (the ask is
            // the agent's, the answer is the user's).
            if self.pending_worktree_confirmation.is_some() {
                self.resolve_pending_worktree_confirmation(true);
                return;
            }
            // A Confirm stacked over the provider-key picker means "forget
            // the API key" — it resolves (and pops) itself.
            if self.resolve_forget_key_confirmation() {
                return;
            }
            if let Some(pending) = self.pending_delete.take() {
                match pending {
                    PendingSessionDelete::One(session_id) => {
                        self.delete_session(&session_id);
                        self.dialog.pop();
                    }
                    PendingSessionDelete::AllOfCwd => {
                        self.delete_all_sessions();
                        self.dialog.pop();
                    }
                }
            } else if self.handle_rag_confirm_delete() {
                // handled (pops the dialog itself)
            } else {
                self.should_quit = true;
            }
        } else {
            // "No" on a worktree confirmation is a DECLINE answer, not a
            // dismissal: the op resolves with a refusal outcome (the pane
            // keeps its binding) so the asking loop is never left waiting.
            if self.pending_worktree_confirmation.is_some() {
                self.resolve_pending_worktree_confirmation(false);
                return;
            }
            self.pending_delete = None;
            self.clear_rag_pending_state();
            self.dialog.pop();
        }
    }
}
