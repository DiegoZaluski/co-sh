use super::super::types::{AstEditOp, FsAstEdit, FsMetadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique per-test temp dir that is removed on drop (even when an assertion
/// fails mid-test), keeping the tests hermetic and out of the repo tree.
struct TempDir(PathBuf);

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cosh_ast_test_{}_{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn meta(root: &Path) -> FsMetadata {
    FsMetadata {
        root: root.to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

fn abs_path(root: &Path, name: &str) -> String {
    root.join(name).to_string_lossy().into_owned()
}

fn ops(pairs: &[(&str, &str)]) -> Vec<AstEditOp> {
    pairs
        .iter()
        .map(|(pat, out)| AstEditOp {
            pat: pat.to_string(),
            out: out.to_string(),
        })
        .collect()
}

#[tokio::test]
async fn ast_edit_rust_statement_rewrite_applies_once() {
    // A statement-level Rust pattern only rewrites full statements (including
    // the trailing semicolon via the contextual pattern), never a call nested
    // inside a `let` initialiser, and never twice (no double application across
    // the normal + contextual patterns).
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(
        &file,
        "fn main() {\n    foo(1);\n    let x = foo(2);\n    foo(3);\n}\n",
    )
    .unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("foo($A);", "qux($A);")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    qux(1);\n    let x = foo(2);\n    qux(3);\n}\n"
    );
}

#[tokio::test]
async fn ast_edits_rename_call_with_multi_metavar_in_rust() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {\n    oldApi(1, 2);\n    oldApi(x);\n}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("oldApi($$$ARGS)", "newApi($$$ARGS)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    newApi(1, 2);\n    newApi(x);\n}\n"
    );
}

#[tokio::test]
async fn ast_edits_delete_call_by_empty_out() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {\n    ignore(1);\n    log();\n}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("ignore($$$ARGS)", "")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    ;\n    log();\n}\n"
    );
}

#[tokio::test]
async fn ast_edit_enforces_metavariable_identity_in_js() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.js");
    std::fs::write(
        &file,
        "function f(x){ return x && x(); }\nfunction g(){ return a && b(); }\n",
    )
    .unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("$A && $A()", "$A?.()")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    // Only `x && x()` matches (same metavariable); `a && b()` is left alone.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "function f(x){ return x?.(); }\nfunction g(){ return a && b(); }\n"
    );
}

#[tokio::test]
async fn ast_edit_swaps_two_arguments_in_js() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.js");
    std::fs::write(&file, "assertEqual(a, b);\nassertEqual(x, y);\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("assertEqual($A, $B)", "assertEqual($B, $A)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "assertEqual(b, a);\nassertEqual(y, x);\n"
    );
}

#[tokio::test]
async fn ast_edit_rewrites_python_print_to_logger() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.py");
    std::fs::write(&file, "print(\"hi\")\nprint(1, 2)\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("print($$$ARGS)", "logger.info($$$ARGS)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "logger.info(\"hi\")\nlogger.info(1, 2)\n"
    );
}

#[tokio::test]
async fn ast_edit_applies_multiple_ops_in_order() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {\n    foo(1);\n    bar(2);\n}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[
                ("foo($$$ARGS)", "qux($$$ARGS)"),
                ("bar($$$ARGS)", "baz($$$ARGS)"),
            ]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    qux(1);\n    baz(2);\n}\n"
    );
}

#[tokio::test]
async fn ast_edit_reports_no_files_touched_when_no_match() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() { hello(); }\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("missing($$$ARGS)", "nothing($$$ARGS)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    // No file was modified, but the no-match feedback must still surface to the
    // model instead of being swallowed as silence.
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].first_changed_line, None);
    assert_eq!(results[0].diff, None);
    assert!(
        results[0]
            .warnings
            .iter()
            .any(|w| w.contains("no AST matches found"))
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() { hello(); }\n"
    );
}

#[tokio::test]
async fn ast_edit_rejects_unsupported_file() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.xyz");
    std::fs::write(&file, "hello\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("x", "y")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    // Unsupported files are skipped, so the result is just empty (no error).
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn ast_edit_reports_first_changed_line() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {\n    untouched(0);\n    rename(a);\n}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("rename($$$A)", "qux($$$A)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await
    .expect("apply should succeed");
    assert_eq!(result.len(), 1);
    // `rename(a)` lives on line 3 of the original file; the reported line must
    // point at it, not at an offset skewed by the rewrite (the preceding op on
    // line 2 is untouched and must not shift the reported line).
    assert_eq!(result[0].first_changed_line, Some(3));
}

#[tokio::test]
async fn ast_edit_first_changed_line_across_two_ops_and_utf8() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.py");
    std::fs::write(&file, "import sys\nwarn(\"ol\u{e1}\")\nhelper(\"x\")\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            // First op rewrites a multi-byte UTF-8 string (`\u{e1}`); second op
            // then matches a later line against already-rewritten text. Offsets
            // for the second op are no longer the identity mapping, so this
            // exercises the anchor remap.
            ops: ops(&[("warn($$$A)", "log($$$A)"), ("helper($A)", "run($A)")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await
    .expect("apply should succeed");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].first_changed_line, Some(2));
    // Content correctness across the two rewrites, with the multi-byte char kept.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "import sys\nlog(\"ol\u{e1}\")\nrun(\"x\")\n"
    );
}

