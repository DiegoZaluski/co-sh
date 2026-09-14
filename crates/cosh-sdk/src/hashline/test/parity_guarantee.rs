//! Parity guarantee suite — pins the edges of the boundary-repair port
//! where the port could be perceptibly WORSE than upstream oh-my-pi
//! (`crates/pi-edit/src/modes/hashline/apply.rs`). Every expectation below
//! was captured from upstream's ACTUAL engine output (the `parity_driver` /
//! `parity_edge` dump harness runs the identical scenarios on both engines
//! and the dumps diff byte-identical modulo op vocabulary), not from guesswork.
//!
//! Sources of each ground truth: `E<n>` refers to the edge-dump scenarios.
//! Independent audit verdict: EQUIVALENT mechanism, port strictly more
//! defensive (no `unwrap()`, saturating casts, corrupt-tree-proof cache).
//!
//! Modules:
//! - `flow` — apply_edits decision order and observable outcomes
//! - `oracle` — parses_cleanly semantics for None/unknown/empty inputs
//! - `ambiguity` — echo rejection + placement-ambiguity boundaries
//! - `variants` — keep/drop boundary variant heuristics
//! - `landings` — after-insert landing shifts and opener escapes
//! - `composite` — multi-group/multi-hunk interactions
//! - `defensive` — invariants where the port must never be worse (no panics)
use super::super::apply::apply_edits;
use super::super::types::{Anchor, Cursor, Edit, Replacement};

fn replacement(start: u32, end: u32, body: &[&str], op_line: u32) -> Vec<Edit> {
    let mut edits: Vec<Edit> = body
        .iter()
        .map(|text| Edit::Insert {
            cursor: Cursor::BeforeAnchor(Anchor { line: start }),
            text: (*text).to_string(),
            line_num: op_line,
            index: 0,
            mode: Some(Replacement::Replacement),
        })
        .collect();
    edits.extend((start..=end).map(|line| delete(line, op_line)));
    edits
}

const fn delete(line: u32, op_line: u32) -> Edit {
    Edit::Delete {
        anchor: Anchor { line },
        line_num: op_line,
        index: 0,
        old_assertion: None,
    }
}

fn insert_after(line: u32, text: &str, op_line: u32) -> Edit {
    Edit::Insert {
        cursor: Cursor::AfterAnchor(Anchor { line }),
        text: text.to_string(),
        line_num: op_line,
        index: 0,
        mode: None,
    }
}

fn apply_ok(text: &str, edits: &[Edit], path: Option<&str>) -> (String, Vec<String>) {
    let result = apply_edits(text, edits, path).expect("apply should succeed");
    (result.text, result.warnings)
}

// ============================ flow ============================

mod flow {
    use super::*;

    /// Upstream (E1 shows the negative case: `new();` is invalid TS, so the
    /// authored result carries the advisory): a genuinely-parsing authored
    /// result applies as-is with no advisories.
    #[test]
    fn authored_that_parses_applies_as_is() {
        let (text, warnings) = apply_ok(
            "const a = 1;\nconst b = 2;",
            &replacement(1, 1, &["const a = 10;"], 1),
            Some("x.ts"),
        );
        assert_eq!(text, "const a = 10;\nconst b = 2;");
        assert!(warnings.is_empty());
    }

    /// Upstream (E1): an authored result that does NOT parse — even a subtle
    /// case like `new();`, which is a TS syntax error — is honored as
    /// written with the machine-confirmed advisory. Never silently rewritten.
    #[test]
    fn authored_parse_break_gets_machine_confirmed_advisory() {
        let (text, warnings) = apply_ok(
            "function f() {\n  old();\n}",
            &replacement(2, 2, &["  new();"], 1),
            Some("x.ts"),
        );
        assert_eq!(text, "function f() {\n  new();\n}");
        let warning = warnings
            .iter()
            .find(|w| w.contains("introduced a syntax error"))
            .expect("machine-confirmed advisory must be present");
        assert!(warning.contains("applied exactly as written"));
        assert!(warning.contains("near line 2"));
    }

