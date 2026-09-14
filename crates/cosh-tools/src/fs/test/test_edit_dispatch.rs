use super::super::{EditEngine, Fs};
use cosh_sdk::hashline::format::compute_file_hash;
use serde_json::json;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root: each dispatch test edits fixture files inside its own
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
        let dir = std::env::temp_dir().join(format!("cosh_fs_dispatch_{label}_{id}_{nanos}"));
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
fn fs_replace(root: &Path) -> Fs {
    fs_root(root).only_replace()
}
fn fs_ast(root: &Path) -> Fs {
    fs_root(root).only_ast()
}

#[tokio::test]
async fn auto_dispatches_to_replace_via_targets() {
    let root = TempRoot::new("replace");
    let path = root.file("cosh_test_dispatch_replace.txt");
    std::fs::write(&path, "line1\nline2\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(&path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+REPL" }]
    });
    let out = fs_root(root.path())
        .edit(args)
        .await
        .expect("replace should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "REPL\nline2\n");
}

#[tokio::test]
async fn auto_dispatches_to_ast_via_ast() {
    let root = TempRoot::new("ast");
    let path = root.file("cosh_test_dispatch_ast.rs");
    std::fs::write(&path, "fn main() { oldApi(1); }\n").unwrap();
    let args = json!({
        "ast": { "ops": [{"pat": "oldApi($$$ARGS)", "out": "newApi($$$ARGS)"}], "paths": [path] }
    });
    let out = fs_root(root.path())
        .edit(args)
        .await
        .expect("ast should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn main() { newApi(1); }\n"
    );
}

#[tokio::test]
async fn only_replace_forces_replace_engine() {
    let root = TempRoot::new("onlyreplace");
    let path = root.file("cosh_test_only_replace.txt");
    std::fs::write(&path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(&path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }]
    });
    let out = fs_replace(root.path())
        .edit(args)
        .await
        .expect("replace should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
}

#[tokio::test]
async fn only_ast_forces_ast_engine() {
    let root = TempRoot::new("onlyast");
    let path = root.file("cosh_test_only_ast.rs");
    std::fs::write(&path, "fn main() { foo(1); }\n").unwrap();
    let args = json!({
        "ast": { "ops": [{"pat": "foo($$$ARGS)", "out": "bar($$$ARGS)"}], "paths": [path] }
    });
    let out = fs_ast(root.path())
        .edit(args)
        .await
        .expect("ast should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn main() { bar(1); }\n"
    );
}

#[tokio::test]
async fn only_ast_rejects_targets_argument() {
    let args = json!({ "targets": [{"path": "x", "file_hash": "H", "ops": "y"}] });
    let err = fs_ast(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("`ast` argument"), "got: {err}");
}

#[tokio::test]
async fn only_replace_rejects_ast_argument() {
    let args = json!({ "ast": { "ops": [], "paths": [] } });
    let err = fs_replace(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("`targets`"), "got: {err}");
}

#[tokio::test]
async fn auto_with_no_arguments_returns_usage_prompt() {
    let err = fs_root(Path::new("."))
        .edit(json!({}))
        .await
        .expect_err("should be rejected");
    assert!(err.contains("targets") || err.contains("ast"), "got: {err}");
}

#[tokio::test]
async fn auto_with_both_arguments_returns_error() {
    let root = TempRoot::new("both");
    let path = root.file("cosh_test_dispatch_both.txt");
    std::fs::write(&path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(&path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }],
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": [path] }
    });
    let err = fs_root(root.path())
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
}

#[tokio::test]
async fn auto_corrects_ast_schema_placed_in_targets() {
    let args = json!({
        "targets": [{ "pat": "oldApi($A)", "out": "newApi($A)" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be corrected");
    assert!(err.contains("`ast` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_corrects_replace_schema_placed_in_ast() {
    let args = json!({
        "ast": { "targets": [{"path": "x", "file_hash": "H", "ops": "y"}] }
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be corrected");
    assert!(err.contains("`targets` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_engine_accessor_reflects_setters() {
    assert_eq!(fs_root(Path::new(".")).edit_engine(), EditEngine::Auto);
    assert_eq!(
        fs_replace(Path::new(".")).edit_engine(),
        EditEngine::Replace
    );
    assert_eq!(fs_ast(Path::new(".")).edit_engine(), EditEngine::Ast);
    assert_eq!(
        fs_ast(Path::new(".")).auto().edit_engine(),
        EditEngine::Auto
    );
}

// ── content replace engine (`edits`) dispatch ─────────────────────────────

#[tokio::test]
async fn auto_dispatches_to_content_via_edits() {
    let root = TempRoot::new("edits");
    let path = root.file("cosh_test_dispatch_edits.txt");
    let text = "alpha\nbeta\n";
    std::fs::write(&path, text).unwrap();
    let hash = cosh_sdk::rollback::record(&path, text).unwrap();
    let args = json!({
        "edits": [{ "path": path, "file_hash": hash, "old_string": "beta", "new_string": "BETA" }]
    });
    let out = fs_root(root.path())
        .edit(args)
        .await
        .expect("edits should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nBETA\n");
}

#[tokio::test]
async fn auto_rejects_targets_and_edits_together() {
    let args = json!({
        "targets": [{ "path": "x", "file_hash": "H", "ops": "y" }],
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(
        err.contains("multiple edit engine arguments") && err.contains("`edits`"),
        "got: {err}"
    );
}

#[tokio::test]
async fn auto_rejects_ast_and_edits_together() {
    let args = json!({
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": ["x"] },
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
}

#[tokio::test]
async fn auto_rejects_all_three_engines_together() {
    let args = json!({
        "targets": [{ "path": "x", "file_hash": "H", "ops": "y" }],
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": ["x"] },
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
}

#[tokio::test]
async fn usage_prompt_lists_the_content_engine() {
    let err = fs_root(Path::new("."))
        .edit(json!({}))
        .await
        .expect_err("should be rejected");
    assert!(
        err.contains("`edits`") && err.contains("old_string"),
        "got: {err}"
    );
}

#[tokio::test]
async fn auto_corrects_content_schema_placed_in_targets() {
    let args = json!({
        "targets": [{ "path": "x", "file_hash": "H", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be corrected");
    assert!(err.contains("`edits` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_corrects_hashline_schema_placed_in_edits() {
    let args = json!({
        "edits": [{ "path": "x", "file_hash": "H", "ops": "replace 1..1:\n+y" }]
    });
    let err = fs_root(Path::new("."))
        .edit(args)
        .await
        .expect_err("should be corrected");
    assert!(err.contains("`targets` argument"), "got: {err}");
}

#[tokio::test]
async fn only_replace_rejects_edits_argument() {
    let args = json!({
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_replace(Path::new("."))
        .edit(args)
        .await
        .expect_err("forced replace engine must reject `edits`");
    assert!(err.contains("`targets`"), "got: {err}");
}

#[test]
fn edit_auto_description_examples_teach_both_engines_argument_shapes() {
    let schema = &Fs::new().description_edit["inputSchema"]["properties"];
    let ops_text = &schema["targets"]["items"]["properties"]["ops"]["description"];
    let edits_text = &schema["edits"]["description"];
    assert!(
        ops_text.as_str().is_some_and(|t| t.contains("Example")),
        "the hashline ops DSL is taught by example"
    );
    assert!(
        edits_text
            .as_str()
            .is_some_and(|t| t.contains("Example") && t.contains("old_string")),
        "the content engine is taught by example"
    );
}
