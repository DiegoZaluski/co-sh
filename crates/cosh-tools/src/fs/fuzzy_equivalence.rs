//! Port of oh-my-pi's `crates/pi-edit/src/fuzzy.rs` test module — the tests
//! covering the surface we ported are reproduced 1:1, same inputs, same
//! expected outputs, so any behavior drift between the two projects fails
//! here. Tests that only exercise upstream-only surfaces (`replace_text`,
//! the patch-mode sequence/context ladders) have no counterpart by design.

use super::fuzzy::{
    DEFAULT_FUZZY_THRESHOLD, ExcludedRange, FindMatchOptions, FuzzyMatch, MatchOutcome, find_match,
    format_match_error, format_occurrence_error, normalize_for_fuzzy,
};

fn options(allow_fuzzy: bool) -> FindMatchOptions<'static> {
    FindMatchOptions {
        allow_fuzzy,
        threshold: None,
        excluded_ranges: &[],
    }
}

#[test]
fn exact_match_and_multiple_occurrences() {
    let found = find_match("line1\nline2\nline3", "line2", &options(false));
    assert_eq!(
        found.matched.as_ref().map(|matched| matched.start_line),
        Some(2)
    );
    assert_eq!(
        found.matched.as_ref().map(|matched| matched.confidence),
        Some(1.0)
    );

    let multiple = find_match("foo\nbar\nfoo", "foo", &options(false));
    assert!(multiple.matched.is_none());
    assert_eq!(multiple.occurrences, Some(2));
    assert_eq!(multiple.occurrence_lines, Some(vec![1, 3]));
    assert_eq!(multiple.occurrence_previews.as_ref().map(Vec::len), Some(2));
}

#[test]
fn tab_space_and_internal_whitespace_normalization() {
    for (content, target) in [
        ("\tfoo\n\t\tbar\n\tbaz", "  foo\n    bar\n  baz"),
        ("  foo\n    bar\n  baz", "\tfoo\n\t\tbar\n\tbaz"),
        ("   foo\n      bar\n   baz", "  foo\n    bar\n  baz"),
        ("foo   bar    baz", "foo bar baz"),
    ] {
        let outcome = find_match(content, target, &options(true));
        assert!(outcome.matched.is_some(), "failed to match {target:?}");
        assert!(outcome.matched.unwrap().confidence >= DEFAULT_FUZZY_THRESHOLD);
    }
}

#[test]
fn fallback_ignores_inconsistent_indentation() {
    let outcome = find_match(
        "\t\t\tline1\n\t\t\tline2\n\t\tline3\n\t\t\tline4",
        "      line1\n      line2\n      line3\n      line4",
        &options(true),
    );
    assert!(outcome.matched.is_some());

    let varied = find_match(
        "  a\n    b\n   c\n    d",
        "  a\n    b\n    c\n    d",
        &options(true),
    );
    assert!(varied.matched.is_some());
}

#[test]
fn single_line_trailing_space_and_empty_line_cases() {
    let single = find_match(
        "prefix\n\t\t\t\"value\",\nsuffix",
        "          \"value\",",
        &options(true),
    );
    assert!(single.matched.is_some());
    let trailing = find_match("line1  \nline2\t", "line1\nline2", &options(true));
    assert!(trailing.matched.is_some());
    let empty_line = find_match("line1\n\nline3", "line1\n\nline3", &options(false));
    assert_eq!(
        empty_line
            .matched
            .as_ref()
            .map(|matched| matched.confidence),
        Some(1.0)
    );
    assert_eq!(
        find_match("some content", "", &options(true)),
        MatchOutcome::default()
    );
    assert!(
        find_match(
            "short",
            "this is much longer than the content",
            &options(true)
        )
        .matched
        .is_none()
    );
}

#[test]
fn threshold_and_dominant_fuzzy_match() {
    let strict = FindMatchOptions {
        allow_fuzzy: true,
        threshold: Some(0.99),
        excluded_ranges: &[],
    };
    assert!(
        find_match("function foo() {}", "function bar() {}", &strict)
            .matched
            .is_none()
    );
    let lenient = FindMatchOptions {
        allow_fuzzy: true,
        threshold: Some(0.7),
        excluded_ranges: &[],
    };
    assert!(
        find_match("function foo() {}", "function bar() {}", &lenient)
            .matched
            .is_some()
    );

    let target = "a".repeat(50);
    let content = format!("{}b\n{}cccccc", "a".repeat(49), "a".repeat(44));
    let dominant = find_match(
        &content,
        &target,
        &FindMatchOptions {
            allow_fuzzy: true,
            threshold: Some(0.8),
            excluded_ranges: &[],
        },
    );
    assert_eq!(dominant.dominant_fuzzy, Some(true));
    assert_eq!(dominant.fuzzy_matches, Some(2));
}