    /// Upstream (E2): when nothing restores a parse the authored edit is
    /// honored — the advisory fires with the first changed line, and the
    /// trailing phantom line is preserved.
    #[test]
    fn unrepairable_edit_applies_with_parse_broken_advisory() {
        let (text, warnings) = apply_ok(
            "fn f() {\n}\n",
            &replacement(1, 2, &["fn f() {"], 1),
            Some("x.rs"),
        );
        assert_eq!(text, "fn f() {\n");
        let warning = warnings
            .iter()
            .find(|w| w.contains("introduced a syntax error"))
            .expect("machine-confirmed advisory must be present");
        assert!(warning.contains("near line 1"));
    }

    /// Upstream (E3): the advisory fires ONLY when the baseline parsed; a
    /// file that was already broken gets no parse advisory.
    #[test]
    fn no_parse_advisory_when_baseline_already_broken() {
        let (_, warnings) = apply_ok(
            "fn broken(",
            &replacement(1, 1, &["fn f() {"], 1),
            Some("x.rs"),
        );
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("introduced a syntax error"))
        );
    }

    /// Upstream: a parse-restoring boundary repair applies with the variant
    /// warning naming kept/dropped counts.
    #[test]
    fn parse_restoring_repair_applies_with_variant_warning() {
        let (text, warnings) = apply_ok(
            "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n};",
            &replacement(5, 5, &["\tb() {", "\t\treturn 2;", "\t},"], 1),
            Some("x.ts"),
        );
        assert_eq!(
            text,
            "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n\tb() {\n\t\treturn 2;\n\t},\n};"
        );
        let warning = warnings
            .iter()
            .find(|w| w.contains("Auto-repaired replacement boundaries"))
            .expect("variant repair warning must be present");
        assert!(warning.contains("retained 1 syntax-essential source boundary row"));
        assert!(warning.contains("verified by the syntax probe"));
    }
}

// ============================ oracle ============================

mod oracle {
    use super::super::super::syntax::parses_cleanly;

    /// Upstream (pi-ast summary.rs `resolve_language` → `unparsed_result`):
    /// a None path or unknown extension can never be syntax-proven — both
    /// return false, so the gates never claim validity they lack.
    #[test]
    fn none_path_and_unknown_extension_are_never_clean() {
        assert!(!parses_cleanly(None, "fn f() {}"));
        assert!(!parses_cleanly(
            Some("x.definitely-unknown-ext"),
            "fn f() {}"
        ));
    }

    /// Upstream: empty source short-circuits to unparsed (summary.rs L166).
    #[test]
    fn empty_text_is_never_clean() {
        assert!(!parses_cleanly(Some("x.rs"), ""));
    }

    /// Upstream: valid code parses, broken code does not.
    #[test]
    fn parse_verdict_is_grammar_truth() {
        assert!(parses_cleanly(Some("x.rs"), "fn f() {}"));
        assert!(!parses_cleanly(Some("x.rs"), "fn broken("));
    }

    /// The port's corrupt-tree guard: probes over the same path with
    /// DIFFERENT texts must each reflect their own text. (The shared
    /// incremental pool would return a stale tree; the port parses fully
    /// and caches by text content, so alternating texts stays correct.)
    #[test]
    fn alternating_texts_never_leak_stale_verdicts() {
        let good = "fn f() {}";
        let bad = "fn broken(";
        for _ in 0..4 {
            assert!(parses_cleanly(Some("x.rs"), good));
            assert!(!parses_cleanly(Some("x.rs"), bad));
        }
    }
}

// ======================= unknown language flow =======================

mod unknown_language {
    use super::*;

    /// Upstream (E4): with no grammar, `parses_cleanly` is false on both
    /// sides and `repair_boundaries` declines — the authored edit applies
    /// untouched, with no advisories and no rejections.
    #[test]
    fn unknown_extension_applies_authored_silently() {
        let (text, warnings) = apply_ok(
            "const handlers = {\n\tkeep\n};",
            &replacement(2, 2, &["changed"], 1),
            Some("x.unknown"),
        );
        assert_eq!(text, "const handlers = {\nchanged\n};");
        assert!(warnings.is_empty());
    }

