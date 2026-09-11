//! Boundary-repair behavior — a port of upstream oh-my-pi's
//! `crates/pi-edit/tests/hashline_apply.rs` apply tests (which superseded the
//! original `packages/hashline/test/boundary-repair.test.ts` TS suite), plus
//! this repo's original corruption-incident scenarios re-expectated to the
//! parse-validated engine. Same scenarios, same expected outputs on both
//! sides; run with `COSH_HASHLINE_TRACE=1` for decision traces.
use super::super::apply::apply_edits;
use super::super::parser::parse_patch;
use super::super::recovery::{Recovery, RecoveryArgs};
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};
use super::super::types::{Anchor, ApplyResult, Cursor, Edit, Replacement};

fn apply_with_warnings(text: &str, diff: &str, path: &str) -> (String, Vec<String>) {
    let (edits, _) = parse_patch(diff).unwrap();
    let result = apply_edits(text, &edits, Some(path)).expect("apply should succeed");
    (result.text, result.warnings)
}

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

fn insert_before(line: u32, text: &str, op_line: u32) -> Edit {
    Edit::Insert {
        cursor: Cursor::BeforeAnchor(Anchor { line }),
        text: text.to_string(),
        line_num: op_line,
        index: 0,
        mode: None,
    }
}

fn insert_bof(text: &str, op_line: u32) -> Edit {
    Edit::Insert {
        cursor: Cursor::Bof,
        text: text.to_string(),
        line_num: op_line,
        index: 0,
        mode: None,
    }
}

fn insert_eof(text: &str, op_line: u32) -> Edit {
    Edit::Insert {
        cursor: Cursor::Eof,
        text: text.to_string(),
        line_num: op_line,
        index: 0,
        mode: None,
    }
}

fn apply(text: &str, edits: &[Edit], path: Option<&str>) -> ApplyResult {
    apply_edits(text, edits, path).expect("apply should succeed")
}

const PATH: &str = "/tmp/__hashline-boundary-recovery__.ts";

// ---------- ported: upstream apply tests ----------

#[test]
fn inserts_before_after_head_and_tail() {
    let edits = vec![
        insert_bof("head", 1),
        insert_before(2, "before", 2),
        insert_after(2, "after", 3),
        insert_eof("tail", 4),
    ];
    assert_eq!(
        apply("one\ntwo\n", &edits, None).text,
        "head\none\nbefore\ntwo\nafter\ntail\n"
    );
}

#[test]
fn ignores_a_delete_of_the_trailing_phantom_line() {
    assert_eq!(apply("one\ntwo\n", &[delete(3, 1)], None).text, "one\ntwo\n");
}

#[test]
fn rejects_an_out_of_bounds_anchor() {
    let error = apply_edits("one\ntwo", &[delete(3, 1)], None).unwrap_err();
    assert_eq!(error, "Line 3 does not exist (file has 2 lines)");
}

#[test]
fn restores_a_uniformly_omitted_base_indent_from_unchanged_structural_rows() {
    let file = "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n\tlet c = 3;\n}";
    let result = apply(
        file,
        &replacement(2, 4, &["let a = 1;", "let b = 20;", "let c = 3;"], 1),
        Some("x.rs"),
    );
    assert_eq!(
        result.text,
        "fn f() {\n\tlet a = 1;\n\tlet b = 20;\n\tlet c = 3;\n}"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("Auto-indented"))
    );
}

#[test]
fn preserves_intentional_indentation_only_replacements() {
    let result = apply(
        "    first();\n    second();",
        &replacement(1, 2, &["first();", "second();"], 1),
        Some("x.ts"),
    );
    assert_eq!(result.text, "first();\nsecond();");
    assert!(result.warnings.is_empty());
}

#[test]
fn spares_the_deleted_closing_line_when_the_payload_omits_it() {
    let file = "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n};";
    let result = apply(
        file,
        &replacement(5, 5, &["\tb() {", "\t\treturn 2;", "\t},"], 1),
        Some("x.ts"),
    );
    assert_eq!(
        result.text,
        "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n\tb() {\n\t\treturn 2;\n\t},\n};"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("Auto-repaired replacement boundaries"))
    );
}

