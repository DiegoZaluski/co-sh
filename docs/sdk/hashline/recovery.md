# `recovery` — rescuing stale edits

When an edit's hash tag no longer matches the live file, the patcher hands
the situation to [`Recovery`](recovery.md). Recovery's job: decide whether
the edit can be **replayed safely** onto drifted content, and if so, do it
— producing the merged result plus warnings that tell the caller what
happened.

Recovery is stateless apart from the `SnapshotStore` it
queries. `Recovery::new(store)` and `Recovery::try_recover(&RecoveryArgs)`.

## The mechanism

`try_recover(args)` where `args` carries `path`, `current_text` (the live
file), `file_hash` (the stale tag), and `edits`:

1. **Look up the snapshot the tag names.** If no snapshot was ever recorded
   for that tag, recovery declines — the tag is unrecognized (fabricated or
   cross-session), and the caller surfaces the "hash is not from this
   session" mismatch.
2. **Replay + 3-way merge.** Apply the edits to the *snapshot's* text (the
   version the model actually saw), compute a structured patch
   (`structured_patch`) between snapshot-before and
   snapshot-after, then `apply_patch` that patch onto the live
   content. This is the same merge a version-control system does: the
   model's *change* is transported to wherever the file is now, without
   clobbering unrelated edits.
3. **Session-chain fallback.** If the tagged version was *not* the most
   recent snapshot (a prior in-session edit advanced the hash), and the
   3-way merge refused, try replaying the edits onto the live content
   directly — but only when **line counts match** and **every anchor line's
   content is identical** between the snapshot and the live file. Both
   guards together still don't fully prove correctness (a coincidental
   insert+delete pair on duplicate rows can land on the wrong row), so this
   path emits a dedicated warning.
4. **Unseen-anchor hedge.** If the snapshot records which lines were
   surfaced to the model (via `record_seen_lines`), and an edit anchors a
   line the model never saw, a warning is appended: `Edit anchors target
   lines N, M which were not surfaced by the preceding tool output; verify
   the diff against the current file.`

## The warnings

| Warning | When |
|---|---|
| `Recovered from a stale file hash using a previous read snapshot (file changed externally between read and edit).` | The tagged snapshot is the head — the file drifted because of an *external* write. |
| `Recovered from a stale file hash using an earlier in-session snapshot (the file hash advanced after a prior edit in this session).` | The tagged snapshot is *not* the head — a prior in-session edit advanced the hash. |
| `Recovered by replaying your edits onto the current file content — your previous edit in this session changed line(s) you re-targeted with a stale hash. Verify the diff matches your intent before continuing.` | The session-chain fallback was taken (the less-certain mode). |

## When recovery declines

Recovery returns `None` — and the patcher rejects with a `MismatchError` —
when:

- the tag resolves to no snapshot (`hash_recognized: false`),
- the replay produced no change (the edit is a no-op against the snapshot),
- the 3-way merge fails to apply cleanly, or
- the session-chain guards fail.

In every case the caller sees the full mismatch diagnostic (expected vs.
actual hash, anchored lines, context) described in
[`patcher`](patcher.md), so the model can re-read and retry.

## `MismatchError`

Defined in `mismatch.rs`: a `Clone` + `Display` + `Error` type
with `path`, `expected_file_hash`, `actual_file_hash`, `file_lines`,
`anchor_lines`, and `hash_recognized`, plus a pre-formatted message
(`format_message()` / `display_message()`). The message includes the
rejection header and a small window of numbered lines around each anchor
(`*` marks anchored lines, `...` marks elided context). The same module
exposes two small helpers used for anchor parsing and validation:
`parse_tag` (a bare line reference like `42`, `*42:foo`,
`> 7`) and `validate_line_ref(line, file_lines)`.
