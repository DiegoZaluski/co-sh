# `path_guard` — filesystem path validation and security

The `path_guard` module provides centralized path validation for filesystem operations. It combines lexical validation (allowlist/blocklist, project root containment) with filesystem canonicalization (symlink resolution, real path discovery) to ensure consistent security checks across all tools.

---

## The security model

Every tool that accepts filesystem paths should use this guard to enforce a consistent security policy:

1. **Blocklist priority** — paths in the blocklist are always denied, regardless of other rules.
2. **Project root containment** — paths outside the project root are denied unless explicitly allowed.
3. **Allowlist exceptions** — paths in the allowlist are allowed even if outside the root.
4. **Scratch directory exemption** — the harness scratch directory (`<OS temp>/cosh`) is allowed for tool-output logs, but still subject to the blocklist.
5. **Symlink protection** — canonicalization catches symlink escapes from the project root.

---

## `PathGuard`

The central guard that enforces the security policy.

```rust
pub struct PathGuard { /* fields are private */ }
```

### Construction

```rust
pub fn new(root: &Path, allowlist: Option<&[PathBuf]>, blocklist: Option<&[PathBuf]>) -> Self
```

Creates a guard with the project root and optional allow/block lists. The lists are cloned internally so the caller retains ownership.

```rust,ignore
use cosh_tools::util::path_guard::PathGuard;
use std::path::PathBuf;

let root = PathBuf::from("/home/user/project");
let allowlist = Some(&[PathBuf::from("/etc/config")]);
let blocklist = Some(&[PathBuf::from("/home/user/project/secrets")]);

let guard = PathGuard::new(&root, allowlist, blocklist);
```

### Resolving paths

```rust
pub fn resolve(&self, path: &str) -> Result<PathBuf, String>
```

Validates and canonicalizes `path` against the guard's root, allowlist, and blocklist.

**Returns:**
- `Ok(PathBuf)` — the canonicalized (real) path on the filesystem
- `Err(String)` — a descriptive error explaining why the path was denied

**Errors occur when:**
- The path is blocked by the blocklist.
- The path is outside the project root and not in the allowlist.
- The path is in both the allowlist and blocklist simultaneously.
- The path cannot be resolved on the filesystem (or escapes via symlinks).

