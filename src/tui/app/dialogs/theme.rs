use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;

impl App {
    pub(in crate::app) fn open_theme_dialog(&mut self) {
        let mut themes: Vec<String> = self
            .theme_registry
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        themes.sort_by_key(|a| a.to_lowercase());

        let current = self
            .theme_registry
            .names()
            .iter()
            .find(|&&name| self.theme_registry.get(name) == Some(&self.theme))
            .map_or_else(|| "opencode".to_string(), |&s| s.to_string());

        // Store the theme name so we can restore on cancel
        self.theme_dialog_original = Some(current.clone());

        self.dialog.replace(DialogType::ThemeList {
            themes,
            current,
            filter: String::new(),
        });
    }

    pub(in crate::app) fn handle_theme_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_theme_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Compute filtered indices and move selection up
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty()
                    && let Some(d) = self.dialog.current_mut()
                {
                    d.selected = if d.selected == 0 {
                        filtered.len() - 1
                    } else {
                        d.selected.saturating_sub(1)
                    };
                    // Preview theme on move
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Down => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty()
                    && let Some(d) = self.dialog.current_mut()
                {
                    d.selected = (d.selected + 1).min(filtered.len() - 1);
                    // Preview theme on move
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Enter => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty() {
                    let name = filtered[self
                        .dialog
                        .current()
                        .map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)))]
                    .clone();
                    if let Some(t) = self.theme_registry.get(&name) {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                    // Persist theme choice so it survives restarts
                    self.setup.appearance.theme = name;
                    self.setup.save();
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Restore original theme
                if let Some(ref orig) = self.theme_dialog_original
                    && let Some(t) = self.theme_registry.get(orig)
                {
                    self.theme = t.clone();
                    self.config.theme_gen += 1;
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                let is_empty = {
                    let Some(d) = self.dialog.current_mut() else {
                        return true;
                    };
                    let DialogType::ThemeList { filter, .. } = &mut d.dialog_type else {
                        return true;
                    };
                    filter.pop();
                    d.selected = 0;
                    d.cursor.note_activity();
                    filter.is_empty()
                };
                if is_empty {
                    // Restore original theme when filter becomes empty (matches opencode)
                    if let Some(ref orig) = self.theme_dialog_original
                        && let Some(t) = self.theme_registry.get(orig)
                    {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                } else {
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Char(ch) => {
                self.theme_dialog_push_filter(ch);
                true
            }
            _ => false,
        }
    }

    /// Get the list of filtered theme names from the current dialog
    pub(in crate::app) fn theme_dialog_filtered(&self) -> Vec<String> {
        self.dialog.current().map_or(Vec::new(), |d| {
            if let DialogType::ThemeList { themes, filter, .. } = &d.dialog_type {
                if filter.is_empty() {
                    themes.clone()
                } else {
                    let lower = filter.to_lowercase();
                    themes
                        .iter()
                        .filter(|t| t.to_lowercase().contains(&lower))
                        .cloned()
                        .collect()
                }
            } else {
                Vec::new()
            }
        })
    }

    /// Preview the currently selected theme from the filtered list
    pub(in crate::app) fn apply_filtered_theme_preview(&mut self) {
        let filtered = self.theme_dialog_filtered();
        let sel = self
            .dialog
            .current()
            .map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)));
        if sel < filtered.len()
            && let Some(t) = self.theme_registry.get(&filtered[sel])
        {
            self.theme = t.clone();
            self.config.theme_gen += 1;
        }
    }

    /// Add a character to the theme filter and reset selection
    pub(in crate::app) fn theme_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ThemeList { filter, .. } = &mut d.dialog_type {
                filter.push(ch);
            }
            d.selected = 0;
            d.cursor.note_activity();
        }
        self.apply_filtered_theme_preview();
    }
}
