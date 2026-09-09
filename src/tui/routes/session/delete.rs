//! Session deletion lifecycle.
//!
//! Deleting a session used to leave its agent loop running: the harness
//! thread kept streaming events for a session that no longer existed, the
//! Working status and spinner never cleared, and the loop's resources
//! (queue channels, per-turn MCP connections) were never torn down.
//!
//! The fix lives here: [`App::delete_session`] is the single deletion
//! entry point shared by every Confirm-dialog resolution path (keyboard,
//! mouse, and the dialog module). When the deleted session owns the
//! running loop, it raises the shared stop signal — the SAME mechanism the
//! ESC stop uses (`stop_signal.store(true)`), which the harness checks at
//! every checkpoint of `run_agent_loop`. The harness then winds the loop
//! down, emits its terminal `Stopped` event (resetting status/spinner and
//! running the normal loop-end cleanup), and shuts down the per-turn MCP
//! connections via `drain_mcp`. The loop-owned queue plumbing is dropped
//! here so nothing can be injected into a loop that is on its way out.
//!
//! Deletions never touch loops belonging to other sessions: the stop
//! decision ([`deletion_stops_running_loop`]) requires BOTH that the
//! deleted session is the recorded loop owner AND that a loop is actually
//! running (`Working`/`Retry` status). Without a running loop the deletion
//! is a pure state + store removal, exactly as before.

use std::sync::atomic::Ordering;

use crate::app::App;
use crate::types::SessionStatus;

/// Decide whether deleting `session_id` must stop a running agent loop.
///
/// The stop fires only when the deleted session owns the live loop: it is
/// the recorded loop owner (`active_loop_session_id`) and the app is
/// actually inside a run (`Working` or `Retry` status — `Retry` matters
/// because an Error event can flip the status while a loop is still
/// winding down). Any other combination — no running loop, or a loop owned
/// by a different session — deletes without touching the shared stop
/// signal.
pub fn deletion_stops_running_loop(
    active_loop_session_id: Option<&str>,
    session_id: &str,
    loop_running: bool,
) -> bool {
    loop_running && active_loop_session_id == Some(session_id)
}

