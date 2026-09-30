//! The weights installer: fill the local hf-hub cache for one checkpoint
//! from a hub repo, then hand the snapshot to the loader.
//!
//! The installer is cache-first and repo-agnostic: the repo id, subfolder,
//! revision and graph candidates all arrive as parameters — no default repo
//! lives in this crate. The application owns the default (setup.json / env).
//!
//! Integrity: every network download is verified before the install
//! succeeds. When the caller supplies an explicit digest map it is applied
//! verbatim ([`verify_digests`]); otherwise the repo's `sha256sums.txt`
//! (the format `sha256sum -c` writes: `<digest>␠␠<path>`), when published,
//! is parsed and the entries scoped to this checkpoint's prefix are
//! verified. A digest mismatch fails the install AND evicts the revision's
//! cache ref, so a later load can never serve the tampered snapshot from a
//! cache hit. Cache hits are not re-verified: the blobs were verified when
//! they were downloaded and the cache is content-addressed by etag.
//!
//! Progress: downloads run through [`hf_hub::api::sync`] with the built-in
//! progress bar DISABLED (`ApiBuilder::with_progress(false)`) and the
//! caller's [`ProgressCallbacks`] in its place — the library never writes
//! to the terminal. The callbacks fire only for bytes that actually cross
//! the network; cache hits skip them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::hub::{resolve_revision, verify_digests};

/// The root sums file every mirror publishes (see the exporter's `publish`).
const SUMS_FILE: &str = "sha256sums.txt";
/// The tokenizer artifact the loader probes (inside the subfolder prefix).
const TOKENIZER_JSON: &str = "tokenizer/tokenizer.json";

/// Progress callbacks for the installer's network downloads.
///
/// Three boxed closures so an embedder never has to implement a trait or
/// touch hf-hub types: `init` starts one file (`size` = total bytes,
/// `filename` = repo-relative path — called again on resume/retry, so the
/// receiver must RESET its per-file counter, not accumulate), `update`
/// adds `size` downloaded bytes, `finish` closes the file.
///
/// `Clone` shares one sink: hf-hub takes the progress by value per
/// download, so the installer clones the handle per file.
#[derive(Clone)]
pub struct ProgressCallbacks {
    inner: std::sync::Arc<std::sync::Mutex<Callbacks>>,
}

struct Callbacks {
    init: Box<dyn FnMut(usize, &str) + Send>,
    update: Box<dyn FnMut(usize) + Send>,
    finish: Box<dyn FnMut() + Send>,
}

impl ProgressCallbacks {
    /// No-op callbacks: the load path uses these (a loader must not own a
    /// progress UI).
    pub fn silent() -> Self {
        Self::new(|_, _| {}, |_| {}, || {})
    }

    /// Build callbacks from three closures.
    pub fn new(
        init: impl FnMut(usize, &str) + Send + 'static,
        update: impl FnMut(usize) + Send + 'static,
        finish: impl FnMut() + Send + 'static,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(Callbacks {
                init: Box::new(init),
                update: Box::new(update),
                finish: Box::new(finish),
            })),
        }
    }
}

// The `Progress` trait lives at `hf_hub::api::Progress` (api/sync.rs only
// privately imports it), and hf-hub takes it BY VALUE per download — so the
// handle is a shared, cloneable sink.
impl hf_hub::api::Progress for ProgressCallbacks {
    fn init(&mut self, size: usize, filename: &str) {
        if let Ok(mut cb) = self.inner.lock() {
            (cb.init)(size, filename);
        }
    }
    fn update(&mut self, size: usize) {
        if let Ok(mut cb) = self.inner.lock() {
            (cb.update)(size);
        }
    }
    fn finish(&mut self) {
        if let Ok(mut cb) = self.inner.lock() {
            (cb.finish)();
        }
    }
}

