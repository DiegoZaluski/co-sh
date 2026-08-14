# The `fs` module: reading, writing, and editing files

This module is the file-system toolbox of `cosh-tools`. It exposes five
operations that an agent uses to work with source files inside a project:

| Operation | What it does |
|---|---|
| [`read`](read.md) | Read whole files, syntactic blocks, named symbols, or exact line ranges, formatted with numbered lines and content-hash tags. |
| [`write`](write.md) | Create a new file or overwrite an existing one completely. |
| [`edit`](edit.md) | Apply targeted, hash-anchored edits to one or more files. |
| [`ast_edit`](ast_edit.md) | Rewrite code structurally by matching patterns against the syntax tree. |
| [`rollback`](rollback.md) | Restore a file to a previously recorded version from the session history. |

The module is designed around a single, central type — [`Fs`](#the-fs-wrapper) —
that bundles the project configuration and dispatches each operation for you.
Because every operation goes through the same wrapper, the permission model is
consistent across all of them.

This page explains the concepts you need before diving into the individual
operations: the shared-state wrapper, the path-guard permission model, and the
two edit engines. The [type reference](types.md) documents the input and output
data structures, and each operation has its own page with full examples.

---

## The `Fs` wrapper

`Fs` is a builder-style struct that holds *shared state* for the file-system
operations. Instead of passing a pile of configuration to every function call,
you configure an `Fs` once and then call its methods:

```rust,ignore
use cosh_tools::fs::{Fs, TargetFile};

let fs = Fs::new()
    .cwd("/home/user/project")   // the project root
    .allowlist(["generated"]);   // optional extra write scope

fs.write(vec![TargetFile {
    path: "notes.txt".to_string(),
    text: "hello world\n".to_string(),
}])
.await;
```

`Fs::new()` starts with **no root set**. Until you call [`cwd`](Fs::cwd) every
path is denied, so a fresh `Fs` can never touch the disk by accident. All the
builder methods consume `self` and return it, which lets you chain them; because
`Fs` implements `Default`, `Fs::default()` is equivalent to `Fs::new()`.

The four operations, as methods on `Fs`:

| Method | Corresponding free function | Returns |
|---|---|---|
| `Fs::read(&self, targets: Vec<Target>)` | `read(metadata, FsRead)` | `Vec<ReadResult>` |
| `Fs::write(&self, targets: Vec<TargetFile>)` | `write(metadata, FsWrite)` | `Result<Vec<WriteResult>, String>` |
| `Fs::edit(&self, args: serde_json::Value)` | `edit(metadata, FsEdit)` / `ast_edit(metadata, FsAstEdit)` | `Result<Vec<EditResult>, String>` |
| `Fs::rollback(&self, path: &str, hash: &str)` | `rollback(config, metadata, path, hash)` | `Result<RollbackResult, String>` |

> **Note on the free functions.** Each operation is *also* exposed as a plain
> `async fn` that takes an explicit [`FsMetadata`](types.md) argument. These
> exist so the module can be embedded in other harnesses that already manage
> their own configuration. In practice you should prefer the `Fs` wrapper: it
> builds the metadata for you and keeps the configuration in one place. The
> rest of this documentation uses `Fs`.

### Reading the wrapper's state

A few read-only accessors let you inspect what an `Fs` is configured with:

```rust,ignore
let fs = Fs::new().cwd("/tmp/proj");
assert_eq!(fs.root(), &PathBuf::from("/tmp/proj"));
assert_eq!(fs.edit_engine(), EditEngine::Auto);
assert!(fs.allowlist_ref().is_none());
assert!(fs.blocklist_ref().is_none());
```

- `root()` returns the configured project root.
- `edit_engine()` returns which edit engine is currently selected (see below).
- `allowlist_ref()` / `blocklist_ref()` return the configured write-scope
  guards as slices, or `None` when not set.

Because `Fs` is not `Clone`, the accessors return references rather than owned
values. If you need to change the allowlist after construction, use the mutating
methods `add_allowlist_path(path)` and `remove_allowlist_path(path)` — the
former is a no-op for duplicate paths, the latter for paths not present.

### Tool descriptions

`Fs` also carries MCP tool descriptions as JSON values in the public fields
`description_read`, `description_write`, `description_edit`, and
`description_rollback`. Each is a ready-to-serve `ToolDescription`
following the [MCP Tool schema](https://modelcontextprotocol.io/specification/2025-11-25/server/tools):
`{ "name", "description", "inputSchema" }`. You can hand these straight to a
tool server without writing the schemas yourself.

---

## The path-guard permission model

Every file-system operation validates each path it touches against a set of
guards before doing anything. The guards are:

| Guard | Meaning |
|---|---|
| `root` | The project root directory. Everything inside it is accessible by default. |
| `allowlist` | Specific paths explicitly granted write access **outside** the root. |
| `blocklist` | Paths explicitly denied access, even inside the root. |

The rule of thumb:

- **Write** operations (write, edit, ast_edit, rollback) are allowed for any
  path under `root`, plus anything in the `allowlist`, minus anything in the
  `blocklist`.
- **Read** operations use the same `root`/`blocklist` but have their *own*
  allowlist: `read_allowlist` / `read_blocklist`. When unset, they fall back to
  the write-scope guards. This lets you grant read access outside the root
  without also granting write access to the same paths.

Two important matching rules:

- **Allowlist entries match exactly.** Listing a path grants access to that
  exact path only — a directory entry does *not* grant its contents. To allow
  writes to a file outside the root, list the file itself. (Relative entries
  are interpreted relative to the root.)
- **Blocklist entries match by prefix.** A blocked directory denies everything
  under it.

A path that appears in both the allowlist and the blocklist is an ambiguous
configuration and is never resolved silently. `write` treats it as a **hard
error** (`permission denied`) that aborts the batch; `read` reports it as a
per-file warning in the result.

On the builder, the methods are:

```rust,ignore
Fs::new()
    .cwd("/tmp/proj")
    .allowlist(["/tmp/proj/vendor/vendor.lock"])  // exactly this file, writable
    .blocklist(["/tmp/proj/secret"])              // denied even though inside the root
    .read_allowlist(["/tmp/shared/data.json"])    // readable but NOT writable
    .read_blocklist(["/tmp/proj/tests"])          // not even readable
```

The underlying validation and canonicalization is performed by the `PathGuard`
utility, which every operation delegates to through an `FsMetadata`.

### The read-only allowlist in practice

The distinction between `allowlist` and `read_allowlist` matters in a real
agent. Consider a model that must read the workspace's shared data directory
but must never modify it:

```rust,ignore
let fs = Fs::new()
    .cwd("/home/user/project")
    .read_allowlist(["/srv/shared-data/catalog.json"]);
```

Reads of `/srv/shared-data/catalog.json` succeed, while a write to the same path
is denied with a `permission denied` warning in the result. This is the safe
default for granting read-only visibility.

---

## The two edit engines

`Fs::edit` is the interesting one: it is a *dispatcher* over two independent
editing engines that solve different problems. Which engine runs is controlled
by the `EditEngine` enum:

| Variant | Behavior |
|---|---|
| `EditEngine::Auto` (default) | Inspect the tool arguments and pick the engine. |
| `EditEngine::Replace` | Force the hashline replace engine. |
| `EditEngine::Ast` | Force the AST structural engine. |

Selecting an engine is done with builder methods:

```rust,ignore
let auto = Fs::new().auto();                  // default, arguments decide
let replace = Fs::new().only_replace();       // hashline engine only
let ast = Fs::new().only_ast();               // AST engine only
```

When forced, the corresponding `description_edit` field is rewritten so the MCP
tool description only mentions the available engine — a model talking to a
replace-only tool must not learn that an AST engine exists, and vice versa.

### How `Auto` dispatches

In `Auto` mode, `Fs::edit` looks at the `serde_json::Value` it received:

- If it contains a **non-empty `targets` array**, the hashline replace engine
  runs.
- If it contains an **`ast` object**, the AST structural engine runs.
- Providing **both** is an error; providing **neither** returns a correction
  prompt.

`Auto` also detects when the agent filled an argument with the *other* engine's
schema — for example, AST metavariables like `$NAME` inside `targets[].ops`, or
a `file_hash` inside `ast`. It returns a descriptive correction prompt instead
of failing silently or, worse, applying the wrong engine.

The two engines are described in depth on their own pages:
[`edit` — the hashline replace engine](edit.md) and
[`ast_edit` — the AST structural engine](ast_edit.md). The short version:

- **Replace** anchors edits on line numbers plus a content-hash tag, supports
  replace / delete / insert / block operations, and attempts a 3-way merge when
  the file changed since it was read. Order carries intention: targets are
  applied in the order listed.
- **AST** matches tree-sitter patterns (`pat`) and rewrites each match to a
  template (`out`), using metavariables `$NAME` / `$$$NAME`. Files are resolved
  from paths, directories, or globs and processed in sorted order.

### Failure semantics differ by engine

When a multi-target batch fails, the two engines report the failure
differently, because *order* means different things to them:

- **Replace** applies targets in your order, so target N may depend on targets
  before it. When N fails the batch stops: targets before N remain applied (and
  are returned with their fresh hash tags in `EditBatchError::applied`), and
  targets after N are deliberately skipped (`EditBatchError::skipped`) as a
  *consequence* of N failing — never reported as independent failures.
- **AST** processes files in sorted, deduplicated order, so there is no
  caller-intended order to preserve. A failure therefore aborts with a plain
  error string naming the failing file.

---

## Summary

- Configure one `Fs`, call its methods — no per-call configuration.
- Guard every path with root + allowlist + blocklist; reads have their own
  read-only allowlist that falls back to the write one.
- `edit` dispatches between the replace and AST engines; you can force either
  or let the arguments decide.

Next: the [data types used by every operation](types.md), then each operation
in turn — [read](read.md), [write](write.md), [edit](edit.md),
[ast_edit](ast_edit.md), and [rollback](rollback.md).
