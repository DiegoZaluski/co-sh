use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::glob::{glob, glob_with};
use super::super::types::Glob;

const FIND_DIR: &str = "/home/inky/cosh/crates/cosh-sdk/src/find";

/// Minimal temp dir that cleans itself up (no extra dependencies).
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
        let path = std::env::temp_dir().join(format!("cosh-glob-tool-{pid}-{nanos}-{seq}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, content).expect("write test file");
}

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

#[test]
fn glob_zero_matches_is_useless_with_note() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("keep.txt"), "x");

    let out =
        glob(&Glob::default(), "*.rs", tmp.path().to_str().unwrap()).expect("glob should succeed");

    assert!(out.matches.is_empty());
    assert_eq!(
        out.useless,
        Some(true),
        "zero-match results must be useless"
    );
    assert_eq!(out.timed_out, None);
    let note = out.note.as_deref().unwrap_or("");
    assert!(
        note.contains("No files found"),
        "expected no-match note, got: {note}"
    );
}

#[test]
fn glob_max_results_sets_limit_reached() {
    let tmp = TempDir::new();
    for i in 0..5 {
        write_file(&tmp.path().join(format!("f{i}.txt")), "x");
    }

    let out = glob(
        &Glob {
            max_results: Some(1),
            ..Default::default()
        },
        "*.txt",
        tmp.path().to_str().unwrap(),
    )
    .expect("glob should succeed");

    assert!(out.matches.len() <= 1, "max_results must cap the output");
    assert_eq!(
        out.limit_reached,
        Some(true),
        "a capped fetch must set limit_reached"
    );
}

#[test]
fn glob_multi_target_merges_and_rebases_paths() {
    let tmp = TempDir::new();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write_file(&a.join("one.rs"), "x");
    write_file(&b.join("two.rs"), "x");

    let targets = vec![
        a.to_str().unwrap().to_string(),
        b.to_str().unwrap().to_string(),
    ];
    let out = glob_with(&Glob::default(), "*.rs", &targets, None).expect("glob should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        paths.contains(&"a/one.rs") && paths.contains(&"b/two.rs"),
        "multi-target results must be rebased to the common ancestor: {paths:?}"
    );
    assert_eq!(out.total, 2);
}

#[test]
fn glob_multi_target_skips_missing_and_reports_them() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("present").join("x.rs"), "x");
    let missing = tmp.path().join("gone");

    let targets = vec![
        tmp.path().join("present").to_str().unwrap().to_string(),
        missing.to_str().unwrap().to_string(),
    ];
    let out = glob_with(&Glob::default(), "*.rs", &targets, None).expect("glob should succeed");

    assert_eq!(
        out.matches.len(),
        1,
        "surviving target still returns results"
    );
    let missing_paths = out.missing_paths.clone().unwrap_or_default();
    assert!(
        missing_paths.iter().any(|p| p.ends_with("gone")),
        "missing target must be reported: {missing_paths:?}"
    );
    let note = out.note.as_deref().unwrap_or("");
    assert!(
        note.contains("Skipped missing"),
        "note must mention skip: {note}"
    );
    assert_eq!(out.useless, None);
}

#[test]
fn glob_all_targets_missing_errors() {
    let tmp = TempDir::new();
    let targets = vec![
        tmp.path().join("a").to_str().unwrap().to_string(),
        tmp.path().join("b").to_str().unwrap().to_string(),
    ];

    let err = glob_with(&Glob::default(), "*.rs", &targets, None)
        .err()
        .expect("all-missing targets must error");
    assert!(err.contains("Path not found"), "got: {err}");
}

