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

        // Palette fingerprints (color chips) per theme, always from the
        // pristine registry entry — NOT from `self.theme`, which carries the
        // /background override and would leak the toggle into the preview.
        let swatches = themes
            .iter()
            .map(|name| {
                self.theme_registry
                    .get(name)
                    .map(crate::ui::theme_swatches::theme_swatches)
                    .unwrap_or_default()
            })
            .collect();

        // Resolve the current theme by comparing against each registry entry
        // WITH the /background preference applied — self.theme carries the
        // transparent-background override, so a plain equality check would
        // miss while the toggle is on.
        let current = self
            .theme_registry
            .names()
            .into_iter()
            .find(|&name| {
                self.theme_registry
                    .get(name)
                    .is_some_and(|t| self.themed(t) == self.theme)
            })
            .map_or_else(|| "opencode".to_string(), str::to_string);
        // Store the theme name so we can restore on cancel
        self.theme_dialog_original = Some(current.clone());

        self.dialog.replace(DialogType::ThemeList {
            themes,
            swatches,
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
                    if let Some(t) = self.theme_registry.get(&name).cloned() {
                        self.set_theme(&t);
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
                    && let Some(t) = self.theme_registry.get(orig).cloned()
                {
                    self.set_theme(&t);
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
                    // The shared list-dialog filter edit: pop + selection
                    // reset + cursor blink, one definition for every list box.
                    crate::component::list_dialog::pop_filter(
                        filter,
                        &mut d.selected,
                        &mut d.cursor,
                    );
                    filter.is_empty()
                };
                if is_empty {
                    // Restore original theme when filter becomes empty (matches opencode)
                    if let Some(ref orig) = self.theme_dialog_original
                        && let Some(t) = self.theme_registry.get(orig).cloned()
                    {
                        self.set_theme(&t);
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
            && let Some(t) = self.theme_registry.get(&filtered[sel]).cloned()
        {
            self.set_theme(&t);
        }
    }

    /// Add a character to the theme filter and reset selection
    pub(in crate::app) fn theme_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ThemeList { filter, .. } = &mut d.dialog_type {
                // The shared list-dialog filter edit: push + selection reset
                // + cursor blink, one definition for every cosh list box.
                crate::component::list_dialog::push_filter(
                    filter,
                    &mut d.selected,
                    &mut d.cursor,
                    ch,
                );
            } else {
                d.selected = 0;
                d.cursor.note_activity();
            }
        }
        self.apply_filtered_theme_preview();
    }
}
