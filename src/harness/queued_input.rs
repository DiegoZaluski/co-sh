//! Shared queued-input queue between the TUI and the agent loop.
//!
//! The TUI keeps the authoritative "next request" queue (its `PendingQueues`
//! deque) and shares it with the running agent loop through this handle: the
//! harness drains the head entries before every request and acknowledges each
//! injection via [`HarnessEvent::UserMessageInjected`].
//!
//! Why not an `mpsc` channel like before: a channel consumes the message at
//! `send` time, so a user DELETE or EDIT of the queued message could no longer
//! reach it — the harness had already taken it, and the message was only
//! removed visually. A shared deque keeps the entry owned by the TUI until the
//! harness actually injects it (pop-on-inject), so:
//!
//! * **Delete** of a message still in the deque simply removes it — the model
//!   never sees it.
//! * **Edit** rewrites the entry in place (the loop picks up the edited text
//!   at injection time, not the stale original).
//! * The entry only disappears when the harness pops it, right before adding
//!   it to the model context — exactly the moment the TUI is acknowledged.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Internal state: the pending messages plus the TUI-side hold flag.
#[derive(Default)]
struct Inner {
    queue: VecDeque<String>,
    /// True while the TUI holds the queue (Queue Actions box open, grace
    /// window, or an edit of the next-delivered message in flight). While
    /// held, the harness's drain returns nothing — a message the user is
    /// about to delete or edit can never be injected in that window.
    held: bool,
}

/// Shared handle to the "next request" queue of a running agent loop.
///
/// Cloned cheaply (`Arc<Mutex<Inner>>`): one clone lives on the
/// TUI (`App::queued_input`), one inside the harness thread for the duration
/// of the loop.
#[derive(Clone, Default)]
pub struct QueuedInput {
    inner: std::sync::Arc<Mutex<Inner>>,
}

impl QueuedInput {
    /// Push a message at the back (FIFO).
    pub fn push(&self, text: String) {
        self.inner
            .lock()
            .expect("queued input poisoned")
            .queue
            .push_back(text);
    }

    /// Drain every pending message in FIFO order (the harness's
    /// before-request drain). Returns nothing while the TUI holds the queue.
    pub fn drain(&self) -> Vec<String> {
        let mut inner = self.inner.lock().expect("queued input poisoned");
        if inner.held {
            return Vec::new();
        }
        inner.queue.drain(..).collect()
    }

    /// Arm or release the hold (TUI-side: Queue Actions box, grace window,
    /// head-edit hold).
    pub fn set_hold(&self, held: bool) {
        self.inner.lock().expect("queued input poisoned").held = held;
    }

    /// True while the TUI holds the queue.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.inner.lock().expect("queued input poisoned").held
    }

    /// Remove the FIRST message whose text equals `text` (used by the TUI to
    /// retract a message it deleted from its own pending queue — the deque
    /// mirrors that queue, so the same text may appear more than once and the
    /// head-most occurrence is the one a user acting on the queue means).
    pub fn remove(&self, text: &str) -> bool {
        let mut inner = self.inner.lock().expect("queued input poisoned");
        if let Some(pos) = inner.queue.iter().position(|t| t == text) {
            inner.queue.remove(pos);
            true
        } else {
            false
        }
    }

    /// Drop every pending message and release the hold (the TUI stops or
    /// clears its queues — anything still pending must not reach the dying
    /// loop, and the next loop starts unheld).
    pub fn clear(&self) {
        let mut inner = self.inner.lock().expect("queued input poisoned");
        inner.queue.clear();
        inner.held = false;
    }

    /// Current number of pending messages (diagnostics/tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("queued input poisoned").queue.len()
    }

    /// True when no message is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for QueuedInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().expect("queued input poisoned");
        f.debug_struct("QueuedInput")
            .field("len", &inner.queue.len())
            .field("held", &inner.held)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::QueuedInput;

    #[test]
    fn fifo_drain_preserves_order() {
        let q = QueuedInput::default();
        q.push("a".into());
        q.push("b".into());
        assert_eq!(q.drain(), vec!["a".to_string(), "b".to_string()]);
        assert!(q.is_empty());
    }

    #[test]
    fn remove_retracts_head_occurrence() {
        let q = QueuedInput::default();
        q.push("m".into());
        q.push("m".into());
        assert!(q.remove("m"));
        assert_eq!(q.drain(), vec!["m".to_string()]);
        assert!(!q.remove("m"));
    }

    #[test]
    fn hold_blocks_the_drain_until_released() {
        let q = QueuedInput::default();
        q.push("pending".into());
        // The TUI opened the Queue Actions box: the harness must not drain.
        q.set_hold(true);
        assert!(q.is_held());
        assert!(q.drain().is_empty());
        assert_eq!(q.len(), 1, "held messages stay owned by the TUI");
        // Hold released (box closed, edit submitted): the drain resumes.
        q.set_hold(false);
        assert_eq!(q.drain(), vec!["pending".to_string()]);
    }

    #[test]
    fn clear_releases_the_hold() {
        let q = QueuedInput::default();
        q.push("x".into());
        q.set_hold(true);
        q.clear();
        assert!(q.is_empty() && !q.is_held());
    }
}
