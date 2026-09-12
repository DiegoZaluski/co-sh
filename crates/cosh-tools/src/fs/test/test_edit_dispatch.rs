use super::super::{EditEngine, Fs};
use cosh_sdk::hashline::format::compute_file_hash;
use serde_json::json;

fn fs_auto() -> Fs {
    Fs::new().cwd("/home/inky/co-sh")
}
fn fs_replace() -> Fs {
    fs_auto().only_replace()
}
fn fs_ast() -> Fs {
    fs_auto().only_ast()
}

#[tokio::test]
async fn auto_dispatches_to_replace_via_targets() {
    let path = "/home/inky/co-sh/cosh_test_dispatch_replace.txt";
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
    let path = "/home/inky/co-sh/cosh_test_dispatch_ast.rs";
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
    let path = "/home/inky/co-sh/cosh_test_only_replace.txt";
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
    let path = "/home/inky/co-sh/cosh_test_only_ast.rs";
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
    let path = "/home/inky/co-sh/cosh_test_dispatch_both.txt";
    std::fs::write(path, "a\n").unwrap();
    let hash = compute_file_hash(&std::fs::read_to_string(path).unwrap());
    let args = json!({
        "targets": [{ "path": path, "file_hash": hash, "ops": "replace 1..1:\n+b" }],
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": [path] }
    });
    let err = fs_auto().edit(args).await.expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
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

// ── content replace engine (`edits`) dispatch ─────────────────────────────

#[tokio::test]
async fn auto_dispatches_to_content_via_edits() {
    let path = "/home/inky/co-sh/cosh_test_dispatch_edits.txt";
    let text = "alpha\nbeta\n";
    std::fs::write(path, text).unwrap();
    let hash = cosh_sdk::rollback::record(path, text).unwrap();
    let args = json!({
        "edits": [{ "path": path, "file_hash": hash, "old_string": "beta", "new_string": "BETA" }]
    });
    let out = fs_auto().edit(args).await.expect("edits should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha\nBETA\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn auto_rejects_targets_and_edits_together() {
    let args = json!({
        "targets": [{ "path": "x", "file_hash": "H", "ops": "y" }],
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_auto().edit(args).await.expect_err("should be rejected");
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
    let err = fs_auto().edit(args).await.expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
}

#[tokio::test]
async fn auto_rejects_all_three_engines_together() {
    let args = json!({
        "targets": [{ "path": "x", "file_hash": "H", "ops": "y" }],
        "ast": { "ops": [{"pat": "x($A)", "out": "y($A)"}], "paths": ["x"] },
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_auto().edit(args).await.expect_err("should be rejected");
    assert!(err.contains("multiple edit engine arguments"), "got: {err}");
}

#[tokio::test]
async fn usage_prompt_lists_the_content_engine() {
    let err = fs_auto()
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
    let err = fs_auto().edit(args).await.expect_err("should be corrected");
    assert!(err.contains("`edits` argument"), "got: {err}");
}

#[tokio::test]
async fn auto_corrects_hashline_schema_placed_in_edits() {
    let args = json!({
        "edits": [{ "path": "x", "file_hash": "H", "ops": "replace 1..1:\n+y" }]
    });
    let err = fs_auto().edit(args).await.expect_err("should be corrected");
    assert!(err.contains("`targets` argument"), "got: {err}");
}

#[tokio::test]
async fn only_replace_rejects_edits_argument() {
    let args = json!({
        "edits": [{ "path": "x", "old_string": "a", "new_string": "b" }]
    });
    let err = fs_replace()
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
