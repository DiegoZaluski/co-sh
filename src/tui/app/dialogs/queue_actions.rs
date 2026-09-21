use super::super::App;
use crossterm::event::KeyCode;
use std::time::Instant;

use super::super::EditRequeueHint;
use super::super::agent_loop::QUEUE_ACTIONS_GRACE;
use crate::routes::session::queue_choice::QueueTarget;
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogType;
use crate::util::selection;

impl App {
    /// Open the Queue Actions box for one queued message. Grants the
    /// [`QUEUE_ACTIONS_GRACE`] window (the agent loop may not consume the
    /// next effective message for 5 seconds), remembers the owning session
    /// and clears the row highlight.
    pub(in crate::app) fn open_queue_actions_for(
        &mut self,
        queue: QueueTarget,
        index: usize,
        text: &str,
    ) {
        self.queue_actions_grace_until = Some(Instant::now() + QUEUE_ACTIONS_GRACE);
        self.active_queue_actions_session = self.state.current_session_id.clone();
        self.hovered_queue_row = None;
        let preview = super::super::dialog_preview_text(text);
        self.dialog.replace(DialogType::QueueActions {
            queue,
            index,
            preview,
        });
    }

    pub(in crate::app) fn is_queue_actions_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::QueueActions { .. }))
    }

    pub(in crate::app) fn handle_queue_actions_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_queue_actions_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 { 2 } else { d.selected - 1 };
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % 3;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self.dialog.current().map_or(0, |d| d.selected.min(2));
                let (queue, index) = match self.dialog.current() {
                    Some(d) => match &d.dialog_type {
                        DialogType::QueueActions { queue, index, .. } => (*queue, *index),
                        _ => return true,
                    },
                    None => return true,
                };
                self.dialog.pop();
                self.run_queue_action(selected, queue, index);
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Execute the picked Queue Actions entry: 0 = Edit, 1 = Delete,
    /// 2 = Copy (same order as the rendered dialog).
    ///
    /// Both mutations operate in-place on the target `VecDeque`: removal
    /// only shifts the following elements, so the FIFO order of the queue
    /// is always preserved.
    pub(in crate::app) fn run_queue_action(
        &mut self,
        action: usize,
        queue: QueueTarget,
        index: usize,
    ) {
        // A box left open across a session switch must never mutate another
        // session's queue.
        let Some(owner) = self.active_queue_actions_session.clone() else {
            return;
        };
        if self.state.current_session_id.as_deref() != Some(owner.as_str()) {
            self.dialog.clear();
            return;
        }
        let text = {
            let Some(queues) = self.state.current_pending_queues_mut() else {
                return;
            };
            let deque = match queue {
                QueueTarget::NextRequest => &mut queues.next_request,
                QueueTarget::NextLoop => &mut queues.next_loop,
            };
            if index >= deque.len() {
                return;
            }
            match action {
                0 => {
                    // Edit: remember the origin BEFORE removing, so
                    // re-submitting into the SAME queue restores its exact
                    // position.
                    let text = deque.remove(index);
                    if text.is_some() {
                        self.edit_requeue_hint = Some(EditRequeueHint {
                            session_id: owner,
                            queue,
                            index,
                        });
                    }
                    text
                }
                // Delete: remove in-place, shifting followers left.
                1 => {
                    let removed = deque.remove(index);
                    // A row before the edited message's home slot just
                    // shifted it one slot closer to the front.
                    if removed.is_some()
                        && let Some(hint) = &mut self.edit_requeue_hint
                        && hint.queue == queue
                        && hint.index > index
                    {
                        hint.index -= 1;
                    }
                    removed
                }
                _ => deque.get(index).cloned(),
            }
        };
        let Some(text) = text else {
            return;
        };

        match action {
            0 => {
                // Edit: take the message out of the queue and load it into
                // the prompt for editing/resending.
                self.load_into_prompt(text);
            }
            1 => {
                // Delete: the message was already removed above.
                self.hovered_queue_row = None;
            }
            2 => {
                // Copy: put the queued text into the clipboard.
                selection::copy_selection(&text, &mut self.toast_state);
            }
            _ => {}
        }
    }

    /// Load `text` into the prompt input for editing/resending.
    fn load_into_prompt(&mut self, text: String) {
        // One Replace group: a single Ctrl+Z undoes the load.
        self.prompt_view.set_draft(text);
        self.prompt_view.focus();
        self.hovered_queue_row = None;
    }

    /// Enqueue a message into the chosen pending queue (the queue-choice
    /// dialog submit path).
    ///
    /// While an edit hint for the CURRENT session is outstanding, submitting
    /// to the hint's queue re-inserts at the tracked position — even if the
    /// text was changed, since editing is the whole point. Choosing the
    /// other queue (or a stale cross-session hint) appends at the end.
    pub(in crate::app) fn enqueue_pending_message(&mut self, target: QueueTarget, text: String) {
        let current = self.state.current_session_id.clone();
        let hint = self.edit_requeue_hint.take();
        // A hint from another session is meaningless here: drop it and send
        // like a plain queued message.
        let hint = match (&hint, &current) {
            (Some(h), Some(id)) if h.session_id == *id => Some(h),
            _ => None,
        };
        let restore = match &hint {
            Some(h) if h.queue == target => Some(h.index),
            _ => None,
        };
        let Some(queues) = self.state.current_pending_queues_mut() else {
            return;
        };
        let deque = match target {
            QueueTarget::NextRequest => &mut queues.next_request,
            QueueTarget::NextLoop => &mut queues.next_loop,
        };
        match restore {
            Some(index) => {
                // The deque may have shrunk since the edit (delivered heads);
                // clamp so the message lands as close to home as possible.
                let index = index.min(deque.len());
                deque.insert(index, text);
            }
            None => {
                deque.push_back(text);
            }
        }
    }

    /// Submit the prompt text as a user message.
    ///
    /// Normally this starts a fresh agent loop right away. But when an
    /// EDITED queued message is outstanding (the hint armed by Queue Actions
    /// → Edit), the message MUST return to its slot in the queue instead of
    /// being treated as a direct provider input — otherwise submitting it
    /// right as the loop ends (status already Idle) would jump ahead of
    /// everything still queued. The queue chain then resumes in FIFO order:
    /// leftover "next request" messages are promoted and the head of "next
    /// agent loop" starts, honoring the queue-actions hold via the deferred
    /// start flag.
    pub(in crate::app) fn submit_prompt_message(&mut self, text: String) {
        let target = match &self.edit_requeue_hint {
            // Only a hint belonging to the CURRENT session rewrites the
            // submit path; stale cross-session hints are dropped below.
            Some(h) if Some(&h.session_id) == self.state.current_session_id.as_ref() => h.queue,
            _ => {
                // Plain submission (no edited message in flight here).
                self.edit_requeue_hint = None;
                self.start_agent_loop(text);
                return;
            }
        };
        self.enqueue_pending_message(target, text);

        if self.state.status != SessionStatus::Idle {
            return;
        }
        if self.queue_actions_hold_active() {
            // Deliveries are on hold: let the pump promote and start the
            // chain once the hold expires.
            self.queue_actions_deferred_start = true;
            return;
        }
        let Some(id) = self.state.current_session_id.clone() else {
            return;
        };
        self.promote_next_request_to_next_loop(&id);
        let next = self
            .state
            .pending_queues
            .get_mut(&id)
            .and_then(|q| q.next_loop.pop_front());
        if let Some(msg) = next {
            self.start_agent_loop(msg);
        }
    }
}
