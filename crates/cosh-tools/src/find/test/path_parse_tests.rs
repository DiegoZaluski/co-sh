use std::path::{Path, PathBuf};

use super::super::glob::{format_path_relative_to_cwd, parse_find_pattern, to_path_list};

#[test]
fn parse_bare_glob_recurses() {
    let parsed = parse_find_pattern("*.rs");
    assert_eq!(parsed.base_path, PathBuf::from("."));
    assert_eq!(parsed.glob_pattern, "**/*.rs");
    assert!(parsed.has_glob);
    assert!(parsed.recursive);
}

#[test]
fn parse_scoped_glob_stays_shallow() {
    let parsed = parse_find_pattern("src/*.rs");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert_eq!(parsed.glob_pattern, "*.rs");
    assert!(parsed.has_glob);
    assert!(!parsed.recursive, "src/*.rs must not recurse into src/sub/");
}

#[test]
fn parse_existing_recursive_glob_unchanged() {
    let parsed = parse_find_pattern("src/**/*.rs");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert_eq!(parsed.glob_pattern, "**/*.rs");
    assert!(parsed.recursive);
}

#[test]
fn parse_dir_star_stays_shallow() {
    let parsed = parse_find_pattern("src/*");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert_eq!(parsed.glob_pattern, "*");
    assert!(!parsed.recursive);
}

#[test]
fn parse_literal_dir_no_glob() {
    let parsed = parse_find_pattern("src");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert!(!parsed.has_glob);
    assert!(!parsed.recursive);
}

#[test]
fn parse_trailing_slash_normalized() {
    let parsed = parse_find_pattern("src/");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert!(!parsed.has_glob);
}

#[test]
fn parse_backslashes_normalized() {
    let parsed = parse_find_pattern("src\\*.rs");
    assert_eq!(parsed.base_path, PathBuf::from("src"));
    assert_eq!(parsed.glob_pattern, "*.rs");
}

#[test]
fn to_path_list_splits_on_semicolon() {
    assert_eq!(
        to_path_list(Some("src/**/*.rs; test/**/*.rs")),
        vec!["src/**/*.rs".to_string(), "test/**/*.rs".to_string()]
    );
    assert!(to_path_list(None).is_empty());
    assert!(to_path_list(Some(" ; ")).is_empty());
}

#[test]
fn format_relative_under_cwd() {
    let cwd = Path::new("/work");
    assert_eq!(
        format_path_relative_to_cwd("/work/src/a.ts", cwd, false),
        "src/a.ts"
    );
    assert_eq!(format_path_relative_to_cwd("/work/src", cwd, true), "src/");
    assert_eq!(
        format_path_relative_to_cwd("/elsewhere/x.ts", cwd, false),
        "/elsewhere/x.ts"
    );
    assert_eq!(
        format_path_relative_to_cwd("rel/a.ts", Path::new("/work"), false),
        "rel/a.ts"
    );
}
