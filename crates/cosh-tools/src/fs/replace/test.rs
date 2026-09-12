use super::super::{Fs, ReplaceEdit};
use cosh_sdk::rollback;

fn fs_auto() -> Fs {
    Fs::new().cwd("/home/inky/co-sh")
}

/// Seed a fixture file and mint its session snapshot tag.
fn fixture(name: &str, text: &str) -> (String, String) {
    let path = format!("/home/inky/co-sh/cosh_test_replace_{name}");
    std::fs::write(&path, text).unwrap();
    let hash = rollback::record(&path, text).expect("snapshot recorded");
    (path, hash)
}

fn edit(path: &str, hash: Option<&str>, old_string: &str, new_string: &str) -> ReplaceEdit {
    ReplaceEdit {
        path: path.to_string(),
        file_hash: hash.map(ToString::to_string),
        old_string: old_string.to_string(),
        new_string: new_string.to_string(),
        replace_all: false,
    }
}

async fn run(edits: Vec<ReplaceEdit>) -> Result<Vec<super::super::EditResult>, String> {
    fs_auto().replace_edits(edits).await
}

#[tokio::test]
async fn unique_multiline_match_replaces_whole_lines() {
    let (path, hash) = fixture("multiline", "fn a() {}\nfn b() {}\nfn c() {}\n");
    let out = run(vec![edit(
        &path,
        Some(&hash),
        "fn b() {}",
        "fn b() {\n    b();\n}",
    )])
    .await
    .expect("unique match should apply");
    assert_eq!(out.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn a() {}\nfn b() {\n    b();\n}\nfn c() {}\n"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn partial_line_match_keeps_surrounding_context() {
    let (path, hash) = fixture("partial", "let value = old_computation(x);\n");
    run(vec![edit(
        &path,
        Some(&hash),
        "old_computation",
        "new_computation",
    )])
    .await
    .expect("partial-line match should apply");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "let value = new_computation(x);\n"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn match_with_trailing_newline_does_not_add_blank_lines() {
    let (path, hash) = fixture("trailing_nl", "a\nb\nc\n");

    run(vec![edit(&path, Some(&hash), "b\n", "B\n")])
        .await
        .expect("newline-terminated match should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB\nc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn newline_terminated_match_growing_content_keeps_lines() {
    let (path, hash) = fixture("grow", "a\nb\nc\n");
    run(vec![edit(&path, Some(&hash), "b\n", "B1\nB2\n")])
        .await
        .expect("growing newline-terminated match should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB1\nB2\nc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn empty_new_string_deletes_whole_lines() {
    let (path, hash) = fixture("delete_lines", "a\nb\nc\n");
    run(vec![edit(&path, Some(&hash), "b\n", "")])
        .await
        .expect("whole-line delete should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn empty_new_string_inside_a_line_merges_context() {
    let (path, hash) = fixture("delete_partial", "call(one, two);\n");
    run(vec![edit(&path, Some(&hash), "one, ", "")])
        .await
        .expect("partial delete should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "call(two);\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn multiple_occurrences_require_replace_all() {
    let (path, hash) = fixture("ambiguous", "x = 1;\ny = x;\nx = 2;\n");
    let err = run(vec![edit(&path, Some(&hash), "x", "z")])
        .await
        .expect_err("ambiguous match must be rejected");
    assert!(
        err.contains("Found 3 occurrences") && err.contains("replace_all"),
        "got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn replace_all_replaces_every_occurrence() {
    let (path, hash) = fixture("all", "x = 1;\ny = x;\nx = 2;\n");
    let mut all = edit(&path, Some(&hash), "x", "z");
    all.replace_all = true;
    run(vec![all]).await.expect("replace_all should apply");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "z = 1;\ny = z;\nz = 2;\n"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn no_match_points_at_file_content() {
    let (path, hash) = fixture("nomatch", "alpha\nbeta\n");
    let err = run(vec![edit(&path, Some(&hash), "alpha ", "gamma")])
        .await
        .expect_err("no match must be rejected");
    assert!(
        err.contains("Could not find the exact text") && err.contains("alpha"),
        "got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn match_must_be_whitespace_exact() {
    let (path, hash) = fixture("ws", "let  x   = 1;\n");
    let err = run(vec![edit(&path, Some(&hash), "let x = 1;", "let x = 2;")])
        .await
        .expect_err("whitespace mismatch must be rejected");
    assert!(err.contains("Could not find the exact text"), "got: {err}");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn empty_old_string_is_rejected() {
    let (path, hash) = fixture("empty_old", "a\n");
    let err = run(vec![edit(&path, Some(&hash), "", "b")])
        .await
        .expect_err("empty old_string must be rejected");
    assert!(err.contains("old_string must not be empty"), "got: {err}");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn missing_file_hash_demands_a_read() {
    let (path, _) = fixture("nohash", "a\n");
    let err = run(vec![edit(&path, None, "a", "b")])
        .await
        .expect_err("missing tag must be rejected");
    assert!(
        err.contains("Missing hashline snapshot tag") && err.contains("read"),
        "got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn unknown_tag_is_rejected() {
    let (path, _) = fixture("badtag", "a\n");
    let err = run(vec![edit(&path, Some("0000"), "a", "b")])
        .await
        .expect_err("unknown tag must be rejected");
    assert!(
        err.contains("No snapshot recorded for tag #0000"),
        "got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn followup_edit_on_same_file_chains_fresh_tag() {
    let (path, hash) = fixture("chain", "one\ntwo\nthree\n");
    let out = run(vec![
        edit(&path, Some(&hash), "one", "1"),
        edit(&path, None, "three", "3"),
    ])
    .await
    .expect("chained edits should apply");
    assert_eq!(out.len(), 2);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "1\ntwo\n3\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn batch_failure_keeps_applied_and_reports_skipped() {
    let (path, hash) = fixture("batch", "a\nb\nc\n");
    let (other, _) = fixture("batch_other", "1\n");
    let (third, _) = fixture("batch_third", "9\n");
    let err = run(vec![
        edit(&path, Some(&hash), "b", "B"),
        edit(&other, None, "nope", "2"),
        edit(&third, None, "9", "8"),
    ])
    .await
    .expect_err("second edit must fail and abort the batch");
    assert!(err.contains("Applied before the failure"), "got: {err}");
    assert!(err.contains("Not applied"), "got: {err}");
    assert!(err.contains(&third), "skipped edit must be listed: {err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a\nB\nc\n",
        "applied edits stay"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&other);
    let _ = std::fs::remove_file(&third);
}

/// A stale tag with an external edit between read and edit is rejected with
/// the mismatch diagnostic — the same behavior as the `targets` engine, whose
/// recovery declines this shape (probe-verified parity).
#[tokio::test]
async fn stale_tag_is_rejected_like_the_targets_engine() {
    let (path, hash) = fixture("stale", "line1\nline2\nline3\n");
    std::fs::write(&path, "line1 edited\nline2\nline3\n").unwrap();
    let err = run(vec![edit(&path, Some(&hash), "line3", "line3 edited")])
        .await
        .expect_err("stale tag must be rejected");
    assert!(
        err.contains("file changed between read and edit"),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "line1 edited\nline2\nline3\n",
        "rejected edit must not touch the file"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn crlf_line_endings_survive_content_edits() {
    let (path, hash) = fixture("crlf", "a\r\nb\r\nc\r\n");
    run(vec![edit(&path, Some(&hash), "b", "B")])
        .await
        .expect("crlf edit should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\r\nB\r\nc\r\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn replacement_payload_of_hashline_header_shape_is_applied_verbatim() {
    let (path, hash) = fixture("headerish", "value = 1;\n");
    run(vec![edit(&path, Some(&hash), "value", "replace 1..1:")])
        .await
        .expect("replace should apply");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "replace 1..1: = 1;\n"
    );
    let _ = std::fs::remove_file(&path);
}

// ── line-terminator boundary semantics ────────────────────────────────────

#[tokio::test]
async fn terminator_consuming_match_without_trailing_newline_joins_next_line() {
    let (path, hash) = fixture("join", "a\nb\nc\n");
    run(vec![edit(&path, Some(&hash), "b\n", "B")])
        .await
        .expect("join edit should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nBc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn terminator_consuming_match_without_trailing_newline_at_eof_is_rejected() {
    let (path, hash) = fixture("join_eof", "a\nb\n");
    let err = run(vec![edit(&path, Some(&hash), "b\n", "B")])
        .await
        .expect_err("joining past the final newline must be rejected");
    assert!(err.contains("trailing newline"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a\nb\n",
        "rejected edit must not touch the file"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn whole_line_content_match_with_empty_new_keeps_an_empty_line() {
    let (path, hash) = fixture("empty_line", "a\nb\nc\n");
    run(vec![edit(&path, Some(&hash), "b", "")])
        .await
        .expect("emptying a line should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n\nc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn terminator_consuming_match_with_empty_new_at_eof_deletes_the_lines() {
    let (path, hash) = fixture("del_eof", "a\nb\n");
    run(vec![edit(&path, Some(&hash), "b\n", "")])
        .await
        .expect("deleting the last line should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n");
    let _ = std::fs::remove_file(&path);
}

// ── placement and payload edge cases ──────────────────────────────────────

#[tokio::test]
async fn replace_all_handles_occurrences_on_the_same_line() {
    let (path, hash) = fixture("same_line", "abab\n");
    let mut all = edit(&path, Some(&hash), "ab", "xy");
    all.replace_all = true;
    run(vec![all])
        .await
        .expect("same-line replace_all should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xyxy\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn match_at_beginning_of_file_spanning_a_newline() {
    let (path, hash) = fixture("bof", "a\nb\nc\n");
    run(vec![edit(&path, Some(&hash), "a\nb", "x")])
        .await
        .expect("BOF multi-line match should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\nc\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn file_without_trailing_newline_edits_cleanly() {
    let (path, hash) = fixture("no_final_nl", "a\nb");
    run(vec![edit(&path, Some(&hash), "b", "B")])
        .await
        .expect("edit on file without trailing newline should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn payload_with_empty_and_plus_prefixed_lines_is_verbatim() {
    let (path, hash) = fixture("plus_lines", "m\n");
    run(vec![edit(&path, Some(&hash), "m", "+x\n\n-y")])
        .await
        .expect("payload with sigil-shaped lines should apply");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "+x\n\n-y\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn boundary_echo_repair_that_would_alter_exact_replacement_is_rejected() {
    let (path, hash) = fixture("echo_guard", "a\nb\nc\nd\n");
    let err = run(vec![edit(&path, Some(&hash), "b\nc", "a\nz\nw")])
        .await
        .expect_err("echo repair would change the exact replacement");
    assert!(
        err.contains("deviated from the exact replacement"),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a\nb\nc\nd\n",
        "rejected edit must not touch the file"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn file_over_snapshot_budget_gets_a_size_diagnostic() {
    let path = "/home/inky/co-sh/cosh_test_replace_oversize";
    std::fs::write(path, "x".repeat(600 * 1024)).unwrap();
    let err = run(vec![edit(path, Some("0000"), "x", "y")])
        .await
        .expect_err("oversize file must be rejected with the size remedy");
    assert!(
        err.contains("snapshot budget") && err.contains("`targets`"),
        "got: {err}"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn chaining_works_across_path_spellings_of_the_same_file() {
    // The chain key is the guard-canonical path, so spellings that collapse
    // to the same absolute path chain without an explicit file_hash.
    let path = std::env::current_dir()
        .unwrap()
        .join("cosh_test_replace_spellings");
    let path = path.to_string_lossy().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let hash = rollback::record(&path, "one\ntwo\n").unwrap();
    let dotted = format!(
        "{}/./cosh_test_replace_spellings",
        path.rsplit_once('/').unwrap().0
    );
    let out = run(vec![
        edit(&path, Some(&hash), "one", "1"),
        edit(&dotted, None, "two", "2"),
    ])
    .await
    .expect("chained edit via a different path spelling should apply");
    assert_eq!(out.len(), 2);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "1\n2\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn grouped_eof_delete_with_surviving_inter_occurrence_text_is_rejected() {
    let (path, hash) = fixture("group_eof", "b\nxb\n");
    let mut all = edit(&path, Some(&hash), "b\n", "");
    all.replace_all = true;
    let err = run(vec![all])
        .await
        .expect_err("the surviving 'x' would join past the final newline");
    assert!(err.contains("trailing newline"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\nxb\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn binary_file_gets_a_binary_diagnostic() {
    let path = "/home/inky/co-sh/cosh_test_replace_binary";
    std::fs::write(path, [b'a', 0, b'b']).unwrap();
    let err = run(vec![edit(path, Some("0000"), "a", "b")])
        .await
        .expect_err("binary content must be rejected as unanchorable");
    assert!(err.contains("binary"), "got: {err}");
    let _ = std::fs::remove_file(path);
}

// ── fuzzy closest-match diagnostics (enrichment only, never substitution) ─

#[tokio::test]
async fn whitespace_drift_no_match_points_at_the_near_identical_region() {
    let (path, hash) = fixture("fuzzy_ws", "let  x   = 1;\n");
    let err = run(vec![edit(&path, Some(&hash), "let x = 1;", "let x = 2;")])
        .await
        .expect_err("whitespace mismatch must be rejected");
    assert!(
        err.contains("A near-identical region (100% similar) exists at line 1")
            && err.contains("copy its text exactly into old_string"),
        "the closest-match hint must be actionable: {err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "let  x   = 1;\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn below_threshold_no_match_uses_the_upstream_wording() {
    let (path, hash) = fixture("fuzzy_below", "alpha\nbeta\ngamma\n");
    let err = run(vec![edit(&path, Some(&hash), "alpha\nZETA", "x")])
        .await
        .expect_err("no match must be rejected");
    assert!(
        err.contains("Could not find a close enough match in")
            && err.contains("(67% similar) at line 1")
            && err.contains("  - ZETA\n  + beta")
            && err.contains("Closest match was below the 95% similarity threshold."),
        "upstream byte-for-byte wording with -/+ lines: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn ambiguous_no_match_lists_occurrence_previews() {
    let (path, hash) = fixture("fuzzy_amb", "x = 1;\ny = x;\nx = 2;\n");
    let err = run(vec![edit(&path, Some(&hash), "x", "z")])
        .await
        .expect_err("ambiguous match must be rejected");
    assert!(
        err.contains("Found 3 occurrences in") && err.contains("Add more context lines"),
        "oh-my-pi occurrence format with previews: {err}"
    );
    assert!(
        err.contains("replace_all"),
        "our schema's escape hatch is named: {err}"
    );
    let _ = std::fs::remove_file(&path);
}
