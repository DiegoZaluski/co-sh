//! Local event queue: JSONL file in the data dir with size cap and expiry.
//!
//! Events are appended as one JSON line each and drained on flush. Semantics:
//!
//! - `drain_batch` REMOVES the returned batch from the file. The sink owns the
//!   batch from that point: on retryable failure it re-`push`es the events, on
//!   success they are gone, on permanent rejection they are dropped.
//! - The queue is bounded in both directions: `push` trims the oldest lines to
//!   make room when the file is full, and the drain rewrite also enforces the
//!   caps. A machine that never flushes therefore stays bounded on disk.
//! - All I/O failures are non-fatal — telemetry must never break the app.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::telemetry::events::EventEnvelope;

/// Events older than this are dropped at flush time.
const QUEUE_TTL_DAYS: u64 = 14;
/// Hard cap on the queue file (bytes). Oldest lines are dropped first.
const QUEUE_MAX_BYTES: u64 = 512 * 1024;
/// Hard cap on a SINGLE serialized event : an event bigger than
/// this can never fit the queue budget — it is rejected on `push` (fail
/// closed) instead of being appended anyway or evicting the whole file.
const MAX_EVENT_BYTES: usize = 64 * 1024;
/// Hard cap on the number of queued lines.
const QUEUE_MAX_LINES: usize = 2_000;
/// Max events sent per batch.
pub const MAX_BATCH: usize = 50;

/// A JSONL-backed queue.
pub struct EventQueue {
    path: PathBuf,
}