    /// Same as above for an explicit None path.
    #[test]
    fn none_path_applies_authored_silently() {
        let (text, warnings) = apply_ok("one\ntwo\nthree", &replacement(2, 2, &["TWO"], 1), None);
        assert_eq!(text, "one\nTWO\nthree");
        assert!(warnings.is_empty());
    }
}

// ============================ ambiguity ============================

mod ambiguity {
    use super::*;

    /// Upstream: a one-sided exact echo that cannot cover the widened range
    /// is REJECTED with the actionable message (E7 confirms the same for a
    /// trailing echo beside a deleted neighbor).
    #[test]
    fn underfilled_leading_echo_rejects() {
        let file = "{\n    auto* handle = payloadFor<PyThreadHandle>(self);\n    if (!handle)\n        return threadError(globalObject, \"thread not started\");\n    handle.setDone();\n}";
        let error = apply_edits(
            file,
            &replacement(
                3,
                4,
                &[
                    "    auto* handle = payloadFor<PyThreadHandle>(self);",
                    "    if (!handle || !handle.isStarted())",
                ],
                1,
            ),
            Some("x.cpp"),
        )
        .unwrap_err();
        assert!(error.contains("body opens by restating the 1 line(s) just above"));
        assert!(error.contains("too short to be the full final content"));
    }

    /// Upstream (E7): a trailing echo whose remainder cannot fill the range
    /// rejects even when a neighboring hunk deletes the echoed line — the
    /// echo ambiguity is evaluated on the authored edit structure.
    #[test]
    fn trailing_echo_beside_deleted_neighbor_rejects() {
        let file = "it('a', () => {\n\tsetup();\n});\n});\nafter();";
        let mut edits = replacement(2, 3, &["\tsetup2();", "});"], 1);
        edits.push(delete(4, 2));
        let error = apply_edits(file, &edits, Some("x.ts")).unwrap_err();
        assert!(
            error.contains("ends by restating the 1 line(s) just below the range"),
            "{error}"
        );
    }

    /// Upstream (E5): the placement-ambiguity error only exists on the
    /// non-parsing path — when the authored result parses, a variant-level
    /// ambiguity flag is inert and the edit applies.
    #[test]
    fn placement_ambiguity_inert_when_authored_parses() {
        let (text, warnings) = apply_ok("a {\n}\n", &replacement(2, 2, &["}"], 1), Some("x.rs"));
        assert_eq!(text, "a {\n}\n");
        assert!(warnings.is_empty());
    }

    /// Upstream (E6): an essential-looking boundary with tied indentation
    /// does NOT reject when the payload's own line is not essential — the
    /// authored result is honored (with the parse advisory if it breaks).
    #[test]
    fn tied_indent_boundary_applies_when_line_not_essential() {
        let (text, warnings) = apply_ok(
            "fn f() {\n    match x {\n        y => 1,\n    }\n}",
            &replacement(3, 3, &["        z => 2,,"], 1),
            Some("x.rs"),
        );
        assert_eq!(text, "fn f() {\n    match x {\n        z => 2,,\n    }\n}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("introduced a syntax error"))
        );
    }
}

// ============================ variants ============================

mod variants {
    use super::*;

    /// Upstream: a payload restating the closer that survives below resolves
    /// to the parse-proven reading — the echo is dropped, with the echo warning.
    #[test]
    fn structural_edge_resolves_to_the_parse_proven_reading() {
        let (text, warnings) = apply_ok(
            "it('a', () => {\n\tsetup();\n\trun();\n});\nafter();",
            &replacement(2, 3, &["\tsetup2();", "\trun2();", "});"], 1),
            Some("x.ts"),
        );
        assert_eq!(
            text,
            "it('a', () => {\n\tsetup2();\n\trun2();\n});\nafter();"
        );
        assert!(warnings.iter().any(|w| w.contains("boundary echo")));
    }

