use super::super::App;
use crossterm::event::KeyCode;
use std::time::Instant;

use super::super::agent_loop::QUEUE_ACTIONS_GRACE;
use crate::routes::session::queue_choice::QueueTarget;
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogType;
use crate::util::selection;

impl App {
    /// Open the Queue Actions box for one queued message. Grants the
    /// [`QUEUE_ACTIONS_GRACE`] window (the agent loop may not consume the
    /// next effective message for 5 seconds) and clears the row highlight.
    pub(in crate::app) fn open_queue_actions_for(
        &mut self,
        queue: QueueTarget,
        index: usize,
        text: &str,
    ) {
        self.queue_actions_grace_until = Some(Instant::now() + QUEUE_ACTIONS_GRACE);
        self.hovered_queue_row = None;
        let preview: String = text.chars().take(36).collect();
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
                    // re-queueing the same text into the SAME queue can
                    // restore its exact position.
                    let text = deque[index].clone();
                    self.edit_requeue_hint = Some((queue, index, text.clone()));
                    deque.remove(index)
                }
                // Delete: remove in-place, shifting followers left.
                1 => deque.remove(index),
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
                self.prompt_view.input = text;
                self.prompt_view.cursor_pos = self.prompt_view.input.len();
                self.prompt_view.focus();
                self.hovered_queue_row = None;
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

    /// Enqueue a message into the chosen pending queue (the queue-choice
    /// dialog submit path).
    ///
    /// The message being submitted IS the edited queued message (the prompt
    /// flow guarantees it): while an edit hint is outstanding, submitting to
    /// the ORIGINAL queue re-inserts at the tracked original position — even
    /// if the text was changed, since editing is the whole point. Choosing
    /// the OTHER queue appends at the end and drops the hint.
    pub(in crate::app) fn enqueue_pending_message(&mut self, target: QueueTarget, text: String) {
        let hint = self.edit_requeue_hint.take();
        let restore = match &hint {
            Some((queue, index, _)) if *queue == target => Some(*index),
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
        let Some(target) = self.edit_requeue_hint.as_ref().map(|(q, _, _)| *q) else {
            // Plain submission (no edited message in flight).
            self.start_agent_loop(text);
            return;
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
