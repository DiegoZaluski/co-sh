use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

impl App {
    /// Open the `/spinner` picker: which agent-loop spinner animation the
    /// prompt row plays. Purely decorative — the pick only swaps what the
    /// user watches while the loop works.
    pub(in crate::app) fn open_spinner_dialog(&mut self) {
        let current = self.setup.appearance.spinner.id().to_string();
        // Preselect the active style's row.
        let selected = cosh::setup::SpinnerStyle::ALL
            .iter()
            .position(|s| s.id() == current)
            .unwrap_or(0);
        self.dialog
            .replace(DialogType::SpinnerList { current });
        if let Some(d) = self.dialog.current_mut() {
            d.selected = selected;
        }
    }

    pub(in crate::app) fn is_spinner_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::SpinnerList { .. }))
    }

    pub(in crate::app) fn handle_spinner_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_spinner_dialog_visible() {
            return false;
        }
        let style_count = cosh::setup::SpinnerStyle::ALL.len();
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 {
                        style_count - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % style_count;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self
                    .dialog
                    .current()
                    .map_or(0, |d| d.selected.min(style_count - 1));
                let style = cosh::setup::SpinnerStyle::ALL[selected];
                // The style drives the NEXT spinner construction; the live
                // one (if any) keeps animating until the loop ends.
                self.setup.appearance.spinner = style;
                self.setup.save();
                self.toast_style_changed(style);
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

    /// Toast feedback after a `/spinner` pick (the same surface /bell and
    /// /anim report through).
    fn toast_style_changed(&mut self, style: cosh::setup::SpinnerStyle) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some("Spinner".into()),
            message: format!("Spinner style: {}.", style.label()),
            variant: ToastVariant::Info,
            duration_ms: 3000,
        });
    }
}