#[test]
fn does_not_spare_a_deleted_closing_line_that_the_payload_restates() {
    let file = "class Foo {\n\tok();\n\t}\n}";
    let result = apply(
        file,
        &replacement(1, 4, &["class Foo {", "\tok();", "}"], 1),
        Some("x.ts"),
    );
    assert_eq!(result.text, "class Foo {\n\tok();\n}");
    assert!(result.warnings.is_empty());
}

#[test]
fn drops_duplicated_leading_and_trailing_boundary_lines_around_a_range_replacement() {
    let file = "function f() {\n  keepA();\n  old1();\n  old2();\n  keepB();\n}";
    let result = apply(
        file,
        &replacement(3, 4, &["  keepA();", "  new1();", "  new2();", "  keepB();"], 1),
        Some("x.ts"),
    );
    assert_eq!(
        result.text,
        "function f() {\n  keepA();\n  new1();\n  new2();\n  keepB();\n}"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("boundary echo"))
    );
}

#[test]
fn rejects_a_leading_keeper_echo_when_the_payload_cannot_fill_the_widened_range() {
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
    assert!(error.contains("body opens by restating"), "{error}");
}

#[test]
fn drops_a_trailing_attribute_echo_in_a_single_line_replacement() {
    let file = "/// Old summary.\n#[napi]\npub fn f() {}";
    let result = apply(
        file,
        &replacement(1, 1, &["/// New summary.", "#[napi]"], 1),
        Some("x.rs"),
    );
    assert_eq!(result.text, "/// New summary.\n#[napi]\npub fn f() {}");
}

#[test]
fn keeps_a_trailing_statement_echo_literal_on_a_single_line_range() {
    let result = apply(
        "foo();\nold();\nbar();",
        &replacement(2, 2, &["new();", "bar();"], 1),
        Some("x.ts"),
    );
    assert_eq!(result.text, "foo();\nnew();\nbar();\nbar();");
}

#[test]
fn slides_a_shallower_body_past_the_closing_line_and_warns() {
    let file = "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n";
    let result = apply(file, &[insert_after(3, "    c();", 1)], None);
    assert_eq!(
        result.text,
        "function f() {\n    if (x) {\n        a();\n    }\n    c();\n    b();\n}\n"
    );
    assert!(
        result.warnings[0]
            .contains("moved past 1 closing line to after line 4")
    );
}

#[test]
fn crosses_multiple_closer_levels_and_stops_at_the_body_depth() {
    let file = "function f() {\n    if (x) {\n        for (y) {\n            a();\n        }\n    }\n    b();\n}\n";
    let outer = apply(file, &[insert_after(4, "    c();", 1)], None);
    assert_eq!(outer.text.lines().nth(6), Some("    c();"));
    assert!(
        outer.warnings[0]
            .contains("moved past 2 closing lines to after line 6")
    );
    let inner = apply(file, &[insert_after(4, "        c();", 1)], None);
    assert_eq!(inner.text.lines().nth(5), Some("        c();"));
}

#[test]
fn refuses_to_cross_a_line_targeted_by_another_hunk() {
    let file = "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n";
    let result = apply(
        file,
        &[insert_after(3, "    c();", 1), delete(4, 2)],
        None,
    );
    assert_eq!(
        result.text,
        "function f() {\n    if (x) {\n        a();\n    c();\n    b();\n}\n"
    );
    assert!(result.warnings.is_empty());
}

