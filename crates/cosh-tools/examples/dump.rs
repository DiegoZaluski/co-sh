use cosh_tools::fs::fuzzy::{ExcludedRange, FindMatchOptions, find_match};
use serde_json::json;

fn dump(name: &str, content: &str, target: &str, o: &cosh_tools::fs::fuzzy::MatchOutcome) {
    let f = |m: &Option<cosh_tools::fs::fuzzy::FuzzyMatch>| match m {
        Some(m) => json!({
            "actual_text": m.actual_text, "start_index": m.start_index,
            "start_line": m.start_line, "confidence": m.confidence }),
        None => serde_json::Value::Null,
    };
    println!(
        "{name}|{}|{}|{}",
        json!(content),
        json!(target),
        json!({
        "matched": f(&o.matched), "closest": f(&o.closest),
        "occurrences": o.occurrences, "occurrence_lines": o.occurrence_lines,
        "occurrence_previews": o.occurrence_previews, "fuzzy_matches": o.fuzzy_matches,
        "dominant_fuzzy": o.dominant_fuzzy })
    );
}

fn main() {
    let dominant_content = format!("{}b\n{}cccccc", "a".repeat(49), "a".repeat(44));
    let dominant_target = "a".repeat(50);
    type Case = (
        String,
        String,
        String,
        bool,
        Option<f64>,
        Vec<(usize, usize)>,
    );
    let cases: Vec<Case> = vec![
        (
            "single".into(),
            "prefix\n\t\t\t\"value\",\nsuffix".into(),
            "          \"value\",".into(),
            true,
            None,
            vec![],
        ),
        (
            "tabs".into(),
            "\tfoo\n\t\tbar\n\tbaz".into(),
            "  foo\n    bar\n  baz".into(),
            true,
            None,
            vec![],
        ),
        (
            "fallback".into(),
            "\t\t\tline1\n\t\t\tline2\n\t\tline3\n\t\t\tline4".into(),
            "      line1\n      line2\n      line3\n      line4".into(),
            true,
            None,
            vec![],
        ),
        (
            "varied".into(),
            "  a\n    b\n   c\n    d".into(),
            "  a\n    b\n    c\n    d".into(),
            true,
            None,
            vec![],
        ),
        (
            "dominant".into(),
            dominant_content.clone(),
            dominant_target.clone(),
            true,
            Some(0.8),
            vec![],
        ),
        (
            "strict".into(),
            "function foo() {}".into(),
            "function bar() {}".into(),
            true,
            Some(0.99),
            vec![],
        ),
        (
            "lenient".into(),
            "function foo() {}".into(),
            "function bar() {}".into(),
            true,
            Some(0.7),
            vec![],
        ),
        (
            "multi_fuzzy".into(),
            "food\nfool".into(),
            "foox".into(),
            true,
            Some(0.7),
            vec![(0, 4)],
        ),
        (
            "exact_multi".into(),
            "foo\nbar\nfoo".into(),
            "foo".into(),
            false,
            None,
            vec![],
        ),
        (
            "trailing".into(),
            "line1  \nline2\t".into(),
            "line1\nline2".into(),
            true,
            None,
            vec![],
        ),
        (
            "empty_lines".into(),
            "line1\n\nline3".into(),
            "line1\n\nline3".into(),
            false,
            None,
            vec![],
        ),
        (
            "long_target".into(),
            "short".into(),
            "this is much longer than the content".into(),
            true,
            None,
            vec![],
        ),
        (
            "no_match".into(),
            "alpha\nbeta\ngamma".into(),
            "alpha\nZETA".into(),
            true,
            None,
            vec![],
        ),
        (
            "unicode_dash".into(),
            "import asyncio  # local import – avoids top‑level dep".into(),
            "import asyncio  # local import - avoids top-level dep".into(),
            true,
            None,
            vec![],
        ),
    ];
    for (name, content, target, fuzzy, threshold, excluded) in &cases {
        let ranges: Vec<ExcludedRange> = excluded
            .iter()
            .map(|(s, e)| ExcludedRange {
                start_index: *s,
                end_index: *e,
            })
            .collect();
        let opts = FindMatchOptions {
            allow_fuzzy: *fuzzy,
            threshold: *threshold,
            excluded_ranges: &ranges,
        };
        dump(name, content, target, &find_match(content, target, &opts));
    }
}
