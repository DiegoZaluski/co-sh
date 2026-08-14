# `grep` — searching file contents

`grep` finds *lines* of file content matching a regex. It answers "which line
talks about X?" and — because every matched file carries a hashline anchor —
gives you the tag you need to edit the match **without re-reading it**.

```
Find::grep(&self, pattern: &str, path: &str) -> Result<GrepOutput, String>
```

For one directory this is the whole story. The longer call forms add multiple
targets, pagination, and a line range: `grep_skipping`, `grep_with`, and
`grep_with_streaming` (the streaming variant pushes each match to a callback as
it is found).

---

## Output bounds

Grep output is windowed so the model's context stays bounded:

| Bound | Value | Meaning |
|---|---|---|
| Files per call | **20** (directory/multi-file scopes) | At most 20 distinct files are surfaced. Page the next window with `skip`. |
| Matches per file | **20** multi-file, **200** single-file | Keeps a hot file from crowding out diverse hits. |
| Line length | **200 chars** | Longer lines are cut with a `...` suffix and flagged via `truncated` — read the file for the full line. |

When more files matched than the window, `file_limit_reached: true` and a
`note` tell you to call again with the same pattern/path plus `skip=<N>` for
the next page. When a single file exceeded its match cap,
`per_file_limit_reached: true`.

**`max_count` changes the contract.** A caller that sets `max_count` is not
paginating: the cap bounds the output directly, and the file window is not
applied (so there is no `skip` to follow).

---

## Options

| Option | Default | Effect |
|---|---|---|
| `paths` | — | Multiple targets (files or directories) in one call; overrides `path`. |
| `line_range` | — | `"start-end"`, 1-based inclusive. **Requires single-file targets.** |
| `glob` / `name_glob` | — | Restrict to files whose names match a glob (e.g. `"*.rs"`). |
| `file_type` / `language` | — | Restrict to a language/type (e.g. `"rust"`, `"py"`). |
| `ignore_case` | `false` | Case-insensitive matching. |
| `max_count` | — | Total match cap; disables file-window paging. |
| `skip` | `0` | Files to skip before collecting — paginate past the window. Ignored for single-file scopes and whenever `max_count` is set. |
| `context_before` / `context_after` | none | Context lines around each match. |
| `hidden` | `true` | Include `.`-prefixed files. |
| `gitignore` | `true` | Respect `.gitignore`. |
| `timeout_ms` | `5000` | Partials with `timed_out: true` on expiry. |

Patterns containing a **newline** (literal or `\n` escape) automatically enable
multiline matching — the engine's line mode would otherwise silently return
zero matches for such patterns.

---

## Reading the result

- `matches` — the matched lines (path, `line_number`, `line`, `truncated`,
  context). Paths are absolute for single-file scopes and root-relative (or
  common-ancestor-relative for multi-target calls) for directory searches.
- `total_matches` — the deduplicated per-line union across targets *before*
  the per-file caps and window trim (itself bounded by the engine's fetch
  caps), so it is a reliable lower bound even when matches were trimmed. With
  `line_range` it is the count of in-range matches.
- `files_searched` / `files_with_matches` — always reported.
- `useless: true` + `note: "No matches found"` — zero matches, no timeout.
  Adjust the pattern or scope instead of retrying blindly. If you were already
  past the last page, the note says so (`No more results (...)`).
- `timed_out: true` — partial results; an empty timed-out result is an
  **incomplete scan**, not proof of absence.

---

## Editing matches directly

Every shown file within the anchor window (the first 20 in encounter order)
gets an entry in the `files` array:

```text
path:      src/a.rs
file_hash: AE23
header:    ¶/abs/project/src/a.rs#AE23
```

These are real, whole-file hashline tags: minting one reads the whole file,
records the snapshot in the session rollback history, and records the surfaced
lines. To edit a match, pass the anchor's **absolute path** (the header's
`¶path#TAG`, so the path is absolute and round-trips through `fs_edit`'s path
validation) and the `file_hash` to `fs_edit` directly — no re-read is needed to
obtain the current hash:

```rust,ignore
let out = find.grep("fn double", "src")?;
let f = &out.files[0];                       // header "¶/abs/project/src/lib.rs#0C54"
let abs_path = f.header
    .trim_start_matches('¶')                 // strip the `¶` file marker (multi-byte)
    .split('#').next().unwrap();             // "/abs/project/src/lib.rs"
fs.edit(serde_json::json!({
    "targets": [{
        "path": abs_path,
        "file_hash": f.file_hash,
        "ops": "replace 1..3:\n+fn triple(x: i32) -> i32 { x * 3 }",
    }]
}))?;
```

Files beyond the anchor window surface plain, headerless matches — there are
no tags for them in this call.

---

## Errors and edge cases

| Situation | What happens |
|---|---|
| Invalid regex | Hard `Err` from the regex engine. |
| No target provided | Hard `Err` (`missing 'path' or 'paths'` / `no search targets provided`). |
| `line_range` with a directory target | Hard `Err` (`line_range requires single-file targets...`). |
| Malformed `line_range` | Hard `Err` (`line_range must be "start-end"...`). |
| Target denied by the guards | Hard `Err` from the path guard. |
| Timeout | Partial matches with `timed_out: true`, never a blind error. |

---

## Example

A complete runnable example is provided at
[`examples/find/grep.rs`](../../../crates/cosh-tools/examples/find/grep.rs).
It builds a scratch project, searches it for a function name, then uses the
returned hashline anchor to edit one of the matched files directly.

Next: [rollback — the previous page's sibling module](../fs/rollback.md), or
back to the [module overview](find.md).