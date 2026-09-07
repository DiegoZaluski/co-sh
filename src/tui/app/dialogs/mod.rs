mod confirm;
mod message_actions;
mod model;
mod provider_key;
mod queue_actions;
mod text_input;
mod theme;
mod tool_call;
mod undo;

use super::super::App;

use crate::ui::dialogs::DialogType;

impl App {
    pub(in crate::app) fn is_theme_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ThemeList { .. })
            )
    }

    pub(in crate::app) fn is_shortcuts_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::Shortcuts { .. })
            )
    }
}