impl EventQueue {
    /// Queue rooted at `dir` (the app data dir). The file is created lazily.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            path: dir.as_ref().join("telemetry-events.jsonl"),
        }
    }

    /// Queue file path (exposed for tests and `/telemetry export`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_dir(&self) {
        if let Some(parent) = self.path.parent() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let _ = std::fs::DirBuilder::new()
                    .mode(0o700) // owner-only: events are app-private on disk
                    .recursive(true)
                    .create(parent);
            }
            #[cfg(not(unix))]
            {
                let _ = std::fs::create_dir_all(parent);
            }
        }
    }

    /// Append an event, trimming the OLDEST lines when the caps would be
    /// exceeded. Never panics; failures are logged and ignored. An event
    /// whose serialized form exceeds [`MAX_EVENT_BYTES`] is REJECTED (audit
    /// F07 — a single oversized value can never defeat the queue cap).
    pub fn push(&self, event: &EventEnvelope) {
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        let line_bytes = line.len() as u64 + 1;
        if line_bytes as usize > MAX_EVENT_BYTES {
            log::warn!("telemetry: oversized event rejected ({} bytes)", line.len());
            return;
        }
        self.ensure_dir();

        let existing = std::fs::read(&self.path).unwrap_or_default();
        // str::lines (not BufRead::lines) so the trim pass can iterate in
        // reverse — BufRead::lines is not DoubleEndedIterator.
        let existing_str = String::from_utf8_lossy(&existing);
        let current_bytes = existing.len() as u64;
        let current_lines = existing_str.lines().count();

        if current_bytes + line_bytes > QUEUE_MAX_BYTES || current_lines + 1 > QUEUE_MAX_LINES {
            // Make room by dropping the oldest lines until the new event fits.
            let mut budget = QUEUE_MAX_BYTES.saturating_sub(line_bytes);
            let mut lines_budget = QUEUE_MAX_LINES - 1;
            let kept: Vec<&str> = existing_str
                .lines()
                .rev()
                .take_while(|l| {
                    let b = l.len() as u64 + 1;
                    if lines_budget > 0 && b <= budget {
                        budget -= b;
                        lines_budget -= 1;
                        true
                    } else {
                        false
                    }
                })
                .collect();
            if let Ok(mut f) = std::fs::File::create(&self.path) {
                for l in kept.iter().rev() {
                    let _ = f.write_all(l.as_bytes());
                    let _ = f.write_all(b"\n");
                }
            }
            log::debug!("telemetry: queue full, oldest events trimmed to make room");
        }

        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
    }

    /// Read and REMOVE up to `MAX_BATCH` pending events, dropping expired and
    /// corrupt lines as a side effect. The caller owns the returned batch:
    /// re-push on retryable failure, discard on permanent rejection.
    pub fn drain_batch(&self) -> Vec<EventEnvelope> {
        let Ok(content) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let ttl = Duration::from_secs(QUEUE_TTL_DAYS * 24 * 3600);

        let mut kept: Vec<String> = Vec::new();
        let mut batch = Vec::new();
        for line in content.lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<EventEnvelope>(&line) else {
                log::warn!("telemetry: dropping corrupt queue line");
                continue;
            };
            let expired = event
                .occurred_at()
                .parse::<chrono::DateTime<chrono::Utc>>()
                .map(|t| {
                    now.duration_since(t.into())
                        .map(|age| age > ttl)
                        .unwrap_or(false)
                })
                .unwrap_or_else(|_| {
                    log::warn!("telemetry: queue line has an unparsable timestamp; dropping");
                    true // fail closed: an undatable event is not worth keeping
                });
            if expired {
                continue;
            }
            if batch.len() < MAX_BATCH {
                batch.push(event); // handed to the caller — NOT kept
            } else {
                kept.push(line);
            }
        }

        // Rewrite the file WITHOUT the batched events and without expired/
        // corrupt lines (best effort).
        if let Ok(mut f) = std::fs::File::create(&self.path) {
            for line in &kept {
                let _ = f.write_all(line.as_bytes());
                let _ = f.write_all(b"\n");
            }
        }
        batch
    }

    /// Remove ALL queued events (used by `/telemetry clear`).
    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Number of pending events (best effort; for `/telemetry status`).
    pub fn pending_count(&self) -> usize {
        std::fs::read(&self.path)
            .map(|content| {
                content
                    .lines()
                    .map_while(Result::ok)
                    .filter(|l| !l.trim().is_empty())
                    .count()
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::events::EventEnvelope;
    use crate::telemetry::schema::EventType;

    fn env() -> EventEnvelope {
        // Fresh timestamp: the queue drops events older than the 14-day TTL,
        // so tests must enqueue events stamped "now".
        crate::telemetry::events::synthetic(EventType::Error)
    }

    #[test]
    fn push_and_drain_roundtrip_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        q.push(&env());
        q.push(&env());
        assert_eq!(q.pending_count(), 2);
        let batch = q.drain_batch();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].event_type(), EventType::Error);
        // The batch is REMOVED from the file — a re-drain returns nothing.
        assert_eq!(q.pending_count(), 0);
        assert!(q.drain_batch().is_empty());
    }

    #[test]
    fn repushed_batch_survives_retry_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        for _ in 0..3 {
            q.push(&env());
        }
        let batch = q.drain_batch();
        assert_eq!(batch.len(), 3);
        // Sink failed retryably → re-push.
        for event in &batch {
            q.push(event);
        }
        assert_eq!(q.pending_count(), 3);
        // And the NEXT drain still yields exactly the batch (no duplicates).
        assert_eq!(q.drain_batch().len(), 3);
    }

    #[test]
    fn newer_events_not_starved_by_head_batch() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        q.push(&env());
        let first = q.drain_batch();
        q.push(&env()); // arrives while the first batch is "in flight"
        // First batch permanently rejected → dropped, not re-queued.
        drop(first);
        assert_eq!(q.drain_batch().len(), 1);
    }

    #[test]
    fn expired_events_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        // Well past the 14-day TTL: a stale stamp pushed directly. The
        // override is valid (minute-rounded).
        let stale = env().with_occurred_at("2020-01-01T00:00:00Z").unwrap();
        q.push(&stale);
        q.push(&env());
        let batch = q.drain_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(q.pending_count(), 0); // the fresh one went into the batch
    }

    #[test]
    fn push_keeps_the_queue_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        for _ in 0..(QUEUE_MAX_LINES + 500) {
            q.push(&env());
        }
        let size = std::fs::metadata(q.path()).unwrap().len();
        assert!(
            size <= QUEUE_MAX_BYTES + 4096,
            "queue grew past the byte cap: {size}"
        );
        assert!(
            q.pending_count() <= QUEUE_MAX_LINES,
            "queue grew past the line cap"
        );
    }

    #[test]
    fn oversized_event_is_rejected_never_appended() {
        // a single oversized event cannot defeat the cap — it is
        // rejected at push time, so the file stays within QUEUE_MAX_BYTES and
        // nothing is drained.
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        let big = env().with_padding(600 * 1024);
        q.push(&big);
        assert!(
            std::fs::metadata(q.path()).map(|m| m.len()).unwrap_or(0) <= QUEUE_MAX_BYTES,
            "oversized event was appended"
        );
        assert_eq!(q.pending_count(), 0);
        assert!(q.drain_batch().is_empty());
    }

    #[test]
    fn clear_removes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let q = EventQueue::new(dir.path());
        q.push(&env());
        q.clear();
        assert_eq!(q.pending_count(), 0);
        assert!(q.drain_batch().is_empty());
    }
}
