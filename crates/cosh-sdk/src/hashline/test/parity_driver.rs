//! Behavioral-parity driver — identical scenarios to upstream oh-my-pi's
//! `crates/pi-edit/tests/parity_driver.rs`. Run both with `--nocapture` and
//! diff the dumps for an exact behavioral comparison.
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
fn parity_dump() {
    dump(
        "spare_deleted_closer",
        "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n};",
        &replacement(5, 5, &["\tb() {", "\t\treturn 2;", "\t},"], 1),
        Some("x.ts"),
    );
    dump(
        "no_spare_when_restated",
        "class Foo {\n\tok();\n\t}\n}",
        &replacement(1, 4, &["class Foo {", "\tok();", "}"], 1),
        Some("x.ts"),
    );
    dump(
        "drop_leading_and_trailing_echo",
        "function f() {\n  keepA();\n  old1();\n  old2();\n  keepB();\n}",
        &replacement(
            3,
            4,
            &["  keepA();", "  new1();", "  new2();", "  keepB();"],
            1,
        ),
        Some("x.ts"),
    );
    dump(
        "ambiguous_leading_echo",
        "{\n    auto* handle = payloadFor<PyThreadHandle>(self);\n    if (!handle)\n        return threadError(globalObject, \"thread not started\");\n    handle.setDone();\n}",
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
    );
    dump(
        "trailing_attribute_echo",
        "/// Old summary.\n#[napi]\npub fn f() {}",
        &replacement(1, 1, &["/// New summary.", "#[napi]"], 1),
        Some("x.rs"),
    );
    dump(
        "trailing_statement_echo_kept",
        "foo();\nold();\nbar();",
        &replacement(2, 2, &["new();", "bar();"], 1),
        Some("x.ts"),
    );
    dump(
        "landing_slide",
        "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
        &[insert_after(3, "    c();", 1)],
        None,
    );
    dump(
        "landing_cross_two_levels",
        "function f() {\n    if (x) {\n        for (y) {\n            a();\n        }\n    }\n    b();\n}\n",
        &[insert_after(4, "    c();", 1)],
        None,
    );
    dump(
        "refuse_cross_targeted_line",
        "function f() {\n    if (x) {\n        a();\n    }\n    b();\n}\n",
        &[insert_after(3, "    c();", 1), delete(4, 2)],
        None,
    );
    dump(
        "indent_restore",
        "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n\tlet c = 3;\n}",
        &replacement(2, 4, &["let a = 1;", "let b = 20;", "let c = 3;"], 1),
        Some("x.rs"),
    );
    dump(
        "broken_parse_advisory",
        "fn f() {\n}\n",
        &replacement(1, 2, &["fn f() {"], 1),
        Some("x.rs"),
    );
    dump(
        "no_path_applies_authored",
        "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n};",
        &replacement(5, 5, &["\tb() {", "\t\treturn 2;", "\t},"], 1),
        None,
    );
}
