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

        let (message_id, is_last_user_message) = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::MessageActions {
                    message_id,
                    is_last_user_message,
                    ..
                } => (message_id.clone(), *is_last_user_message),
                _ => return false,
            },
            None => return false,
        };

        let max_options = if is_last_user_message { 3 } else { 1 };

        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 {
                        max_options - 1
                    } else {
                        d.selected - 1
                    };
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
                let selected = self
                    .dialog
                    .current()
                    .map_or(0, |d| d.selected.min(max_options - 1));
                self.dialog.pop();
                let action = Self::message_action_index(selected, is_last_user_message);
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

    /// Map the visual selection index to the action enum index.
    ///
    /// - If `is_last_user_message`: selection 0=Revert, 1=Copy, 2=Fork
    /// - Otherwise: only Copy is shown (visual index 0) → action index 1
    pub(in crate::app) fn message_action_index(
        selected: usize,
        is_last_user_message: bool,
    ) -> usize {
        if is_last_user_message {
            debug_assert!(selected < 3, "selected must be 0..2 for last user message");
            selected
        } else {
            1 // Copy
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
                let removed_ids: std::collections::HashSet<u64> = session.messages[idx..]
                    .iter()
                    .flat_map(|m| session.ctx_ids.get(&m.id).cloned().unwrap_or_default())
                    .collect();
                let session = self.state.current_session_mut().expect("session");
                session.messages.truncate(idx);
                // Drop bookkeeping entries for the removed messages so a later
                // display-only save cannot re-persist dead mappings.
                let remaining: std::collections::HashSet<&str> =
                    session.messages.iter().map(|m| m.id.as_str()).collect();
                session
                    .ctx_ids
                    .retain(|k, _| remaining.contains(k.as_str()));
                // Reflect in the model-facing context: the items behind the
                // reverted messages are removed from the `.ctx` too. Unknown
                // ids (compaction/sweep shrank the timeline) are simply not
                // there anymore — a filter can never miss. ACCEPTED inverse
                // gap: messages created after a compaction/sweep carry no
                // ctx_ids (frozen mapping), so their items cannot be located
                // and survive a revert of them — the model may still see
                // post-compaction content the display dropped. No `.ctx` on
                // disk means there is nothing to reflect (display-only
                // session). NOTE the snapshot is read HERE, on the caller
                // thread: a context save queued-but-unexecuted for this
                // session is superseded by this job (FIFO lands the revert
                // last) — benign while the action is idle-gated, since the
                // superseded items back tail messages being removed anyway.
                if let Some(context) = self
                    .session_store
                    .load_ctx_filtered(&session.id, |id| !removed_ids.contains(&id))
                {
                    self.session_store
                        .save_session_async_with_context(session, context);
                } else {
                    self.session_store.save_session_async(session);
                }
                self.finalize_stale_compaction_lines();
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
                let parent_id = session.id.clone();
                let mut forked = session.clone();
                forked.messages.truncate(idx + 1);
                forked.id = generate_session_id();
                forked.title = format!("{} (fork)", session.title);
                forked.created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                forked.title_generated = session.title_generated;
                // The fork's model-facing context: the parent's items that
                // back the KEPT messages (bookkeeping ids in the mapping that
                // are not on disk anymore — compaction/sweep — are inert and
                // filtered out here too). No parent `.ctx` → the fork is
                // saved display-only and starts without context (a later
                // resume warns, truthfully).
                let kept_ids: std::collections::HashSet<u64> = forked
                    .messages
                    .iter()
                    .flat_map(|m| forked.ctx_ids.get(&m.id).cloned().unwrap_or_default())
                    .collect();
                let kept_msg_ids: std::collections::HashSet<&str> =
                    forked.messages.iter().map(|m| m.id.as_str()).collect();
                forked
                    .ctx_ids
                    .retain(|k, _| kept_msg_ids.contains(k.as_str()));
                let fork_context = self
                    .session_store
                    .load_ctx_filtered(&parent_id, |id| kept_ids.contains(&id));
                match fork_context {
                    Some(context) => self
                        .session_store
                        .save_session_async_with_context(&mut forked, context),
                    None => self.session_store.save_session_async(&forked),
                }
                let new_id = forked.id.clone();
                self.state.add_session(forked);
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