**Canonicalization strategy:**
1. Canonicalize the project root and strip Windows verbatim prefix.
2. Normalize the input path and check if it's inside the project root.
3. Try to canonicalize the input path (resolves symlinks and catches escapes).
4. If canonicalization fails (file doesn't exist), try canonicalizing the parent directory.
5. If the parent also fails but the path is inside the project root, use the normalized path directly.
6. Strip Windows verbatim prefix from the resolved path.
7. Final containment check: if the path was in the root before canonicalization, verify the resolved path is still within the canonicalized root (catches symlink escapes).

```rust,ignore
let safe_path = guard.resolve("src/main.rs")?;
// safe_path is the real path, with symlinks resolved
```

### Accessors

```rust
pub const fn root(&self) -> &PathBuf
pub fn allowlist(&self) -> Option<&[PathBuf]>
pub fn blocklist(&self) -> Option<&[PathBuf]>
```

Read-only access to the guard's configuration.

### Session-level persistence

```rust
pub fn add_allowlist_path(&mut self, path: PathBuf)
pub fn remove_allowlist_path(&mut self, path: &Path)
```

- `add_allowlist_path` adds a path to the allowlist (creates the list if `None`). Duplicates are ignored.
- `remove_allowlist_path` removes a path from the allowlist (no-op if not present). Used for AllowOnce cleanup. If the allowlist becomes empty after removal, it stays as `Some(vec![])` to preserve the distinction between "no allowlist" (deny everything) and "empty allowlist" (deny everything outside root).

---

## `GuardResult`

The result of a path validation check.

```rust
pub enum GuardResult {
    Allowed(PathBuf),   // Path is allowed, contains the normalized (safe) path
    Denied(String),     // Path is explicitly denied
    Mismatch(String),   // Configuration error (path matched both allowlist and blocklist)
}
```

---

## `normalize_path`

Lexical path normalization (no filesystem access).

```rust
pub fn normalize_path(path: &Path, root: &Path) -> PathBuf
```

Resolves `.` and `..` components lexically. If the path is relative, it is first made absolute against the given root.

**Purely lexical** — no filesystem access, no symlink resolution. Used by `validate_path` to compare paths on equal footing before filesystem canonicalization.

```rust,ignore
use std::path::Path;

let normalized = normalize_path(Path::new("../src"), Path::new("/home/user/project"));
// Returns "/home/user/src" (parent directory resolved)
```

---

## `validate_path`

The core validation logic (no filesystem canonicalization).

```rust
pub fn validate_path(
    path: &str,
    root: &Path,
    allowlist: Option<&[PathBuf]>,
    blocklist: Option<&[PathBuf]>,
) -> GuardResult
```

**Validation order:**
1. Normalize the path (resolve `.`/`..`).
2. Check if the path is in the blocklist (prefix match or exact match).
3. Check if the path is in the allowlist (exact match only).
4. Check if the path is inside the project root (prefix match).
5. Check if the path is in the harness scratch directory (`<OS temp>/cosh`).

**Rules:**
- Blocklist always has priority.
- If a path is in both allowlist and blocklist, returns `Mismatch` (configuration error).
- A path is allowed if it is inside the root, in the allowlist, or in the scratch directory (and not blocked).

```rust,ignore
use cosh_tools::util::path_guard::validate_path;

let result = validate_path(
    "/etc/config",
    Path::new("/home/user/project"),
    Some(&[PathBuf::from("/etc/config")]),
    None,
);
// Returns GuardResult::Allowed(PathBuf::from("/etc/config"))
```

---

## `assert_editable_file`

Refuses to modify auto-generated files.

```rust
pub fn assert_editable_file(path: &Path) -> Result<(), String>
```

Called **only** by write-path tools (`fs::write`, `fs::edit`, `fs::ast_edit`) because those tools replace file content. Read-only tools never call it.

**The check:**
- Scans the first 10 lines of the file (lazy line-by-line read, never loads the whole file).
- Looks for conventional auto-generated markers (case-insensitive):
  - `DO NOT EDIT`
  - `@generated`
  - `automatically generated`
  - `auto-generated`
  - `autogenerated`
- Returns an error if a marker is found, with an actionable message.

**Passes when:**
- The file doesn't exist (new file).
- The file is unreadable (caller surfaces its own read error).
- The file has no auto-generated marker in the first 10 lines.

```rust,ignore
use cosh_tools::util::path_guard::assert_editable_file;

assert_editable_file(Path::new("src/main.rs"))?; // OK
assert_editable_file(Path::new("generated/proto.rs"))?; // Error if header has marker
```

---

## `validate_asset_path`

Validates asset paths for skills `read_asset`.

```rust
pub fn validate_asset_path(base_dir: &Path, asset_path: &str) -> Result<PathBuf, String>
```

**Constraints:**
- The asset path must be relative (no absolute paths).
- Must not contain `..` components (no parent directory references).
- After joining with `base_dir` and canonicalizing, must stay within `base_dir`.

**Returns:**
- `Ok(PathBuf)` — the canonicalized asset path
- `Err(String)` — describes the violation or why the path couldn't be resolved

```rust,ignore
use cosh_tools::util::path_guard::validate_asset_path;

let asset = validate_asset_path(
    Path::new("/home/user/skills/my-skill"),
    "assets/icon.png"
)?;
// asset is the canonicalized path, guaranteed to be within the skill directory
```

---

## Constants

### `HARNESS_SCRATCH_DIR`

```rust
pub const HARNESS_SCRATCH_DIR: &str = "cosh";
```

The name of the harness scratch directory under the OS temp directory. The harness writes truncated tool-output logs here, and the model reads them back with `fs_read`/`find_grep`. This directory is exempt from the outside-root denial (but never from the blocklist).

### `AUTO_GENERATED_SCAN_LINES`

```rust
const AUTO_GENERATED_SCAN_LINES: usize = 10;
```

How many leading lines are scanned for auto-generated markers (implementation detail). Generated-file headers always live in the first few lines, so a small scan window keeps the check cheap.

---

## Windows verbatim path handling

Windows `canonicalize` returns verbatim paths (`\\?\C:\...`). Every consumer downstream (rollback keys, seen-lines keys, hashline headers, diagnostics) spells paths the plain way, so a verbatim-resolved key would silently split the store in two.

The module strips the verbatim prefix in both directions:
- `\\?\UNC\server\share` → `\\server\share`
- `\\?\C:\path` → `C:\path`

This ensures consistent path spelling across the codebase.

---

## Summary

- `PathGuard` enforces a consistent security policy: blocklist priority, project root containment, allowlist exceptions, scratch directory exemption, and symlink protection.
- `resolve` validates and canonicalizes paths, returning the real filesystem path or a descriptive error.
- `GuardResult` indicates whether a path is allowed, denied, or mismatched (configuration error).
- `normalize_path` performs lexical normalization (resolves `.`/`..`, no filesystem access).
- `validate_path` is the core validation logic (no filesystem canonicalization, used by `resolve`).
- `assert_editable_file` refuses to modify auto-generated files (scans first 10 lines for conventional markers).
- `validate_asset_path` validates skill asset paths (must be relative, no `..`, must stay within base directory).
- `HARNESS_SCRATCH_DIR = "cosh"` is the harness scratch directory name (exempt from outside-root denial).
- Windows verbatim prefixes are stripped to maintain consistent path spelling.

Next: [fs — filesystem tools](../fs/fs.md).
