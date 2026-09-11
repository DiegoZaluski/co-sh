use super::super::apply::apply_edits;
use super::super::parser::parse_patch;

fn apply_patch(text: &str, diff: &str) -> String {
    let (edits, _) = parse_patch(diff).unwrap();
    apply_edits(text, &edits, None).expect("apply_edits should succeed").text
}

const FILE: &str = "a\nb\nc\nd\ne";

#[test]
fn rejects_bare_single_number_hunk_header() {
    let result = parse_patch("2\n+B");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("hunk headers need a verb"));
}

#[test]
fn rejects_bare_numeric_range() {
    let result = parse_patch("2 3\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Hunk headers need a verb"));
}

#[test]
fn accepts_canonical_replace_delete_insert_forms() {
    assert_eq!(apply_patch(FILE, "replace 2..3:\n+X"), "a\nX\nd\ne");
    assert_eq!(apply_patch(FILE, "delete 2..3"), "a\nd\ne");
    assert_eq!(
        apply_patch(FILE, "insert before 2:\n+X"),
        "a\nX\nb\nc\nd\ne"
    );
    assert_eq!(apply_patch(FILE, "insert after 2:\n+X"), "a\nb\nX\nc\nd\ne");
    assert_eq!(apply_patch(FILE, "insert head:\n+X"), "X\na\nb\nc\nd\ne");
    assert_eq!(apply_patch(FILE, "insert tail:\n+X"), "a\nb\nc\nd\ne\nX");
}

#[test]
fn accepts_single_number_replace_and_delete_shorthand() {
    assert_eq!(apply_patch(FILE, "replace 2:\n+X"), "a\nX\nc\nd\ne");
    assert_eq!(apply_patch(FILE, "delete 2"), "a\nc\nd\ne");
}

#[test]
fn accepts_alternate_replace_range_separators_and_missing_colon() {
    assert_eq!(apply_patch(FILE, "replace 2-3:\n+X"), "a\nX\nd\ne");
    assert_eq!(apply_patch(FILE, "replace 2…3:\n+X"), "a\nX\nd\ne");
    assert_eq!(apply_patch(FILE, "replace 2 3:\n+X"), "a\nX\nd\ne");
    assert_eq!(apply_patch(FILE, "replace 2..3\n+X"), "a\nX\nd\ne");
}

#[test]
fn accepts_missing_colon_on_insert_headers() {
    assert_eq!(apply_patch(FILE, "insert before 2\n+X"), "a\nX\nb\nc\nd\ne");
    assert_eq!(apply_patch(FILE, "insert head\n+X"), "X\na\nb\nc\nd\ne");
}

#[test]
fn auto_pipes_bare_body_row_while_warning() {
    let result = parse_patch("replace 2..2:\n  hello").unwrap();
    assert_eq!(apply_edits(FILE, &result.0, None).expect("apply_edits should succeed").text, "a\n  hello\nc\nd\ne");
    assert!(
        result
            .1
            .iter()
            .any(|w| w.contains("Auto-prefixed bare body row"))
    );
}

#[test]
fn rejects_minus_body_rows_with_teaching_error() {
    let result = parse_patch("replace 2..2:\n-old\n+new");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("`-` rows are not valid"));
}

#[test]
fn allows_literal_text_starting_with_minus_or_plus_when_prefixed() {
    assert_eq!(
        apply_patch(FILE, "replace 2..2:\n+-literal\n++plus"),
        "a\n-literal\n+plus\nc\nd\ne"
    );
}

#[test]
fn rejects_empty_replace_and_insert_hunks() {
    let result = parse_patch("replace 2..2:");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("To delete lines, use `delete"));

    let result = parse_patch("insert tail:");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("`insert` needs"));
}

#[test]
fn rejects_delete_with_body() {
    let result = parse_patch("delete 2\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("does not take body rows"));
}

#[test]
fn rejects_delete_with_colon() {
    let result = parse_patch("delete 2:\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("has no colon"));
}

#[test]
fn rejects_apply_patch_sentinels_as_contamination() {
    let result = parse_patch("*** Update File: a.ts\nreplace 2..2:\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("apply_patch sentinel"));

    let result = parse_patch("*** Add File: a.ts\nreplace 2..2:\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("apply_patch sentinel"));
}

#[test]
fn rejects_unified_diff_hunk_headers_as_contamination() {
    let result = parse_patch("@@ -1,3 +1,3 @@\nreplace 2..2:\n+X");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unified-diff hunk header"));
}

#[test]
fn treats_top_level_plus_text_as_orphan_literal_payload() {
    let result = parse_patch("+const X = 1;\nreplace 2..2:");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("payload line has no preceding hunk header")
    );
}

#[test]
fn keeps_replacement_boundary_echoes_literal_unless_balance_repair_applies() {
    let text = ["// one", "// two", "old();"].join("\n");
    let diff = "replace 3..3:\n+// one\n+// two\n+new();";
    assert_eq!(
        apply_patch(&text, diff),
        ["// one", "// two", "// one", "// two", "new();"].join("\n")
    );
}

#[test]
fn keeps_pure_insert_context_echoes_literal() {
    let text = ["aaa", "bbb", "ccc"].join("\n");
    let diff = "insert tail:\n+bbb\n+ccc\n+NEW";
    assert_eq!(apply_patch(&text, diff), "aaa\nbbb\nccc\nbbb\nccc\nNEW");
}