#[test]
fn glob_multi_target_streams_matches() {
    let tmp = TempDir::new();
    let a = tmp.path().join("a");
    write_file(&a.join("one.rs"), "x");
    write_file(&a.join("two.rs"), "x");

    let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cb: std::sync::Arc<super::super::glob::GlobMatchCallback> = {
        let streamed = std::sync::Arc::clone(&streamed);
        std::sync::Arc::new(move |m: &cosh_sdk::find::GlobMatch| {
            streamed.lock().unwrap().push(m.path.clone());
        })
    };
    let targets = vec![a.to_str().unwrap().to_string()];
    let out = glob_with(&Glob::default(), "*.rs", &targets, Some(cb)).expect("glob should succeed");

    assert_eq!(out.matches.len(), 2);
    assert_eq!(
        streamed.lock().unwrap().len(),
        2,
        "every match must be streamed through on_match"
    );
}

#[test]
fn glob_precancelled_timeout_is_error() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("f.txt"), "x");

    // A timeout that already elapsed before the scan starts has nothing to
    // salvage — it surfaces as an error, not as empty partials.
    let err = glob(
        &Glob {
            timeout_ms: Some(0),
            ..Default::default()
        },
        "*",
        tmp.path().to_str().unwrap(),
    )
    .err()
    .expect("pre-cancelled glob must error");
    assert!(
        err.contains("Timeout"),
        "expected timeout error, got: {err}"
    );
}

// -------------------------------------------------------------------------
// Features: advanced pattern parsing (glob|dir|file), working-dir-relative
// paths with trailing slashes, explicit recursion control, and output formats
// (flat/grouped/tree).
// -------------------------------------------------------------------------

use super::super::glob::{GlobTargetSpec, glob_targets_with};
use super::super::glob::{parse_find_pattern, to_path_list};

