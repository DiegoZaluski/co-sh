//! Ported tests: the portable parts of `tests/test_revision_pinning.py` from
//! the laya repository — `resolve_revision`, `snapshot_revision`,
//! `verify_digests`. The Router and torch-Agent pinning cases arrive with
//! their phases.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{resolve_revision, snapshot_revision, verify_digests};
use crate::error::Error;

fn digest_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// --------------------------------------------------------------- resolve_revision
// ResolveRevisionTests
#[test]
fn explicit_revision_is_returned() {
    assert_eq!(
        resolve_revision("acme/published-model", Some("abc123")),
        Some("abc123".to_string())
    );
}

#[test]
fn unknown_repo_keeps_the_hub_default() {
    assert_eq!(resolve_revision("acme/custom-model", None), None);
    assert_eq!(resolve_revision("acme/custom-model", Some("")), None);
}

// --------------------------------------------------------------- snapshot_revision
// SnapshotRevisionTests
#[test]
fn snapshot_layout() {
    assert_eq!(
        snapshot_revision(Path::new("/cache/models--a--b/snapshots/deadbeef")),
        Some("deadbeef".to_string())
    );
}

#[test]
fn plain_directory() {
    assert_eq!(snapshot_revision(Path::new("/plain/dir")), None);
    assert_eq!(snapshot_revision(Path::new("")), None);
}

// --------------------------------------------------------------- verify_digests
// VerifyDigestsTests
#[test]
fn matching_digest_passes() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), b"weights").unwrap();
    let digest = crate::hub::sha256_of(b"weights");
    let map = digest_map(&[("weights.bin", digest.as_str())]);
    verify_digests(&dir, Some(&map), None).unwrap();
    let upper = digest_map(&[("weights.bin", digest.to_uppercase().as_str())]);
    verify_digests(&dir, Some(&upper), None).unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn mismatch_raises() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-m{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), b"weights").unwrap();
    let map = digest_map(&[("weights.bin", &"0".repeat(64))]);
    match verify_digests(&dir, Some(&map), None) {
        Err(Error::Value(msg)) => assert!(msg.contains("SHA-256 mismatch"), "{}", msg),
        other => panic!("expected a ValueError, got {:?}", other.map(|_| ())),
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_file_raises() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-f{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let map = digest_map(&[("absent.bin", "0")]);
    match verify_digests(&dir, Some(&map), None) {
        Err(Error::Runtime(msg)) => assert!(msg.contains("no such file"), "{}", msg),
        other => panic!(
            "expected a FileNotFoundError-equivalent, got {:?}",
            other.map(|_| ())
        ),
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn escaping_paths_rejected() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-e{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), b"weights").unwrap();
    let digest = crate::hub::sha256_of(b"weights");
    for rel in [
        "../evil",
        "..",
        "a/../../evil",
        "\\..\\evil",
        "/absolute/evil",
        "C:\\absolute\\evil",
    ] {
        let map = digest_map(&[(rel, digest.as_str())]);
        match verify_digests(&dir, Some(&map), None) {
            Err(Error::Value(msg)) => {
                assert!(msg.contains("unsafe"), "path {:?}: {}", rel, msg)
            }
            other => panic!(
                "path {:?}: expected a ValueError, got {:?}",
                rel,
                other.map(|_| ())
            ),
        }
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn digest_map_can_come_from_environment() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-v{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), b"weights").unwrap();
    let digest = crate::hub::sha256_of(b"weights");
    // A test-local environment variable; set and restored around the call.
    // cargo test runs tests on parallel threads in one process, so this is
    // only safe while no other test in this binary reads the environment
    // (today none does).
    let key = "LAYA_SHA256_DIGESTS";
    let saved = std::env::var(key).ok();
    // SAFETY: see the comment above — no other test in this binary touches
    // the environment.
    unsafe {
        std::env::set_var(key, format!(r#"{{"weights.bin": "{}"}}"#, digest));
    }
    let result = verify_digests(&dir, None, None);
    unsafe {
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    result.unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn external_onnx_digest_is_supported() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-o{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let model = dir.join("model.onnx");
    fs::write(&model, b"onnx").unwrap();
    let digest = crate::hub::sha256_of(b"onnx");
    let map = digest_map(&[("onnx", digest.as_str())]);
    verify_digests(&dir, Some(&map), Some(&model)).unwrap();
    fs::remove_dir_all(&dir).ok();
}

// --------------------------------------------------------------- AgentPinningTests (local parts)
// The digest-mismatch-before-weights-load case: verification runs before any
// weight is parsed, so a tampered artifact never reaches the runtime.
#[test]
fn digest_mismatch_raises_before_weights_load() {
    let dir = std::env::temp_dir().join(format!("cosh-onnx-digest-w{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("rl_agent_config.json"),
        r#"{"act_costs": {"a": 0}}"#,
    )
    .unwrap();
    fs::write(dir.join("model.safetensors"), b"not the reviewed weights").unwrap();
    let map = digest_map(&[("model.safetensors", &"0".repeat(64))]);
    match verify_digests(&dir, Some(&map), None) {
        Err(Error::Value(msg)) => assert!(msg.contains("SHA-256 mismatch"), "{}", msg),
        other => panic!("expected a ValueError, got {:?}", other.map(|_| ())),
    }
    fs::remove_dir_all(&dir).ok();
}