#[tokio::test]
async fn ast_edit_rejects_empty_pattern() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("", "x")]),
            paths: vec![file],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("non-empty pattern"));
}

#[tokio::test]
async fn ast_edit_rejects_duplicate_pattern() {
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("foo($A)", "bar($A)"), ("foo($A)", "baz($A)")]),
            paths: vec![file],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("duplicate rewrite pattern"));
}

#[tokio::test]
async fn ast_edit_empty_file_metavar_pattern() {
    // Regression: an empty supported file + a metavariable pattern used to panic
    // (`anchor` is empty while ast-grep yields a zero-length root match). It must
    // now be a clean no-op with a warning, not a crash.
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "empty.rs");
    std::fs::write(&file, "").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("$A", "x")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(
        result.is_ok(),
        "empty file must not panic: {:?}",
        result.err()
    );
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].first_changed_line, None);
    assert_eq!(results[0].diff, None);
    assert!(
        results[0]
            .warnings
            .iter()
            .any(|w| w.contains("no AST matches found"))
    );
    // The empty file is left untouched.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
}

#[cfg(unix)]
#[tokio::test]
async fn ast_edit_does_not_follow_symlinks() {
    // A symlinked directory inside the tree must not be traversed (it could
    // escape the root or cycle), and a symlinked file must not be rewritten.
    let dir = TempDir::new();
    let target = std::env::temp_dir().join(format!(
        "cosh_ast_outside_{}_{}",
        std::process::id(),
        DIR_SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&target).unwrap();
    let outside = target.join("outside.rs");
    std::fs::write(&outside, "fn main() { foo(1); }\n").unwrap();

    let root = dir.root();
    std::os::unix::fs::symlink(&target, root.join("linked")).expect("make file symlink");
    let result = crate::fs::ast_edit::ast_edit(
        meta(root),
        FsAstEdit {
            ops: ops(&[("foo($A)", "bar($A)")]),
            paths: vec![root.to_string_lossy().into_owned()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    // Nothing outside was touched.
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "fn main() { foo(1); }\n"
    );
    let _ = std::fs::remove_dir_all(&target);
}

#[tokio::test]
async fn ast_edit_second_op_after_full_delete() {
    // Regression: the first op deletes the whole file (so `text` becomes empty
    // and `anchor` empty), then a second op matches against the empty buffer. The
    // engine must not index an empty `anchor` (no panic) and must keep the first
    // changed line well-defined.
    let dir = TempDir::new();
    let file = abs_path(dir.root(), "sample.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("fn main() {}", ""), ("$A", "x")]),
            paths: vec![file.clone()],
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_ok(), "must not panic: {:?}", result.err());
    let results = result.unwrap();
    // Whatever the exact rewrite, the engine must not have panicked and the
    // full-delete + second-op flow must yield a valid, reported edit.
    assert!(!results.is_empty());
    assert_eq!(results[0].first_changed_line, Some(1));
}

#[tokio::test]
async fn ast_edit_skips_nonexistent_path() {
    let dir = TempDir::new();
    // No file is created; the explicit path does not exist.
    let missing = abs_path(dir.root(), "missing.rs");

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("foo($A)", "bar($A)")]),
            paths: vec![missing],
            ..Default::default()
        },
    )
    .await;
    // A nonexistent path yields nothing rather than a hard error (upstream-like).
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn ast_edit_caps_files_with_warning() {
    let dir = TempDir::new();
    let a = abs_path(dir.root(), "a.rs");
    let b = abs_path(dir.root(), "b.rs");
    std::fs::write(&a, "fn main() { foo(1); }\n").unwrap();
    std::fs::write(&b, "fn main() { foo(2); }\n").unwrap();

    let result = crate::fs::ast_edit::ast_edit(
        meta(dir.root()),
        FsAstEdit {
            ops: ops(&[("foo($A)", "bar($A)")]),
            paths: vec![dir.root().to_string_lossy().into_owned()],
            max_files: Some(1),
        },
    )
    .await;
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
    let results = result.unwrap();
    // Only one of the two matched files is edited, and the cap is surfaced.
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .iter()
            .any(|w| w.contains("limit reached"))
    );
    let edited = results[0].path.contains("a.rs") || results[0].path.contains("b.rs");
    assert!(
        edited,
        "one of the capped files should be reported as edited"
    );
}