#[test]
fn glob_scoped_glob_does_not_recurse_into_subdirs() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("sub").join("a.rs"), "x");
    write_file(&tmp.path().join("nested").join("deep").join("b.rs"), "x");

    let spec = parse_find_pattern("sub/*.rs");
    assert_eq!(spec.base_path.to_string_lossy(), "sub");
    assert_eq!(spec.glob_pattern, "*.rs");
    assert!(
        !spec.recursive,
        "src/*.rs must not auto-recurse into src/deep/"
    );

    let out = glob_targets_with(
        &Glob::default(),
        &[GlobTargetSpec {
            base_path: tmp.path().join("sub"),
            pattern: spec.glob_pattern,
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("scoped glob should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, vec!["sub/a.rs"], "got: {paths:?}");
}

#[test]
fn glob_recursive_glob_with_globstar_matches_deep() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("sub").join("a.rs"), "x");
    write_file(&tmp.path().join("sub").join("deep").join("b.rs"), "x");

    let spec = parse_find_pattern("sub/**/*.rs");
    assert!(spec.recursive, "** must recurse");
    let out = glob_targets_with(
        &Glob::default(),
        &[GlobTargetSpec {
            base_path: tmp.path().join("sub"),
            pattern: spec.glob_pattern,
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("recursive glob should succeed");

    let mut paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["sub/a.rs", "sub/deep/b.rs"]);
}

#[test]
fn glob_directory_literal_lists_contents() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("a.rs"), "x");
    write_file(&tmp.path().join("b.txt"), "x");
    std::fs::create_dir_all(tmp.path().join("sub")).ok();

    let spec = parse_find_pattern(tmp.path().to_str().unwrap());
    assert!(!spec.has_glob, "a directory literal has no glob chars");
    let out = glob_targets_with(
        &Glob::default(),
        &[GlobTargetSpec {
            base_path: tmp.path().to_path_buf(),
            pattern: String::new(),
            has_glob: false,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("directory literal should list contents");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        paths.contains(&"a.rs") && paths.contains(&"b.txt") && paths.contains(&"sub/"),
        "literal dir must list all children: {paths:?}"
    );
}

#[test]
fn glob_file_literal_returns_the_file() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("target.rs"), "fn main() {}");

    let out = glob_targets_with(
        &Glob::default(),
        &[GlobTargetSpec {
            base_path: tmp.path().to_path_buf(),
            pattern: "target.rs".to_string(),
            has_glob: false,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("file literal should succeed");

    assert_eq!(out.matches.len(), 1);
    assert_eq!(out.matches[0].path, "target.rs");
    assert_eq!(out.matches[0].file_type, "file");
}

#[test]
fn glob_paths_relative_to_cwd_with_dir_trailing_slash() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("src").join("a.rs"), "x");
    write_file(&tmp.path().join("README.md"), "hello");

    let out = glob_targets_with(
        &Glob::default(),
        &[GlobTargetSpec {
            base_path: tmp.path().join("src"),
            pattern: "**/*".to_string(),
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("cwd-relative glob should succeed");

    for entry in &out.matches {
        if entry.file_type == "dir" {
            assert!(
                entry.path.ends_with('/'),
                "directories must carry a trailing slash: {}",
                entry.path
            );
        }
        assert!(
            !entry.path.starts_with(tmp.path().to_str().unwrap()),
            "paths must be CWD-relative, got absolute: {}",
            entry.path
        );
    }
    assert_eq!(out.cwd.as_deref(), Some(tmp.path().to_str().unwrap()));
}

#[test]
fn glob_grouped_format_groups_by_directory() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("src").join("a.rs"), "x");
    write_file(&tmp.path().join("src").join("b.rs"), "x");
    write_file(&tmp.path().join("tests").join("t.rs"), "x");

    let out = glob_targets_with(
        &Glob {
            format: Some("grouped".to_string()),
            ..Default::default()
        },
        &[GlobTargetSpec {
            base_path: tmp.path().to_path_buf(),
            pattern: "**/*.rs".to_string(),
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("grouped glob should succeed");

    assert!(out.formatted.contains("src/\n  a.rs\n  b.rs"));
    assert!(out.formatted.contains("tests/\n  t.rs"));
}

#[test]
fn glob_tree_format_indents_by_depth() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("src").join("a.rs"), "x");
    write_file(&tmp.path().join("src").join("deep").join("b.rs"), "x");

    let out = glob_targets_with(
        &Glob {
            format: Some("tree".to_string()),
            ..Default::default()
        },
        &[GlobTargetSpec {
            base_path: tmp.path().to_path_buf(),
            pattern: "**/*.rs".to_string(),
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .expect("tree glob should succeed");

    assert!(out.formatted.contains("src/\n  a.rs"));
    assert!(
        out.formatted.contains("deep/\n    b.rs"),
        "{}",
        out.formatted
    );
}

#[test]
fn glob_invalid_format_rejected() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("a.rs"), "x");
    let err = glob_targets_with(
        &Glob {
            format: Some("json".to_string()),
            ..Default::default()
        },
        &[GlobTargetSpec {
            base_path: tmp.path().to_path_buf(),
            pattern: "*".to_string(),
            has_glob: true,
        }],
        None,
        Some(tmp.path()),
    )
    .err()
    .expect("invalid format must error");
    assert!(err.contains("invalid format"), "got: {err}");
}

#[test]
fn to_path_list_handles_semicolons() {
    assert_eq!(
        to_path_list(Some("a.rs; b.rs")),
        vec!["a.rs".to_string(), "b.rs".to_string()]
    );
    assert!(to_path_list(None).is_empty());
}

// -------------------------------------------------------------------------
// find_glob `max_results` semantics (mirrors the oh-my-pi reference): a
// default cap of 200; the model can only LOWER the cap, never raise it above
// the ceiling — a larger value is clamped, not honored.
// -------------------------------------------------------------------------

use super::super::Find;

const DEFAULT_GLOB_LIMIT: u32 = 200;

fn seed_tree(root: &Path, files: usize) {
    for i in 0..files {
        write_file(&root.join(format!("f{i:04}.txt")), "x");
    }
}

fn find_glob_full(
    find: &Find,
    path: &Path,
    max_results: Option<u32>,
) -> Result<super::super::types::GlobOutput, String> {
    find.glob_full(
        "*.txt",
        Some(path.to_string_lossy().into_owned()),
        None,
        super::super::GlobCallOptions {
            max_results,
            ..Default::default()
        },
        None,
    )
}

#[test]
fn find_glob_default_caps_at_200() {
    let tmp = TempDir::new();
    seed_tree(tmp.path(), 250);
    let find = Find::new().cwd(tmp.path());

    let out = find_glob_full(&find, tmp.path(), None).expect("glob should succeed");

    assert_eq!(out.matches.len(), DEFAULT_GLOB_LIMIT as usize);
    assert_eq!(out.total, DEFAULT_GLOB_LIMIT);
    assert_eq!(
        out.limit_reached,
        Some(true),
        "a capped fetch must flag limit_reached"
    );
    let note = out.note.as_deref().unwrap_or("");
    assert!(note.contains("Limit reached"), "got: {note}");
}

#[test]
fn find_glob_max_results_lowers_result_set() {
    let tmp = TempDir::new();
    seed_tree(tmp.path(), 250);
    let find = Find::new().cwd(tmp.path());

    let out = find_glob_full(&find, tmp.path(), Some(50)).expect("glob should succeed");

    assert_eq!(out.matches.len(), 50);
    assert_eq!(out.total, 50);
    assert_eq!(out.limit_reached, Some(true));
}

#[test]
fn find_glob_max_results_above_ceiling_is_clamped() {
    let tmp = TempDir::new();
    seed_tree(tmp.path(), 250);
    let find = Find::new().cwd(tmp.path());

    let out = find_glob_full(&find, tmp.path(), Some(500)).expect("glob should succeed");

    assert_eq!(
        out.matches.len(),
        DEFAULT_GLOB_LIMIT as usize,
        "a max_results above 200 must clamp to the ceiling"
    );
    assert_eq!(out.limit_reached, Some(true));
}

#[test]
fn find_glob_small_tree_returns_all_without_flag() {
    let tmp = TempDir::new();
    seed_tree(tmp.path(), 5);
    let find = Find::new().cwd(tmp.path());

    let out = find_glob_full(&find, tmp.path(), None).expect("glob should succeed");

    assert_eq!(out.matches.len(), 5);
    assert_eq!(out.total, 5);
    assert_eq!(out.limit_reached, None, "no cap cut means no flag");
}

#[test]
fn find_glob_zero_max_results_is_rejected() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("a.txt"), "x");
    let find = Find::new().cwd(tmp.path());

    let err = find_glob_full(&find, tmp.path(), Some(0))
        .err()
        .expect("a zero max_results must be rejected");
    assert!(err.contains("positive number"), "got: {err}");
}

// -------------------------------------------------------------------------
// find_glob `file_type`, `hidden`, and `gitignore` (mirrors the oh-my-pi
// reference: `hidden` and `gitignore` default to true; `file_type` is an
// extension over the reference).
// -------------------------------------------------------------------------

fn glob_full_typed(
    find: &Find,
    path: &Path,
    pattern: &str,
    file_type: Option<String>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
) -> Result<super::super::types::GlobOutput, String> {
    find.glob_full(
        pattern,
        Some(path.to_string_lossy().into_owned()),
        None,
        super::super::GlobCallOptions {
            file_type,
            hidden,
            gitignore,
            ..Default::default()
        },
        None,
    )
}

fn seed_typed_tree(root: &Path) {
    write_file(&root.join("a.rs"), "x");
    write_file(&root.join("sub").join("b.rs"), "x");
    let _ = std::os::unix::fs::symlink("a.rs", root.join("link.rs"));
}

#[test]
fn find_glob_file_type_filters_dirs() {
    let tmp = TempDir::new();
    seed_typed_tree(tmp.path());
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(
        &find,
        tmp.path(),
        "**/*",
        Some("dir".to_string()),
        None,
        None,
    )
    .expect("glob with file_type=dir should succeed");

    assert!(!out.matches.is_empty());
    for entry in &out.matches {
        assert_eq!(entry.file_type, "dir", "got: {}", entry.path);
    }
}

#[test]
fn find_glob_file_type_filters_files() {
    let tmp = TempDir::new();
    seed_typed_tree(tmp.path());
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(
        &find,
        tmp.path(),
        "**/*",
        Some("file".to_string()),
        None,
        None,
    )
    .expect("glob with file_type=file should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(paths.contains(&"a.rs") && paths.contains(&"sub/b.rs"));
    assert!(paths.iter().all(|p| !p.ends_with('/')), "got: {paths:?}");
}

#[test]
fn find_glob_file_type_includes_symlinks() {
    let tmp = TempDir::new();
    seed_typed_tree(tmp.path());
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(
        &find,
        tmp.path(),
        "**/*",
        Some("symlink".to_string()),
        None,
        None,
    )
    .expect("glob with file_type=symlink should succeed");

    assert_eq!(out.matches.len(), 1);
    assert_eq!(out.matches[0].file_type, "symlink");
    assert_eq!(out.matches[0].path, "link.rs");
}

#[test]
fn find_glob_invalid_file_type_is_rejected() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("a.rs"), "x");
    let find = Find::new().cwd(tmp.path());

    let err = glob_full_typed(
        &find,
        tmp.path(),
        "**/*",
        Some("executable".to_string()),
        None,
        None,
    )
    .err()
    .expect("an unknown file_type must be rejected");
    assert!(err.contains("invalid file_type"), "got: {err}");
}

#[test]
fn find_glob_hidden_defaults_to_true() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("visible.txt"), "x");
    write_file(&tmp.path().join(".hidden.txt"), "x");
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(&find, tmp.path(), "**", None, None, None)
        .expect("glob over the tree should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with(".hidden.txt")),
        "hidden files must be included by default: {paths:?}"
    );
}

