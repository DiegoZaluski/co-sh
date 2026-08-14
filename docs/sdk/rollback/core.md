# The `rollback` core API

The whole module is two functions, one helper, and the shared store:

```rust,ignore
pub fn session_store() -> &'static Arc<Mutex<InMemorySnapshotStore>>;
pub fn record(path: &str, text: &str) -> Option<String>;
pub fn record_seen_lines(path: &str, hash: &str, lines: &[(u32, String)]);
pub async fn restore(input: RestoreInput) -> Result<RestoreOutput, String>;
```

Plus three constants and the two I/O structs — see
[types](types.md) for those.

---

## `session_store()` — the shared history

Returns the **process-global singleton** snapshot store (an
`Arc<Mutex<InMemorySnapshotStore>>`), initializing it on first call with the
module's window (50 paths × 10 versions per path). Every tool in the session
shares this one store, which is what makes a version recorded by one tool
restorable by another.

The mutex is `std::sync::Mutex` (critical sections are short, never held
across `.await`) and is recovered transparently if poisoned. To hand this
store to a hashline `Patcher` so edits and rollbacks share history:

```rust,ignore
let patcher = Patcher::new_shared(disk_fs, rollback::session_store(), None);
```

## `record(path, text)` — mint a version

Call from **every tool that reads or writes a file**. It returns the 4-hex
content tag for the version (or `None` when the file was skipped), and the
tag is exactly the token a hashline header carries (`¶path#HASH`).

Behavior:

- **Normalizes before storing**: strips a UTF-8 BOM and converts to LF, so
  CRLF/BOM variants of the same content hash identically (and a later
  restore never double-encodes).
- **Skips oversized content** — `text.len() > MAX_SNAPSHOT_BYTES` (512 KiB)
  → `None`.
- **Skips binary content** — any `\0` byte → `None`.
- **Deduplicates** — recording byte-identical content again returns the same
  tag and does not create a second history entry.
- **Empty content is valid** — clearing a file records a real (empty)
  version.

## `record_seen_lines(path, hash, lines)` — surface provenance

Records which 1-indexed lines of `path` the model actually saw at version
`hash` (grep/read/search output). Recovery consults these to flag edits that
anchor lines the model never observed. No-op for an empty `lines` slice.

## `restore(input)` — put a version back

The heart of the module. Reads the current disk state, resolves the target
from history, **snapshots the pre-restore state** (so the restore can itself
be rolled back), writes the restored content preserving the current disk's
BOM and line endings, and invalidates the tree-sitter parse cache for the
path.

### Resolution table

`RestoreInput { path, hash }` resolves the target as follows:

| `hash` | Disk | Result |
|---|---|---|
| `Some(h)` | exists | Restore exactly `h`. **Warning** if disk was modified externally (proceeds anyway). **Error** if `h` unknown (lists known versions) or already on disk. |
| `Some(h)` | deleted | Recreate the file from `h`. **Warning**: "`{path}` no longer exists on disk; it will be recreated from version `{h}`". |
| `None` | exists | Restore the version immediately before the current disk state (step back one). **Error** if the disk state isn't in history (external modification — lists known versions, tells you to pass an explicit hash) or is already the oldest retained version. |
| `None` | deleted | Restore to the last recorded state (head). **Warning**: "`{path}` no longer exists on disk; restoring to last session state (`{hash}`)". |

### The output

```rust,ignore
pub struct RestoreOutput {
    pub path: String,
    pub file_hash: String,       // tag of the restored version — anchor follow-up edits on it
    pub header: String,          // "¶path#HASH" for the restored version
    pub replaced_hash: String,   // tag of what was on disk before — undo the restore with it
    pub warning: Option<String>,
}
```

Two details matter for caller logic:

- **`replaced_hash` enables undo-of-undo.** A second `restore` targeting
  `replaced_hash` rolls the file forward again. It is the empty string when
  the file did not exist before the restore — do not pass it back as a hash.
- **Explicit-hash restores re-anchor the target to head.** After an explicit
  restore, the restored version becomes the newest entry, so a subsequent
  `hash: None` steps back from *it* — this is what makes "restore, edit,
  then undo the edit" navigate correctly (see `restore_then_record_new_edit_navigates_correctly`
  in the test suite).

### Error table

| Situation | Error |
|---|---|
| No history for the path | `no rollback history for \`{path}\`; the file must be read or written during this session before rollback is available` |
| Hash not in history | `version \`{h}\` not found in rollback history for \`{path}\`; known versions: [h1, h2, …]` |
| Disk already at target | `\`{path}\` is already at version \`{h}\`; nothing to restore` |
| `hash: None`, disk is oldest | `no previous version for \`{path}\`; the current version is the oldest in the rollback window (N versions retained)` |
| `hash: None`, disk modified externally | `\`{path}\` was modified externally (disk hash \`{h}\` not in session history); cannot determine the previous session version automatically. Pass an explicit hash to roll back to. Known versions: […]` |
| Read/write failure | `failed to read \`{path}\`: …` / `failed to write \`{path}\` during restore: …` |

### Edge cases handled

- **External modification + explicit hash**: warns (`\`{path}\` was
  modified externally since the last session write (disk \`{d}\` differs
  from last session state \`{s}\`); external changes will be replaced`) and
  restores anyway.
- **File deleted externally + explicit hash**: recreates it (warning).
- **File deleted externally + `hash: None`**: restores to head (warning).
- **BOM / CRLF round-trip**: encoding is detected from the *current* disk
  state and reapplied to the restored content (defaults to LF, no BOM when
  the file didn't exist).
- **Oversized / binary files**: `record` skipped them, so a `hash: None`
  navigation sees the disk state as "not in history" and surfaces the
  external-modification error listing the known hashes.
- **The pre-restore snapshot is only recorded when absent** — re-recording
  an existing version would refresh its timestamp and break sequential
  `hash: None` navigation.

---

## A note on the panic

`restore` panics (via `expect`) only in one unreachable-ish case: `hash:
None`, file missing on disk, *and* empty history. The code path requires
history to be non-empty first (it errors otherwise), so this is defensive.