/// One install request. Everything is a parameter: the crate carries no
/// default repo (the application's configuration does).
pub struct InstallRequest<'a> {
    /// The hub repo id (`owner/name`). A local directory path is a caller
    /// error — local checkpoints load directly, no install.
    pub repo: &'a str,
    /// The checkpoint's subfolder inside a bundling repo (`None` = root).
    pub subfolder: Option<&'a str>,
    /// Pin the download to a commit SHA/branch/tag. `None` resolves the
    /// Hub default revision and the result carries the resolved SHA so the
    /// caller can pin subsequent installs to it.
    pub revision: Option<&'a str>,
    /// Graph file candidates in preference order; the first one present in
    /// the repo wins (e.g. `["laya.int8.onnx", "laya.onnx"]` prefers the
    /// quantized graph and falls back when the mirror does not ship it).
    pub graphs: &'a [&'a str],
    /// Explicit artifact → SHA-256 map ([`verify_digests`] semantics). When
    /// `None`, the repo's `sha256sums.txt` — when published — supplies the
    /// digests, scoped to this checkpoint.
    pub expected_sha256: Option<&'a HashMap<String, String>>,
    /// Progress sink for the network downloads (cache hits never fire it).
    pub progress: ProgressCallbacks,
}

/// A completed install: the snapshot the loader consumes plus the facts the
/// caller needs to pin and to configure the load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCheckpoint {
    /// The snapshot root (`<cache>/models--*/snapshots/<sha>`) — the
    /// checkpoint directory BEFORE any subfolder is applied, matching what
    /// the loader resolves and joins itself.
    pub snapshot: PathBuf,
    /// The commit SHA the artifacts came from (requested, resolved, or
    /// derived from the cached snapshot).
    pub revision: String,
    /// The graph file that was installed, repo-relative WITH the subfolder
    /// prefix (e.g. `english/laya.int8.onnx`) — the name to pass as
    /// [`crate::LoadOptions::onnx_path`] relative to the checkpoint dir.
    pub graph: String,
}

/// The cache-only probe: when every artifact the loader needs is already in
/// the local cache, return `(snapshot, revision, graph)` without touching
/// the network. `None` means a download is required.
///
/// The probe enumerates the repo's snapshot directories directly instead of
/// walking a single ref: hf-hub 0.5 only writes `refs/<revision>` for the
/// revision a download was made under, so a snapshot installed under its
/// SHA (the pinned-download contract) is invisible to a `refs/main` probe.
/// Enumeration sees every installed revision; an explicit `revision` is
/// probed first (the SHA itself, then the SHA a `refs/<revision>` pointer
/// resolves to).
pub fn cache_status(
    repo: &str,
    subfolder: Option<&str>,
    graphs: &[&str],
    revision: Option<&str>,
) -> Option<(PathBuf, String, String)> {
    let cache = hf_hub::Cache::from_env();
    let hf_repo = hf_hub::Repo::new(repo.to_string(), hf_hub::RepoType::Model);
    let snapshots = cache.path().join(hf_repo.folder_name()).join("snapshots");

    let prefix = subfolder_prefix(subfolder);
    let config_rel = format!("{}rl_agent_config.json", prefix);

    // Candidate revisions. An explicit pin is a HARD constraint: probe
    // ONLY the pinned revision (the SHA itself, then the commit a
    // branch/tag ref file points at) — a cached snapshot from another
    // revision must never satisfy a pin.
    //
    // Without a pin, every installed snapshot is a candidate, MOST
    // RECENT FIRST. The unpinned probe serves the newest install, which
    // keeps the daemon's revision-less load (Kind::Custom carries no
    // revision over the wire, by design) on the snapshot the app last
    // installed — and makes the upgrade path work: clear the pin, send a
    // message, a fresh download lands a newer snapshot, and the probe
    // picks THAT one instead of freezing on the stale lexicographic
    // minimum. The invariant this relies on: every snapshot under the
    // mirror's cache dir was digest-verified at install time, so any
    // candidate is safe to serve.
    let mut revisions: Vec<String> = Vec::new();
    match resolve_revision(repo, revision) {
        Some(r) => {
            revisions.push(r.clone());
            if let Ok(sha) = std::fs::read_to_string(
                cache
                    .path()
                    .join(hf_repo.folder_name())
                    .join("refs")
                    .join(&r),
            ) {
                let sha = sha.trim().to_string();
                if !sha.is_empty() {
                    revisions.push(sha);
                }
            }
        }
        None => {
            if let Ok(entries) = std::fs::read_dir(&snapshots) {
                // Sort by modified time, newest first (ties broken by
                // name for determinism).
                let mut dirs: Vec<(std::time::SystemTime, String)> = entries
                    .flatten()
                    .map(|e| {
                        (
                            e.metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                            e.file_name().to_string_lossy().to_string(),
                        )
                    })
                    .collect();
                dirs.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
                revisions.extend(dirs.into_iter().map(|(_, name)| name));
            }
        }
    }

    for rev in revisions {
        let snapshot = snapshots.join(&rev);
        for graph in graphs {
            let graph_rel = format!("{prefix}{graph}");
            // All three essentials cached → the loader can serve offline.
            // The tokenizer check mirrors the loader's selection rule
            // exactly (`tokenizer_usable`): a snapshot whose `tokenizer/`
            // directory exists but carries no `tokenizer.json` would pass a
            // naive presence probe and then FAIL in the loader, which picks
            // `tokenizer/` whenever the directory exists, with no fallback.
            let cfg = snapshot.join(&config_rel);
            let graph_file = snapshot.join(&graph_rel);
            if cfg.is_file() && tokenizer_usable(&snapshot, &prefix) && graph_file.is_file() {
                return Some((snapshot, rev, graph_rel));
            }
        }
    }
    None
}

