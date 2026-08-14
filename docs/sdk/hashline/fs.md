# Storage seams: `fs` and `snapshots`

The patcher never touches disk directly — it works through two traits that
let the same engine run against any backing store:

- [`Filesystem`](fs.md) — where file *content* comes from and goes to.
- [`SnapshotStore`](snapshots.md) — where full-file *versions* are cached so
  stale tags can be recovered.

This page covers both. They're the two seams to implement when embedding
hashline in a new host.

---

## `Filesystem` — the content seam

```rust
pub trait Filesystem {
    async fn read_text(&self, path: &str) -> Result<String>;
    async fn write_text(&self, path: &str, content: &str) -> Result<WriteResult>;
    async fn preflight_write(&self, path: &str) -> Result<()> { Ok(()) }  // default no-op
    async fn exists(&self, path: &str) -> Result<bool> { /* probes read_text */ }
    async fn canonical_path(&self, path: &str) -> String { path.to_string() }
}
```

The trait is intentionally minimal — three required behaviors (read, write,
probe) plus two defaults — so any backing store can adapt: disk, memory, an
LSP text-document protocol, a Git tree, a VFS.

Contract details:

- **Raw text only.** The patcher does its own BOM stripping and LF
  normalization between `read_text` and `write_text`; the FS deals in raw
  strings.
- **`read_text` MUST signal missing files with a not-found error.** The
  patcher detects create-vs-update (and the "use the write tool" rejection)
  by checking `is_not_found(err)`. The module ships `NotFoundError` (with a
  `path()`, `Display` "File not found: {path}", and a `code`-compatible
  shape) plus `is_not_found`, which walks the error chain and also accepts
  `std::io::ErrorKind::NotFound`.
- **`write_text` returns `WriteResult { text }`** — the *actual* final text,
  so adapters that transform on serialization (notebooks, formatters) can
  report what really landed.
- **`canonical_path`** is the cache key. Default identity; override to
  return absolute paths so snapshot producers and consumers agree on keys.

### Ships with the module

- `DiskFilesystem` — `std::fs`/`tokio::fs` backed; the default for CLI use.
  Paths are accepted as-is; callers own any cwd or sandbox resolution.
- `InMemoryFilesystem` — a `Mutex<HashMap>` with convenient synchronous
  helpers (`set`, `get`, `delete`, `clear`, `entries`) for tests, sandboxes,
  and dry runs.

---

## `SnapshotStore` — the version seam

```rust
pub trait SnapshotStore {
    fn head(&mut self, path: &str) -> Option<Snapshot>;
    fn history(&mut self, path: &str) -> Vec<Snapshot>;            // newest first
    fn by_hash(&mut self, path: &str, hash: &str) -> Option<Snapshot>;
    fn record(&mut self, path: &str, full_text: &str) -> String;   // returns the tag
    fn seen_lines(&mut self, path: &str, hash: &str) -> Vec<(u32, String)>;      // default []
    fn record_seen_lines(&mut self, path: &str, hash: &str, lines: &[(u32, String)]); // default no-op
    fn invalidate(&mut self, path: &str);
    fn clear(&mut self);
}
```

A `Snapshot` is one observed version: `path`, the full normalized text, the
computed `hash` tag, and `recorded_at` (ms since epoch).

- **Producers** (read / grep / write tools, or `Patcher::commit`) call
  `record(path, full_text)` — the store hashes the text and returns the tag.
- **Consumers** (the patcher, recovery) call `by_hash(path, tag)` to resolve
  a stale tag back to the exact text that minted it.
- Recording **byte-identical content** refreshes recency and reuses the
  existing tag (read fusion — any read of the same state mints the same
  tag). Recording new content prepends a fresh version.
- `record_seen_lines` is how producers tell recovery which lines the model
  actually saw, powering the unseen-anchor warning.

### Ships with the module

`InMemorySnapshotStore` — an LRU-bounded default: up to 30 paths (default),
each with a short ring of up to 4 full-file versions (oldest dropped
first). Configure via `InMemorySnapshotStoreOptions { max_paths,
max_versions_per_path }`.

An `Arc<Mutex<S>>` also implements `SnapshotStore`, so you can share one
store across the patcher and other session tools without duplicating state.

> Note: `Patcher` requires a store. Without one, section tags would be
> unverifiable pointers — the whole safety model collapses. `Patcher::new`
> takes the store by value; `Patcher::new_shared` takes an existing
> `Arc<Mutex<S>>` (e.g. the global store from `rollback::session_store`).
