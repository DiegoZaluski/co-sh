use super::super::glob::glob;
use super::super::types::Glob;

const FIND_DIR: &str = "/home/inky/cosh/crates/cosh-sdk/src/find";

#[test]
fn glob_finds_rust_files() {
    let out = glob(
        &Glob {
            recursive: Some(true),
            max_results: Some(20),
            gitignore: Some(true),
            ..Default::default()
        },
        "*.rs",
        FIND_DIR,
    )
    .expect("glob should succeed");

    assert!(!out.matches.is_empty(), "expected at least one .rs file");
    assert_eq!(out.total, out.matches.len() as u32);
    for entry in &out.matches {
        assert!(
            entry.path.ends_with(".rs"),
            "path should end with .rs: {}",
            entry.path
        );
        assert_eq!(entry.file_type, "file");
    }
}

#[test]
fn glob_dir_filter_returns_only_dirs() {
    let out = glob(
        &Glob {
            file_type: Some("dir".to_string()),
            recursive: Some(true),
            gitignore: Some(true),
            ..Default::default()
        },
        "*",
        "/home/inky/cosh/crates",
    )
    .expect("glob with dir filter should succeed");

    for entry in &out.matches {
        assert_eq!(
            entry.file_type, "dir",
            "expected dir, got: {}",
            entry.file_type
        );
    }
}

#[test]
fn glob_max_results_limits_output() {
    let out = glob(
        &Glob {
            recursive: Some(true),
            max_results: Some(3),
            gitignore: Some(true),
            ..Default::default()
        },
        "*.rs",
        "/home/inky/cosh",
    )
    .expect("glob with max_results should succeed");

    assert!(
        out.matches.len() <= 3,
        "result count must not exceed max_results"
    );
}

#[test]
fn glob_rejects_unknown_file_type() {
    let err = glob(
        &Glob {
            file_type: Some("executable".to_string()),
            ..Default::default()
        },
        "*",
        "/home/inky/cosh",
    )
    .err()
    .expect("glob with unknown file_type should return an error");

    assert!(
        err.contains("invalid file_type"),
        "expected invalid file_type error, got: {err}"
    );
}

#[test]
fn glob_nonexistent_path_returns_error() {
    let result = glob(&Glob::default(), "*", "/nonexistent/cosh_test_path");

    assert!(result.is_err(), "nonexistent path should return an error");
}
