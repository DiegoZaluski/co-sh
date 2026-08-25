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
            _ => false,
        }
    }
}
