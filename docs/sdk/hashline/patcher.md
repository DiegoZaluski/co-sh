# `patcher` — validating and applying a patch to real files

The `Patcher` is the only stage of the hashline pipeline that touches the
filesystem. It wires everything together: read the section's target file
through a [`Filesystem`](fs.md), strip the BOM and normalize line endings,
validate the section's snapshot tag (with [recovery](recovery.md)), apply
the edits in memory, and write the result back — restoring the original BOM
and line endings.

```rust,ignore
let mut patcher = Patcher::new(
    DiskFilesystem::new(),
    InMemorySnapshotStore::new(&Default::default()),
    None, // optional BlockResolver for `replace block N:`
);
let result = patcher.apply(&patch).await?;
```

The `Patcher` is **stateless across calls** — construct one per filesystem
configuration and reuse it.

## The two layers

| API | Behavior |
|---|---|
| `Patcher::apply(&patch)` | High-level, **all-or-nothing**. Preflights every section in memory *before* any write hits disk, then commits in order. |
| `Patcher::prepare(&section)` → `PreparedSection` | The read-side work only: read, parse, validate, apply in memory. Returns a prepared token. |
| `Patcher::commit(prepared)` | Writes a prepared result, restores line endings/BOM, records a fresh snapshot. |
| `Patcher::preflight(&patch)` | The preflight pass only — no writes. Use for CI checks and dry runs. |

Because `prepare` already runs the full apply, a multi-file batch is
naturally all-or-nothing: by the time any `commit` runs, every section has
been validated. A section whose apply produced no text change aborts the
batch with `Edits to {path} resulted in no changes being made.` — this fires
in the multi-section path (different target files); a single section (or
same-path sections, which merge into one) short-circuits to the fast path
and reports the section with `op: Noop` instead of failing.

Two constructors exist: `Patcher::new(fs, store, resolver)` wraps the store
in its own `Arc<Mutex<S>>`; `Patcher::new_shared(fs, arc_store, resolver)`
accepts an already-shared store — use it when the patcher must record
snapshots into the same store other session tools use (e.g. the global
`rollback::session_store`).

## The prepare contract

`prepare` (and therefore `apply`) enforces, in order:

1. **The section must carry a hash tag.** A missing tag is a hard error:
   `Missing hashline snapshot tag for edit to {path}; use ¶{path}#tag from
   your latest read/search output. To create a new file, use the write tool.`
2. **The file must exist.** A missing target errors: `File not found: {path}.
   Use the write tool to create new files.` (Hashline edits existing files;
   `write` creates.)
3. **The tag must match the live content** — see below.
4. **No two sections may resolve to the same canonical path**: `Multiple
   hashline sections resolve to the same file (A and B). Merge their ops
   under one header before applying.`

## Tag validation: the heart of the safety model

The section's tag is a fingerprint of the file as the model saw it. At
apply time the patcher compares it against a hash of the live file:

| Live content vs. tag | Behavior |
|---|---|
| **Matches** (or no tag needed) | Apply directly. The file is exactly what the model read. |
| **Stale, but only head/tail inserts** | Apply onto the live content with a warning: `Applied an \`insert head:\`/\`insert tail:\` edit onto the current file content even though the snapshot tag was stale … re-read if the drift was unexpected.` Head/tail position is content-independent, so the insert is safe. |
| **Stale, anchored edit, snapshot recorded** | Attempt [recovery](recovery.md): replay the edit against the recorded snapshot and 3-way-merge onto the live content. Success yields a recovery banner warning. |
| **Stale, anchored edit, no recovery** | Reject with a [`MismatchError`](mismatch.md): `Edit rejected: file changed between read and edit.` |

The mismatch rejection is rich on purpose — it re-reads nothing, but it
carries the expected/actual tags, the anchored lines, and the surrounding
context, and it distinguishes two cases:

- **Hash recognized** (a snapshot exists for it — the file drifted since):
  `Section is bound to #EXPECTED, but the current file hashes to #ACTUAL.
  If a prior edit in this session modified this file, copy the ¶path#newhash
  header from that edit's response; otherwise re-read the file with \`read\`
  to refresh the tag before retrying.`
- **Hash not recognized** (never recorded — fabricated or carried over from
  a prior session): `Edit rejected: hash #TAG is not from this session. …
  never invent the tag and never reuse one from a prior session.`

## The result

Each committed section yields a `PatchSectionResult`:

```rust
pub struct PatchSectionResult {
    pub path: String,
    pub canonical_path: String,
    pub op: PatchOp,                 // Create | Update | Noop
    pub before: String,              // LF-normalized pre-edit text
    pub after: String,               // LF-normalized post-edit text
    pub persisted: String,           // after, with original BOM + line endings
    pub written: String,             // what the Filesystem actually stored
    pub file_hash: String,           // fresh 4-hex tag for the new content
    pub header: String,              // "¶path#tag" for follow-up edits
    pub first_changed_line: Option<u32>,
    pub warnings: Vec<String>,
}
```

`header`/`file_hash` are the anchor for the *next* edit: an agent applies an
edit, reads `header` from the response, and anchors the follow-up against it
— that's how edit chains stay valid.
