//! Supply-chain integrity for published checkpoints: opt-in pins and digest
//! checks.
//!
//! Runtime loaders keep the Hub default revision unless the caller supplies
//! one. This preserves compatibility with existing offline caches, including
//! `HF_HUB_OFFLINE=1` deployments. A model with published checkpoints may
//! offer opt-in reviewed commit SHAs (see `laya::checkpoints` for the laya
//! ones), and every loader accepts an optional SHA-256 map to verify artifact
//! integrity before weights reach the runtime.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result};

/// `resolve_revision`: pick the revision to download.
///
/// An explicit `revision` is returned unchanged. Otherwise `None` is returned
/// so the Hub client applies its normal default, preserving existing
/// online/offline caches. An empty string is falsy in Python (`revision or
/// None`), so it also means "no revision".
pub fn resolve_revision(model_id_or_path: &str, revision: Option<&str>) -> Option<String> {
    let _ = model_id_or_path;
    revision.filter(|r| !r.is_empty()).map(|r| r.to_string())
}

/// `snapshot_revision`: commit SHA a Hub snapshot directory points at, or
/// `None` for a plain directory.
///
/// `snapshot_download` returns `<cache>/snapshots/<sha>`; resolving symlinks
/// keeps this correct when the snapshot entry is a link into the blob store.
pub fn snapshot_revision(path: &Path) -> Option<String> {
    let real = path
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    let real = real.trim_end_matches('/').trim_end_matches('\\');
    let path = Path::new(real);
    let base = path.file_name()?.to_string_lossy().to_string();
    let parent = path.parent()?;
    if parent.file_name()?.to_string_lossy() == "snapshots" && !base.is_empty() {
        Some(base)
    } else {
        None
    }
}

/// Windows absolute-path check mirroring `ntpath.isabs` for the digest map.
///
/// Python's rule (3.12): a path is absolute when its `splitdrive` remainder
/// starts with a separator. That means `C:\x` and `C:/x` are absolute, while a
/// drive-relative path like `C:x` is *relative* (it joins under `model_dir`
/// and fails with the no-such-file message if absent), and the drive letter
/// check is Unicode-alpha (`É:/x` counts as absolute). A UNC prefix
/// (`\\server\share`) is always absolute.
fn ntpath_isabs(rel: &str) -> bool {
    let chars: Vec<char> = rel.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' && chars[0].is_alphabetic() {
        return chars.len() >= 3 && (chars[2] == '/' || chars[2] == '\\');
    }
    rel.starts_with("\\\\")
}

/// Python `%r` of a string for the path/file messages: delegates to the
/// shared `pycompat::py_repr_str` (single quotes unless the string itself
/// carries one and no double quote, in which case Python switches to double
/// quotes).
fn py_repr(s: &str) -> String {
    crate::pycompat::py_repr_str(s)
}

/// `verify_digests`: verify SHA-256 digests of files under `model_dir` against
/// `{relpath: hexdigest}`.
///
/// Returns an error naming the file when a listed file is absent and on a
/// digest mismatch or an unsafe (absolute or escaping) relative path.
/// Verification runs before any weight is parsed or executed, so a tampered
/// artifact never reaches the runtime.
///
/// Upstream distinguishes `FileNotFoundError` (absent file, unreadable file)
/// from `ValueError` (mismatch, unsafe path, bad digest map); they surface as
/// [`Error::Runtime`] and [`Error::Value`] respectively, with the
/// upstream message text.
pub fn verify_digests(
    model_dir: &Path,
    expected: Option<&HashMap<String, String>>,
    onnx_path: Option<&Path>,
) -> Result<()> {
    let expected = match expected {
        Some(map) => map.clone(),
        None => {
            // When the map is omitted, the `LAYA_SHA256_DIGESTS` environment
            // variable may carry one; an unset or empty variable is a no-op.
            let raw = std::env::var("LAYA_SHA256_DIGESTS")
                .unwrap_or_default()
                .trim()
                .to_string();
            if raw.is_empty() {
                return Ok(());
            }
            let parsed: HashMap<String, String> = match serde_json::from_str::<Value>(&raw) {
                Ok(Value::Object(map)) => map
                    .into_iter()
                    .map(|(k, v)| {
                        let s = match v {
                            Value::String(s) => s,
                            other => other.to_string(),
                        };
                        (k, s)
                    })
                    .collect(),
                // Python distinguishes unparseable JSON from a JSON value that
                // is not an object; keep the two messages apart.
                Err(_) => {
                    return Err(Error::Value(
                        "LAYA_SHA256_DIGESTS must be a JSON object of artifact->sha256".to_string(),
                    ));
                }
                Ok(_) => {
                    return Err(Error::Value(
                        "expected_sha256 must be a mapping of artifact paths to digests"
                            .to_string(),
                    ));
                }
            };
            parsed
        }
    };

    for (rel, want) in &expected {
        let raw_rel = rel.replace('\\', "/");
        let path: PathBuf = match (rel.as_str(), onnx_path) {
            ("onnx" | "onnx_path", Some(onnx)) => onnx.to_path_buf(),
            _ => {
                if raw_rel.starts_with('/')
                    || Path::new(&raw_rel).is_absolute()
                    || ntpath_isabs(rel)
                {
                    return Err(Error::Value(format!(
                        "cosh-onnx: unsafe absolute path in expected digests: {}",
                        py_repr(rel)
                    )));
                }
                let rel_norm = raw_rel.trim_start_matches('/');
                if rel_norm.is_empty()
                    || rel_norm == ".."
                    || rel_norm.starts_with("../")
                    || rel_norm.contains("/../")
                {
                    return Err(Error::Value(format!(
                        "cosh-onnx: unsafe path in expected digests: {}",
                        py_repr(rel)
                    )));
                }
                model_dir.join(rel_norm)
            }
        };
        if !path.is_file() {
            return Err(Error::Runtime(format!(
                "cosh-onnx: cannot verify {}: no such file under {}",
                py_repr(rel),
                model_dir.display()
            )));
        }
        let digest = file_sha256(&path)?;
        // The mismatch message formats the caller's original text (Python
        // compares the normalized pair but prints the raw `want`).
        let normalized = want.trim().to_lowercase();
        if digest != normalized {
            return Err(Error::Value(format!(
                "cosh-onnx: SHA-256 mismatch for {}: expected {}, got {}. The artifact does \
                 not match the reviewed digest; refusing to load it.",
                rel, want, digest
            )));
        }
    }
    Ok(())
}

/// SHA-256 of a file, read in 1 MiB chunks like the upstream digest loop.
fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = File::open(path).map_err(|_| {
        Error::Runtime(format!(
            "cosh-onnx: cannot verify {:?}: no such file under {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            path.parent().unwrap_or(Path::new("")).display()
        ))
    })?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            Error::Runtime(format!(
                "cosh-onnx: could not read {}: {}",
                path.display(),
                e
            ))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Test helper: SHA-256 of an in-memory byte string, matching the
/// `hashlib.sha256(...).hexdigest()` calls the upstream tests make.
#[cfg(test)]
pub(crate) fn sha256_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub mod install;

pub use install::{
    InstallRequest, InstalledCheckpoint, ProgressCallbacks, cache_status, install, parse_sha256sums,
};

#[cfg(test)]
mod tests;
