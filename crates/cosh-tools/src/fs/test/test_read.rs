use std::path::{Path, PathBuf};

use super::super::read::read;
use super::super::types::{FsMetadata, FsRead, Target};
use cosh_sdk::hashline::snapshots::SnapshotStore;

/// Create a scratch directory rooted at itself, so the path guard accepts the
/// fixtures inside it. Recreated fresh on each call.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cosh_fs_read_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp test directory");
    dir
}

fn meta_with_root(root: &Path) -> FsMetadata {
    FsMetadata {
        root: root.to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

#[tokio::test]
async fn test_function_search() {
    // Rust fixtures with known symbols (a fn for the symbol search, a plain
    // file for the whole-file read) — seeded in a scratch dir so the test
    // exercises the same code paths without depending on repo layout.
    let dir = temp_dir("symbol_search");
    let fn_file = dir.join("fixture_fn.rs");
    std::fs::write(&fn_file, "pub fn fixture_symbol() -> u32 {\n    42\n}\n").unwrap();
    let plain_file = dir.join("fixture_plain.rs");
    std::fs::write(&plain_file, "line one\nline two\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![
                Target {
                    path: fn_file.to_string_lossy().to_string(),
                    line: None,
                    symbol: Some("fixture_symbol".to_string()),
                    line_range: None,

                    offset: None,
                    limit: None,
                },
                Target {
                    path: plain_file.to_string_lossy().to_string(),
                    line: None,
                    symbol: None,
                    line_range: None,

                    offset: None,
                    limit: None,
                },
            ],
        },
    )
    .await;
    assert!(!results.is_empty(), "expected at least one result");
    for r in &results {
        assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
        assert!(!r.file_hash.is_empty(), "file_hash should not be empty");
        assert!(!r.header.is_empty(), "header should not be empty");
        assert!(!r.content.is_empty(), "content should not be empty");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_line_block() {
    let dir = temp_dir("line_block");
    let file = dir.join("fixture.rs");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(5),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;
    assert!(!results.is_empty(), "expected one result");
    let result = &results[0];
    assert!(
        result.warnings.is_none(),
        "unexpected warning: {:?}",
        result.warnings
    );
    assert!(!result.file_hash.is_empty());
    assert!(!result.header.is_empty());
    assert!(!result.content.is_empty());
    assert!(result.content.contains("5| l5"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&dir);
}

// ── line_range exact reads ─────────────────────────────────────────────

#[tokio::test]
async fn test_offset_limit_reads_exact_lines() {
    let dir = temp_dir("offset_limit");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: Some(2),
                limit: Some(3),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(r.content.contains("2| l2"), "line 2 shown: {}", r.content);
    assert!(r.content.contains("3| l3"), "line 3 shown: {}", r.content);
    assert!(r.content.contains("4| l4"), "line 4 shown: {}", r.content);
    assert!(
        !r.content.contains("5| l5") && !r.content.contains("1| l1"),
        "lines outside offset..offset+limit-1 must not appear: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_offset_limit_clamps_beyond_eof() {
    let dir = temp_dir("offset_limit_eof");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: Some(2),
                limit: Some(100),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(
        r.warnings.is_none(),
        "clamped range is not an error: {:?}",
        r.warnings
    );
    assert!(
        r.content.contains("2| l2") && r.content.contains("3| l3"),
        "lines to EOF shown: {}",
        r.content
    );
    assert!(
        !r.content.contains("4| "),
        "nothing beyond EOF may appear: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_limit_without_offset_is_ignored_and_reads_whole_file() {
    let dir = temp_dir("limit_only");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: None,
                limit: Some(1),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(
        r.content.contains("1| l1") && r.content.contains("3| l3"),
        "limit without offset must not silently slice: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_offset_wins_over_legacy_line_range() {
    let dir = temp_dir("offset_wins");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("4-5".to_string()),
                offset: Some(2),
                limit: Some(1),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(
        r.content.contains("2| l2") && !r.content.contains("4| l4"),
        "offset+limit range takes precedence over legacy line_range: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_offset_without_limit_reads_syntactic_block() {
    let dir = temp_dir("offset_block");
    let file = dir.join("a.rs");
    std::fs::write(&file, "// padding\nfn main() {\n    println!(\"hi\");\n}\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: Some(2),
                limit: None,
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(
        r.content.contains("fn main"),
        "offset without limit reads the syntactic block containing the line: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_huge_offset_limit_saturates_without_overflow() {
    let dir = temp_dir("offset_overflow");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();

    // usize::MAX-scale values must clamp, never panic (debug overflow) or
    // leak past the u32 legacy range parser as a parse error.
    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: Some(usize::MAX - 1),
                limit: Some(usize::MAX),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    // A start beyond EOF is the parser's clean "beyond end of file" warning,
    // not a panic and not a silent whole-file fallback.
    assert!(
        r.warnings
            .as_deref()
            .is_some_and(|w| w.contains("beyond end of file")),
        "huge offset must surface the beyond-EOF warning: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_offset_zero_is_clamped_to_first_line() {
    let dir = temp_dir("offset_zero");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,
                offset: Some(0),
                limit: Some(2),
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(
        r.content.contains("1| l1") && r.content.contains("2| l2"),
        "offset 0 clamps to line 1: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_line_range_reads_exact_lines() {
    let dir = temp_dir("range");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("2-4".to_string()),

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(r.content.contains("2| l2"), "line 2 shown: {}", r.content);
    assert!(r.content.contains("3| l3"), "line 3 shown: {}", r.content);
    assert!(r.content.contains("4| l4"), "line 4 shown: {}", r.content);
    assert!(!r.content.contains("1| l1"), "line 1 must not be shown");
    assert!(!r.content.contains("5| l5"), "line 5 must not be shown");
    assert!(
        r.content.contains("more lines in file"),
        "remaining-lines hint expected: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_line_range_multi_with_gap_marker() {
    let dir = temp_dir("multirange");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("1-2,5-5".to_string()),

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(r.content.contains("1| l1"), "{}", r.content);
    assert!(r.content.contains("2| l2"), "{}", r.content);
    assert!(r.content.contains("5| l5"), "{}", r.content);
    assert!(!r.content.contains("3| l3"), "gap must not be shown");
    assert!(!r.content.contains("4| l4"), "gap must not be shown");
    assert!(
        r.content.contains("lines between ranges elided"),
        "gap marker expected: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_line_range_beyond_eof_is_skipped_with_notice() {
    let dir = temp_dir("beyeof");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("10-12".to_string()),

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    let warning = r
        .warnings
        .as_deref()
        .expect("out-of-bounds notice expected");
    assert!(
        warning.contains("Range 10-12 is beyond end of file (6 lines total); skipped"),
        "got: {warning}"
    );
    assert!(
        !r.content.contains("10|"),
        "no out-of-bounds lines may render: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_line_range_malformed_returns_warning_with_full_body() {
    let dir = temp_dir("badrange");
    let file = dir.join("a.txt");
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("5-1".to_string()),

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    let warning = r
        .warnings
        .as_deref()
        .expect("malformed-range warning expected");
    assert!(
        warning.contains("line_range must satisfy 1 <= start <= end"),
        "got: {warning}"
    );
    assert!(
        r.content.contains("1| l1"),
        "malformed ranges fall back to the whole numbered file: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Block elision (token-saving structural read) ────────────────────────

#[tokio::test]
async fn test_large_block_is_elided_with_rereread_footer() {
    let dir = temp_dir("elide");
    let file = dir.join("big.rs");
    let mut src = String::from("fn big() {\n");
    for i in 0..38 {
        src.push_str(&format!("    // body {i}\n"));
    }
    src.push_str("}\n");
    std::fs::write(&file, &src).unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(1),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(
        r.content.contains("1| fn big() {"),
        "block head must be shown: {}",
        r.content
    );
    assert!(
        r.content.contains('…'),
        "elided interior must carry a marker: {}",
        r.content
    );
    assert!(
        r.content.contains("lines elided; re-read with offset"),
        "re-read footer expected: {}",
        r.content
    );
    // The interior must NOT be emitted wholesale.
    assert!(
        !r.content.contains("// body 30"),
        "elided interior must not be shown: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_small_block_is_returned_whole() {
    let dir = temp_dir("smallblock");
    let file = dir.join("small.rs");
    std::fs::write(&file, "fn small() {\n    let x = 1;\n}\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(1),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(
        r.content.contains("let x = 1;"),
        "small blocks stay whole: {}",
        r.content
    );
    assert!(
        !r.content.contains("lines elided"),
        "no elision for small blocks: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_block_starting_after_line_one_renders_its_body() {
    let dir = temp_dir("blockafter1");
    let file = dir.join("app.rs");
    std::fs::write(
        &file,
        "fn helper() -> i32 {\n    1\n}\n\nfn main() {\n    let x = helper();\n    println!(\"{x}\");\n}\n",
    )
    .unwrap();

    // The block starts at line 5, past the file's first line — a regression
    // guard for block reads that used to render an empty body.
    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(5),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(
        r.content.contains("5| fn main() {"),
        "block head must be shown at its absolute line: {}",
        r.content
    );
    assert!(
        r.content.contains("6|     let x = helper();")
            && r.content.contains("7|     println!(\"{x}\");")
            && r.content.contains("8| }"),
        "block body must render with absolute line numbers: {}",
        r.content
    );
    assert!(
        !r.content.contains("1| fn helper"),
        "earlier lines must not leak into the block: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_symbol_starting_after_line_one_renders_its_body() {
    let dir = temp_dir("symafter1");
    let file = dir.join("app.rs");
    std::fs::write(
        &file,
        "fn helper() -> i32 {\n    1\n}\n\nfn main() {\n    let x = helper();\n    println!(\"{x}\");\n}\n",
    )
    .unwrap();

    // The symbol's definition starts at line 5. Used to render a shifted or
    // empty body.
    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: Some("main".to_string()),
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(
        r.content.contains("5| fn main() {")
            && r.content.contains("6|     let x = helper();")
            && r.content.contains("8| }"),
        "symbol body must render with absolute line numbers: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Seen-lines + column truncation ─────────────────────────────────────

#[tokio::test]
async fn test_whole_file_records_seen_lines() {
    let dir = temp_dir("seenfull");
    let file = dir.join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    let key = file.to_string_lossy().to_string();
    let store = cosh_sdk::rollback::session_store();
    let seen = store
        .lock()
        .expect("session store lock")
        .seen_lines(&key, &r.file_hash);
    assert_eq!(
        seen.len(),
        3,
        "whole-file read records every line, got: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_large_block_starting_after_line_one_elides_with_correct_numbers() {
    let dir = temp_dir("elideafter");
    let file = dir.join("big.rs");
    let mut src = String::from("// preamble\n");
    src.push_str("fn big() {\n");
    for i in 0..38 {
        src.push_str(&format!("    // body {i}\n"));
    }
    src.push_str("}\n");
    std::fs::write(&file, &src).unwrap();

    // The block starts at line 2. Before the fix, elision clamped the end to
    // the block's own length and the tail segment rendered under the wrong
    // line numbers (or not at all).
    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(2),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
    assert!(
        r.content.contains("2| fn big() {"),
        "block head must be shown with its real line number: {}",
        r.content
    );
    assert!(
        r.content.contains('…'),
        "elided interior must carry a marker: {}",
        r.content
    );
    assert!(
        r.content.contains("41| }"),
        "closing brace must be shown with its real line number: {}",
        r.content
    );
    assert!(
        !r.content.contains("// body 30"),
        "elided interior must not be shown: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_elided_block_records_only_surfaced_lines() {
    let dir = temp_dir("seenelide");
    let file = dir.join("big.rs");
    let mut src = String::from("fn big() {\n");
    for i in 0..38 {
        src.push_str(&format!("    // body {i}\n"));
    }
    src.push_str("}\n");
    std::fs::write(&file, &src).unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: Some(1),
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    let key = file.to_string_lossy().to_string();
    let store = cosh_sdk::rollback::session_store();
    let seen = store
        .lock()
        .expect("session store lock")
        .seen_lines(&key, &r.file_hash);
    assert!(
        seen.iter().any(|(n, _)| *n == 1),
        "the kept head line must be seen, got: {seen:?}"
    );
    assert!(
        !seen.iter().any(|(n, _)| *n == 20),
        "the elided interior must NOT be seen, got: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_long_lines_are_column_truncated() {
    let dir = temp_dir("coltrunc");
    let file = dir.join("a.txt");
    let long = format!("x{}y", "a".repeat(300));
    std::fs::write(&file, format!("{long}\nshort\n")).unwrap();

    let results = read(
        meta_with_root(&dir),
        FsRead {
            targets: vec![Target {
                path: file.to_string_lossy().to_string(),
                line: None,
                symbol: None,
                line_range: Some("1-2".to_string()),

                offset: None,
                limit: None,
            }],
        },
    )
    .await;

    let r = &results[0];
    let line1 = r
        .content
        .lines()
        .find(|l| l.starts_with("1| "))
        .expect("line 1 rendered");
    assert!(
        line1.ends_with("..."),
        "long line must end with ellipsis: {line1}"
    );
    let body = line1.strip_prefix("1| ").unwrap_or(line1);
    assert!(
        body.chars().count() <= 200,
        "line content must be cut to the column limit: {line1}"
    );
    assert!(
        r.content.contains("truncated to 200 columns"),
        "truncation notice expected in content: {}",
        r.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}
