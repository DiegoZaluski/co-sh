# `write` — creating and overwriting files

`write` replaces a file's content completely. It is the simplest way to put
text on disk: create a new file, or overwrite an existing one in full. For
surgical changes to an existing file, prefer [`edit`](edit.md).

```
Fs::write(&self, targets: Vec<TargetFile>) -> Result<Vec<WriteResult>, String>
```

One [`TargetFile`](types.md#targetfile-and-fswrite--the-write-input) is a path
plus the complete new text. There is no partial write here — if you want to
change a few lines, read the file first, then [`edit`](edit.md) it.

---

## What `write` does to each target

For every `TargetFile`, `write` performs this sequence:

1. **Normalize the text.** Input is normalized exactly like `read` does — LF
   line endings and no UTF-8 BOM — so the hash and header returned here match
   what a follow-up read of the written file would report.
2. **Strip hashline display prefixes.** If the text was copied from output, any
   legacy hashline prefixes — `[path#hash]` headers and `N: text` (colon) line
   prefixes — are removed automatically when the whole content is consistently
   prefixed, and a warning reports the strip. Note: the current `read` format
   (`¶path#TAG` headers and `N| text` pipe prefixes) is **not** auto-stripped,
   so paste the raw file text without line-number prefixes.
3. **Validate the path** against the root, allowlist, and blocklist. An
   inconsistent configuration — the same path in both allowlist and blocklist —
   is a **hard error** (`permission denied`), not a per-file warning.
4. **Refuse machine-generated files.** A file that declares itself
   machine-generated (`assert_editable_file`) is not overwritten: the change
   would be lost on the next generation run. Creating a brand-new file is always
   allowed. This is a per-file warning, not a batch abort.
5. **Write and record.** The previous content (if any) is recorded in the
   session's rollback history, then the new text is written, then the new
   content is recorded. The tree-sitter parse cache is invalidated for the path.
6. **Make shebang scripts executable.** On Unix, if the content starts with
   `#!`, the file is chmod'd +x. A failed chmod never aborts the write.
7. **Report.** A [`WriteResult`](types.md#writeresult) is returned with the new
   content's hash and header — your anchor for a follow-up `edit`.

Empty text is treated as a no-op: `write` produces a warning result and sends
nothing to disk, rather than truncating a file to zero bytes.

`write` does **not** create parent directories: the parent of each `path` must
already exist, otherwise the OS write fails and is reported as a per-file
warning. Create missing directories first (for example with a `bash` run before
the write).

---

## Errors and edge cases

`write` is the one `fs` operation whose failure is explicit at the batch level,
via the `Result<Vec<WriteResult>, String>`:

| Situation | How it is reported |
|---|---|
| Path in both allowlist and blocklist | Hard `Err` — the whole batch aborts. |
| Write denied by the guards | Per-file `WriteResult.warnings`; batch continues. |
| Machine-generated file | Per-file warning; file untouched. |
| Empty text (after normalization/stripping) | Per-file warning; nothing written. |
| Disk write fails | Per-file warning with the OS error. |

Everything else is a per-file warning, keeping the batch alive. Treat the
top-level `Err` as *"your permission configuration is broken"*, not *"a file
failed to write"*.

---

## Example

A complete runnable example is provided at
[`examples/fs/write.rs`](../../../crates/cosh-tools/examples/fs/write.rs). It
creates a scratch project, writes a few files, and demonstrates the permission
guards — including why a path outside the root is refused and how the allowlist
grants access.

Next: [edit — the hashline replace engine](edit.md).