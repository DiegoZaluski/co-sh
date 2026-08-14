# `rollback` — restoring previous versions

`rollback` restores a file to a version that was recorded earlier in the
session. It is the safety net for the whole [`fs` module](fs.md): every read
and write records the file's content in the session's rollback history, and
`rollback` lets you step back through those recordings.

```
Fs::rollback(&self, path: &str, hash: &str) -> Result<RollbackResult, String>
```

---

## The session rollback history

Every time a file is **read or written** during a session, the tool records its
content into an in-memory, session-scoped snapshot store. The history is
per-path and ordered by time of recording. This means:

- Reading a file before editing it guarantees there is a recorded version to
  come back to.
- Writing always records the *previous* content first, so a mistaken overwrite
  can be undone.
- `rollback` needs nothing but the path — the version to restore to is found
  from the history.

There is no explicit "save version" call: recordings happen automatically as a
side effect of the normal operations.

## Choosing which version to restore

There are two ways to select the target version, controlled by the `hash`
argument:

| `hash` | Behavior |
|---|---|
| **Empty** `""` | Restore the version immediately **preceding** the file's current disk content. The natural "undo last change" — you don't need to know any hash. |
| **A hash tag** | Restore the specific recorded version identified by that tag (the `file_hash`/`header` of an earlier result). |

After an explicit-hash restore, the restored version is what is now on disk —
asking for it again is an error (`already at version ...`). To reverse a
restore, pass back the result's `replaced_hash`: it identifies the version that
was on disk *before* the restore, so a follow-up rollback with it undoes the
undo.

### Example session

```text
write file.txt   → "version one"        (history: [v1])
write file.txt   → "version two"        (history: [v1, v2], disk = v2)
rollback("", hash="")                    → disk restored to v1
```

Because `write` records the previous content before overwriting, the history
after the two writes is `[v1, v2]`, and rolling back with an empty hash restores
`v1` — the version right before the current disk state.

## Behavior details

- Write permission is enforced before anything happens; a denied path returns
  `restore permission denied`.
- A path with **no rollback history** is an error (`no rollback history for ...`).
  The file must have been read or written during this session first.
- A hash that is not in the history is an error listing the known versions.
- Restoring to the version already on disk is an error.
- If the file was modified externally since the snapshot: an **explicit-hash**
  restore still succeeds but the result carries a `warning` flagging the
  anomaly; the empty-hash "previous version" restore is a hard error (`was
  modified externally ... Pass an explicit hash to roll back to`).
- If the file no longer exists on disk, it is recreated from the requested
  version (with a warning), or from the head of history when `hash` is empty.
- The file's original BOM and line endings (CRLF vs LF) are preserved on
  restore.

## What you get back

A successful restore returns a [`RollbackResult`](types.md#rollbackresult): the
restored path, the hash tag and header of the version now on disk (use them as
the anchor for follow-up reads/edits), the `replaced_hash` of the previous disk
state, and an optional `warning`.

---

## Example

A complete runnable example is provided at
[`examples/fs/rollback.rs`](../../../crates/cosh-tools/examples/fs/rollback.rs).
It writes two versions of a file, rolls back to the first, and then rolls
forward again using the recorded hash — the full undo/redo cycle.