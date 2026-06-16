use super::super::apply::apply_edits;
use super::super::parser::parse_patch;
use super::super::recovery::{Recovery, RecoveryArgs};
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};

fn apply_with_warnings(text: &str, diff: &str) -> (String, Vec<String>) {
    let (edits, _) = parse_patch(diff).unwrap();
    let result = apply_edits(text, &edits);
    (result.text, result.warnings)
}

const PATH: &str = "/tmp/__hashline-boundary-recovery__.ts";

#[test]
fn drops_duplicated_multi_line_closing_block() {
    // The canonical incident: a range-replace whose payload restates the
    // fragment + paren close that still live just below the range, doubling
    // `</>` and `);`. `replace 11..31:` covers `const …` through the second `/>`.
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
    // Range 7..16 = `const …` through the first `/>`; payload restates the
    // `</>` + `);` that survive at lines 17-18.
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
    let (text, warnings) = apply_with_warnings(&file, &diff);
    // Exactly one `</>` and one `);` survive — no doubling.
    assert_eq!(text.split('\n').filter(|l| l.trim() == "</>").count(), 1);
    assert_eq!(text.split('\n').filter(|l| l.trim() == ");").count(), 1);
    assert!(text.ends_with("\t\t</>\n\t);\n};"));
    assert!(warnings.iter().any(|w| w.contains("delimiter-balance")));
}

#[test]
fn drops_single_duplicated_structural_closer() {
    // Single structural-closer duplication: the range ends one line short and
    // the payload restates the `});` that survives just below it.
    let file = [
        "it('a', () => {",
        "\tsetup();",
        "\trun();",
        "});",
        "after();",
    ]
    .join("\n");
    // `replace 2..3:` replaces the two body lines but the payload also restates the
    // `});` at line 4, which survives — a duplicate close.
    let diff = ["replace 2..3:", "+\tsetup2();", "+\trun2();", "+});"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff);
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
    assert!(warnings.iter().any(|w| w.contains("delimiter-balance")));
}

#[test]
fn spares_deleted_closing_line_when_payload_omits_it() {
    // Genuine missing-closer: payload omits the trailing `});`.
    let file = [
        "const handlers = {",
        "\ta() {",
        "\t\treturn 1;",
        "\t},",
        "};",
    ]
    .join("\n");
    // `replace 5..5:` is the final `};`. Model inserts a new method but forgets to
    // restate `};`; sparing it keeps the object literal balanced.
    let diff = ["replace 5..5:", "+\tb() {", "+\t\treturn 2;", "+\t},"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff);
    assert_eq!(
        text,
        [
            "const handlers = {",
            "\ta() {",
            "\t\treturn 1;",
            "\t},",
            "\tb() {",
            "\t\treturn 2;",
            "\t},",
            "};",
        ]
        .join("\n")
    );
    assert!(warnings.iter().any(|w| w.contains("delimiter-balance")));
}

#[test]
fn leaves_balance_preserving_replacement_alone() {
    // Balance-preserving edits are never touched, even when the payload's last
    // line coincidentally equals the line just below the range.
    let file = ["foo();", "bar();", "bar();", "baz();"].join("\n");
    // Replace line 2 with two balanced statements; the tail `bar();` equals
    // the surviving line 3 but the payload is balanced — must NOT be dropped.
    let diff = ["replace 2..2:", "+qux();", "+bar();"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff);
    assert_eq!(
        text,
        ["foo();", "qux();", "bar();", "bar();", "baz();"].join("\n")
    );
    assert!(warnings.is_empty());
}

#[test]
fn does_not_drop_balance_neutral_duplicated_statement() {
    // A duplicated full statement (balance-neutral) is left intact: dropping it
    // could discard intended content, and it does not break syntax.
    let file = ["a = 1;", "b = 2;", "c = 3;"].join("\n");
    let diff = ["replace 1..1:", "+a = 1;", "+b = 2;"].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff);
    assert_eq!(text, ["a = 1;", "b = 2;", "b = 2;", "c = 3;"].join("\n"));
    assert!(warnings.is_empty());
}

#[test]
fn ignores_brackets_inside_string_literals() {
    // Brackets inside strings must not trigger a spurious balance mismatch.
    let file = [
        r#"const a = "}";"#,
        r#"const b = "x";"#,
        r#"const c = "y";"#,
    ]
    .join("\n");
    let diff = [r#"replace 2..2:"#, r#"+const b = "}}}";"#].join("\n");
    let (text, warnings) = apply_with_warnings(&file, &diff);
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
    // Recovery composes `applyEdits` to compute the intended change, so the
    // boundary repair runs there too. The snapshot (what the model read)
    // carries the structure; the live file has drifted far from the edit
    // region, so the stale-hash 3-way merge succeeds and the repaired
    // (de-duplicated) hunk lands without doubling the closer.
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
    // Live file drifted only at the tail (line 13) — far outside the edit
    // region (lines 4-6), so the 3-way merge applies cleanly.
    let current_text = snapshot_text.replace("const tail = 0;", "const tail = 99;");

    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let file_hash = store.record(PATH, &snapshot_text);

    // `replace 4..5:` replaces the body lines but the payload also restates the `});`
    // that survives at line 6 — the duplicate-closer mistake.
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
    // Exactly one `});` — the duplicate was absorbed during recovery.
    assert_eq!(
        recovered.text.split('\n').filter(|l| *l == "});").count(),
        1
    );
    assert!(recovered.text.contains("setup2();"));
    assert!(recovered.text.contains("run2();"));
    // The unrelated drift on the live file survives the merge.
    assert!(recovered.text.contains("const tail = 99;"));
    // The repair warning propagates out through the recovery result.
    assert!(recovered
        .warnings
        .iter()
        .any(|w| w.contains("delimiter-balance")));
}
