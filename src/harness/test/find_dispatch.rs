use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::tools::{CoshTools, Tools};
use serde_json::json;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("cosh-find-dispatch-{pid}-{nanos}-{seq}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn seed_tree(root: &Path, files: usize) {
    for i in 0..files {
        let path = root.join(format!("f{i:04}.txt"));
        std::fs::write(path, "x").expect("write test file");
    }
}

fn make_tools(root: &Path) -> CoshTools {
    CoshTools::new(root.to_str().expect("temp dir is utf-8"))
}

#[tokio::test]
async fn find_glob_default_caps_at_200() {
    let tmp = TempDir::new();
    seed_tree(tmp.0.as_path(), 250);
    let tools = make_tools(tmp.0.as_path());

    let result = tools
        .dispatch("find_glob", json!({"pattern": "*.txt", "path": "."}))
        .await
        .expect("glob should succeed");
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    assert_eq!(out["total"], 200, "default cap is 200");
    assert_eq!(
        out["limit_reached"],
        true,
        "a scan exceeding the cap must flag limit_reached"
    );
    assert_eq!(out["matches"].as_array().unwrap().len(), 200);
}

#[tokio::test]
async fn find_glob_max_results_is_honored() {
    let tmp = TempDir::new();
    seed_tree(tmp.0.as_path(), 250);
    let tools = make_tools(tmp.0.as_path());

    let result = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "*.txt", "path": ".", "max_results": 5}),
        )
        .await
        .unwrap();
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    assert_eq!(out["total"], 5, "the requested max_results shrinks the result set");
    assert_eq!(out["matches"].as_array().unwrap().len(), 5);
    assert_eq!(out["limit_reached"], true);
}

#[tokio::test]
async fn find_glob_max_results_above_ceiling_is_clamped() {
    let tmp = TempDir::new();
    seed_tree(tmp.0.as_path(), 250);
    let tools = make_tools(tmp.0.as_path());

    let result = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "*.txt", "path": ".", "max_results": 500}),
        )
        .await
        .expect("an over-ceiling max_results must clamp, not fail");
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    assert_eq!(out["total"], 200, "the ceiling is 200");
    assert_eq!(out["limit_reached"], true);
}

#[tokio::test]
async fn find_glob_invalid_max_results_is_rejected() {
    let tmp = TempDir::new();
    seed_tree(tmp.0.as_path(), 3);
    let tools = make_tools(tmp.0.as_path());

    let err = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "*.txt", "path": ".", "max_results": "many"}),
        )
        .await
        .unwrap_err();
    assert!(
        err.contains("positive integer"),
        "expected a positive-integer error, got: {err}"
    );

    let err = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "*.txt", "path": ".", "max_results": 0}),
        )
        .await
        .unwrap_err();
    assert!(
        err.contains("positive number"),
        "expected a positive-number error, got: {err}"
    );
}

#[tokio::test]
async fn find_glob_file_type_dir_filters_dirs() {
    let tmp = TempDir::new();
    std::fs::create_dir_all(tmp.0.as_path().join("sub")).expect("create subdir");
    std::fs::write(tmp.0.as_path().join("a.txt"), "x").expect("write file");
    let tools = make_tools(tmp.0.as_path());

    let result = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "**", "path": ".", "file_type": "dir"}),
        )
        .await
        .expect("glob should succeed");
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    let matches = out["matches"].as_array().expect("matches is an array");
    assert_eq!(matches.len(), 1, "only the subdir matches file_type=dir");
    assert_eq!(matches[0]["path"], "sub/", "dirs carry a trailing slash");
    assert_eq!(matches[0]["file_type"], "dir");
}

#[tokio::test]
async fn find_glob_hidden_flags_via_dispatch() {
    let tmp = TempDir::new();
    std::fs::write(tmp.0.as_path().join("visible.txt"), "x").expect("write file");
    std::fs::write(tmp.0.as_path().join(".hidden.txt"), "x").expect("write file");
    let tools = make_tools(tmp.0.as_path());

    let result = tools
        .dispatch("find_glob", json!({"pattern": "**", "path": "."}))
        .await
        .expect("glob should succeed");
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    let paths: Vec<&str> = out["matches"]
        .as_array()
        .expect("matches is an array")
        .iter()
        .map(|m| m["path"].as_str().unwrap_or_default())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with(".hidden.txt")),
        "hidden must default to true: {paths:?}"
    );

    let result = tools
        .dispatch(
            "find_glob",
            json!({"pattern": "**", "path": ".", "hidden": false}),
        )
        .await
        .expect("glob should succeed");
    let out: serde_json::Value = serde_json::from_str(&result).expect("glob output is JSON");
    let paths: Vec<&str> = out["matches"]
        .as_array()
        .expect("matches is an array")
        .iter()
        .map(|m| m["path"].as_str().unwrap_or_default())
        .collect();
    assert!(
        paths.iter().all(|p| !p.ends_with(".hidden.txt")),
        "hidden=false must exclude dotfiles: {paths:?}"
    );
}