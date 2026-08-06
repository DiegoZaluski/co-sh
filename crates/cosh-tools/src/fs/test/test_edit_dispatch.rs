use super::super::{EditEngine, Fs};
use cosh_sdk::hashline::format::compute_file_hash;
use serde_json::json;

fn fs_auto() -> Fs {
    Fs::new().cwd("/home/inky/cosh")
}
fn fs_replace() -> Fs {
    fs_auto().only_replace()
}
fn fs_ast() -> Fs {
    fs_auto().only_ast()
}

#[tokio::test]
async fn auto_dispatches_to_replace_via_targets() {
    let path = "/home/inky/cosh/cosh_test_dispatch_replace.txt";
    std::fs::write(path, "line1\nline2\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+REPL" }]
    });
    let out = fs_auto().edit(args).await.expect("replace should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "REPL\nline2\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn auto_dispatches_to_ast_via_ast() {
    let path = "/home/inky/cosh/cosh_test_dispatch_ast.rs";
    std::fs::write(path, "fn main() { oldApi(1); }\n").unwrap();
    let args = json!({
        "ast": { "ops": [{"pat": "oldApi($$$ARGS)", "out": "newApi($$$ARGS)"}], "paths": [path] }
    });
    let out = fs_auto().edit(args).await.expect("ast should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "fn main() { newApi(1); }\n"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn only_replace_forces_replace_engine() {
    let path = "/home/inky/cosh/cosh_test_only_replace.txt";
    std::fs::write(path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }]
    });
    let out = fs_replace()
        .edit(args)
        .await
        .expect("replace should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "b\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn only_ast_forces_ast_engine() {
    let path = "/home/inky/cosh/cosh_test_only_ast.rs";
    std::fs::write(path, "fn main() { foo(1); }\n").unwrap();
    let args = json!({
        "ast": { "ops": [{"pat": "foo($$$ARGS)", "out": "bar($$$ARGS)"}], "paths": [path] }
    });
    let out = fs_ast().edit(args).await.expect("ast should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "fn main() { bar(1); }\n"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn only_ast_rejects_targets_argument() {
    let args = json!({ "targets": [{"path": "x", "file_hash": "H", "ops": "y"}] });
    let err = fs_ast().edit(args).await.expect_err("should be rejected");
    assert!(err.contains("`ast` argument"), "got: {err}");
}

#[tokio::test]
async fn only_replace_rejects_ast_argument() {
    let args = json!({ "ast": { "ops": [], "paths": [] } });
    let err = fs_replace()
        .edit(args)
        .await
        .expect_err("should be rejected");
    assert!(err.contains("`targets`"), "got: {err}");
}

#[tokio::test]
async fn auto_with_no_arguments_returns_usage_prompt() {
    let err = fs_auto()
        .edit(json!({}))
        .await
        .expect_err("should be rejected");
    assert!(
        err.contains("`targets`") && err.contains("`ast`"),
        "got: {err}"
    );
}

#[tokio::test]
async fn auto_with_both_arguments_returns_error() {
    let path = "/home/inky/cosh/cosh_test_dispatch_both.txt";
    std::fs::write(path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }],
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": [path] }
    });
    let err = fs_auto().edit(args).await.expect_err("should be rejected");
    assert!(err.contains("both"), "got: {err}");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn auto_corrects_ast_schema_placed_in_targets() {
    let args = json!({
        "targets": [{ "pat": "oldApi($A)", "out": "newApi($A)" }]
    });
    let err = fs_auto().edit(args).await.expect_err("should be corrected");
    assert!(err.contains("`ast` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_corrects_replace_schema_placed_in_ast() {
    let args = json!({
        "ast": { "targets": [{"path": "x", "file_hash": "H", "ops": "y"}] }
    });
    let err = fs_auto().edit(args).await.expect_err("should be corrected");
    assert!(err.contains("`targets` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_engine_accessor_reflects_setters() {
    assert_eq!(fs_auto().edit_engine(), EditEngine::Auto);
    assert_eq!(fs_replace().edit_engine(), EditEngine::Replace);
    assert_eq!(fs_ast().edit_engine(), EditEngine::Ast);
    assert_eq!(fs_ast().auto().edit_engine(), EditEngine::Auto);
}