    /// Upstream: an intentional duplicated statement (still parsing) is
    /// NEVER dropped — `text == authored` candidates are skipped in the
    /// combo loop (apply.rs L713-714).
    #[test]
    fn intentional_duplicate_statement_survives() {
        let (text, warnings) = apply_ok(
            "a = 1;\nb = 2;\nc = 3;",
            &replacement(1, 1, &["a = 1;", "b = 2;"], 1),
            Some("x.js"),
        );
        assert_eq!(text, "a = 1;\nb = 2;\nb = 2;\nc = 3;");
        assert!(warnings.is_empty());
    }

    /// Upstream (E13): `underfilled` — a payload shorter than its range with
    /// the closer off-range still allows keep-trailing; the parse-proven
    /// variant spares the essential closer.
    #[test]
    fn underfilled_range_keeps_essential_closer() {
        let (text, warnings) = apply_ok(
            "fn f() {\n\ta();\n\tb();\n}\n",
            &replacement(2, 4, &["\ta();"], 1),
            Some("x.rs"),
        );
        assert_eq!(text, "fn f() {\n\ta();\n}\n");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Auto-repaired replacement boundaries"))
        );
    }

    /// Upstream: `annotation_echo` — a single-line replacement whose payload
    /// re-emits an attribute node that already survives below drops it.
    #[test]
    fn attribute_echo_dropped_on_single_line_range() {
        let (text, warnings) = apply_ok(
            "/// Old summary.\n#[napi]\npub fn f() {}",
            &replacement(1, 1, &["/// New summary.", "#[napi]"], 1),
            Some("x.rs"),
        );
        assert_eq!(text, "/// New summary.\n#[napi]\npub fn f() {}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("boundary echo") && w.contains("1 trailing"))
        );
    }

    /// Upstream (E11 + E19): a statement echo on a single-line range is kept
    /// literally (duplicating) — dropping could discard intended content.
    /// With valid TS the result parses clean (E19); with a TS-invalid
    /// keyword (`new();`) the advisory fires (E11).
    #[test]
    fn statement_echo_kept_on_single_line_range() {
        let (text, warnings) = apply_ok(
            "foo();\nold();\nbar();",
            &replacement(2, 2, &["zed();", "bar();"], 1),
            Some("x.ts"),
        );
        assert_eq!(text, "foo();\nzed();\nbar();\nbar();");
        assert!(warnings.is_empty());

        let (text, warnings) = apply_ok(
            "foo();\nold();\nbar();",
            &replacement(2, 2, &["new();", "bar();"], 1),
            Some("x.ts"),
        );
        assert_eq!(text, "foo();\nnew();\nbar();\nbar();");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("introduced a syntax error"))
        );
    }
}

// ============================ landings ============================

mod landings {
    use super::*;

    /// Upstream (E15): shallower body after an anchor slides past trailing
    /// closer lines, stopping at the body depth, with the shift warning.
    #[test]
    fn shallower_body_slides_past_closer() {
        let (text, warnings) = apply_ok(
            "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
            &[insert_after(3, "    c();", 1)],
            None,
        );
        assert_eq!(
            text,
            "function f() {\n    if (x) {\n        a();\n    }\n    c();\n    b();\n}\n"
        );
        assert!(warnings[0].contains("moved past 1 closing line to after line 4"));
    }

    /// Upstream (E16): the slide never crosses a line targeted by another hunk.
    #[test]
    fn slide_never_crosses_targeted_line() {
        let (text, warnings) = apply_ok(
            "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
            &[insert_after(3, "    c();", 1), delete(4, 2)],
            None,
        );
        assert_eq!(
            text,
            "function f() {\n    if (x) {\n        a();\n    c();\n    b();\n}\n"
        );
        assert!(warnings.is_empty());
    }

    /// Upstream (E17): the slide stops at the first closer at the body's own
    /// depth; a body indented like the inner block stays inside.
    #[test]
    fn slide_stops_at_body_depth() {
        let file = "function f() {\n    if (x) {\n        for (y) {\n            a();\n        }\n    }\n    b();\n}\n";
        let (outer_text, outer_warnings) = apply_ok(file, &[insert_after(4, "    c();", 1)], None);
        assert_eq!(outer_text.lines().nth(6), Some("    c();"));
        assert!(outer_warnings[0].contains("moved past 2 closing lines to after line 6"));
        let (inner_text, _) = apply_ok(file, &[insert_after(4, "        c();", 1)], None);
        assert_eq!(inner_text.lines().nth(5), Some("        c();"));
    }

