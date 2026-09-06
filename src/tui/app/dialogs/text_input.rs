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
                        | DialogType::CacheTtlInput { .. }
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
                    | DialogType::CacheTtlInput { cursor_pos, .. }
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
                    | DialogType::CacheTtlInput {
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
                    | DialogType::CacheTtlInput { cursor_pos, .. }
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
                    | DialogType::CacheTtlInput {
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
                    | DialogType::CacheTtlInput {
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
                    | DialogType::CacheTtlInput {
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
                    | DialogType::CacheTtlInput {
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

    /// Editing keys shared by the multi-field registration forms (hook and
    /// MCP panels): the active line behaves like a single-line input,
    /// Up/Down move between visual lines first and only leave the field at
    /// its first/last line, Enter saves (invalid input keeps the panel open
    /// with an error toast), Esc discards. Returns whether the key was
    /// consumed; with no form open every key is refused.
    pub(in crate::app) fn handle_registration_form_key(&mut self, key: KeyEvent) -> bool {
        if !self.dialog.visible() {
            return false;
        }
        let is_form = matches!(
            self.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { .. }) | Some(DialogType::McpForm { .. })
        );
        if !is_form {
            return false;
        }
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }
        match key.code {
            KeyCode::Enter => {
                if self.save_registration_form() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => {
                let cols = crate::ui::dialogs::form_panel_content_w(
                    crate::ui::dialogs::form_panel_dialog_w(self.terminal_size()),
                ) as usize;
                if let Some(d) = self.dialog.current_mut() {
                    match &mut d.dialog_type {
                        DialogType::HookInput {
                            name,
                            matcher,
                            command,
                            timeout,
                            field,
                            cursor_pos,
                            ..
                        } => drive_form_edit(
                            [name, matcher, command, timeout],
                            field,
                            cursor_pos,
                            key,
                            cols,
                        ),
                        DialogType::McpForm {
                            name,
                            endpoint,
                            timeout,
                            field,
                            cursor_pos,
                        } => {
                            drive_form_edit([name, endpoint, timeout], field, cursor_pos, key, cols)
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Validate and persist whichever registration form is open.
    fn save_registration_form(&mut self) -> bool {
        let is_hook = matches!(
            self.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { .. })
        );
        let is_mcp = matches!(
            self.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::McpForm { .. })
        );
        if is_hook {
            self.save_hook_input_dialog()
        } else if is_mcp {
            self.save_mcp_form_dialog()
        } else {
            false
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

    /// Open the cache-duration input box for `setting` (a Settings-item id),
    /// prefilled with the currently configured duration (blank = default).
    pub(in crate::app) fn open_cache_ttl_input(&mut self, setting: &'static str) {
        let minutes = match setting {
            "anthropic_cache_ttl" => self.setup.cache.anthropic_ttl_min,
            _ => self.setup.cache.openai_retention_min,
        };
        let current = crate::util::setup::format_cache_duration(minutes);
        let input = if current == "default" {
            String::new()
        } else {
            current
        };
        self.dialog.show(DialogType::CacheTtlInput {
            setting,
            cursor_pos: input.len(),
            input,
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
            DialogType::CacheTtlInput { setting, input, .. } => {
                match crate::util::setup::parse_cache_duration(input) {
                    Ok(minutes) => {
                        let minutes = minutes.unwrap_or(0);
                        match *setting {
                            "anthropic_cache_ttl" => {
                                self.setup.cache.anthropic_ttl_min = minutes;
                            }
                            _ => {
                                self.setup.cache.openai_retention_min = minutes;
                            }
                        }
                        self.setup.save();
                        true
                    }
                    Err(message) => {
                        use crate::ui::toast::{ToastOptions, ToastVariant};
                        self.toast_state.show(ToastOptions {
                            title: Some("Duration not saved".into()),
                            message,
                            variant: ToastVariant::Error,
                            duration_ms: 6000,
                        });
                        false
                    }
                }
            }
            DialogType::HookInput { .. } => self.save_hook_input_dialog(),
            DialogType::McpForm { .. } => self.save_mcp_form_dialog(),
            _ => false,
        }
    }

    /// Error toast shared by the registration-form saves.
    fn toast_form_error(&mut self, title: &str, message: String) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some(title.into()),
            message,
            variant: ToastVariant::Error,
            duration_ms: 6000,
        });
    }

    /// Success toast shared by the registration-form saves.
    fn toast_form_success(&mut self, title: &str, message: String) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some(title.into()),
            message,
            variant: ToastVariant::Success,
            duration_ms: 4000,
        });
    }

    /// Validate and persist the MCP registration form. `build_mcp_entry`
    /// owns every rule (endpoint shape, timeout range, entry validation),
    /// so the form only adds the duplicate-name check. Invalid input keeps
    /// the panel open with an error toast.
    pub(in crate::app) fn save_mcp_form_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        let DialogType::McpForm {
            name,
            endpoint,
            timeout,
            ..
        } = &d.dialog_type
        else {
            return false;
        };
        let entry = match cosh::mcp::build_mcp_entry(name, endpoint, timeout) {
            Ok(entry) => entry,
            Err(message) => {
                self.toast_form_error("Server not saved", message);
                return false;
            }
        };
        if self.setup.mcp.servers.iter().any(|s| s.name == entry.name) {
            self.toast_form_error(
                "Server not saved",
                format!("A server named “{}” is already registered.", entry.name),
            );
            return false;
        }
        let saved = entry.name.clone();
        self.setup.mcp.servers.push(entry);
        self.setup.save();
        self.toast_form_success(
            "MCP server added",
            format!("“{saved}” connects on the next agent loop."),
        );
        true
    }

    /// Open the MCP registration form: all three fields on one panel, blank
    /// for a new server.
    pub(in crate::app) fn open_mcp_form(&mut self) {
        self.dialog.show(DialogType::McpForm {
            name: String::new(),
            endpoint: String::new(),
            timeout: String::new(),
            field: 0,
            cursor_pos: 0,
        });
    }
}

/// One editing step of a registration form's active line. `lines` are the
/// field texts in field order, `field`/`cursor_pos` the active position
/// (`cursor_pos` is a byte index into the active line). The last field is
/// always the plain-number timeout, so Ctrl word jumps apply to every field
/// but the last. Up/Down first move between the value's wrapped visual lines
/// and only change fields at the first/last line. Returns whether the key
/// was consumed.
fn drive_form_edit<const N: usize>(
    lines: [&mut String; N],
    field: &mut usize,
    cursor_pos: &mut usize,
    key: KeyEvent,
    cols: usize,
) -> bool {
    debug_assert!(N > 0, "a registration form always has fields");
    let last = N - 1;
    let word_ops = *field != last;
    match key.code {
        KeyCode::Up | KeyCode::Down => {
            let up = key.code == KeyCode::Up;
            // Inside a wrapped value: move between visual lines and
            // only leave the field at its first/last line.
            let moved = {
                let target = &mut *lines[*field];
                crate::util::word_ops::move_visual_line(target, *cursor_pos, cols, up)
            };
            if moved != *cursor_pos {
                *cursor_pos = moved;
            } else {
                *field = if !up {
                    (*field + 1).min(last)
                } else {
                    field.saturating_sub(1)
                };
                *cursor_pos = lines[*field].len();
            }
            true
        }
        KeyCode::Left => {
            let target = &mut *lines[*field];
            if key.modifiers.contains(KeyModifiers::CONTROL) && word_ops {
                *cursor_pos = crate::util::word_ops::find_word_start(target, *cursor_pos);
            } else if *cursor_pos > 0 {
                *cursor_pos = target.floor_char_boundary(*cursor_pos - 1);
            }
            true
        }
        KeyCode::Right => {
            let target = &mut *lines[*field];
            if key.modifiers.contains(KeyModifiers::CONTROL) && word_ops {
                *cursor_pos = crate::util::word_ops::find_word_end(target, *cursor_pos);
            } else if *cursor_pos < target.len() {
                let next = target.floor_char_boundary(*cursor_pos + 1).min(target.len());
                *cursor_pos = next;
            }
            true
        }
        KeyCode::Home => {
            *cursor_pos = 0;
            true
        }
        KeyCode::End => {
            *cursor_pos = lines[*field].len();
            true
        }
        KeyCode::Delete => {
            let target = &mut *lines[*field];
            if *cursor_pos < target.len() {
                let next = target
                    .floor_char_boundary(*cursor_pos + 1)
                    .min(target.len());
                target.drain(*cursor_pos..next);
            }
            true
        }
        KeyCode::Backspace => {
            let target = &mut *lines[*field];
            if *cursor_pos > 0 {
                if key.modifiers.contains(KeyModifiers::CONTROL) && word_ops {
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
            let target = &mut *lines[*field];
            target.insert(*cursor_pos, ch);
            *cursor_pos += ch.len_utf8();
            true
        }
        _ => false,
    }
}
