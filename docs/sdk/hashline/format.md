# Format, normalization, and small utilities

A grab-bag of support modules that keep the core small. Most callers never
touch these directly — they exist so the parser, patcher, and host renderers
share one source of truth.

---

## `format` — the single source of truth for the syntax

Every sigil and separator the language uses is a constant here, so the
parser, the tokenizer, the prompt, and any renderer can never drift apart:

| Constant | Value | Meaning |
|---|---|---|
| `HL_FILE_PREFIX` | `"¶"` | Section header prefix |
| `HL_FILE_HASH_SEP` | `"#"` | Path/tag separator in a header |
| `HL_FILE_HASH_LENGTH` | `4` | Hex chars in a content tag |
| `HL_PAYLOAD_REPLACE` | `"+"` | Literal body-row sigil |
| `HL_REPLACE_KEYWORD` | `"replace"` | Replace verb |
| `HL_DELETE_KEYWORD` | `"delete"` | Delete verb |
| `HL_INSERT_KEYWORD` | `"insert"` | Insert verb |
| `HL_BLOCK_KEYWORD` | `"block"` | `replace block N:` sub-keyword |
| `HL_INSERT_BEFORE` / `AFTER` / `HEAD` / `TAIL` | `"before"` / `"after"` / `"head"` / `"tail"` | Insert positions |
| `HL_HEADER_COLON` | `":"` | Op header terminator |
| `HL_RANGE_SEP` | `".."` | Range separator |
| `HL_LINE_BODY_SEP` | `"\| "` | Numbered-line separator in displays |

### Hashing

`compute_file_hash(text)` is the **content tag function**: it normalizes
the text (trim trailing `[ \t\r]` from every line so CRLF endings and
display-trimmed lines don't invalidate a tag), runs xxh32, and returns the
low 16 bits as 4 uppercase hex chars (`"{:04X}"`). Any read of
byte-identical content mints the same tag — that's what makes tags
verifiable.

### Formatting helpers

- `format_hashline_header(path, hash)` → `¶path#hash` — the header a tool
  emits after reading or editing, and the anchor for follow-up edits.
- `format_replace_header(start, end)` / `format_delete_header(start, end)` /
  `format_insert_header(&cursor)` — build op headers programmatically
  (`delete 5` for a single line, `delete 5..7` for a range).
- `format_numbered_line(n, line)` → `5| text` — one displayed line.
- `format_numbered_lines(text, start_line)` → a whole file with `N| `-style
  prefixes, hashline mode.
- `describe_anchor_examples(prefix)` → `"160", "42", "7"` — example anchors
  for error messages.
- `hl_file_hash_re_raw()` / `hl_line_body_sep_re_raw()` — regex fragments
  for embedding in larger patterns.

---

## `normalize` — text-shape round-trip

The patcher canonicalizes file text to LF before applying edits and restores
the original shape on write-back:

- `strip_bom(content)` → `BomResult { bom, text }` — strips a UTF-8 BOM,
  returning both halves.
- `detect_line_ending(content)` → `LineEnding::Crlf | Lf` — first line
  ending wins; defaults to `Lf`.
- `normalize_to_lf(text)` — every `\r\n`/`\r` → `\n`.
- `restore_line_endings(text, ending)` — LF text back to CRLF if needed.

---

## `prefixes` — stripping display prefixes from payloads

Payloads authored against `read`/`search` output may carry display prefixes
(line numbers `5| `, diff `+`, or `>>>`/`>>` continuation markers).
`strip_new_line_prefixes` (opportunistic) and `strip_hashline_prefixes`
(strict — only when *every* content line is prefixed) recover the raw
text before tokenizing; `hashline_parse_text` is the single-call wrapper.
Truncation notices like `[Showing lines 1-10 of 42 … Use :L5` are also
detected and dropped.

---

## `diff` and `diff_preview` — showing what changed

- `structured_patch(old, new, context)` → a `Patch` of `Hunk`s —
  a minimal structured diff used by recovery and previews.
- `apply_patch(target, &patch)` → `Option<String>` — applies a structured
  patch, `None` when it doesn't fit cleanly.
- `Patch::to_unified_diff(old_path, new_path)` — render as classic unified
  diff.
- `build_compact_diff_preview(old, new, &options)` →
  `CompactDiffPreview { preview, added_lines, removed_lines }` — a bounded
  preview that truncates unchanged runs (`max_unchanged_run`, default 2)
  so a huge edit still produces a reviewable snippet.

---

## `stream` — chunked formatting

`stream_hash_lines(source, &StreamOptions)` → `Vec<String>` — formats a
file with hashline line prefixes in bounded chunks (default 200 lines / 64
KiB per chunk, configurable via `StreamOptions`), so a renderer can stream
a large file without holding one giant string.

---

## `messages` — the exact words

All error and warning strings are centralized as named constants
(`EMPTY_REPLACE`, `DELETE_TAKES_NO_BODY`, `BARE_BODY_AUTO_PIPED_WARNING`,
the `RECOVERY_*` banners, `HEADTAIL_DRIFT_WARNING`, …) plus two builders:
`missing_snapshot_tag_message(path)` and `block_unresolved_message(line)`.
The full text of each appears inline in [the syntax reference](input.md),
[apply](apply.md), [patcher](patcher.md), and [recovery](recovery.md).
`MISMATCH_CONTEXT` (2) sets the context window in mismatch diagnostics.
The envelope markers `*** Begin Patch` / `*** End Patch` / `*** Abort` are
also defined here. (A few constants — e.g.
`REPLACE_PAIR_COALESCED_WARNING` — are defined but not yet emitted by any
code path; treat the constants as the source of truth for wording.)