/// Whether `snapshot` can serve the loader's tokenizer selection rule:
/// the loader uses the `tokenizer/` directory whenever it EXISTS (no
/// fallback), else the encoder directory named by the config. So the
/// snapshot is usable when `tokenizer/tokenizer.json` is present, or when
/// the `tokenizer/` directory is absent AND `encoder/tokenizer.json` is
/// present — a `tokenizer/` directory that exists but carries no
/// `tokenizer.json` must REJECT the snapshot.
fn tokenizer_usable(snapshot: &Path, prefix: &str) -> bool {
    let tok = snapshot.join(format!("{prefix}{TOKENIZER_JSON}"));
    if tok.is_file() {
        return true;
    }
    match tok.parent() {
        Some(dir) if dir.exists() => false,
        _ => snapshot
            .join(format!("{prefix}encoder/tokenizer.json"))
            .is_file(),
    }
}

/// Install one checkpoint into the local hf-hub cache.
///
/// Cache-first: a fully cached checkpoint returns immediately (no network,
/// no progress callbacks). Otherwise the repo listing is resolved, every
/// artifact the loader reads is downloaded with the built-in progress bar
/// disabled and the caller's callbacks in its place, and the whole set is
/// digest-verified before the install reports success. On verification
/// failure the revision's cache ref is evicted so the tampered snapshot can
/// never be served from a cache hit.
pub fn install(req: InstallRequest<'_>) -> Result<InstalledCheckpoint> {
    if req.repo.is_empty() {
        return Err(Error::Value("cosh-onnx: empty hub repo id".to_string()));
    }
    if req.graphs.is_empty() {
        return Err(Error::Value(
            "cosh-onnx: install requires at least one graph candidate".to_string(),
        ));
    }
    if Path::new(req.repo).is_dir() {
        return Err(Error::Value(format!(
            "cosh-onnx: '{}' is a local directory; local checkpoints load directly, no install",
            req.repo
        )));
    }

    // Fast path: the essentials are already cached — no network at all.
    if let Some((snapshot, revision, graph)) =
        cache_status(req.repo, req.subfolder, req.graphs, req.revision)
    {
        // An explicit digest map still applies on the cache path (the
        // caller asked for verification of THIS snapshot, whatever fetched
        // it). Sums-based verification is install-time only: the blobs were
        // verified when downloaded and the cache is content-addressed.
        //
        // The map gets the SAME normalization as the network path
        // (`normalize_caller_map`): special keys point at the graph
        // actually being served, ordinary keys are checkpoint-relative and
        // verification here runs against the snapshot ROOT. And like every
        // integrity rejection, a failed verification EVICTS the snapshot:
        // `cache_status` only checks artifact presence, so a surviving
        // snapshot dir would be served as a cache hit on the next turn and
        // bypass checksum validation entirely.
        if let Some(map) = req.expected_sha256 {
            let prefix = subfolder_prefix(req.subfolder);
            let digests = normalize_caller_map(map, &prefix, &graph);
            if !digests.contains_key(&graph) {
                evict_ref(req.repo, &revision);
                let _ = std::fs::remove_dir_all(&snapshot);
                return Err(Error::Runtime(format!(
                    "cosh-onnx: {}: the caller digest map does not cover the \
                     ONNX graph ({graph}); refusing to serve unverified",
                    req.repo
                )));
            }
            let graph_path = snapshot.join(&graph);
            if let Err(e) = verify_digests(&snapshot, Some(&digests), Some(&graph_path)) {
                evict_ref(req.repo, &revision);
                let _ = std::fs::remove_dir_all(&snapshot);
                return Err(e);
            }
        }
        return Ok(InstalledCheckpoint {
            snapshot,
            revision,
            graph,
        });
    }

    // Network path. hf-hub reads the token from the cache token file only;
    // layer the HF_TOKEN environment variable over it the way
    // huggingface_hub does, for gated repositories. The built-in progress
    // bar is DISABLED — the caller's callbacks replace it, and nothing the
    // library writes can reach the terminal.
    let mut builder = hf_hub::api::sync::ApiBuilder::from_env().with_progress(false);
    if let Ok(token) = std::env::var("HF_TOKEN")
        && !token.is_empty()
    {
        builder = builder.with_token(Some(token));
    }
    let api = builder
        .build()
        .map_err(|e| Error::Runtime(format!("cosh-onnx: hub client failed: {}", e)))?;
    let base = match resolve_revision(req.repo, req.revision) {
        Some(r) => hf_hub::Repo::with_revision(req.repo.to_string(), hf_hub::RepoType::Model, r),
        None => hf_hub::Repo::new(req.repo.to_string(), hf_hub::RepoType::Model),
    };
    let repo_api = api.repo(base);
    let info = repo_api
        .info()
        .map_err(|e| Error::Runtime(format!("cosh-onnx: could not resolve {}: {}", req.repo, e)))?;
    // Pin every download to the revision the listing came from, so a stale
    // `refs/main` pointer cannot mix files from two revisions.
    let pinned = api.repo(hf_hub::Repo::with_revision(
        req.repo.to_string(),
        hf_hub::RepoType::Model,
        info.sha.clone(),
    ));
    let cache_repo = hf_hub::Cache::from_env().repo(hf_hub::Repo::with_revision(
        req.repo.to_string(),
        hf_hub::RepoType::Model,
        info.sha.clone(),
    ));

    let prefix = subfolder_prefix(req.subfolder);
    let config_rel = format!("{}rl_agent_config.json", prefix);

    // The graph: the first candidate the repo actually ships.
    let graph = req
        .graphs
        .iter()
        .find(|g| {
            info.siblings
                .iter()
                .any(|s| s.rfilename == format!("{prefix}{g}"))
        })
        .ok_or_else(|| {
            Error::Runtime(format!(
                "cosh-onnx: {} ships none of the graphs {}{}requested",
                req.repo,
                prefix,
                req.graphs.join(", ")
            ))
        })?
        .to_string();
    let graph_rel = format!("{prefix}{graph}");

    // Everything the loader reads, plus the sums file when published.
    let mut wanted: Vec<String> = vec![config_rel.clone(), graph_rel.clone()];
    for sib in &info.siblings {
        let Some(stripped) = sib.rfilename.strip_prefix(&prefix) else {
            continue;
        };
        if stripped.starts_with("tokenizer/") || stripped.starts_with("encoder/") {
            wanted.push(sib.rfilename.clone());
        }
    }
    if req.expected_sha256.is_none() && info.siblings.iter().any(|s| s.rfilename == SUMS_FILE) {
        wanted.push(SUMS_FILE.to_string());
    }

    let progress = req.progress;
    for name in &wanted {
        // Per-file cache check: a partially cached checkpoint tops up, and
        // cached bytes never fire the progress callbacks.
        if cache_repo.get(name).is_some() {
            continue;
        }
        let path = pinned
            .download_with_progress(name, progress.clone())
            .map_err(|e| {
                Error::Runtime(format!("cosh-onnx: could not download {}: {}", name, e))
            })?;
        let _ = path;
    }

    let Some(cfg_file) = cache_repo.get(&config_rel) else {
        return Err(Error::Runtime(format!(
            "Incompatible model: {} does not contain 'rl_agent_config.json'.",
            crate::pycompat::py_repr_str(req.repo)
        )));
    };

    // The snapshot root: the config sits at
    // <cache>/models--*/snapshots/<sha>[/<sub>]/rl_agent_config.json.
    // Pop ONE parent per subfolder path component — a nested subfolder
    // ("a/b") must not leave `a/` on the root.
    let mut snapshot = cfg_file.parent().unwrap_or(Path::new(".")).to_path_buf();
    if let Some(sub) = req.subfolder {
        for _ in sub.split('/').filter(|c| !c.is_empty()) {
            snapshot = snapshot.parent().unwrap_or(Path::new(".")).to_path_buf();
        }
    }

    // Digests before success: caller-supplied map wins; otherwise the
    // published sums file scoped to this checkpoint's prefix. A checkpoint
    // whose mirror publishes NO sums (and no caller map) is a FAILURE, not
    // a silent pass-through: the whole contract is verified supply chain —
    // an unverifiable download never installs.
    //
    // EVERY integrity rejection below evicts the freshly materialized
    // snapshot first: `cache_status` only checks artifact presence, so a
    // surviving snapshot dir would be served as a cache hit on the next
    // turn and bypass checksum validation entirely.
    let caller_map = req.expected_sha256.is_some();
    let evict = |snapshot: &Path| {
        evict_ref(req.repo, &info.sha);
        let _ = std::fs::remove_dir_all(snapshot);
    };
    let digests: HashMap<String, String> = match req.expected_sha256 {
        Some(map) => {
            // The loader's special keys point at the graph actually being
            // installed and its ordinary keys are CHECKPOINT-relative;
            // verification HERE runs against the snapshot ROOT.
            // `normalize_caller_map` does that translation — the SAME
            // helper the cache fast path uses, so the two paths cannot
            // drift.
            let map = normalize_caller_map(map, &prefix, &graph_rel);
            // A caller-supplied map REPLACES the sums manifest (a caller
            // who pins digests does not need the mirror's). But the graph
            // — the artifact worth attacking — must be covered either way.
            if !map.contains_key(&graph_rel) {
                evict(&snapshot);
                return Err(Error::Runtime(format!(
                    "cosh-onnx: {}: the caller digest map does not cover the \
                     ONNX graph ({graph_rel}); refusing to install unverified",
                    req.repo
                )));
            }
            map
        }
        None => match std::fs::read_to_string(snapshot.join(SUMS_FILE)) {
            // The published sums cover the WHOLE repo (one file, many
            // kinds); scope the verification to this checkpoint's prefix.
            Ok(text) => {
                let parsed = parse_sha256sums(&text);
                let scoped: HashMap<String, String> = parsed
                    .into_iter()
                    .filter(|(path, _)| prefix.is_empty() || path.strip_prefix(&prefix).is_some())
                    .collect();
                // Empty after parsing = a truncated/corrupt sums file (the
                // mirror always carries at least the graph + config in the
                // scoped set). Never verify nothing.
                if scoped.is_empty() {
                    evict(&snapshot);
                    return Err(Error::Runtime(format!(
                        "cosh-onnx: {}: no usable entries in {} for this \
                         checkpoint; refusing to install unverified",
                        req.repo, SUMS_FILE
                    )));
                }
                scoped
            }
            Err(e) => {
                evict(&snapshot);
                return Err(Error::Runtime(format!(
                    "cosh-onnx: {}: {} unreadable ({}); refusing to install \
                     unverified",
                    req.repo, SUMS_FILE, e
                )));
            }
        },
    };
    // Completeness BEFORE verifying — sums path ONLY (a caller-supplied
    // map verifies exactly what it lists, the upstream semantics; the
    // graph-coverage check above already guards the critical artifact):
    // every downloaded artifact must carry a digest. A manifest covering
    // only part of the wanted set (e.g. just the config) would pass the
    // verify loop while leaving the rest unverified. The sums file itself
    // is the manifest, not cargo.
    if !caller_map {
        let unverified: Vec<&str> = wanted
            .iter()
            .filter(|name| name.as_str() != SUMS_FILE)
            .map(|name| name.as_str())
            .filter(|name| !digests.contains_key(*name))
            .collect();
        if !unverified.is_empty() {
            evict(&snapshot);
            return Err(Error::Runtime(format!(
                "cosh-onnx: {}: {} does not cover the downloaded artifacts \
                 {:?}; refusing to install unverified",
                req.repo, SUMS_FILE, unverified
            )));
        }
    }
    let graph_path = snapshot.join(&graph_rel);
    if let Err(e) = verify_digests(&snapshot, Some(&digests), Some(&graph_path)) {
        // The snapshot failed verification: EVICT IT ENTIRELY. `evict_ref`
        // alone is not enough — the cache probe enumerates snapshot dirs
        // (refs are unreliable), so a surviving snapshot dir stays
        // servable. Best effort: a leftover dir only wastes disk.
        evict(&snapshot);
        return Err(e);
    }

    Ok(InstalledCheckpoint {
        snapshot,
        revision: info.sha,
        graph: graph_rel,
    })
}

