use super::super::App;
use crossterm::event::KeyCode;

use super::super::message_prompt_text;
use crate::session_store::generate_session_id;
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogType;

use crate::util::selection;

impl App {
    pub(in crate::app) fn is_message_actions_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::MessageActions { .. }))
    }

    pub(in crate::app) fn handle_message_actions_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_message_actions_dialog_visible() {
            return false;
        }

        let is_last_user_message = self.dialog.current().and_then(|d| {
            if let DialogType::MessageActions { is_last_user_message, .. } = &d.dialog_type {
                Some(*is_last_user_message)
            } else {
                None
            }
        }).unwrap_or(false);

        let max_options = if is_last_user_message { 3 } else { 1 };

        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 { max_options - 1 } else { d.selected - 1 };
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % max_options;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self.dialog.current().map_or(0, |d| d.selected.min(max_options - 1));
                let (message_id, is_last_user_message) = match self.dialog.current() {
                    Some(d) => match &d.dialog_type {
                        DialogType::MessageActions { message_id, is_last_user_message, .. } => {
                            (message_id.clone(), *is_last_user_message)
                        }
                        _ => return true,
                    },
                    None => return true,
                };
                self.dialog.pop();
                // Map the action index: if not last user message, only Copy (index 0)
                // exists, so we need to map it to action 1 (Copy) in the original enum
                let action = if is_last_user_message {
                    selected
                } else {
                    // Single option (Copy) maps to action 1
                    1
                };
                self.run_message_action(action, &message_id);
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Execute the picked Message Actions entry: 0 = Revert, 1 = Copy,
    /// 2 = Fork (same order as the opencode dialog).
    pub(in crate::app) fn run_message_action(&mut self, action: usize, message_id: &str) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let working = self.state.status == SessionStatus::Working;
        let session = self.state.current_session();
        let msg_idx = session.and_then(|s| s.messages.iter().position(|m| m.id == message_id));
        let Some(session) = session else {
            return;
        };
        let Some(idx) = msg_idx else {
            return;
        };
        let msg = &session.messages[idx];

        match action {
            0 => {
                // Revert: drop this message and everything after it, and put
                // its text back into the prompt for editing/resending.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let prompt_text = message_prompt_text(msg);
                let session = self.state.current_session_mut().expect("session");
                session.messages.truncate(idx);
                self.session_store.save_session_async(session);
                self.session_view.hovered_msg_idx = None;
                if !prompt_text.is_empty() {
                    self.prompt_view.input = prompt_text;
                    self.prompt_view.cursor_pos = self.prompt_view.input.len();
                }
                self.prompt_view.focus();
            }
            1 => {
                // Copy: join non-synthetic text parts into the clipboard.
                let text = message_prompt_text(msg);
                if !text.is_empty() {
                    selection::copy_selection(&text, &mut self.toast_state);
                } else {
                    self.toast_state.show(ToastOptions {
                        title: Some("Copy".into()),
                        message: "Nothing to copy in this message.".into(),
                        variant: ToastVariant::Info,
                        duration_ms: 2500,
                    });
                }
            }
            2 => {
                // Fork: branch a new session containing everything up to and
                // including the clicked message, then switch to it.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let mut forked = session.clone();
                forked.messages.truncate(idx + 1);
                forked.id = generate_session_id();
                forked.title = format!("{} (fork)", session.title);
                forked.created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                forked.title_generated = false;
                let new_id = forked.id.clone();
                self.state.add_session(forked);
                self.session_store.save_session(
                    self.state
                        .current_session()
                        .expect("forked session just added"),
                );
                self.state
                    .ensure_session_summary(&self.state.current_session_id.clone().expect("id"));
                self.finalize_stale_compaction_lines();
                self.state.switch_to_session(new_id, &self.session_store);
                self.session_view.hovered_msg_idx = None;
                self.toast_state.show(ToastOptions {
                    title: Some("Fork".into()),
                    message: "New session created from this point.".into(),
                    variant: ToastVariant::Success,
                    duration_ms: 3000,
                });
            }
            _ => {}
        }
    }
}