#[test]
fn syntax_helpers_use_tree_sitter() {
    use super::super::syntax::{enclosing_boundaries, node_chain, parses_cleanly};
    let lines = vec![
        "mod m {".into(),
        "\t#[test]".into(),
        "\tfn f() {}".into(),
        "}".into(),
    ];
    assert!(
        node_chain(&lines, "x.rs", 2)
            .iter()
            .any(|span| span.kind == "attribute_item")
    );
    assert!(enclosing_boundaries(&lines, "x.rs", 1, 1).contains(&4));
    assert!(parses_cleanly(Some("x.rs"), &lines.join("\n")));
    assert!(!parses_cleanly(Some("x.rs"), "fn broken("));
    assert!(!parses_cleanly(None, "fn f() {}"));
}

// ---------- this repo's corruption-incident scenarios, re-expectated to the
// parse-validated engine (same inputs as the pre-port regression tests) ----------

#[test]
fn drops_duplicated_multi_line_closing_block() {
    let file = [
        r#"import type React from "react";"#,
        r#"import { Composition } from "remotion";"#,
        r#"import { Sizzle, type SizzleProps } from "./compositions/Sizzle";"#,
        r#"import { FPS, totalDurationInFrames } from "./lib/scenes";"#,
        "",
        "export const RemotionRoot: React.FC = () => {",
        "\tconst durationInFrames = totalDurationInFrames();",
        "\treturn (",
        "\t\t<>",
        "\t\t\t<Composition",
        r#"\t\t\t\tid="Sizzle""#,
        "\t\t\t\tcomponent={Sizzle}",
        "\t\t\t\tdurationInFrames={durationInFrames}",
        "\t\t\t\twidth={1920}",
        r#"\t\t\t\tdefaultProps={{ layout: "landscape" }}"#,
        "\t\t\t/>",
        "\t\t</>",
        "\t);",
        "};",
    ]
    .join("\n");
    let diff = [
        "replace 7..16:",
        "+\treturn (",
        "+\t\t<>",
        "+\t\t\t<Composition",
        r#"+\t\t\t\tid="Sizzle""#,
        "+\t\t\t\tcomponent={Sizzle}",
        "+\t\t\t\tdurationInFrames={durationInFrames}",
        "+\t\t\t\twidth={1920}",
        r#"+\t\t\t\tdefaultProps={{ layout: "landscape" } satisfies SizzleProps}"#,
        "+\t\t\t/>",
        "+\t\t</>",
        "+\t);",
    ]
    .join("\n");
    let (edits, _) = parse_patch(&diff).unwrap();
    let error = apply_edits(&file, &edits, Some("x.tsx")).unwrap_err();
    assert!(
        error.contains("ends by restating the 2 line(s) just below the range"),
        "{error}"
    );
}

