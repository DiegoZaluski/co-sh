use super::super::Find;
use super::super::grep::{grep, grep_targets};
use super::super::types::Grep;
use cosh_sdk::hashline::snapshots::SnapshotStore;

/// Seed a Rust file with `pub fn glob` and `pub fn grep` in a scratch dir —
/// the shape the grep tests need (a `.rs` tree with known symbols). Each call
/// gets a fresh directory so tests stay independent.
fn fixture_dir(name: &str) -> std::path::PathBuf {
    let dir = temp_dir(name);
    std::fs::write(
        dir.join("glob.rs"),
        "pub fn glob(pattern: &str) -> usize {\n    pattern.len()\n}\n",
    )
    .expect("write glob.rs fixture");
    std::fs::write(
        dir.join("grep.rs"),
        "pub fn grep(needle: &str) -> usize {\n    needle.len()\n}\n",
    )
    .expect("write grep.rs fixture");
    dir
}

/// Path of the seeded `glob.rs` fixture inside a [`fixture_dir`].
fn fixture_glob_file(dir: &std::path::Path) -> String {
    dir.join("glob.rs").to_string_lossy().into_owned()
}

/// Create a scratch directory for grep tests that need controlled fixtures.
/// Recreated fresh on each call so tests are independent of prior state.
fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cosh_find_test_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp test directory");
    dir
}

