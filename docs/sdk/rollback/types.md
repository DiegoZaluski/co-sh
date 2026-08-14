# `rollback` types and constants

The module's data surface is deliberately tiny: two I/O structs and three
constants. Everything else is the shared [`InMemorySnapshotStore`](../hashline/snapshots.md)
from `hashline::snapshots`, reused as-is.

---

## `RestoreInput` — what to restore

```rust
pub struct RestoreInput {
    /// Path of the file to restore.
    pub path: String,
    /// Hash of the target version, as shown in a `¶path#HASH` header.
    ///
    /// When `None`, the version immediately preceding the current disk state
    /// is restored. Requires the disk content to be present in the session
    /// history; returns an error if the file was modified externally.
    pub hash: Option<String>,
}
```

The two fields encode a four-way decision (see the
[resolution table](core.md#resolution-table)):

- `hash: Some(h)` — restore exactly version `h`.
- `hash: None` — "step back one" from whatever is currently on disk.

## `RestoreOutput` — what happened

```rust
pub struct RestoreOutput {
    /// Path of the restored file.
    pub path: String,
    /// Content hash of the version now on disk. Use this as the anchor for
    /// follow-up edits.
    pub file_hash: String,
    /// Hashline header `¶path#HASH` for the restored version.
    pub header: String,
    /// Content hash of the content that was on disk before the restore.
    /// Pass this to a subsequent `restore` call to undo the restore.
    pub replaced_hash: String,
    /// Non-fatal warning emitted when an anomaly was detected but the restore
    /// still succeeded (e.g., file was modified externally).
    pub warning: Option<String>,
}
```

Field-by-field usage:

- **`file_hash` / `header`** — the new anchor. After a restore, an agent
  continues editing from *this* version: `header` is a ready-to-use
  `¶path#HASH` for the next hashline edit.
- **`replaced_hash`** — the undo handle. Restoring `replaced_hash` puts the
  file back where it was before this restore. It is `""` when the file did
  not exist before the restore (nothing to return to) — never pass it back
  as a hash in that case.
- **`warning`** — always `Some` when an anomaly was tolerated (external
  modification, file recreated from history) and `None` on a clean restore.

## Constants

| Constant | Value | Meaning |
|---|---|---|
| `MAX_SNAPSHOT_BYTES` | 512 KiB | `record` silently skips files larger than this (returns `None`) |
| `MAX_PATHS` | 50 | Distinct paths tracked in the session store; LRU evicts cold paths |
| `MAX_VERSIONS_PER_PATH` | 10 | Versions retained per path; oldest dropped first |

`MAX_PATHS` and `MAX_VERSIONS_PER_PATH` configure the `session_store()`
singleton at first use (via `InMemorySnapshotStoreOptions`).

---

## The store itself

`session_store()` returns `Arc<Mutex<InMemorySnapshotStore>>` — the same
type family documented in [`hashline::snapshots`](../hashline/snapshots.md).
The rollback module adds nothing to it: recording, deduplication, per-path
history, LRU eviction, and `by_hash` lookups are all the store's own
behavior. The `Arc<Mutex<S>>` wrapper also implements the `SnapshotStore`
trait, so the patcher can share it directly.
