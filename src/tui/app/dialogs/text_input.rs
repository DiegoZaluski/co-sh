use super::super::App;
use super::super::providers::{is_valid_local_url, save_provider_api_key};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::dialogs::DialogType;

impl App {
    pub(in crate::app) fn is_text_input_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(
                    DialogType::ApiKeyInput { .. }
                        | DialogType::LocalUrlInput { .. }
                        | DialogType::RenameSession { .. },
                )
            )
    }

    pub(in crate::app) fn handle_text_input_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.dialog.visible() {
            return false;
        }
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }

        match key {
            KeyCode::Enter => {
                if self.save_text_input_dialog() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Left => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput { cursor_pos, .. }
                    | DialogType::LocalUrlInput { cursor_pos, .. }
                    | DialogType::RenameSession { cursor_pos, .. } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    *cursor_pos -= 1;
                }
                true
            }
            KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos < input.len()
                {
                    *cursor_pos += 1;
                }
                true
            }
            KeyCode::Home => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput { cursor_pos, .. }
                    | DialogType::LocalUrlInput { cursor_pos, .. }
                    | DialogType::RenameSession { cursor_pos, .. } = &mut d.dialog_type
                {
                    *cursor_pos = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                {
                    *cursor_pos = input.len();
                }
                true
            }
            KeyCode::Delete => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos < input.len()
                {
                    let next = input.floor_char_boundary(*cursor_pos + 1).min(input.len());
                    input.drain(*cursor_pos..next);
                }
                true
            }
            KeyCode::Backspace => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    let char_start = input.floor_char_boundary(*cursor_pos - 1);
                    input.remove(char_start);
                    *cursor_pos = char_start;
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                {
                    input.insert(*cursor_pos, ch);
                    *cursor_pos += ch.len_utf8();
                }
                true
            }
            _ => false,
        }
    }

    /// Editing keys for the hook registration box: the active field behaves
    /// exactly like a single-line input; Up/Down move between fields.
    pub(in crate::app) fn handle_hook_input_key(&mut self, key: KeyEvent) -> bool {
        if !self.dialog.visible() {
            return false;
        }
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }

        const LAST_FIELD: usize = 3;

        match key.code {
            KeyCode::Enter => {
                if self.save_hook_input_dialog() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Up | KeyCode::Down => {
                let up = key.code == KeyCode::Up;
                let cols = crate::ui::dialogs::hook_input_content_w(
                    crate::ui::dialogs::hook_input_dialog_w(self.terminal_size()),
                ) as usize;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let lens = [name.len(), matcher.len(), command.len(), timeout.len()];
                    // Inside a wrapped value: move between visual lines and
                    // only leave the field at its first/last line.
                    let moved = {
                        let target = match *field {
                            0 => name,
                            1 => matcher,
                            2 => command,
                            _ => timeout,
                        };
                        crate::util::word_ops::move_visual_line(target, *cursor_pos, cols, up)
                    };
                    if moved != *cursor_pos {
                        *cursor_pos = moved;
                    } else {
                        *field = if !up {
                            (*field + 1).min(LAST_FIELD)
                        } else {
                            field.saturating_sub(1)
                        };
                        *cursor_pos = lens[*field];
                    }
                }
                true
            }
            KeyCode::Left => {
                let word_jump =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Left;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if word_jump && *field != 3 {
                        // Timeout is a plain number: words make no sense.
                        *cursor_pos = crate::util::word_ops::find_word_start(target, *cursor_pos);
                    } else if *cursor_pos > 0 {
                        *cursor_pos = target.floor_char_boundary(*cursor_pos - 1);
                    }
                }
                true
            }
            KeyCode::Right => {
                let word_jump =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Right;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let len = match *field {
                        0 => name.len(),
                        1 => matcher.len(),
                        2 => command.len(),
                        _ => timeout.len(),
                    };
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if word_jump && *field != 3 {
                        *cursor_pos = crate::util::word_ops::find_word_end(target, *cursor_pos);
                    } else if *cursor_pos < len {
                        let next = target.floor_char_boundary(*cursor_pos + 1).min(len);
                        *cursor_pos = next;
                    }
                }
                true
            }
            KeyCode::Home => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput { cursor_pos, .. } = &mut d.dialog_type
                {
                    *cursor_pos = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    *cursor_pos = match *field {
                        0 => name.len(),
                        1 => matcher.len(),
                        2 => command.len(),
                        _ => timeout.len(),
                    };
                }
                true
            }
            KeyCode::Delete => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    let len = target.len();
                    if *cursor_pos < len {
                        let next = target.floor_char_boundary(*cursor_pos + 1).min(len);
                        target.drain(*cursor_pos..next);
                    }
                }
                true
            }
            KeyCode::Backspace => {
                let delete_word =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Backspace;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if delete_word && *field != 3 {
                        let start = crate::util::word_ops::find_word_start(target, *cursor_pos);
                        target.drain(start..*cursor_pos);
                        *cursor_pos = start;
                    } else {
                        let char_start = target.floor_char_boundary(*cursor_pos - 1);
                        target.remove(char_start);
                        *cursor_pos = char_start;
                    }
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    target.insert(*cursor_pos, ch);
                    *cursor_pos += ch.len_utf8();
                }
                true
            }
            _ => false,
        }
    }

    /// Validate and persist the hook registration box. Invalid input keeps
    /// the dialog open with an error toast (same contract as the local URL).
    pub(in crate::app) fn save_hook_input_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        let DialogType::HookInput {
            event,
            editing_index,
            name,
            matcher,
            command,
            timeout,
            ..
        } = &d.dialog_type
        else {
            return false;
        };

        match crate::routes::settings::validate_hook(name, matcher, command, timeout) {
            Ok(entry) => {
                let list = self
                    .setup
                    .hooks
                    .events
                    .entry(event.to_string())
                    .or_default();
                match editing_index {
                    Some(i) if *i < list.len() => list[*i] = entry,
                    _ => list.push(entry),
                }
                self.setup.save();
                true
            }
            Err(message) => {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("Hook not saved".into()),
                    message,
                    variant: ToastVariant::Error,
                    duration_ms: 6000,
                });
                false
            }
        }
    }

    /// Open the hook registration box for `event`: blank for creation,
    /// prefilled with the current values when editing the hook at `index`.
    pub(in crate::app) fn open_hook_form(&mut self, event: &'static str, index: Option<usize>) {
        let existing = index.and_then(|i| {
            crate::routes::settings::hook_entries(&self.setup, event)
                .get(i)
                .cloned()
        });
        let (name, matcher, command, timeout) = match &existing {
            Some(entry) => (
                entry.name.clone(),
                entry.matcher.clone(),
                entry.command.clone(),
                entry.timeout.map(|t| t.to_string()).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        self.dialog.show(DialogType::HookInput {
            event,
            editing_index: index.filter(|_| existing.is_some()),
            name,
            matcher,
            command,
            timeout,
            field: 0,
            cursor_pos: 0,
        });
    }

    /// Perform the save for the current text input dialog (API key → keyring,
    /// local URL → setup.json). Returns `true` when the input was accepted.
    pub(in crate::app) fn save_text_input_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        match &d.dialog_type {
            DialogType::RenameSession { input, .. } => {
                // An empty/whitespace title just closes the dialog without
                // touching the session (opencode behavior: cancel, not clear).
                let trimmed = input.trim();
                if !trimmed.is_empty()
                    && let Some(id) = self.state.current_session_id.clone()
                {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    if let Some(session) = self.state.session_cache.get_mut(&id) {
                        session.title = trimmed.to_string();
                        session.title_generated = true;
                    }
                    // Update the sidebar summary.
                    if let Some(summary) = self
                        .state
                        .session_summaries
                        .iter_mut()
                        .find(|s| s.session_id == id)
                    {
                        summary.title = trimmed.to_string();
                        summary.title_generated = true;
                    }
                    self.session_store.update_title(&id, trimmed);
                    self.toast_state.show(ToastOptions {
                        title: Some("Renamed".into()),
                        message: format!("Session renamed to “{trimmed}”."),
                        variant: ToastVariant::Success,
                        duration_ms: 3000,
                    });
                }
                true
            }
            DialogType::ApiKeyInput {
                provider,
                env_var,
                input,
                ..
            } if !input.is_empty() => {
                if let Err(e) = save_provider_api_key(env_var, input) {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("Key not saved".to_string()),
                        message: format!("Failed to store the API key in the OS keyring: {e}"),
                        variant: ToastVariant::Error,
                        duration_ms: 6000,
                    });
                } else {
                    cosh_sdk::connector::invalidate_api_key(env_var);
                }
                // Invalidate model cache for this provider so the next dialog
                // open fetches fresh models with the new key.
                self.model_cache.invalidate(provider);
                true
            }
            DialogType::LocalUrlInput {
                provider, input, ..
            } if !input.is_empty() => {
                let trimmed = input.trim();
                if !is_valid_local_url(trimmed) {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("URL not saved".to_string()),
                        message: format!(
                            "Invalid server URL: \"{trimmed}\". Expected http://host:port"
                        ),
                        variant: ToastVariant::Error,
                        duration_ms: 6000,
                    });
                    false
                } else {
                    self.setup.set_local_base_url(provider, trimmed);
                    self.model_cache.invalidate(provider);
                    true
                }
            }
            DialogType::HookInput { .. } => self.save_hook_input_dialog(),
            _ => false,
        }
    }
}
