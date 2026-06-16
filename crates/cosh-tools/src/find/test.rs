use super::glob::glob;
use super::grep::grep;
use super::types::{GlobInput, GrepInput};

const FIND_DIR: &str = "/home/inky/cosh/crates/cosh-sdk/src/find";
const GLOB_FILE: &str = "/home/inky/cosh/crates/cosh-sdk/src/find/glob.rs";

#[test]
fn glob_finds_rust_files() {
    let out = glob(GlobInput {
        pattern: "*.rs".to_string(),
        path: FIND_DIR.to_string(),
        file_type: None,
        recursive: Some(true),
        hidden: None,
        max_results: Some(20),
        gitignore: Some(true),
        sort_by_mtime: None,
        timeout_ms: None,
    })
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
    let out = glob(GlobInput {
        pattern: "*".to_string(),
        path: "/home/inky/cosh/crates".to_string(),
        file_type: Some("dir".to_string()),
        recursive: Some(true),
        hidden: None,
        max_results: None,
        gitignore: Some(true),
        sort_by_mtime: None,
        timeout_ms: None,
    })
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
    let out = glob(GlobInput {
        pattern: "*.rs".to_string(),
        path: "/home/inky/cosh".to_string(),
        file_type: None,
        recursive: Some(true),
        hidden: None,
        max_results: Some(3),
        gitignore: Some(true),
        sort_by_mtime: None,
        timeout_ms: None,
    })
    .expect("glob with max_results should succeed");

    assert!(
        out.matches.len() <= 3,
        "result count must not exceed max_results"
    );
}

#[test]
fn glob_rejects_unknown_file_type() {
    let err = glob(GlobInput {
        pattern: "*".to_string(),
        path: "/home/inky/cosh".to_string(),
        file_type: Some("executable".to_string()),
        recursive: None,
        hidden: None,
        max_results: None,
        gitignore: None,
        sort_by_mtime: None,
        timeout_ms: None,
    })
    .err()
    .expect("glob with unknown file_type should return an error");

    assert!(
        err.contains("invalid file_type"),
        "expected invalid file_type error, got: {err}"
    );
}

#[test]
fn glob_nonexistent_path_returns_error() {
    let result = glob(GlobInput {
        pattern: "*".to_string(),
        path: "/nonexistent/cosh_test_path".to_string(),
        file_type: None,
        recursive: None,
        hidden: None,
        max_results: None,
        gitignore: None,
        sort_by_mtime: None,
        timeout_ms: None,
    });

    assert!(result.is_err(), "nonexistent path should return an error");
}

#[test]
fn grep_finds_matches_in_directory() {
    let out = grep(GrepInput {
        pattern: "pub fn glob".to_string(),
        path: FIND_DIR.to_string(),
        glob: Some("*.rs".to_string()),
        file_type: None,
        ignore_case: None,
        max_count: Some(10),
        context_before: None,
        context_after: None,
        hidden: None,
        gitignore: Some(true),
        timeout_ms: None,
    })
    .expect("grep should succeed");

    assert!(!out.matches.is_empty(), "expected at least one match");
    assert!(out.files_searched > 0);
    assert!(out.files_with_matches > 0);
    for m in &out.matches {
        assert!(
            m.line.contains("pub fn glob"),
            "matched line should contain the pattern, got: {}",
            m.line
        );
    }
}

#[test]
fn grep_finds_matches_in_single_file() {
    let out = grep(GrepInput {
        pattern: "pub fn glob".to_string(),
        path: GLOB_FILE.to_string(),
        glob: None,
        file_type: None,
        ignore_case: None,
        max_count: None,
        context_before: None,
        context_after: None,
        hidden: None,
        gitignore: None,
        timeout_ms: None,
    })
    .expect("grep on a single file should succeed");

    assert!(
        !out.matches.is_empty(),
        "expected at least one match in glob.rs"
    );
    assert_eq!(out.files_searched, 1);
}

#[test]
fn grep_returns_context_lines() {
    let out = grep(GrepInput {
        pattern: "pub fn glob".to_string(),
        path: GLOB_FILE.to_string(),
        glob: None,
        file_type: None,
        ignore_case: None,
        max_count: Some(1),
        context_before: Some(2),
        context_after: Some(2),
        hidden: None,
        gitignore: None,
        timeout_ms: None,
    })
    .expect("grep with context should succeed");

    assert!(!out.matches.is_empty(), "expected at least one match");
    let first = &out.matches[0];
    assert!(
        !first.context_before.is_empty() || !first.context_after.is_empty(),
        "expected at least one context line around the match"
    );
}

#[test]
fn grep_ignore_case_widens_results() {
    let sensitive = grep(GrepInput {
        pattern: "PUB FN GLOB".to_string(),
        path: GLOB_FILE.to_string(),
        glob: None,
        file_type: None,
        ignore_case: Some(false),
        max_count: None,
        context_before: None,
        context_after: None,
        hidden: None,
        gitignore: None,
        timeout_ms: None,
    })
    .expect("case-sensitive grep should succeed");

    let insensitive = grep(GrepInput {
        pattern: "PUB FN GLOB".to_string(),
        path: GLOB_FILE.to_string(),
        glob: None,
        file_type: None,
        ignore_case: Some(true),
        max_count: None,
        context_before: None,
        context_after: None,
        hidden: None,
        gitignore: None,
        timeout_ms: None,
    })
    .expect("case-insensitive grep should succeed");

    assert_eq!(
        sensitive.total_matches, 0,
        "uppercase pattern must not match in case-sensitive mode"
    );
    assert!(
        insensitive.total_matches > 0,
        "uppercase pattern must match in case-insensitive mode"
    );
}

#[test]
fn grep_nonexistent_path_returns_error() {
    let result = grep(GrepInput {
        pattern: "fn".to_string(),
        path: "/nonexistent/cosh_test_path".to_string(),
        glob: None,
        file_type: None,
        ignore_case: None,
        max_count: None,
        context_before: None,
        context_after: None,
        hidden: None,
        gitignore: None,
        timeout_ms: None,
    });

    assert!(result.is_err(), "nonexistent path should return an error");
}