    /// Upstream (E9): a body whose depth equals the anchor's does NOT slide —
    /// the shift requires the body strictly shallower than the anchor.
    #[test]
    fn body_at_anchor_depth_does_not_slide() {
        let mut edits = vec![insert_after(3, "    x();", 1)];
        edits.extend(replacement(2, 2, &["    a2();"], 2));
        let (text, warnings) = apply_ok(
            "function f() {\n    a();\n    b();\n}\n",
            &edits,
            Some("x.js"),
        );
        assert_eq!(text, "function f() {\n    a2();\n    b();\n    x();\n}\n");
        assert!(warnings.is_empty());
    }

    /// Upstream (E18): a single-statement body is never relocated by the
    /// opener-escape stage — `body_relocatable` requires a multi-line node
    /// starting within the body.
    #[test]
    fn opener_escape_needs_multiline_body() {
        let (text, warnings) = apply_ok(
            "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
            &[insert_after(2, "    c();", 1)],
            Some("x.js"),
        );
        assert_eq!(
            text,
            "function f() {\n    if (x) {\n    c();\n        a();\n    }\n    b();\n}\n"
        );
        assert!(warnings.is_empty());
    }

    /// Upstream (E20): a multi-line block body claiming a position outside
    /// the opener's block escapes it — parse-proven, with the escape warning.
    #[test]
    fn opener_escape_with_block_body() {
        let edits = vec![
            insert_after(2, "  if (y) {", 1),
            insert_after(2, "    d();", 1),
            insert_after(2, "  }", 1),
        ];
        let (text, warnings) = apply_ok(
            "function f() {\n    if (x) {\n        a();\n    }\n}\n",
            &edits,
            Some("x.js"),
        );
        assert_eq!(
            text,
            "function f() {\n    if (x) {\n        a();\n    }\n}\n  if (y) {\n    d();\n  }\n"
        );
        let warning = warnings
            .iter()
            .find(|w| w.contains("opens a block"))
            .expect("escape warning must be present");
        assert!(warning.contains("verified by the syntax probe"));
        assert!(warning.contains("after line 5"));
    }
}

// ============================ composite ============================

mod composite {
    use super::*;

