# The `hashline` module: the anchored edit engine

`hashline` is the **edit engine** behind `cosh-tools`' `fs_edit` tool. It
parses a small, line-anchored patch language and applies the resulting edits
to files — safely, with content-hash validation against drift and, when the
file changed between read and edit, automatic 3-way-merge recovery against a
cached snapshot.

```
cosh-tools fs_edit  ──  hashline text  ──▶  Patch::parse  ──▶  Patcher::apply  ──▶  file
                    (¶path#TAG + ops)      (sections)          (validate + apply)
```

If you only ever use `cosh-tools`, you never touch this module directly —
`fs_edit` is the wrapper. This documentation exists for anyone embedding the
engine in their own tool (an editor, a CI linter, an agent harness), and to
explain *why* `fs_edit` behaves the way it does.

---

## The core idea: anchored edits, not diffs

Unified diffs describe a change as *before/after pairs*: the apply side has
to search for the old lines and guess where they moved. Hashline does the
opposite. The model anchors every edit to **concrete line numbers it just
saw**, and the file is bound to a **content hash tag** minted when it was
read:

```text
¶src/lib.rs#0A3F
replace 5..7:
+fn double(x: i32) -> i32 {
+    x * 2
+}
```

Three properties follow from this design:

1. **No searching.** `replace 5..7:` means "lines 5 through 7", nothing more.
   The payload is only the *final desired content* — never both old and new.
2. **Staleness is detectable.** The `#0A3` tag is a 4-hex fingerprint of the
   whole file's normalized text (see [`format`](format.md)). If the live file
   hashes to something else, the edit is *known* to be stale.
3. **Stale edits can often be rescued.** If the tag names a snapshot the
   session recorded (any `read`/`grep`/`fs_edit` result records one), the
   engine replays the edit against that snapshot and 3-way-merges the result
   onto the live file (see [recovery](recovery.md)).

The tag is **required** on every section that edits an existing file — the
engine refuses edits that can't be validated (`Missing hashline snapshot tag
for edit to …`). New files are created with `write`, not `edit`.

---

## The language in one glance

| Construct | Shape | Meaning |
|---|---|---|
| Section header | `¶path#TAG` | Target file, bound to content-hash `TAG` |
| Replace | `replace 5..7:` + `+rows` | Replace lines 5–7 with the `+` rows |
| Delete | `delete 5..7` | Delete lines 5–7 (no body, no colon) |
| Insert before | `insert before 5:` + `+rows` | Insert rows above line 5 |
| Insert after | `insert after 5:` + `+rows` | Insert rows below line 5 |
| Insert head | `insert head:` + `+rows` | Insert at the top of the file |
| Insert tail | `insert tail:` + `+rows` | Insert at the bottom of the file |
| Replace block | `replace block 5:` + `+rows` | Resolve line 5 to a syntactic block, then replace it (see [block](block.md)) |
| Delete block | `delete block 5` | Delete the syntactic block beginning on line 5 |

Every body row starts with `+`; a bare `-` row is rejected (`-` rows are not
valid … write `+-…` to insert a literal line starting with `-`). Ranges use
1-indexed, inclusive numbers, and `replace`/`delete` accept a single line
(`replace 5:`).

The full grammar, including every error message, lives in the
[syntax reference](input.md). The format constants (sigils, separators, hash
length) are centralized in [`format`](format.md).

---

## The pipeline, layer by layer

Hashline is deliberately split into stages with a clean boundary between
**pure parsing** and **filesystem I/O**:

| Stage | Module | Pure? | What it does |
|---|---|---|---|
| Tokenize | [tokenizer](tokenizer.md) | yes | Classify each line: header, op, payload, envelope |
| Parse | [parser](parser.md) | yes | Turn tokens into a flat list of [`Edit`](types.md) |
| Split | [input](input.md) | yes | Split a multi-section patch into per-file [`PatchSection`]s; parse lazily |
| Resolve blocks | [block](block.md) | yes | Expand `replace block N:` against file text + language |
| Apply | [apply](apply.md) | yes | Execute the edits on a text body in memory |
| Normalize | [normalize](normalize.md) | yes | BOM stripping, LF normalization, line-ending round-trip |
| Store snapshots | [snapshots](snapshots.md) | — | Record full-file versions + their tags (trait, LRU default) |
| Validate + write | [patcher](patcher.md) | no | Read file → check tag → recover → write back |
| Recover | [recovery](recovery.md) | — | 3-way-merge stale edits onto drifted content |

The boundary matters: `Patch::parse` and `apply_edits` never touch the
filesystem, so they're trivially testable and usable in memory-only
settings. The `Patcher` is the only stage that performs I/O, through the
[`Filesystem`](fs.md) trait — which is why a single patch against a
directory, a database, or an in-memory map looks identical to the parser.

---

## What the module does *not* do

- **It does not read or write files itself.** All I/O goes through the
  [`Filesystem`](fs.md) trait (`DiskFilesystem` and `InMemoryFilesystem`
  ship).
- **It does not create files.** `Patcher::prepare` errors on a missing
  target (`File not found: … Use the write tool to create new files.`) —
  creation is the `write` tool's job. (At the *parsing* level, pure
  `insert head:`/`insert tail:` literals are flagged as safe-for-new-files
  via `PatchSection::has_anchor_scoped_edit`, but the patcher still rejects
  a missing target.)
- **It does not embed a language model.** Parsing is deterministic; recovery
  is algorithmic. The module is the *plumbing*, not the intelligence.

---

## Example

A complete, runnable walkthrough lives at
[`examples/hashline/patcher.rs`](../../../crates/cosh-sdk/examples/hashline/patcher.rs):
it hashes content, parses a multi-section patch, applies edits in memory,
records snapshots, demonstrates 3-way-merge recovery when the file drifts
(external edit at the tail survives the merge), shows a hard mismatch
rejection for an unrecognized tag, and runs a real `Patcher` + disk apply.

Next: the [types](types.md) the pipeline passes around, then the
[syntax](input.md) and the [patcher](patcher.md).
