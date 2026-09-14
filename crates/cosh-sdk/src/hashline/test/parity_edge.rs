//! Edge-case parity dump — identical scenarios to upstream's
//! `crates/pi-edit/tests/parity_driver.rs::parity_dump_edge`. Run both sides
//! with `--nocapture --test-threads=1` and diff.
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

fn dump(name: &str, text: &str, edits: &[Edit], path: Option<&str>) {
    println!("=== {name} ===");
    match apply_edits(text, edits, path) {
        Ok(result) => {
            println!("TEXT:");
            println!("{}", result.text);
            println!("WARNINGS:");
            for warning in &result.warnings {
                println!("{warning}");
            }
        }
        Err(error) => {
            println!("ERROR:");
            println!("{error}");
        }
    }
    println!();
}

#[test]
fn parity_dump_edge() {
    dump(
        "E1_authored_parses",
        "function f() {\n  old();\n}",
        &replacement(2, 2, &["  new();"], 1),
        Some("x.ts"),
    );
    dump(
        "E2_unrepairable",
        "fn f() {\n}\n",
        &replacement(1, 2, &["fn f() {"], 1),
        Some("x.rs"),
    );
    dump(
        "E3_broken_baseline",
        "fn broken(",
        &replacement(1, 1, &["fn f() {"], 1),
        Some("x.rs"),
    );
    dump(
        "E4_unknown_ext",
        "const handlers = {\n\tkeep\n};",
        &replacement(2, 2, &["changed"], 1),
        Some("x.unknown"),
    );
    dump(
        "E5_placement_a",
        "a {\n}\n",
        &replacement(2, 2, &["}"], 1),
        Some("x.rs"),
    );
    dump(
        "E6_placement_b",
        "fn f() {\n    match x {\n        y => 1,\n    }\n}",
        &replacement(3, 3, &["        z => 2,,"], 1),
        Some("x.rs"),
    );
    let mut e7 = replacement(2, 3, &["\tsetup2();", "});"], 1);
    e7.push(delete(4, 2));
    dump(
        "E7_deleted_neighbor",
        "it('a', () => {\n\tsetup();\n});\n});\nafter();",
        &e7,
        Some("x.ts"),
    );
    let mut e8 = replacement(2, 3, &["\tz();"], 1);
    e8.extend(replacement(5, 5, &["\tc();"], 2));
    dump(
        "E8_clean_beside_repaired",
        "fn f() {\n\ta();\n}\nfn g() {\n\tb();\n}\n",
        &e8,
        Some("x.rs"),
    );
    let mut e9 = vec![insert_after(3, "    x();", 1)];
    e9.extend(replacement(2, 2, &["    a2();"], 2));
    dump(
        "E9_insert_replace_compose",
        "function f() {\n    a();\n    b();\n}\n",
        &e9,
        Some("x.js"),
    );
    dump(
        "E10_opener_escape",
        "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
        &[insert_after(2, "    c();", 1)],
        Some("x.js"),
    );
    dump(
        "E11_statement_echo",
        "foo();\nold();\nbar();",
        &replacement(2, 2, &["new();", "bar();"], 1),
        Some("x.ts"),
    );
    let mut e12 = replacement(5, 5, &["\tw() {", "\t\treturn 9;", "\t},"], 1);
    e12.extend(replacement(
        10,
        10,
        &["\tz() {", "\t\treturn 8;", "\t},"],
        2,
    ));
    dump(
        "E12_two_groups",
        "const a = {\n\tx() {\n\t\treturn 1;\n\t},\n};\nconst b = {\n\ty() {\n\t\treturn 2;\n\t},\n};",
        &e12,
        Some("x.ts"),
    );
    dump(
        "E13_underfilled",
        "fn f() {\n\ta();\n\tb();\n}\n",
        &replacement(2, 4, &["\ta();"], 1),
        Some("x.rs"),
    );
    dump(
        "E14_tab_columns",
        "function f() {\n\tif (x) {\n\t\t\ta();\n\t\t}\n\t\tb();\n\t}\n",
        &[insert_after(3, "\t\tc();", 1)],
        Some("x.js"),
    );
    dump(
        "E15_landing_slide",
        "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
        &[insert_after(3, "    c();", 1)],
        None,
    );
    dump(
        "E16_slide_cross_targeted",
        "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
        &[insert_after(3, "    c();", 1), delete(4, 2)],
        None,
    );
    dump(
        "E17_slide_two_levels",
        "function f() {\n    if (x) {\n        for (y) {\n            a();\n        }\n    }\n    b();\n}\n",
        &[insert_after(4, "    c();", 1)],
        None,
    );
}
