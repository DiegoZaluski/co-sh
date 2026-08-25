use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

impl App {
    /// Open the tool-call mode picker (`native` | `inline`).
    pub(in crate::app) fn open_tool_call_dialog(&mut self) {
        let current = match self.llm_config.tool_call_mode {
            cosh_sdk::connector::ToolCallMode::Native => "native",
            cosh_sdk::connector::ToolCallMode::Inline => "inline",
        };
        self.dialog.replace(DialogType::ToolCallList {
            current: current.to_string(),
        });
        // Preselect the current mode (index 0 = native, 1 = inline).
        if let Some(d) = self.dialog.current_mut() {
            d.selected = if current == "inline" { 1 } else { 0 };
        }
    }

    pub(in crate::app) fn is_tool_call_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::ToolCallList { .. }))
    }

    pub(in crate::app) fn handle_tool_call_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_tool_call_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 { 1 } else { 0 };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % 2;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self.dialog.current().map_or(0, |d| d.selected.min(1));
                self.llm_config.tool_call_mode = if selected == 1 {
                    cosh_sdk::connector::ToolCallMode::Inline
                } else {
                    cosh_sdk::connector::ToolCallMode::Native
                };
                crate::routes::tools::save_tool_call_mode(
                    &mut self.setup,
                    self.llm_config.tool_call_mode,
                );
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }
}