/// The checkpoint's subfolder as a repo-relative path prefix: `Some("en")`
/// → `"en/"`, `None` → `""`.
fn subfolder_prefix(subfolder: Option<&str>) -> String {
    subfolder.map(|s| format!("{s}/")).unwrap_or_default()
}

/// Translate a caller-supplied digest map into SNAPSHOT-ROOT-relative keys
/// for verification inside `install`.
///
/// The loader's special keys point at the graph actually being installed;
/// its ordinary keys are CHECKPOINT-relative (the loader's own semantics —
/// its post-install re-verify applies them after the subfolder join).
/// Verification in `install` runs against the snapshot ROOT, so the special
/// keys are normalized to the graph's repo-relative path and ordinary keys
/// are prefixed into repo-relative paths; keys that already carry the
/// prefix stay as they are. Shared by the network path and the cache fast
/// path so the two cannot drift.
fn normalize_caller_map(
    map: &HashMap<String, String>,
    prefix: &str,
    graph_rel: &str,
) -> HashMap<String, String> {
    // Special keys FIRST (upstream order): they name the graph wherever it
    // sits, so they must not be treated as checkpoint-relative paths and
    // prefixed. `onnx` wins over `onnx_path` when both are present, and an
    // explicit graph key already in the map keeps its value (`or_insert`).
    // Ordinary keys are inserted in TWO passes so collisions are
    // deterministic: an already-prefixed (explicit repo-relative) key wins
    // over a bare checkpoint-relative key that prefixes to the same path —
    // HashMap iteration order must never decide which digest applies.
    let mut out: HashMap<String, String> = map
        .iter()
        .filter(|(k, _)| k.as_str() != "onnx" && k.as_str() != "onnx_path" && k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in map.iter() {
        if k == "onnx" || k == "onnx_path" {
            continue;
        }
        if !prefix.is_empty() && k.starts_with(prefix) {
            continue;
        }
        let repo_rel = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}{k}")
        };
        out.entry(repo_rel).or_insert(v.clone());
    }
    if let Some(v) = map.get("onnx").or_else(|| map.get("onnx_path")) {
        out.entry(graph_rel.to_string()).or_insert(v.clone());
    }
    out
}