#[test]
fn applies_multi_line_closing_block_when_range_covers_exactly_the_changed_lines() {
    // The corrected range (7..15, ending at the first `/>`): the payload is
    // the complete final content of the range and applies cleanly with no
    // doubling of the surviving closers.
    let file = [
        r#"import type React from "react";"#,
        r#"import { Composition } from "remotion";"#,
        r#"import { Sizzle, type SizzleProps } from "./compositions/Sizzle";"#,
        r#"import { FPS, totalDurationInFrames } from "./lib/scenes";"#,
        "",
        "export const RemotionRoot: React.FC = () => {",
        "\tconst durationInFrames = totalDurationInFrames();",
        "\treturn (",
        "\t\t<>",
        "\t\t\t<Composition",
        r#"\t\t\t\tid="Sizzle""#,
        "\t\t\t\tcomponent={Sizzle}",
        "\t\t\t\tdurationInFrames={durationInFrames}",
        "\t\t\t\twidth={1920}",
        r#"\t\t\t\tdefaultProps={{ layout: "landscape" }}"#,
        "\t\t\t/>",
        "\t\t</>",
        "\t);",
        "};",
    ]
    .join("\n");
    let diff = [
        "replace 7..16:",
        "+\t\t\t<Composition",
        r#"+\t\t\t\tid="Sizzle""#,
        "+\t\t\t\tcomponent={Sizzle}",
        "+\t\t\t\tdurationInFrames={durationInFrames}",
        "+\t\t\t\twidth={1920}",
        r#"+\t\t\t\tdefaultProps={{ layout: "landscape" } satisfies SizzleProps}"#,
        "+\t\t\t/>",
    ]
    .join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff, "x.tsx");
    assert_eq!(text.split('\n').filter(|l| l.trim() == "</>").count(), 1);
    assert_eq!(text.split('\n').filter(|l| l.trim() == ");").count(), 1);
    assert!(text.ends_with("\t\t</>\n\t);\n};"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn drops_single_duplicated_structural_closer() {
    let file = [
        "it('a', () => {",
        "\tsetup();",
        "\trun();",
        "});",
        "after();",
    ]
    .join("\n");
    let diff = ["replace 2..3:", "+\tsetup2();", "+\trun2();", "+});"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff, "x.ts");
    assert_eq!(
        text,
        [
            "it('a', () => {",
            "\tsetup2();",
            "\trun2();",
            "});",
            "after();"
        ]
        .join("\n")
    );
    assert!(warnings.iter().any(|w| w.contains("boundary echo")));
}

#[test]
fn leaves_balance_preserving_replacement_alone() {
    let file = ["foo();", "bar();", "bar();", "baz();"].join("\n");
    let diff = ["replace 2..2:", "+qux();", "+bar();"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff, "x.ts");
    assert_eq!(
        text,
        ["foo();", "qux();", "bar();", "bar();", "baz();"].join("\n")
    );
    assert!(warnings.is_empty());
}

#[test]
fn does_not_drop_balance_neutral_duplicated_statement() {
    let file = ["a = 1;", "b = 2;", "c = 3;"].join("\n");
    let diff = ["replace 1..1:", "+a = 1;", "+b = 2;"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff, "x.js");
    assert_eq!(text, ["a = 1;", "b = 2;", "b = 2;", "c = 3;"].join("\n"));
    assert!(warnings.is_empty());
}

#[test]
fn ignores_brackets_inside_string_literals() {
    let file = [
        r#"const a = "}";"#,
        r#"const b = "x";"#,
        r#"const c = "y";"#,
    ]
    .join("\n");
    let diff = [r#"replace 2..2:"#, r#"+const b = "}}}";"#].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff, "x.js");
    assert_eq!(
        text,
        [
            r#"const a = "}";"#,
            r#"const b = "}}}";"#,
            r#"const c = "y";"#
        ]
        .join("\n")
    );
    assert!(warnings.is_empty());
}

#[test]
fn de_duplicates_closer_while_recovering_from_drifted_file() {
    let snapshot_lines = [
        r#"import { x } from "y";"#,
        "",
        "it('a', () => {",
        "\tsetup();",
        "\trun();",
        "});",
        "",
        "function filler1() { return 1; }",
        "function filler2() { return 2; }",
        "function filler3() { return 3; }",
        "function filler4() { return 4; }",
        "function filler5() { return 5; }",
        "const tail = 0;",
        "export { tail };",
    ];
    let snapshot_text = format!("{}\n", snapshot_lines.join("\n"));
    let current_text = snapshot_text.replace("const tail = 0;", "const tail = 99;");

    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let file_hash = store.record(PATH, &snapshot_text);

    let (edits, _) =
        parse_patch(&["replace 4..5:", "+\tsetup2();", "+\trun2();", "+});"].join("\n")).unwrap();
    let mut recovery = Recovery::new(store);
    let recovered = recovery.try_recover(&RecoveryArgs {
        path: PATH.to_string(),
        current_text: current_text.clone(),
        file_hash,
        edits,
    });

    assert!(recovered.is_some());
    let recovered = recovered.unwrap();
    assert_eq!(
        recovered.text.split('\n').filter(|l| *l == "});").count(),
        1
    );
    assert!(recovered.text.contains("setup2();"));
    assert!(recovered.text.contains("run2();"));
    assert!(recovered.text.contains("const tail = 99;"));
    assert!(
        recovered
            .warnings
            .iter()
            .any(|w| w.contains("boundary echo"))
    );
}