#[test]
fn find_glob_hidden_false_excludes_dotfiles() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("visible.txt"), "x");
    write_file(&tmp.path().join(".hidden.txt"), "x");
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(&find, tmp.path(), "**", None, Some(false), None)
        .expect("glob with hidden=false should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        paths.iter().all(|p| !p.ends_with(".hidden.txt")),
        "hidden files must stay out when hidden=false: {paths:?}"
    );
}

#[test]
fn find_glob_hidden_never_leaks_git() {
    let tmp = TempDir::new();
    write_file(&tmp.path().join("a.txt"), "x");
    write_file(&tmp.path().join(".git").join("config"), "x");
    let find = Find::new().cwd(tmp.path());

    let out = glob_full_typed(&find, tmp.path(), "**", None, None, None)
        .expect("glob should succeed");

    for entry in &out.matches {
        assert!(
            !entry.path.contains(".git"),
            ".git must never surface even with hidden default true: {}",
            entry.path
        );
    }
}

#[test]
fn find_glob_gitignore_respected_by_default() {
    let tmp = TempDir::new();
    let root = tmp.path();
    // A `.git` marker dir lets the ignore walker apply `.gitignore` rules.
    std::fs::create_dir_all(root.join(".git")).expect("create .git marker");
    write_file(&root.join(".gitignore"), "ignored.log\n");
    write_file(&root.join("kept.txt"), "x");
    write_file(&root.join("ignored.log"), "x");
    let find = Find::new().cwd(root);

    let out = glob_full_typed(&find, root, "**", None, Some(false), None)
        .expect("glob should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.ends_with("kept.txt")));
    assert!(
        paths.iter().all(|p| !p.ends_with("ignored.log")),
        ".gitignore must be respected by default: {paths:?}"
    );
}

#[test]
fn find_glob_gitignore_false_includes_ignored() {
    let tmp = TempDir::new();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("create .git marker");
    write_file(&root.join(".gitignore"), "ignored.log\n");
    write_file(&root.join("kept.txt"), "x");
    write_file(&root.join("ignored.log"), "x");
    let find = Find::new().cwd(root);

    let out = glob_full_typed(&find, root, "**/*", None, Some(false), Some(false))
        .expect("glob with gitignore=false should succeed");

    let paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("ignored.log")),
        "gitignore=false must include ignored files: {paths:?}"
    );
}
