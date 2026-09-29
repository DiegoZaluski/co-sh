mod checkup;
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

    /// Is a modal box (a popup the user must dismiss) currently on
    /// screen? ESC belongs to the BOX first — its own handler closes it
    /// and the agent loop keeps running — and only the SECOND Esc, with
    /// no box open anymore, interrupts the loop. Covered here: the
    /// dialog stack (theme/model/reasoning pickers, message and queue
    /// actions, text inputs, registration forms, alerts, shortcuts,
    /// confirm), the slash menu, the queue-choice and free-gateway
    /// dialogs, and the router's ACP picker (a stub today — the term
    /// stays so the picker is protected the day it exists). The question
    /// and permission dialogs are deliberately EXCLUDED: their Esc is a
    /// semantic REJECT of a pending tool call / permission request (an
    /// answer is sent back to the model), not a mere close.
    pub(in crate::app) fn modal_box_open(&self) -> bool {
        self.dialog.visible()
            || self.slash_menu.visible
            || self.queue_choice_dialog.visible
            || self.free_gateway_dialog.visible
            || self.router_view.acp_picker_open()
    }
}
