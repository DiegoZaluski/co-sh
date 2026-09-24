use super::super::Fs;
use cosh_sdk::hashline::format::compute_file_hash;
use serde_json::json;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root: each edit-tool test edits fixture files inside its own
/// directory, so tests never touch a developer's real project tree.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cosh_fs_edit_tools_{label}_{id}_{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Path of a fixture file inside the root.
    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fs_root(root: &Path) -> Fs {
    Fs::new().cwd(root)
}

// ── fs_edit (content replace engine) ─────────────────────────────────────

#[tokio::test]
async fn fs_edit_replaces_exact_string() {
    let root = TempRoot::new("content");
    let path = root.file("cosh_test_edit_content.txt");
    let text = "alpha\nbeta\n";
    std::fs::write(&path, text).unwrap();
    let hash = cosh_sdk::rollback::record(&path, text).unwrap();
    let args = json!({
        "path": path,
        "file_hash": hash,
        "old_string": "beta",
        "new_string": "BETA"
    });
    let out = fs_root(root.path())
        .edit(args)
        .await
        .expect("content edit should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nBETA\n");
}

#[tokio::test]
async fn fs_edit_without_file_hash_is_rejected_with_anchor_hint() {
    let root = TempRoot::new("content_notag");
    let path = root.file("cosh_test_edit_notag.txt");
    std::fs::write(&path, "alpha\n").unwrap();
    let args = json!({
        "path": path,
        "old_string": "alpha",
        "new_string": "BETA"
    });
    let err = fs_root(root.path())
        .edit(args)
        .await
        .expect_err("missing file_hash must be rejected with a correction");
    assert!(
        err.contains(&path) && err.contains("¶"),
        "the error should carry the file's live anchor, got: {err}"
    );
}

#[tokio::test]
async fn fs_edit_requires_old_string() {
    let args = json!({ "path": "x", "new_string": "b" });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(
        err.contains("content replace engine") && err.contains("old_string"),
        "got: {err}"
    );
}

// ── fs_edit_lines (hashline replace engine) ──────────────────────────────

#[tokio::test]
async fn fs_edit_lines_applies_flat_ops_shape() {
    let root = TempRoot::new("lines");
    let path = root.file("cosh_test_edit_lines.txt");
    std::fs::write(&path, "line1\nline2\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(&path).unwrap());
    let args = json!({
        "path": path,
        "file_hash": hash,
        "ops": "replace 1..1:\n+REPL"
    });
    let out = fs_root(root.path())
        .edit_lines(args)
        .await
        .expect("line edit should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "REPL\nline2\n");
}

#[tokio::test]
async fn fs_edit_lines_keeps_legacy_targets_batch_form() {
    let root = TempRoot::new("lines_legacy");
    let path = root.file("cosh_test_edit_lines_legacy.txt");
    std::fs::write(&path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(&path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }]
    });
    let out = fs_root(root.path())
        .edit_lines(args)
        .await
        .expect("legacy batch form is kept for benchmarks");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
}

#[tokio::test]
async fn fs_edit_lines_requires_path_and_ops() {
    let err = fs_root(Path::new("."))
        .edit_lines(json!({ "path": "x" }))
        .await
        .expect_err("should be rejected");
    assert!(err.contains("replace engine requires"), "got: {err}");
}

// ── fs_ast_edit (AST structural engine) ─────────────────────────────────────

#[tokio::test]
async fn ast_edit_applies_flat_ops_shape() {
    let root = TempRoot::new("ast");
    let path = root.file("cosh_test_edit_ast.rs");
    std::fs::write(&path, "fn main() { oldApi(1); }\n").unwrap();
    let args = json!({
        "path": path,
        "ops": [{ "pat": "oldApi($$$ARGS)", "out": "newApi($$$ARGS)" }]
    });
    let out = fs_root(root.path())
        .edit_ast(args)
        .await
        .expect("AST edit should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn main() { newApi(1); }\n"
    );
}

#[tokio::test]
async fn ast_edit_keeps_legacy_ast_batch_form() {
    let root = TempRoot::new("ast_legacy");
    let path = root.file("cosh_test_edit_ast_legacy.rs");
    std::fs::write(&path, "fn main() { foo(1); }\n").unwrap();
    let args = json!({
        "ast": { "ops": [{"pat": "foo($$$ARGS)", "out": "bar($$$ARGS)"}], "paths": [path] }
    });
    let out = fs_root(root.path())
        .edit_ast(args)
        .await
        .expect("legacy batch form is kept for codemods");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn main() { bar(1); }\n"
    );
}

#[tokio::test]
async fn ast_edit_requires_path_and_ops() {
    let err = fs_root(Path::new("."))
        .edit_ast(json!({ "path": "x" }))
        .await
        .expect_err("should be rejected");
    assert!(err.contains("AST engine requires"), "got: {err}");
}

// ── tool descriptions teach exactly one engine each ──────────────────────

#[test]
fn description_edit_teaches_the_content_replace_shape() {
    let desc = &Fs::new().description_edit;
    assert_eq!(desc["name"], "fs_edit");
    let props = &desc["inputSchema"]["properties"];
    assert!(
        props.get("old_string").is_some() && props.get("new_string").is_some(),
        "fs_edit teaches the CC-trained old_string/new_string shape"
    );
    assert!(
        props.get("ops").is_none() && props.get("targets").is_none(),
        "fs_edit must not advertise other engines' arguments"
    );
    assert!(
        desc["description"]
            .as_str()
            .is_some_and(|t| t.contains("old_string") && t.contains("Example")),
        "the content engine is taught by example"
    );
    assert_eq!(
        desc["inputSchema"]["required"],
        json!(["path", "old_string", "new_string"])
    );
}

#[test]
fn description_edit_lines_teaches_the_hashline_shape() {
    let desc = &Fs::new().description_edit_lines;
    assert_eq!(desc["name"], "fs_edit_lines");
    let props = &desc["inputSchema"]["properties"];
    assert!(
        props.get("ops").is_some() && props.get("file_hash").is_some(),
        "fs_edit_lines teaches the path/file_hash/ops shape"
    );
    assert!(
        props.get("old_string").is_none() && props.get("ast").is_none(),
        "fs_edit_lines must not advertise other engines' arguments"
    );
    assert!(
        props["ops"]
            ["description"]
            .as_str()
            .is_some_and(|t| t.contains("Example") && t.contains("replace")),
        "the ops DSL is taught by example"
    );
}

#[test]
fn description_ast_edit_teaches_the_pattern_shape() {
    let desc = &Fs::new().description_ast_edit;
    assert_eq!(desc["name"], "fs_ast_edit");
    let props = &desc["inputSchema"]["properties"];
    assert!(
        props["ops"].get("items").is_some(),
        "fs_ast_edit teaches the pattern/out array shape"
    );
    assert!(
        props.get("old_string").is_none() && props.get("file_hash").is_none(),
        "fs_ast_edit must not advertise other engines' arguments"
    );
    assert!(
        props["ops"]["items"]["required"]
            == json!(["pat", "out"]),
        "each AST op requires pat and out"
    );
}
