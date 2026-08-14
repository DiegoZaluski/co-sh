# The `rollback` module: session-scoped file version history

`rollback` is the **undo engine** behind `cosh-tools`' `fs_rollback` tool. It
keeps a bounded, in-memory history of every file version the session has
observed and can restore any recorded version by its content hash — or step
back one version at a time.

```
read / edit / write  ──▶  rollback::record(path, text)  ──▶  session store
                                                              (LRU, bounded)
fs_rollback          ──▶  rollback::restore(RestoreInput) ──▶  disk
```

It is small on purpose: one shared store, two functions (`record`,
`restore`), one optional helper (`record_seen_lines`).

---

## The core idea: hashes are version handles

The 4-hex content tags already minted by the hashline engine
([`compute_file_hash`](../hashline/format.md)) are the *same* tokens used to
name versions here. Every tool that reads or writes a file calls
[`record`](core.md), which returns the tag. The model then already knows how
to target a version — a header it saw in `read` output (`¶path#HASH`) is
exactly what `restore` accepts. No new vocabulary to learn.

Three properties follow:

1. **Deterministic handles.** Recording identical content mints the same
   tag (read fusion), so `record` is idempotent — calling it again with the
   same bytes does not bloat the history.
2. **Content-addressable, not edit-addressable.** Versions are named by what
   the file *contained*, never by "the 3rd change". Two different paths with
   identical content share the same tag, which is fine — tags are scoped per
   path in the store.
3. **Session-scoped and bounded.** History lives for the process lifetime
   (it is a process-global singleton), not on disk. Old versions are evicted
   by an LRU window, so the memory footprint is fixed.

---

## The window

| Constant | Value | Meaning |
|---|---|---|
| `MAX_PATHS` | 50 | Distinct paths tracked at once; LRU evicts cold paths |
| `MAX_VERSIONS_PER_PATH` | 10 | Versions retained per path; oldest dropped first |
| `MAX_SNAPSHOT_BYTES` | 512 KiB | Files larger than this are **silently skipped** by `record` (returns `None`) |

Content containing null bytes (binary) is also skipped at record time —
`record` returns `None` for it.

---

## The restore contract

[`restore`](core.md) reads the *current* disk state, resolves the target
version from history, snapshots the pre-restore state (so every rollback can
itself be rolled back), writes the restored content **preserving the current
disk's BOM and line endings**, and invalidates the tree-sitter parse cache
for the path.

Targets are resolved from a `RestoreInput { path, hash }`:

| `hash` | Disk state | Behavior |
|---|---|---|
| `Some(h)` | exists | Restore exactly version `h`. Warns (but proceeds) if the disk was modified externally. Errors if `h` is unknown or already on disk. |
| `Some(h)` | deleted | Recreate the file from version `h`, with a warning. |
| `None` | exists | Restore the version **immediately preceding** the current disk state (step back one). Errors if the disk state is not in session history (external modification) or is already the oldest version. |
| `None` | deleted | Restore to the last recorded state (head), with a warning. |

Every failure is an actionable message: no history, unknown hash (lists the
known versions), already at target, no previous version, external
modification (lists known versions and tells you to pass an explicit hash).

---

## Relationship to hashline

`rollback` and `hashline` share the same snapshot-store machinery
(`hashline::snapshots`) and the same hash
function. They differ in *who owns the store*:

- `rollback` owns a process-global singleton (`session_store()`) with its
  own window (50 paths × 10 versions).
- `hashline`'s `Patcher` takes whatever store you hand it — and the
  recommended wiring is to hand it `rollback::session_store()` via
  `Patcher::new_shared`, so edits and rollbacks share one history.

In this crate's host, `cosh-tools`' `fs_edit` uses a patcher backed by the
same session store, which is why a `fs_edit` response's new header can
immediately be targeted by `fs_rollback`.

---

## What the module does *not* do

- **No disk persistence.** History is in memory and dies with the process.
- **No diffs.** Versions are whole files, not deltas.
- **No policy about *when* to record** — the caller (each file tool) decides
  when to call [`record`](core.md). If a tool forgets, that version is not
  recoverable.

---

## Example

A complete, runnable walkthrough lives at
[`examples/rollback/core.rs`](../../../crates/cosh-sdk/examples/rollback/core.rs):
it records a version chain, restores by explicit hash, steps back one
version at a time, undoes a rollback, and exercises the error paths.

Next: the [core API](core.md) — `record`, `restore`, and the error table.
