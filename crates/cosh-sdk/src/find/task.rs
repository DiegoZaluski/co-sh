//! Cancellation support for find operations.
//!
//! # Overview
//! Provides token-based cooperative cancellation for long-running operations.
//!
//! # Cancellation
//! Pass a `CancelToken` to blocking tasks. Work must check
//! `CancelToken::heartbeat()` periodically to respect cancellation.
//!
//! # Usage
//! ```ignore
//! use crate::find::task::CancelToken;
//!
//! let ct = CancelToken::new(None);
//! // ... heavy computation with ct.heartbeat()? ...
//! ```

use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

// ─────────────────────────────────────────────────────────────────────────────
// Cancellation
// ─────────────────────────────────────────────────────────────────────────────

/// Reason for task abortion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    Timeout,
    Signal,
    User,
}

impl std::fmt::Display for AbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbortReason::Timeout => write!(f, "Timeout"),
            AbortReason::Signal => write!(f, "Signal"),
            AbortReason::User => write!(f, "User"),
        }
    }
}

/// Error returned when a task is cancelled.
#[derive(Debug, Clone)]
pub struct CancelledError {
    pub reason: AbortReason,
}

impl std::fmt::Display for CancelledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            AbortReason::Timeout => write!(f, "Timeout"),
            AbortReason::Signal => write!(f, "Signal"),
            AbortReason::User => write!(f, "User"),
        }
    }
}

impl std::error::Error for CancelledError {}

struct CancelState {
    aborted: bool,
    reason: AbortReason,
    created_at: Instant,
    timeout_ms: Option<u64>,
}

/// Token for cooperative cancellation of blocking work.
///
/// Call `heartbeat()` periodically inside long-running work to check for
/// cancellation requests from timeouts or abort signals.
#[derive(Clone)]
pub struct CancelToken {
    state: Arc<Mutex<CancelState>>,
    notify: Arc<Notify>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new(None)
    }
}

impl From<()> for CancelToken {
    fn from((): ()) -> Self {
        Self::default()
    }
}

impl CancelToken {
    /// Create a new cancel token from an optional timeout.
    pub fn new(timeout_ms: Option<u32>) -> Self {
        Self {
            state: Arc::new(Mutex::new(CancelState {
                aborted: false,
                reason: AbortReason::User,
                created_at: Instant::now(),
                timeout_ms: timeout_ms.map(u64::from),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Check if cancellation has been requested.
    ///
    /// Returns `Ok(())` if work should continue, or an error if cancelled.
    /// Call this periodically in long-running loops.
    ///
    /// # Errors
    /// Returns an error if the operation has been aborted or has timed out.
    ///
    /// # Panics
    /// Panics if the internal state mutex is poisoned.
    pub fn heartbeat(&self) -> Result<(), String> {
        let state = self.state.lock().expect("cancel state lock poisoned");
        if state.aborted {
            return Err(state.reason.to_string());
        }
        if let Some(timeout_ms) = state.timeout_ms
            && u64::try_from(state.created_at.elapsed().as_millis()).unwrap_or(u64::MAX)
                >= timeout_ms
        {
            return Err("Timeout".to_string());
        }
        Ok(())
    }

    /// Wait for the cancel token to be aborted or the timeout to elapse.
    ///
    /// # Panics
    /// Panics if the internal state mutex is poisoned.
    pub async fn wait(&self) -> AbortReason {
        // Fast path: already aborted
        {
            let state = self.state.lock().expect("cancel state lock poisoned");
            if state.aborted {
                return state.reason;
            }
        }

        let notify = self.notify.clone();
        let state = self.state.clone();

        let timeout_ms = {
            let s = state.lock().expect("cancel state lock poisoned");
            s.timeout_ms
        };

        if let Some(ms) = timeout_ms {
            // Race notify against timeout
            tokio::select! {
                () = notify.notified() => {
                    let s = state.lock().expect("cancel state lock poisoned");
                    s.reason
                }
                () = tokio::time::sleep(tokio::time::Duration::from_millis(ms)) => {
                    AbortReason::Timeout
                }
            }
        } else {
            notify.notified().await;
            let s = state.lock().expect("cancel state lock poisoned");
            s.reason
        }
    }

    /// Get an abort token for external cancellation.
    #[must_use]
    pub fn abort_token(&self) -> AbortToken {
        AbortToken {
            state: Arc::clone(&self.state),
            notify: Arc::clone(&self.notify),
        }
    }

    /// Check if already aborted (non-blocking).
    ///
    /// # Panics
    /// Panics if the internal state mutex is poisoned.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.state
            .lock()
            .expect("cancel state lock poisoned")
            .aborted
    }
}

/// Token for requesting cancellation from outside the task.
#[derive(Clone)]
pub struct AbortToken {
    state: Arc<Mutex<CancelState>>,
    notify: Arc<Notify>,
}

impl AbortToken {
    /// Request cancellation of the associated task.
    ///
    /// # Panics
    /// Panics if the internal state mutex is poisoned.
    pub fn abort(&self, reason: AbortReason) {
        let mut state = self.state.lock().expect("cancel state lock poisoned");
        state.aborted = true;
        state.reason = reason;
        self.notify.notify_waiters();
    }
}