#[test]
fn grep_finds_matches_in_directory() {
    let dir = fixture_dir("grep_dir");
    let out = grep(
        &Grep {
            glob: Some("*.rs".to_string()),
            max_count: Some(10),
            gitignore: Some(true),
            ..Default::default()
        },
        "pub fn glob",
        dir.to_str().unwrap(),
    )
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_finds_matches_in_single_file() {
    let dir = fixture_dir("grep_single");
    let file = fixture_glob_file(&dir);
    let out =
        grep(&Grep::default(), "pub fn glob", &file).expect("grep on a single file should succeed");

    assert!(
        !out.matches.is_empty(),
        "expected at least one match in glob.rs"
    );
    assert_eq!(out.files_searched, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_returns_context_lines() {
    let dir = fixture_dir("grep_ctx");
    let file = fixture_glob_file(&dir);
    let out = grep(
        &Grep {
            max_count: Some(1),
            context_before: Some(2),
            context_after: Some(2),
            ..Default::default()
        },
        "pub fn glob",
        &file,
    )
    .expect("grep with context should succeed");

    assert!(!out.matches.is_empty(), "expected at least one match");
    let first = &out.matches[0];
    assert!(
        !first.context_before.is_empty() || !first.context_after.is_empty(),
        "expected at least one context line around the match"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_ignore_case_widens_results() {
    let dir = fixture_dir("grep_case");
    let file = fixture_glob_file(&dir);
    let sensitive = grep(
        &Grep {
            ignore_case: Some(false),
            ..Default::default()
        },
        "PUB FN GLOB",
        &file,
    )
    .expect("case-sensitive grep should succeed");

    let insensitive = grep(
        &Grep {
            ignore_case: Some(true),
            ..Default::default()
        },
        "PUB FN GLOB",
        &file,
    )
    .expect("case-insensitive grep should succeed");

    assert_eq!(
        sensitive.total_matches, 0,
        "uppercase pattern must not match in case-sensitive mode"
    );
    assert!(
        insensitive.total_matches > 0,
        "uppercase pattern must match in case-insensitive mode"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_nonexistent_path_returns_error() {
    let result = grep(&Grep::default(), "fn", "/nonexistent/cosh_test_path");

    assert!(result.is_err(), "nonexistent path should return an error");
}

#[test]
fn grep_surfaces_hashline_anchors_per_file() {
    let dir = fixture_dir("grep_anchors");
    let out = grep(
        &Grep {
            glob: Some("*.rs".to_string()),
            max_count: Some(10),
            gitignore: Some(true),
            ..Default::default()
        },
        "pub fn glob",
        dir.to_str().unwrap(),
    )
    .expect("grep should succeed");

    assert!(!out.matches.is_empty(), "expected at least one match");
    assert!(
        !out.files.is_empty(),
        "expected hashline anchors for matched files"
    );
    assert!(
        out.files.len() <= 20,
        "anchors must respect the file window bound"
    );
    for f in &out.files {
        assert_eq!(
            f.file_hash.len(),
            4,
            "file_hash must be a 4-hex tag: {}",
            f.file_hash
        );
        assert!(
            f.file_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "file_hash must be hex: {}",
            f.file_hash
        );
        assert!(
            f.header.starts_with('\u{b6}'),
            "header must start with the hashline sigil: {}",
            f.header
        );
        assert!(
            f.header.ends_with(&format!("#{}", f.file_hash)),
            "header must end with #TAG: {}",
            f.header
        );
        // Each anchor must pair with at least one match.
        let paired = out.matches.iter().filter(|m| m.path == f.path).count();
        assert!(paired > 0, "no match pairs with anchor {}", f.path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_single_file_anchor_matches_content_hash() {
    let dir = fixture_dir("grep_anchor_hash");
    let file = fixture_glob_file(&dir);
    let out =
        grep(&Grep::default(), "pub fn glob", &file).expect("grep on a single file should succeed");

    assert!(
        !out.matches.is_empty(),
        "expected at least one match in glob.rs"
    );
    assert_eq!(
        out.files.len(),
        1,
        "single-file grep must anchor exactly one file"
    );

    let text = std::fs::read_to_string(&file).expect("read search target");
    let expected = cosh_sdk::hashline::format::compute_file_hash(&text);
    assert_eq!(
        out.files[0].file_hash, expected,
        "anchor hash must equal the file content hash"
    );
    assert!(
        out.files[0].header.contains(file.as_str()),
        "header should carry the absolute path: {}",
        out.files[0].header
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_without_matches_has_no_anchors() {
    let dir = fixture_dir("grep_no_match");
    let file = fixture_glob_file(&dir);
    let out =
        grep(&Grep::default(), "zzz_no_such_pattern_xyz", &file).expect("grep should succeed");

    assert!(out.matches.is_empty());
    assert!(out.files.is_empty(), "no matches means no anchors");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_auto_enables_multiline_for_cross_line_patterns() {
    let dir = temp_dir("multiline");
    std::fs::write(dir.join("a.txt"), "fn foo() {\n  return 1;\n}\n").unwrap();

    let out = grep(
        &Grep::default(),
        r"foo\(\) \{\n  return",
        dir.to_str().unwrap(),
    )
    .expect("grep should succeed");

    assert_eq!(
        out.total_matches, 1,
        "a pattern spanning lines must match without an explicit multiline flag"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_truncates_long_lines() {
    let dir = temp_dir("trunc");
    let long = format!("x{}y", "a".repeat(1000));
    std::fs::write(dir.join("a.txt"), format!("{long}\n")).unwrap();

    let out = grep(&Grep::default(), "x", dir.to_str().unwrap()).expect("grep should succeed");

    assert!(!out.matches.is_empty(), "expected a match");
    let m = &out.matches[0];
    assert_eq!(
        m.truncated,
        Some(true),
        "long line must be flagged truncated"
    );
    assert!(
        m.line.ends_with("..."),
        "truncated line must end with an ellipsis, got: {}",
        m.line
    );
    assert!(
        m.line.len() <= 200,
        "line must be cut to the column limit, got {} chars",
        m.line.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_file_window_paginates_with_skip() {
    let dir = temp_dir("window");
    for i in 0..25 {
        std::fs::write(dir.join(format!("f{i:02}.txt")), format!("needle {i}\n")).unwrap();
    }

    let first =
        grep(&Grep::default(), "needle", dir.to_str().unwrap()).expect("grep should succeed");
    assert!(
        first.file_limit_reached,
        "more files than the window must flag pagination"
    );
    let note = first.note.as_deref().expect("pagination note expected");
    assert!(note.contains("skip="), "note must suggest skip: {note}");
    let mut page_one: Vec<&str> = first.matches.iter().map(|m| m.path.as_str()).collect();
    page_one.sort_unstable();
    page_one.dedup();
    assert_eq!(page_one.len(), 20, "window must surface at most 20 files");

    let second = grep(
        &Grep {
            skip: Some(20),
            ..Default::default()
        },
        "needle",
        dir.to_str().unwrap(),
    )
    .expect("paged grep should succeed");
    assert!(
        !second.matches.is_empty(),
        "page two must still have matches"
    );
    let page_two: Vec<&str> = second.matches.iter().map(|m| m.path.as_str()).collect();
    assert!(
        page_two.iter().any(|p| !page_one.contains(p)),
        "page two must surface files beyond the first window"
    );
    assert!(
        !second.file_limit_reached,
        "last page must not flag more files"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_per_file_cap_limits_hot_file() {
    let dir = temp_dir("hot");
    let mut hot = String::new();
    for i in 0..30 {
        hot.push_str(&format!("needle {i}\n"));
    }
    std::fs::write(dir.join("hot.txt"), &hot).unwrap();
    std::fs::write(dir.join("other.txt"), "needle other\n").unwrap();

    let out = grep(&Grep::default(), "needle", dir.to_str().unwrap()).expect("grep should succeed");

    assert!(
        out.per_file_limit_reached,
        "a hot file must flag the per-file cap"
    );
    let hot_count = out
        .matches
        .iter()
        .filter(|m| m.path.ends_with("hot.txt"))
        .count();
    assert_eq!(
        hot_count, 20,
        "hot file matches must be capped at the multi-file per-file limit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_total_cap_skips_file_window_pagination() {
    let dir = temp_dir("capwin");
    for i in 0..25 {
        std::fs::write(dir.join(format!("f{i:02}.txt")), format!("needle {i}\n")).unwrap();
    }

    // A caller with a total match cap is not paginating: the window note must
    // be absent and `skip` is ignored (the engine's match-bounded fetch cannot
    // cover files beyond the first `max_count` matches).
    let capped = grep(
        &Grep {
            max_count: Some(5),
            ..Default::default()
        },
        "needle",
        dir.to_str().unwrap(),
    )
    .expect("grep should succeed");
    assert_eq!(capped.matches.len(), 5, "total cap must bound the matches");
    assert!(
        !capped.file_limit_reached,
        "no window flag for capped calls"
    );
    assert!(capped.note.is_none(), "no pagination note when capped");

    let skipped = grep(
        &Grep {
            max_count: Some(5),
            skip: Some(5),
            ..Default::default()
        },
        "needle",
        dir.to_str().unwrap(),
    )
    .expect("grep should succeed");
    assert_eq!(skipped.matches.len(), 5, "cap must still bound the matches");
    let capped_paths: Vec<&str> = capped.matches.iter().map(|m| m.path.as_str()).collect();
    let skipped_paths: Vec<&str> = skipped.matches.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        capped_paths, skipped_paths,
        "skip must be ignored for capped calls"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_zero_matches_is_marked_useless() {
    let dir = fixture_dir("grep_useless");
    let file = fixture_glob_file(&dir);
    let out =
        grep(&Grep::default(), "zzz_no_such_pattern_xyz", &file).expect("grep should succeed");

    assert!(out.matches.is_empty());
    assert_eq!(
        out.useless,
        Some(true),
        "zero-match results must be flagged useless"
    );
    assert_eq!(out.note.as_deref(), Some("No matches found"));
}

#[test]
fn grep_skip_past_end_reports_no_more_results() {
    let dir = temp_dir("skipe");
    std::fs::write(dir.join("a.txt"), "needle\n").unwrap();

    let out = grep(
        &Grep {
            skip: Some(5),
            ..Default::default()
        },
        "needle",
        dir.to_str().unwrap(),
    )
    .expect("grep should succeed");

    assert!(out.matches.is_empty());
    assert_eq!(out.useless, Some(true));
    let note = out.note.as_deref().expect("no-more-results note expected");
    assert!(
        note.contains("No more results") && note.contains("skip=5"),
        "note must explain the past-the-end skip: {note}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_records_seen_lines_for_anchored_files() {
    let dir = temp_dir("seen");
    std::fs::write(dir.join("a.txt"), "line one\nneedle here\nline three\n").unwrap();

    let out = grep(&Grep::default(), "needle", dir.to_str().unwrap()).expect("grep should succeed");

    assert_eq!(out.files.len(), 1, "one anchored file expected");
    let anchor = &out.files[0];
    let key = std::fs::canonicalize(dir.join("a.txt"))
        .expect("canonicalize fixture")
        .to_string_lossy()
        .to_string();

    let store = cosh_sdk::rollback::session_store();
    let seen = store
        .lock()
        .expect("session store lock")
        .seen_lines(&key, &anchor.file_hash);

    assert!(!seen.is_empty(), "anchored file must record seen lines");
    assert!(
        seen.iter()
            .any(|(line, content)| *line == 2 && content.contains("needle")),
        "seen lines must cover the matched line, got: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Idea 6: multi-target + line_range (structured, guard-safe) ───────────

#[test]
fn grep_targets_multi_target_rebases_paths_to_common_ancestor() {
    let dir = temp_dir("multi");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/util.rs"), "needle in a\n").unwrap();
    std::fs::write(dir.join("b/util.rs"), "needle in b\n").unwrap();

    let a = dir.join("a");
    let b = dir.join("b");
    let out = grep_targets(
        &Grep::default(),
        "needle",
        &[
            a.to_string_lossy().to_string(),
            b.to_string_lossy().to_string(),
        ],
    )
    .expect("multi-target grep should succeed");

    assert_eq!(out.total_matches, 2, "one match per target");
    assert_eq!(out.matches.len(), 2, "both matches surfaced");
    let mut paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        vec!["a/util.rs", "b/util.rs"],
        "same-named files in different targets must stay unique via the common-ancestor rebase"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_targets_dedups_overlapping_targets() {
    let dir = temp_dir("dedup");
    std::fs::write(dir.join("a.txt"), "needle only line\n").unwrap();
    let target = dir.to_string_lossy().to_string();

    // The same directory twice — the second run's matches are duplicates.
    let out = grep_targets(&Grep::default(), "needle", &[target.clone(), target])
        .expect("duplicate targets should succeed");

    assert_eq!(
        out.matches.len(),
        1,
        "a repeated target must not duplicate the surfaced matches"
    );
    assert_eq!(
        out.total_matches, 1,
        "total_matches must reflect the deduplicated union across targets"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_line_range_filters_matches() {
    let dir = temp_dir("lrange");
    std::fs::write(dir.join("a.txt"), "m1\nm2\nm3\nm4\nm5\n").unwrap();
    let file = dir.join("a.txt");

    let out = grep(
        &Grep {
            line_range: Some("2-4".to_string()),
            ..Default::default()
        },
        "m",
        file.to_str().unwrap(),
    )
    .expect("grep with line_range should succeed");

    let mut lines: Vec<u32> = out.matches.iter().map(|m| m.line_number).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec![2, 3, 4], "only in-range lines must be kept");
    assert_eq!(out.total_matches, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_line_range_requires_single_file_targets() {
    let dir = temp_dir("lrdir");
    std::fs::write(dir.join("a.txt"), "m1\n").unwrap();

    let result = grep(
        &Grep {
            line_range: Some("1-2".to_string()),
            ..Default::default()
        },
        "m",
        dir.to_str().unwrap(),
    );

    let err = match result {
        Ok(_) => panic!("a directory with line_range must fail"),
        Err(e) => e,
    };
    assert!(
        err.contains("line_range requires single-file"),
        "error must explain the constraint, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_line_range_malformed_errors() {
    let dir = fixture_dir("grep_range_err");
    let file = fixture_glob_file(&dir);
    for bad in ["abc", "1-", "-3", "5-1", "0-2"] {
        let result = grep(
            &Grep {
                line_range: Some(bad.to_string()),
                ..Default::default()
            },
            "fn",
            &file,
        );
        assert!(result.is_err(), "malformed line_range {bad:?} must fail");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_with_paths_array_overrides_single_path() {
    let dir = temp_dir("gwith");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/x.txt"), "needle a\n").unwrap();
    std::fs::write(dir.join("b/x.txt"), "needle b\n").unwrap();

    let find = Find::new().cwd(dir.clone());
    let out = find
        .grep_with(
            "needle",
            None, // no single path — the array drives the search
            Some(vec![
                dir.join("a").to_string_lossy().to_string(),
                dir.join("b").to_string_lossy().to_string(),
            ]),
            None,
            None,
        )
        .expect("grep_with should succeed");

    assert_eq!(out.matches.len(), 2, "both targets searched in one call");
    let mut paths: Vec<&str> = out.matches.iter().map(|m| m.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        vec!["a/x.txt", "b/x.txt"],
        "each target resolves through the path guard and rebases uniquely"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