/// Parse a `sha256sum -c` listing: `<hexdigest>␠␠<path>` per line. Malformed
/// lines are skipped (an unparsable entry cannot be verified anyway); a
/// file with NO valid entries yields an empty map, which the caller treats
/// as "nothing to verify".
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Two-space separator is the `sha256sum` text format; a single
        // space would be a binary-mode marker (`\<newline>` escapes), which
        // this format never produces for our paths.
        let Some((digest, path)) = line.split_once("  ") else {
            continue;
        };
        let digest = digest.trim().to_lowercase();
        let path = path.trim();
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) || path.is_empty() {
            continue;
        }
        map.insert(path.to_string(), digest);
    }
    map
}

/// Remove the cache ref for `revision` (best effort): the pointer into the
/// blob store that every cache probe resolves through. Orphaned blobs stay
/// on disk but are unreachable from probes. Resolved through
/// `Cache::from_env()` — the same resolution every cache probe uses —
/// because the `Api` handle does not expose its cache.
fn evict_ref(repo: &str, revision: &str) {
    let hf_repo = hf_hub::Repo::with_revision(
        repo.to_string(),
        hf_hub::RepoType::Model,
        revision.to_string(),
    );
    let refs = hf_hub::Cache::from_env()
        .path()
        .join(hf_repo.folder_name())
        .join("refs")
        .join(revision);
    let _ = std::fs::remove_file(refs);
}

#[cfg(test)]
mod tests;
