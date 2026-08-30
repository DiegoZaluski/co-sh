use super::super::App;
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
                self.pending_delete_session_id = None;
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
            if let Some(session_id) = self.pending_delete_session_id.take() {
                self.state.remove_session(&session_id);
                self.session_store.delete_session(&session_id);
                self.dialog.pop();
            } else if self.handle_rag_confirm_delete() {
                // handled (pops the dialog itself)
            } else {
                self.should_quit = true;
            }
        } else {
            self.pending_delete_session_id = None;
            self.clear_rag_pending_state();
            self.dialog.pop();
        }
    }
}
