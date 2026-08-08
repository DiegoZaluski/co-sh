use super::super::glob::{PathFormat, format_paths, parent_dir};

fn entries() -> Vec<String> {
    vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "tests/t.rs".to_string(),
        "root.txt".to_string(),
    ]
}

#[test]
fn flat_joins_lines() {
    assert_eq!(
        format_paths(&entries(), PathFormat::Flat),
        "src/a.rs\nsrc/b.rs\ntests/t.rs\nroot.txt"
    );
}

#[test]
fn grouped_adds_directory_headers() {
    let out = format_paths(&entries(), PathFormat::Grouped);
    assert!(out.contains("src/\n  a.rs\n  b.rs"));
    assert!(out.contains("tests/\n  t.rs"));
    assert!(out.contains("./\n  root.txt"));
}

#[test]
fn tree_indents_by_depth() {
    let out = format_paths(&entries(), PathFormat::Tree);
    assert!(out.contains("src/\n  a.rs\n  b.rs"));
    assert!(out.contains("tests/\n  t.rs"));
    assert!(out.contains("root.txt"));
}

#[test]
fn tree_prints_inner_dir_once() {
    let paths = vec!["a/x.rs".to_string(), "a/b/y.rs".to_string()];
    let out = format_paths(&paths, PathFormat::Tree);
    // Pre-order: `a/`, then children in sorted order (`b/` before `x.rs`).
    assert_eq!(out, "a/\n  b/\n    y.rs\n  x.rs");
}

#[test]
fn parse_format_validates() {
    assert_eq!(PathFormat::parse_format("flat").unwrap(), PathFormat::Flat);
    assert_eq!(
        PathFormat::parse_format("GROUPED").unwrap(),
        PathFormat::Grouped
    );
    assert!(PathFormat::parse_format("json").is_err());
}

#[test]
fn parent_of_root() {
    assert_eq!(parent_dir("root.txt"), ".");
    assert_eq!(parent_dir("src/a.rs"), "src");
    assert_eq!(parent_dir("/abs/x.ts"), "/abs");
}
