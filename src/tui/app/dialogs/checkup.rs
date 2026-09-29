use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

/// The three named checkpoint kinds the picker offers, in `CheckupModel`'s
/// `ModelKind::name()` order. A `Custom` checkpoint matches none of these,
/// so no row carries the current indicator.
const CHECKUP_MODEL_OPTIONS: [&str; 3] = ["english", "multilingual", "typed-decisions"];

impl App {
    /// Open the checkup-model picker (Settings → Checkup model).
    pub(in crate::app) fn open_checkup_model_dialog(&mut self) {
        let current = self.setup.decision.model.kind_name().to_string();
        self.dialog.show(DialogType::CheckupModelList {
            current: current.clone(),
        });
        // Preselect the current kind (custom checkpoints select none).
        if let Some(index) = CHECKUP_MODEL_OPTIONS
            .iter()
            .position(|name| *name == current)
            && let Some(d) = self.dialog.current_mut()
        {
            d.selected = index;
        }
    }

    pub(in crate::app) fn is_checkup_model_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::CheckupModelList { .. }))
    }

    pub(in crate::app) fn handle_checkup_model_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_checkup_model_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 {
                        CHECKUP_MODEL_OPTIONS.len() - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % CHECKUP_MODEL_OPTIONS.len();
                }
                true
            }
            KeyCode::Enter => {
                let selected = self
                    .dialog
                    .current()
                    .map_or(0, |d| d.selected.min(CHECKUP_MODEL_OPTIONS.len() - 1));
                if let Some(kind) = CHECKUP_MODEL_OPTIONS.get(selected) {
                    self.setup.decision.model = match *kind {
                        "multilingual" => crate::util::setup::DecisionModel::Multilingual,
                        "typed-decisions" => crate::util::setup::DecisionModel::TypedDecisions,
                        _ => crate::util::setup::DecisionModel::English,
                    };
                    self.setup.save();
                }
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

    /// Open the checkup min-confidence input box (Settings → Checkup min
    /// confidence), prefilled with the configured floor (blank = default).
    pub(in crate::app) fn open_checkup_min_confidence_input(&mut self) {
        let floor = self.setup.decision.termination.min_confidence;
        let input = if (floor - 0.6).abs() < f64::EPSILON {
            String::new()
        } else {
            format!("{floor}")
        };
        self.dialog.show(DialogType::CheckupMinConfidenceInput {
            cursor_pos: input.len(),
            input,
        });
    }
}
