use std::panic::catch_unwind;
use std::panic::AssertUnwindSafe;

use super::super::apply::apply_edits;
use super::super::parser::{parse_patch, parse_patch_streaming};

fn apply_patch(text: &str, diff: &str) -> String {
    let (edits, _) = parse_patch(diff).unwrap();
    apply_edits(text, &edits).text
}

#[test]
fn replaces_concrete_range_with_literal_body_rows() {
    let text = "a\nb\nc";
    let diff = ["replace 2..2:", "+before", "+after"].join("\n");
    assert_eq!(apply_patch(text, &diff), "a\nbefore\nafter\nc");
}

#[test]
fn deletes_single_source_line() {
    let text = "a\nb\nc";
    assert_eq!(apply_patch(text, "delete 2"), "a\nc");
}

#[test]
fn deletes_concrete_range() {
    let text = "a\nb\nc\nd";
    assert_eq!(apply_patch(text, "delete 2..3"), "a\nd");
}

#[test]
fn inserts_before_and_after_concrete_anchors() {
    let text = "a\nb\nc";
    let diff = ["insert before 2:", "+before", "insert after 2:", "+after"].join("\n");
    assert_eq!(apply_patch(text, &diff), "a\nbefore\nb\nafter\nc");
}

#[test]
fn inserts_at_head_and_tail() {
    let text = "a\nb";
    assert_eq!(apply_patch(text, "insert head:\n+HEAD"), "HEAD\na\nb");
    assert_eq!(apply_patch(text, "insert tail:\n+TAIL"), "a\nb\nTAIL");
}

#[test]
fn rejects_empty_body_bearing_hunks() {
    let result = parse_patch("replace 2..2:");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("needs at least one"));

    let result = parse_patch("insert head:");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("needs at least one"));
}

#[test]
fn rejects_body_rows_under_delete() {
    let result = parse_patch("delete 2\n+replacement");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("does not take body rows"));
}

#[test]
fn auto_pipes_bare_body_rows_as_literal_text() {
    let text = "a\nb\nc";
    assert_eq!(apply_patch(text, "replace 2..2:\nraw"), "a\nraw\nc");
    let (_, warnings) = parse_patch("replace 2..2:\nraw").unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.contains("Auto-prefixed bare body row")));
}

#[test]
fn validates_insert_anchors_against_file_bounds() {
    let (edits, _) = parse_patch("insert before 4:\n+x").unwrap();
    let result = catch_unwind(AssertUnwindSafe(|| apply_edits("a\nb", &edits)));
    assert!(result.is_err());
    let msg = result
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| "".to_string());
    assert!(msg.contains("Line 4 does not exist"));
}

#[test]
fn does_not_flush_streaming_pending_empty_replace_block() {
    let (edits, _) = parse_patch_streaming("replace 5..5:\n");
    assert!(edits.is_empty());
}
