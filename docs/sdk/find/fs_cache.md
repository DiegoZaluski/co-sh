# `find::fs_cache` — the shared scan cache

`fs_cache` is a process-global, TTL-based cache of scanned directory entries.
Both [`glob`](glob.md) and [`grep`](grep.md) use it when you pass
`cache: true`: a second search of the same directory with the same visibility
settings reuses the earlier scan instead of re-walking the tree.

It exists because directory scans are the expensive part of discovery — the
matching itself is cheap. Caching the scan amortizes the walk across many
queries (think: an agent running a dozen `find_glob` calls against the same
project in a session).

## Policy

| Setting | Default | Env override |
|---|---|---|
| Cache TTL | `1000` ms | `FS_SCAN_CACHE_TTL_MS` |
| Empty-result recheck age | `200` ms | `FS_SCAN_EMPTY_RECHECK_MS` |
| Max cache entries | `16` | `FS_SCAN_CACHE_MAX_ENTRIES` |
| Walker threads | `4` | `PI_GREP_WORKERS` |

- `FS_SCAN_CACHE_TTL_MS = 0` disables caching entirely (always fresh scans).
- The cache is keyed by (root, `include_hidden`, `use_gitignore`,
  `skip_node_modules`, `ScanDetail`) — a scan with different visibility
  settings is a different cache entry.
- The oldest entry is evicted when the cache exceeds `max_cache_entries`.
- **Partial (timed-out) scans are never cached** — a snapshot that was cut
  short must not be served later as a complete scan.

## The three cache operations

```rust,ignore
pub fn get_or_scan(root: &Path, options: ScanOptions, ct: &CancelToken) -> Result<ScanResult, String>
pub fn force_rescan(root: &Path, options: ScanOptions, store: bool, ct: &CancelToken) -> Result<(Vec<GlobMatch>, bool), String>
pub fn invalidate_path(target: &Path)   // drop entries whose root is a prefix of target
pub fn invalidate_all()                 // clear everything
```

- `get_or_scan` — serve from cache if fresh, otherwise scan and store. The
  returned `ScanResult` carries `cache_age_ms` (how stale the served data was)
  and `timed_out`.
- `force_rescan` — always scan fresh; `store: false` returns the result
  without repopulating the cache.
- `invalidate_path` / `invalidate_all` — call these after **mutating** files
  so subsequent searches see the new state. This is exactly what the
  `cosh-tools` wrappers do after every `fs_edit`.

## The empty-result fast recheck

A cached *negative* can be stale: a file created 50 ms ago is invisible to a
scan cached 900 ms ago. So `glob` and `grep` implement a **fast recheck**: if
a cached scan yields **zero** matches and the cache is older than
`empty_recheck_ms()`, they `force_rescan` and try once more before returning
empty. This keeps "create file, then immediately search for it" flows working
without paying for a fresh scan on every query.

## `ScanDetail` — how much metadata

| Detail | Collects |
|---|---|
| `Minimal` | path + `file_type` only (`mtime`/`size` are `None`) |
| `Full` | path + `file_type` + `mtime` (ms) + `size` |

`glob` uses `Full` only when `sort_by_mtime` is on (it needs mtimes to rank);
otherwise it scans `Minimal`. `grep` always scans `Minimal` — it only needs
the file list.

## Types

```rust,ignore
pub struct GlobMatch {
    pub path: String,         // relative to the scan root, forward slashes
    pub file_type: FileType,  // File | Dir | Symlink
    pub mtime: Option<f64>,   // millis since Unix epoch
    pub size: Option<f64>,    // bytes, files only
}
pub enum ScanOptions { include_hidden, use_gitignore, skip_node_modules, follow_links, detail }
pub struct ScanResult { entries: Vec<GlobMatch>, cache_age_ms: u64, timed_out: bool }
```

---

Next: [task — cancellation](task.md).