impl App {
    /// Delete a session, stopping and cleaning up its agent loop when one
    /// is still running for it.
    ///
    /// Shared by every Confirm-dialog resolution path (keyboard, mouse and
    /// the dialog module) so the deletion semantics cannot drift between
    /// them. The loop is stopped with the shared stop signal; the harness's
    /// terminal `Stopped` event performs the remaining cleanup (status,
    /// spinner, stream reset, queue parking, MCP shutdown), so this method
    /// only has to raise the flag and drop the loop-owned senders.
    pub fn delete_session(&mut self, session_id: &str) {
        let loop_running = matches!(
            self.state.status,
            SessionStatus::Working | SessionStatus::Retry { .. }
        );
        if deletion_stops_running_loop(
            self.active_loop_session_id.as_deref(),
            session_id,
            loop_running,
        ) {
            // Same mechanism as the ESC stop. Raising the flag is safe even
            // if the user already pressed ESC (an idempotent store).
            self.stop_signal.store(true, Ordering::Relaxed);
            // Drop the loop-owned plumbing NOW so nothing can be injected
            // into a loop that is on its way out, and no queued auto-start
            // can fire for the deleted session between this delete and the
            // harness's `Stopped` event.
            self.queued_input_tx = None;
            self.next_request_in_flight = false;
            self.queue_actions_deferred_start = false;
            self.hovered_queue_row = None;
            if self
                .edit_requeue_hint
                .as_ref()
                .is_some_and(|hint| hint.session_id == session_id)
            {
                // The hint points into the deleted session's queues — they
                // are removed below, so the hint would be invalid.
                self.edit_requeue_hint = None;
            }
        }
        self.state.remove_session(session_id);
        self.session_store.delete_session(session_id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{App, deletion_stops_running_loop};
    use crate::types::SessionStatus;

    fn app_with_running_loop_on(id: &str) -> App {
        let mut app = App::new("/tmp".to_string());
        app.state.add_empty_session(id.to_string(), "t".into(), 0);
        app.state.current_session_id = Some(id.to_string());
        app.state.status = SessionStatus::Working;
        app.active_loop_session_id = Some(id.to_string());
        app.stop_signal.store(false, Ordering::Relaxed);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.queued_input_tx = Some(tx);
        app.next_request_in_flight = true;
        app
    }

    /// Deleting a session whose agent loop is running must stop that loop:
    /// the shared stop signal is raised (the harness reacts by winding the
    /// loop down and emitting `Stopped`) and the loop-owned plumbing is
    /// dropped so nothing can be injected into the dying loop.
    #[tokio::test]
    async fn deleting_the_loop_owner_session_stops_its_running_loop() {
        let mut app = app_with_running_loop_on("loop-owner");
        app.delete_session("loop-owner");

        assert!(
            app.stop_signal.load(Ordering::Relaxed),
            "the running loop must be stopped when its session is deleted"
        );
        assert!(
            app.queued_input_tx.is_none(),
            "the loop's queued-input channel must be dropped"
        );
        assert!(
            !app.next_request_in_flight,
            "no in-flight queued message may outlive the loop"
        );
        // The session itself is fully removed.
        assert!(app.state.current_session_id.is_none());
        assert!(
            !app.state
                .session_summaries
                .iter()
                .any(|s| s.session_id == "loop-owner"),
            "the deleted session must vanish from the sidebar summaries"
        );
        assert!(
            app.state.pending_queues.get("loop-owner").is_none(),
            "the deleted session's pending queues must be removed"
        );
    }

    /// Deleting a session without a running loop is a pure state + store
    /// removal: the stop signal and the loop plumbing stay untouched (the
    /// flag is reset by `start_agent_loop`, and a `false` here may not
    /// pre-emptively stop a loop the user is about to start).
    #[tokio::test]
    async fn deleting_a_session_without_a_running_loop_keeps_working_normally() {
        let mut app = App::new("/tmp".to_string());
        app.state.add_empty_session("idle-one".into(), "t".into(), 0);
        app.state.current_session_id = Some("idle-one".into());
        app.state.status = SessionStatus::Idle;
        app.stop_signal.store(false, Ordering::Relaxed);

        app.delete_session("idle-one");

        assert!(
            !app.stop_signal.load(Ordering::Relaxed),
            "deleting an idle session must not raise the stop signal"
        );
        assert!(app.state.current_session_id.is_none());
    }

    /// A loop belonging to ANOTHER session must survive an unrelated
    /// deletion: neither the stop signal nor the loop-owner record may be
    /// touched.
    #[tokio::test]
    async fn deleting_an_unrelated_session_never_touches_another_sessions_loop() {
        let mut app = app_with_running_loop_on("loop-owner");
        app.state.add_empty_session("bystander".into(), "t".into(), 0);

        app.delete_session("bystander");

        assert!(
            !app.stop_signal.load(Ordering::Relaxed),
            "another session's running loop must not be stopped"
        );
        assert!(
            app.queued_input_tx.is_some(),
            "the running loop's channel must stay alive"
        );
        assert_eq!(app.active_loop_session_id.as_deref(), Some("loop-owner"));
        assert!(app.state.current_session_id.is_some());
    }

    /// The stop decision itself: it fires only for the loop owner while a
    /// loop is actually running. An Error event can flip the status while
    /// a loop is still winding down, so `Retry` counts as running too.
    #[test]
    fn stop_decision_requires_owner_and_running_loop() {
        assert!(deletion_stops_running_loop(Some("s"), "s", true));
        assert!(!deletion_stops_running_loop(Some("s"), "other", true));
        assert!(!deletion_stops_running_loop(Some("s"), "s", false));
        assert!(!deletion_stops_running_loop(None, "s", true));
    }
}

