use super::super::App;
use super::super::providers::{is_valid_local_url, save_provider_api_key};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::ui::dialogs::DialogType;
use crate::util::field_selection::DragSelection;

impl App {
    pub(in crate::app) fn is_text_input_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(
                    DialogType::ApiKeyInput { .. }
                        | DialogType::LocalUrlInput { .. }
                        | DialogType::CacheTtlInput { .. }
                        | DialogType::EditorInput { .. }
                        | DialogType::SkillsInput { .. }
                        | DialogType::CheckupMinConfidenceInput { .. }
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
                    | DialogType::EditorInput { cursor_pos, .. }
                    | DialogType::SkillsInput { cursor_pos, .. }
                    | DialogType::CheckupMinConfidenceInput { cursor_pos, .. }
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
                    | DialogType::EditorInput { input, cursor_pos }
                    | DialogType::SkillsInput { input, cursor_pos }
                    | DialogType::CheckupMinConfidenceInput {
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
                    | DialogType::EditorInput { cursor_pos, .. }
                    | DialogType::SkillsInput { cursor_pos, .. }
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
                    | DialogType::EditorInput { input, cursor_pos }
                    | DialogType::SkillsInput { input, cursor_pos }
                    | DialogType::CheckupMinConfidenceInput {
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
                    | DialogType::EditorInput { input, cursor_pos }
                    | DialogType::SkillsInput { input, cursor_pos }
                    | DialogType::CheckupMinConfidenceInput {
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
                    | DialogType::EditorInput { input, cursor_pos }
                    | DialogType::SkillsInput { input, cursor_pos }
                    | DialogType::CheckupMinConfidenceInput {
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
                    | DialogType::EditorInput { input, cursor_pos }
                    | DialogType::SkillsInput { input, cursor_pos }
                    | DialogType::CheckupMinConfidenceInput {
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

    /// Whether a multi-field registration form (hook or MCP panel) is
    /// the active dialog. The form owns the keyboard while open, so the
    /// key and bracketed-paste dispatch both consult this.
    pub(in crate::app) fn is_registration_form_open(&self) -> bool {
        matches!(
            self.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::HookInput { .. }) | Some(DialogType::McpForm { .. })
        )
    }

    /// Paste bracketed-paste text into the registration form's active
    /// field at the cursor. Fields are single-line, so newlines are
    /// stripped exactly like the single-line dialogs' paste handling in
    /// `process_event` and `QuestionDialog::handle_paste`. A drag
    /// selection is replaced by the pasted text.
    pub(in crate::app) fn paste_registration_form(&mut self, text: &str) {
        let cleaned: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
        if cleaned.is_empty() {
            return;
        }
        let Some(d) = self.dialog.current_mut() else {
            return;
        };
        let (line, cursor, selection, owner): (
            &mut String,
            &mut usize,
            &mut Option<DragSelection<usize>>,
            usize,
        ) = match &mut d.dialog_type {
            DialogType::HookInput {
                name,
                matcher,
                command,
                timeout,
                field,
                cursor_pos,
                selection,
                ..
            } => match [name, matcher, command, timeout].into_iter().nth(*field) {
                Some(line) => (line, cursor_pos, selection, *field),
                None => return,
            },
            DialogType::McpForm {
                name,
                endpoint,
                timeout,
                api_key,
                field,
                cursor_pos,
                selection,
            } => match [name, endpoint, timeout, api_key].into_iter().nth(*field) {
                Some(line) => (line, cursor_pos, selection, *field),
                None => return,
            },
            _ => return,
        };
        if let Some((s, e)) = take_form_selection(owner, selection, line.len()) {
            line.drain(s..e);
            *cursor = s;
        }
        line.insert_str(*cursor, &cleaned);
        *cursor += cleaned.len();
        d.cursor.note_activity();
        self.sync_mcp_form_draft();
    }
    /// Copy the registration form's active field to the system clipboard
    /// through the shared selection helper (same toast contract as every
    /// other copy path). A drag selection copies its range; otherwise the
    /// whole field. Always consumes the key while a form is open, even
    /// for an empty field, which simply has nothing to copy.
    pub(in crate::app) fn copy_registration_form_field(&mut self) -> bool {
        let text = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::HookInput {
                    name,
                    matcher,
                    command,
                    timeout,
                    field,
                    selection,
                    ..
                } => [name, matcher, command, timeout]
                    .get(*field)
                    .copied()
                    .map(|line| {
                        selection
                            .as_ref()
                            .and_then(|sel| sel.slice_of(*field, line))
                            .unwrap_or_else(|| line.to_string())
                    }),
                DialogType::McpForm {
                    name,
                    endpoint,
                    timeout,
                    api_key,
                    field,
                    selection,
                    cursor_pos: _,
                } => [name, endpoint, timeout, api_key]
                    .get(*field)
                    .copied()
                    .map(|line| {
                        selection
                            .as_ref()
                            .and_then(|sel| sel.slice_of(*field, line))
                            .unwrap_or_else(|| line.to_string())
                    }),
                _ => None,
            },
            None => None,
        };
        let Some(text) = text else {
            return false;
        };
        if !text.is_empty() {
            crate::util::selection::copy_selection(&text, &mut self.toast_state);
        }
        true
    }

    /// Clear the registration form's drag selection, if any. Reports
    /// whether one was active (first-Esc semantics).
    fn clear_form_selection(&mut self) -> bool {
        let Some(d) = self.dialog.current_mut() else {
            return false;
        };
        let selection = match &mut d.dialog_type {
            DialogType::HookInput { selection, .. } => selection,
            DialogType::McpForm { selection, .. } => selection,
            _ => return false,
        };
        selection.take().is_some()
    }

    /// Anchor a drag selection on a registration-panel value row: focuses
    /// the field, parks the caret and anchors `(field, byte, byte)`,
    /// mirroring the create-db fields' press handling. Labels, padding
    /// and outside clicks return false so the existing click dispatch
    /// (focus/dismiss) runs untouched.
    pub(in crate::app) fn start_form_selection_at(&mut self, x: u16, y: u16) -> bool {
        use crate::ui::dialogs::{byte_at_char, form_panel_hit_field};
        let area = self.terminal_size();
        let hit: Option<(usize, usize)> = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::HookInput {
                    name,
                    matcher,
                    command,
                    timeout,
                    ..
                } => {
                    let values = [
                        name.as_str(),
                        matcher.as_str(),
                        command.as_str(),
                        timeout.as_str(),
                    ];
                    form_panel_hit_field(area, &values, x, y)
                        .and_then(|(f, ci)| ci.map(|c| (f, byte_at_char(values[f], c))))
                }
                DialogType::McpForm {
                    name,
                    endpoint,
                    timeout,
                    api_key,
                    ..
                } => {
                    let values = [
                        name.as_str(),
                        endpoint.as_str(),
                        timeout.as_str(),
                        api_key.as_str(),
                    ];
                    form_panel_hit_field(area, &values, x, y)
                        .and_then(|(f, ci)| ci.map(|c| (f, byte_at_char(values[f], c))))
                }
                _ => None,
            },
            None => None,
        };
        let Some((field, byte)) = hit else {
            return false;
        };
        let Some(d) = self.dialog.current_mut() else {
            return false;
        };
        let slots = match &mut d.dialog_type {
            DialogType::HookInput {
                field: active,
                cursor_pos: cursor,
                selection: sel,
                ..
            } => Some((active, cursor, sel)),
            DialogType::McpForm {
                field: active,
                cursor_pos: cursor,
                selection: sel,
                ..
            } => Some((active, cursor, sel)),
            _ => None,
        };
        let Some((active, cursor, sel)) = slots else {
            return false;
        };
        *active = field;
        *cursor = byte;
        *sel = Some(DragSelection::anchor(field, byte));
        d.cursor.note_activity();
        true
    }

    /// Extend the active drag selection to the value cell under (`x`, `y`),
    /// clamped to the selected field's ends when the drag leaves its rows
    /// (mirrors the create-db `extend_field_selection_at`). The caret
    /// follows the drag end. Returns false when no selection is active.
    pub(in crate::app) fn extend_form_selection_at(&mut self, x: u16, y: u16) -> bool {
        let area = self.terminal_size();
        let target: Option<(usize, usize)> = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::HookInput {
                    name,
                    matcher,
                    command,
                    timeout,
                    selection: Some(sel),
                    ..
                } => {
                    let values = [
                        name.as_str(),
                        matcher.as_str(),
                        command.as_str(),
                        timeout.as_str(),
                    ];
                    values.get(sel.field()).map(|_| {
                        (
                            sel.field(),
                            form_extend_byte(area, &values, sel.field(), x, y),
                        )
                    })
                }
                DialogType::McpForm {
                    name,
                    endpoint,
                    timeout,
                    api_key,
                    selection: Some(sel),
                    ..
                } => {
                    let values = [
                        name.as_str(),
                        endpoint.as_str(),
                        timeout.as_str(),
                        api_key.as_str(),
                    ];
                    values.get(sel.field()).map(|_| {
                        (
                            sel.field(),
                            form_extend_byte(area, &values, sel.field(), x, y),
                        )
                    })
                }
                _ => None,
            },
            None => None,
        };
        let Some((_, byte)) = target else {
            return false;
        };
        let Some(d) = self.dialog.current_mut() else {
            return false;
        };
        let slots = match &mut d.dialog_type {
            DialogType::HookInput {
                cursor_pos: cursor,
                selection: sel,
                ..
            } => Some((cursor, sel)),
            DialogType::McpForm {
                cursor_pos: cursor,
                selection: sel,
                ..
            } => Some((cursor, sel)),
            _ => None,
        };
        let Some((cursor, sel)) = slots else {
            return false;
        };
        *cursor = byte;
        if let Some(s) = sel {
            s.extend(byte);
        }
        d.cursor.note_activity();
        true
    }

    /// Release after a drag over a registration panel: copy the selected
    /// range through the shared selection helper and clear the highlight,
    /// mirroring the create-db release path. Returns whether a non-empty
    /// selection was consumed — plain clicks fall through to the normal
    /// click dispatch (focus/dismiss).
    pub(in crate::app) fn copy_form_selection_on_release(&mut self) -> bool {
        let text: Option<String> = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::HookInput {
                    name,
                    matcher,
                    command,
                    timeout,
                    field,
                    selection,
                    ..
                } => [name, matcher, command, timeout]
                    .get(*field)
                    .copied()
                    .and_then(|line| {
                        selection
                            .as_ref()
                            .and_then(|sel| sel.slice_of(*field, line))
                    }),
                DialogType::McpForm {
                    name,
                    endpoint,
                    timeout,
                    api_key,
                    field,
                    selection,
                    ..
                } => [name, endpoint, timeout, api_key]
                    .get(*field)
                    .copied()
                    .and_then(|line| {
                        selection
                            .as_ref()
                            .and_then(|sel| sel.slice_of(*field, line))
                    }),
                _ => None,
            },
            None => None,
        };
        let Some(text) = text else {
            return false;
        };
        crate::util::selection::copy_selection(&text, &mut self.toast_state);
        self.clear_form_selection();
        true
    }

    /// Editing keys shared by the multi-field registration forms (hook and
    /// MCP panels): the active line behaves like a single-line input,
    /// Up/Down move between visual lines first and only leave the field at
    /// its first/last line, Enter saves (invalid input keeps the panel open
    /// with an error toast), Esc drops a drag highlight first and discards
    /// on the next press. Typing, paste and deletions replace a selected
    /// range; navigation drops it. Ctrl+Left/Right jump by word and
    /// Ctrl+Backspace/Ctrl+W delete the word before the cursor, mirroring
    /// the chat prompt and the question dialog; any other Ctrl+letter is
    /// consumed but ignored so it never leaks into the field. Paste arrives
    /// through `paste_registration_form`, Ctrl+C through
    /// `copy_registration_form_field`. Returns whether the key was
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
        let consumed = match key.code {
            KeyCode::Enter => {
                if self.save_registration_form() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                // First Esc drops a drag highlight (prompt convention);
                // the next one discards the panel.
                if self.clear_form_selection() {
                    true
                } else {
                    self.dialog.pop();
                    true
                }
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
                            selection,
                            ..
                        } => drive_form_edit(
                            [name, matcher, command, timeout],
                            field,
                            cursor_pos,
                            selection,
                            key,
                            cols,
                        ),
                        DialogType::McpForm {
                            name,
                            endpoint,
                            timeout,
                            api_key,
                            field,
                            cursor_pos,
                            selection,
                        } => drive_form_edit(
                            [name, endpoint, timeout, api_key],
                            field,
                            cursor_pos,
                            selection,
                            key,
                            cols,
                        ),
                        _ => false,
                    }
                } else {
                    false
                }
            }
        };
        // Keep the accidental-close draft in sync with whatever is now on
        // the panel (a no-op unless the MCP form is still open — a save
        // or Esc has just popped it).
        self.sync_mcp_form_draft();
        consumed
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
            selection: None,
        });
    }

    /// Open the cache-duration input box for `setting` (a Settings-item id),
    /// prefilled with the currently configured duration (blank = default).
    pub(in crate::app) fn open_cache_ttl_input(&mut self, setting: &'static str) {
        let minutes = match setting {
            "anthropic_cache_ttl" => self.setup.cache.anthropic_ttl_min,
            _ => self.setup.cache.openai_retention_min,
        };
        let current = cosh::setup::format_cache_duration(minutes);
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

    /// Open the editor-command input box (Settings → Editor), prefilled
    /// with the configured command (blank = auto-detect fallback chain).
    pub(in crate::app) fn open_editor_input(&mut self) {
        self.dialog.show(DialogType::EditorInput {
            cursor_pos: self.setup.editor.len(),
            input: self.setup.editor.clone(),
        });
    }

    /// Open the skill-directories input box (Settings → Skill directories),
    /// prefilled with the configured list (blank = the `~/.skills` default).
    pub(in crate::app) fn open_skills_input(&mut self) {
        let input = self.setup.skills.dirs.join(":");
        self.dialog.show(DialogType::SkillsInput {
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
                match cosh::setup::parse_cache_duration(input) {
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
            DialogType::EditorInput { input, .. } => {
                // Any typed value is accepted verbatim (it may be a full
                // command like "vim -u NONE"); empty clears back to the
                // auto-detected fallback chain. Launch failures stay silent
                // by design, so there is nothing to validate here.
                self.setup.editor = input.trim().to_string();
                self.setup.save();
                true
            }
            DialogType::SkillsInput { input, .. } => {
                // Colon-separated skill directories; empty restores the
                // ~/.skills default (an empty `dirs` list). Missing paths
                // are skipped silently at discovery time.
                self.setup.skills.dirs = input
                    .split(':')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(String::from)
                    .collect();
                self.setup.save();
                true
            }
            DialogType::CheckupMinConfidenceInput { input, .. } => {
                // A 0–1 float; empty or "default" restores the measured
                // 0.6 floor. Out-of-range values are clamped (same clamp
                // the harness applies when consuming the floor) — any
                // other garbage keeps the dialog open with an error toast,
                // exactly like the cache-duration input.
                let trimmed = input.trim();
                let parsed = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
                    Ok(0.6)
                } else {
                    trimmed.parse::<f64>().map_err(|_| {
                        "Enter a number between 0 and 1 (e.g. 0.6), or \"default\"".to_string()
                    })
                };
                match parsed {
                    Ok(value) => {
                        self.setup.decision.termination.min_confidence = value.clamp(0.0, 1.0);
                        self.setup.save();
                        true
                    }
                    Err(message) => {
                        use crate::ui::toast::{ToastOptions, ToastVariant};
                        self.toast_state.show(ToastOptions {
                            title: Some("Confidence not saved".into()),
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
}

/// Byte offset of the drag end over a registration-panel field: the
/// clicked character inside its wrapped value rows, 0 above them and the
/// line end below them (mirrors the create-db drag clamping). `field`
/// always indexes `values` — the caller only extends the anchored field.
fn form_extend_byte(area: Rect, values: &[&str], field: usize, x: u16, y: u16) -> usize {
    use crate::ui::dialogs::{byte_at_char, form_field_geometries, form_panel_metrics};
    let (_, dialog_y, _, _, content_x, cols) = form_panel_metrics(area, values);
    let g = &form_field_geometries(dialog_y, values, cols)[field];
    let value = values[field];
    if y < g.value_y {
        0
    } else if y >= g.value_y + g.rows as u16 {
        value.len()
    } else {
        let line = (y - g.value_y) as usize;
        let col = x.saturating_sub(content_x) as usize;
        let ci = (line * cols + col).min(value.chars().count());
        byte_at_char(value, ci)
    }
}

/// Take the drag selection owned by `field`, normalised to `(start, end)`
/// bytes clamped to `len`. Editing keys consume it (typing replaces the
/// range); navigation drops it without effect. Returns `None` — leaving
/// no selection behind either way — when there is nothing to take.
fn take_form_selection(
    field: usize,
    selection: &mut Option<DragSelection<usize>>,
    len: usize,
) -> Option<(usize, usize)> {
    let range = selection.as_ref()?.range_for(field, len);
    *selection = None;
    range
}

impl App {
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
    /// so the form only adds the duplicate-name check. A directly typed
    /// API key goes to the OS keyring (`mcp:<name>`), never to setup.json;
    /// a `$VAR` reference is stored in the entry and resolved at dial time.
    /// Invalid input keeps the panel open with an error toast.
    pub(in crate::app) fn save_mcp_form_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        let DialogType::McpForm {
            name,
            endpoint,
            timeout,
            api_key,
            ..
        } = &d.dialog_type
        else {
            return false;
        };
        let draft = match cosh::mcp::build_mcp_entry(name, endpoint, timeout, api_key) {
            Ok(draft) => draft,
            Err(message) => {
                self.toast_form_error("Server not saved", message);
                return false;
            }
        };
        if self
            .setup
            .mcp
            .servers
            .iter()
            .any(|s| s.name == draft.entry.name)
        {
            self.toast_form_error(
                "Server not saved",
                format!(
                    "A server named “{}” is already registered.",
                    draft.entry.name
                ),
            );
            return false;
        }
        // Keyring FIRST: the entry is only persisted once the credential is
        // safely stored, so a failed store can never leave a server that
        // connects without its key. The typed secret is dropped right after.
        if let Some(key) = &draft.secret
            && let Err(err) = cosh::mcp::auth::store_key(&draft.entry.name, key)
        {
            self.toast_form_error(
                "Server not saved",
                format!("Failed to store the API key in the OS keyring: {err}"),
            );
            return false;
        }
        let saved = draft.entry.name.clone();
        self.setup.mcp.servers.push(draft.entry);
        self.setup.save();
        self.toast_form_success(
            "MCP server added",
            format!("“{saved}” connects on the next agent loop."),
        );
        // The user confirmed with Enter: the draft served its purpose and
        // must not leak into the next registration.
        self.mcp_form_draft = None;
        true
    }

    /// Open the MCP registration form: all four fields on one panel.
    /// Prefills from the accidental-close draft (see
    /// [`crate::routes::settings::McpFormDraft`]) so closing the box
    /// without Enter never loses typed content; the draft is only dropped
    /// by a successful save or by the user erasing the fields themselves.
    pub(in crate::app) fn open_mcp_form(&mut self) {
        let draft = self.mcp_form_draft.clone().unwrap_or_default();
        self.dialog.show(DialogType::McpForm {
            name: draft.name,
            endpoint: draft.endpoint,
            timeout: draft.timeout,
            api_key: draft.api_key,
            field: draft.field,
            cursor_pos: draft.cursor_pos,
            selection: None,
        });
    }

    /// Refresh the MCP registration draft from the open form so an
    /// accidental close (Esc, click outside) can be reopened with
    /// everything the user had typed. A no-op unless the topmost dialog
    /// is an MCP form — in particular it never touches the draft after a
    /// successful save has cleared it.
    pub(in crate::app) fn sync_mcp_form_draft(&mut self) {
        let draft = self.dialog.current().and_then(|d| match &d.dialog_type {
            DialogType::McpForm {
                name,
                endpoint,
                timeout,
                api_key,
                field,
                cursor_pos,
                ..
            } => Some(crate::routes::settings::McpFormDraft {
                name: name.clone(),
                endpoint: endpoint.clone(),
                timeout: timeout.clone(),
                api_key: api_key.clone(),
                field: *field,
                cursor_pos: *cursor_pos,
            }),
            _ => None,
        });
        if let Some(draft) = draft {
            self.mcp_form_draft = Some(draft);
        }
    }
}

/// One editing step of a registration form's active line. `lines` are the
/// field texts in field order, `field`/`cursor_pos` the active position
/// (`cursor_pos` is a byte index into the active line). The last field is
/// word-op-free (it is a timeout in the hook form, a masked API key in the
/// MCP form), so Ctrl word jumps apply to every field but the last.
/// Up/Down first move between the value's wrapped visual lines and only
/// change fields at the first/last line. A mouse drag selection owned by
/// the active field is replaced by typing, paste and deletions; navigation
/// drops it. Returns whether the key was consumed.
fn drive_form_edit<const N: usize>(
    lines: [&mut String; N],
    field: &mut usize,
    cursor_pos: &mut usize,
    selection: &mut Option<DragSelection<usize>>,
    key: KeyEvent,
    cols: usize,
) -> bool {
    debug_assert!(N > 0, "a registration form always has fields");
    let last = N - 1;
    let word_ops = *field != last;
    // A selection only lives on the field that owns it; a stale one from
    // another field dies before any key can observe it.
    if selection.is_some_and(|sel| sel.field() != *field) {
        *selection = None;
    }
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
            // A field switch (or a plain move) is a new caret.
            *selection = None;
            true
        }
        KeyCode::Left => {
            let target = &mut *lines[*field];
            if key.modifiers.contains(KeyModifiers::CONTROL) && word_ops {
                *cursor_pos = crate::util::word_ops::find_word_start(target, *cursor_pos);
            } else if *cursor_pos > 0 {
                *cursor_pos = target.floor_char_boundary(*cursor_pos - 1);
            }
            *selection = None;
            true
        }
        KeyCode::Right => {
            let target = &mut *lines[*field];
            if key.modifiers.contains(KeyModifiers::CONTROL) && word_ops {
                *cursor_pos = crate::util::word_ops::find_word_end(target, *cursor_pos);
            } else if *cursor_pos < target.len() {
                let next = target
                    .floor_char_boundary(*cursor_pos + 1)
                    .min(target.len());
                *cursor_pos = next;
            }
            *selection = None;
            true
        }
        KeyCode::Home => {
            *cursor_pos = 0;
            *selection = None;
            true
        }
        KeyCode::End => {
            *cursor_pos = lines[*field].len();
            *selection = None;
            true
        }
        KeyCode::Delete => {
            let target = &mut *lines[*field];
            if let Some((s, e)) = take_form_selection(*field, selection, target.len()) {
                target.drain(s..e);
                *cursor_pos = s;
            } else if *cursor_pos < target.len() {
                let next = target
                    .floor_char_boundary(*cursor_pos + 1)
                    .min(target.len());
                target.drain(*cursor_pos..next);
            }
            true
        }
        KeyCode::Backspace => {
            let target = &mut *lines[*field];
            if let Some((s, e)) = take_form_selection(*field, selection, target.len()) {
                target.drain(s..e);
                *cursor_pos = s;
            } else if *cursor_pos > 0 {
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
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+W deletes the word before the cursor (universal
                // terminal shortcut, same as the chat prompt and the
                // question dialog) — or the selected range when one is
                // active. Every other Ctrl+letter is consumed but ignored
                // (dropping a lingering selection) so it never leaks a
                // bare letter into the field — again matching the question
                // dialog.
                if ch == 'w' && word_ops {
                    if let Some((s, e)) = take_form_selection(*field, selection, target.len()) {
                        target.drain(s..e);
                        *cursor_pos = s;
                    } else if *cursor_pos > 0 {
                        let start = crate::util::word_ops::find_word_start(target, *cursor_pos);
                        target.drain(start..*cursor_pos);
                        *cursor_pos = start;
                    }
                } else {
                    *selection = None;
                }
                true
            } else {
                // Typing replaces the selected range (standard editor
                // behavior, chosen over the RAG form's clear-and-type).
                if let Some((s, e)) = take_form_selection(*field, selection, target.len()) {
                    target.drain(s..e);
                    *cursor_pos = s;
                }
                target.insert(*cursor_pos, ch);
                *cursor_pos += ch.len_utf8();
                true
            }
        }
        _ => false,
    }
}