#[test]
fn excluded_ranges_hide_exact_and_fuzzy_candidates() {
    let range = ExcludedRange {
        start_index: 0,
        end_index: 3,
    };
    let exact = find_match(
        "foo\nfoo",
        "foo",
        &FindMatchOptions {
            allow_fuzzy: false,
            threshold: None,
            excluded_ranges: &[range],
        },
    );
    assert_eq!(
        exact.matched.as_ref().map(|matched| matched.start_index),
        Some(4)
    );

    let fuzzy = find_match(
        "food\nfool",
        "foox",
        &FindMatchOptions {
            allow_fuzzy: true,
            threshold: Some(0.7),
            excluded_ranges: &[ExcludedRange {
                start_index: 0,
                end_index: 4,
            }],
        },
    );
    assert_eq!(
        fuzzy.matched.as_ref().map(|matched| matched.start_line),
        Some(2)
    );
}

#[test]
fn match_error_matches_typescript_formatter() {
    let closest = FuzzyMatch {
        actual_text: "alpha\ngamma".to_owned(),
        start_index: 10,
        start_line: 4,
        confidence: 0.874,
    };
    assert_eq!(
        format_match_error("src/a.ts", "alpha\nbeta", Some(&closest), true, 0.95, None),
        "Could not find a close enough match in src/a.ts.\n\nClosest match (87% similar) at line \
             4:\n  - beta\n  + gamma\nClosest match was below the 95% similarity threshold."
    );
    assert_eq!(
        format_match_error("src/a.ts", "x", None, false, 0.95, None),
        "Could not find the exact text in src/a.ts. The old text must match exactly including \
             all whitespace and newlines."
    );
    assert_eq!(
        format_match_error(
            "src/a.ts",
            "alpha\nbeta",
            Some(&closest),
            true,
            0.95,
            Some(3)
        ),
        "Could not find a close enough match in src/a.ts.\n\nClosest match (87% similar) at line \
             4:\n  - beta\n  + gamma\nFound 3 high-confidence matches. Provide more context to make \
             it unique."
    );
    assert_eq!(
        format_match_error("src/a.ts", "alpha\nbeta", Some(&closest), false, 0.95, None),
        "Could not find the exact text in src/a.ts.\n\nClosest match (87% similar) at line 4:\n  \
             - beta\n  + gamma\nFuzzy matching is disabled. Enable 'Edit fuzzy match' in settings to \
             accept high-confidence matches."
    );
    assert_eq!(
        format_match_error("src/a.ts", "x", None, true, 0.95, None),
        "Could not find a close enough match in src/a.ts."
    );
}

#[test]
fn occurrence_error_includes_preview_limit_suffix() {
    let outcome = MatchOutcome {
        occurrences: Some(7),
        occurrence_previews: Some(vec!["preview".to_owned()]),
        ..MatchOutcome::default()
    };
    assert_eq!(
        format_occurrence_error("a.ts", &outcome),
        "Found 7 occurrences in a.ts (showing first 5 of 7):\n\npreview\n\nAdd more context \
             lines to disambiguate."
    );
}

#[test]
fn fuzzy_normalization_folds_punctuation() {
    assert_eq!(
        normalize_for_fuzzy("  a \t b \u{201C}x\u{2014}y\u{201F}  "),
        "a b \u{201C}x-y\""
    );
}

#[test]
fn blank_line_inside_a_depth_window_uses_the_prefix_only_form() {
    // The precompute deviation's only output-visible touchpoint: an empty
    // trimmed line renders as the bare depth prefix (`"N|"`), never an
    // off-by-window mix of neighbor folds.
    let outcome = find_match("header\n\nbody", "header\n\nbody", &options(false));
    assert_eq!(
        outcome.matched.as_ref().map(|matched| matched.confidence),
        Some(1.0)
    );
    let with_depth = find_match("  a\n\n  c", "  a\n\n  c", &options(true));
    assert!(with_depth.matched.is_some());
}