    /// Upstream (E12): multiple replacement groups are repaired
    /// independently; each fires its own variant warning.
    #[test]
    fn two_groups_repaired_independently() {
        let file = "const a = {\n\tx() {\n\t\treturn 1;\n\t},\n};\nconst b = {\n\ty() {\n\t\treturn 2;\n\t},\n};";
        let mut edits = replacement(5, 5, &["\tw() {", "\t\treturn 9;", "\t},"], 1);
        edits.extend(replacement(
            10,
            10,
            &["\tz() {", "\t\treturn 8;", "\t},"],
            2,
        ));
        let (text, warnings) = apply_ok(file, &edits, Some("x.ts"));
        assert_eq!(
            text,
            "const a = {\n\tx() {\n\t\treturn 1;\n\t},\n\tw() {\n\t\treturn 9;\n\t},\n};\nconst b = {\n\ty() {\n\t\treturn 2;\n\t},\n\tz() {\n\t\treturn 8;\n\t},\n};"
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|w| w.contains("Auto-repaired replacement boundaries"))
                .count(),
            2
        );
    }

    /// Upstream (E8): a group whose closer is dropped still gets the
    /// parse-proven keep-trailing repair even beside another group.
    #[test]
    fn repaired_group_beside_clean_group() {
        let file = "fn f() {\n\ta();\n}\nfn g() {\n\tb();\n}\n";
        let mut edits = replacement(2, 3, &["\tz();"], 1);
        edits.extend(replacement(5, 5, &["\tc();"], 2));
        let (text, warnings) = apply_ok(file, &edits, Some("x.rs"));
        assert_eq!(text, "fn f() {\n\tz();\n}\nfn g() {\n\tc();\n}\n");
        let warning = warnings
            .iter()
            .find(|w| w.contains("Auto-repaired replacement boundaries"))
            .expect("line-2 group needs its essential closer");
        assert!(warning.contains("at line 2"));
    }

    /// Upstream: replacement bodies have their uniformly-omitted base indent
    /// restored BEFORE the other stages run (repair_indentation runs first).
    #[test]
    fn indent_restore_feeds_the_parse_stages() {
        let (text, warnings) = apply_ok(
            "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n\tlet c = 3;\n}",
            &replacement(2, 4, &["let a = 1;", "let b = 20;", "let c = 3;"], 1),
            Some("x.rs"),
        );
        assert_eq!(
            text,
            "fn f() {\n\tlet a = 1;\n\tlet b = 20;\n\tlet c = 3;\n}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Auto-indented a replacement body"))
        );
    }

    /// Upstream (E14): `indent_columns` counts a tab as advancing to the
    /// next multiple of 4 — mixed tab/space indentation is measured
    /// consistently through the landing logic.
    #[test]
    fn tab_columns_round_to_four() {
        let (text, warnings) = apply_ok(
            "function f() {\n\tif (x) {\n\t\t\ta();\n\t\t}\n\t\tb();\n\t}\n",
            &[insert_after(3, "\t\tc();", 1)],
            Some("x.js"),
        );
        assert_eq!(
            text,
            "function f() {\n\tif (x) {\n\t\t\ta();\n\t\t}\n\t\tc();\n\t\tb();\n\t}\n"
        );
        assert!(warnings[0].contains("moved past 1 closing line to after line 4"));
    }
}

// ============================ defensive ============================

mod defensive {
    use super::*;

    /// The port returns Err (never panics) on malformed authored input —
    /// upstream returns EditError the same way; a panic here would be a
    /// regression against both.
    #[test]
    fn unresolved_block_is_an_error_not_a_panic() {
        let error = apply_edits(
            "x",
            &[Edit::Block {
                anchor: Anchor { line: 1 },
                payloads: vec!["y".into()],
                line_num: 1,
                index: 0,
            }],
            None,
        )
        .unwrap_err();
        assert!(error.contains("unresolved"));
    }

    /// Empty edit list is a no-op (upstream early-return).
    #[test]
    fn empty_edits_are_a_no_op() {
        let (text, warnings) = apply_ok("unchanged", &[], Some("x.rs"));
        assert_eq!(text, "unchanged");
        assert!(warnings.is_empty());
    }

    /// Out-of-bounds anchors are a clean error with the upstream message.
    #[test]
    fn out_of_bounds_anchor_is_an_error() {
        let error = apply_edits("one\ntwo", &[delete(3, 1)], None).unwrap_err();
        assert_eq!(error, "Line 3 does not exist (file has 2 lines)");
    }

    /// The trailing phantom line (file ending in \n) is never deletable —
    /// upstream `phantom_line` guard (apply.rs L83-85).
    #[test]
    fn phantom_trailing_line_delete_is_ignored() {
        let (text, _) = apply_ok("one\ntwo\n", &[delete(3, 1)], None);
        assert_eq!(text, "one\ntwo\n");
    }

    /// The ambiguous candidate tie-breaker: two VALID candidates with
    /// identical materialized text are one candidate, not an ambiguity —
    /// the ambiguity error requires DISTINCT texts (apply.rs L724-732).
    #[test]
    fn duplicate_candidate_text_is_not_ambiguous() {
        let (text, warnings) = apply_ok(
            "it('a', () => {\n\tsetup();\n\trun();\n});\nafter();",
            &replacement(2, 3, &["\tsetup2();", "\trun2();", "});"], 1),
            Some("x.ts"),
        );
        assert_eq!(
            text,
            "it('a', () => {\n\tsetup2();\n\trun2();\n});\nafter();"
        );
        assert!(!warnings.is_empty());
    }
}
