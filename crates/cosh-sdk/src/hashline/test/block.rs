use super::super::block::{ResolveBlockEditsOptions, resolve_block_edits};
use super::super::format::compute_file_hash;
use super::super::fs::InMemoryFilesystem;
use super::super::input::Patch;
use super::super::mismatch::MismatchError;
use super::super::parser::parse_patch;
use super::super::patcher::{PatchOp, Patcher};
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};
use super::super::types::{
    Anchor, BlockResolver, BlockResolverRequest, BlockSpan, Cursor, Edit, Replacement,
    ResolveAction, SplitOptions,
};

const PATH: &str = "x.rs";

// Deterministic stub: the block beginning on line N spans [N, N+1]. The exact
// shape does not matter — the unit tests only need a resolver that is not the
// real tree-sitter native (that is exercised by the coding-agent integration
// test).
fn stub_resolver(request: BlockResolverRequest) -> Option<BlockSpan> {
    Some(BlockSpan {
        start: request.line,
        end: request.line + 1,
    })
}

fn null_resolver(_: BlockResolverRequest) -> Option<BlockSpan> {
    None
}

/// Strip parser/transform bookkeeping that `apply_edits` re-derives anyway.
#[derive(Debug, PartialEq)]
enum NormalizedEdit {
    Insert {
        cursor: Cursor,
        text: String,
        mode: Option<Replacement>,
    },
    Delete {
        anchor: Anchor,
    },
    Block {
        anchor: Anchor,
        payloads: Vec<String>,
    },
}

fn normalize(edit: &Edit) -> NormalizedEdit {
    match edit {
        Edit::Insert {
            cursor, text, mode, ..
        } => NormalizedEdit::Insert {
            cursor: cursor.clone(),
            text: text.clone(),
            mode: *mode,
        },
        Edit::Delete { anchor, .. } => NormalizedEdit::Delete { anchor: *anchor },
        Edit::Block {
            anchor, payloads, ..
        } => NormalizedEdit::Block {
            anchor: *anchor,
            payloads: payloads.clone(),
        },
    }
}

fn normalize_edits(edits: &[Edit]) -> Vec<NormalizedEdit> {
    edits.iter().map(normalize).collect()
}

#[test]
fn parses_replace_block_into_single_deferred_block_edit() {
    let (edits, _) = parse_patch("replace block 2:\n+A\n+B").unwrap();

    assert_eq!(edits.len(), 1);
    let edit = &edits[0];
    assert!(matches!(edit, Edit::Block { .. }));
    if let Edit::Block {
        anchor, payloads, ..
    } = edit
    {
        assert_eq!(anchor.line, 2);
        assert_eq!(payloads, &["A", "B"]);
    }
}

#[test]
fn parses_literal_range_without_block_subkeyword() {
    let (edits, _) = parse_patch("replace 2..3:\n+A").unwrap();

    assert!(!edits.iter().any(|e| matches!(e, Edit::Block { .. })));
    assert!(edits.iter().any(|e| matches!(e, Edit::Delete { .. })));
}

#[test]
fn rejects_empty_block_hunk() {
    let result = parse_patch("replace block 2:");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("needs at least one"));
}

#[test]
fn expands_block_edit_like_equivalent_range_replace() {
    let (block_edits, _) = parse_patch("replace block 2:\n+A\n+B").unwrap();
    let resolved = resolve_block_edits(
        &block_edits,
        "ignored",
        PATH,
        Some(stub_resolver as BlockResolver),
        None,
    )
    .unwrap();
    let (replace_edits, _) = parse_patch("replace 2..3:\n+A\n+B").unwrap();

    assert!(!resolved.iter().any(|e| matches!(e, Edit::Block { .. })));
    assert_eq!(normalize_edits(&resolved), normalize_edits(&replace_edits));
}

#[test]
fn fast_path_returns_input_untouched_when_no_block_edits() {
    let (edits, _) = parse_patch("replace 1..1:\n+X").unwrap();
    let original = edits.clone();
    let resolved = resolve_block_edits(
        &edits,
        "ignored",
        PATH,
        Some(stub_resolver as BlockResolver),
        None,
    )
    .unwrap();
    assert_eq!(resolved, original);
}

#[test]
fn errors_when_no_resolver_wired() {
    let (edits, _) = parse_patch("replace block 2:\n+X").unwrap();
    // Err, never a panic — authored-input rejection flows through the same
    // channel as every other malformed edit (parity guarantee).
    let msg = resolve_block_edits(&edits, "ignored", PATH, None, None).unwrap_err();
    assert!(msg.contains("not available here"));
}

#[test]
fn drops_unresolvable_block_edit_in_drop_mode() {
    let (edits, _) = parse_patch("replace block 2:\n+X").unwrap();
    let resolved = resolve_block_edits(
        &edits,
        "ignored",
        PATH,
        Some(null_resolver as BlockResolver),
        Some(ResolveBlockEditsOptions {
            on_unresolved: ResolveAction::Drop,
        }),
    )
    .unwrap();
    assert!(resolved.is_empty());
}

#[test]
fn errors_block_unresolved_when_resolver_returns_null() {
    let (edits, _) = parse_patch("replace block 7:\n+X").unwrap();
    // Err, never a panic — the patcher has no catch_unwind around this call,
    // so a panic here would kill the whole agent loop instead of returning
    // the diagnostic to the model.
    let msg = resolve_block_edits(
        &edits,
        "ignored",
        PATH,
        Some(null_resolver as BlockResolver),
        None,
    )
    .unwrap_err();
    assert!(
        msg.contains("could not resolve a syntactic block beginning on line 7"),
        "{msg}"
    );
}

#[test]
fn apply_to_resolves_block_and_matches_replace() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let block_section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\nreplace block 2:\n+  if (y || z) {{\n+  }}"),
        &SplitOptions::default(),
    )
    .unwrap();
    let replace_section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\nreplace 2..3:\n+  if (y || z) {{\n+  }}"),
        &SplitOptions::default(),
    )
    .unwrap();

    let block_result = block_section
        .apply_to(text, Some(stub_resolver as BlockResolver))
        .expect("apply_to should succeed");
    let replace_result = replace_section
        .apply_to(text, None)
        .expect("apply_to should succeed");

    assert_eq!(
        block_result.text,
        "function x() {\n  if (y || z) {\n  }\n}\n"
    );
    assert_eq!(block_result.text, replace_result.text);
}

#[test]
fn apply_to_errors_when_block_edit_has_no_resolver() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\nreplace block 2:\n+X"),
        &SplitOptions::default(),
    )
    .unwrap();
    // Clean Err, never a panic — apply_to's Throw path returns the
    // diagnostic through the same channel as every other malformed edit.
    let msg = section
        .apply_to(text, None)
        .unwrap_err();
    assert!(msg.contains("replace block"), "{msg}");
}

#[test]
fn apply_partial_to_drops_unresolvable_block_edit() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\nreplace block 2:\n+X"),
        &SplitOptions::default(),
    )
    .unwrap();
    // No resolver → drop. The lone block edit vanishes, so the text is unchanged.
    let result = section
        .apply_partial_to(text, None)
        .expect("apply_partial_to should succeed");
    assert_eq!(result.text, text);
}

#[tokio::test]
async fn patcher_applies_block_edit_on_hash_match_path() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), text.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, text);
    let mut patcher = Patcher::new(fs, store, Some(stub_resolver as BlockResolver));
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace block 2:\n+  if (y || z) {{\n+  }}"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    assert_eq!(result.sections[0].op, PatchOp::Update);
    assert_eq!(
        result.sections[0].after,
        "function x() {\n  if (y || z) {\n  }\n}\n"
    );
}

#[tokio::test]
async fn resolves_against_tagged_snapshot_and_recovers_onto_drifted_content() {
    let snapshot_text = "line0\nline1\nline2\nline3\nline4\n";
    // The live file gained a trailing line after the read minted the tag.
    let live_text = "line0\nline1\nline2\nline3\nline4\nline5\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), live_text.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, snapshot_text);
    let mut patcher = Patcher::new(fs, store, Some(stub_resolver as BlockResolver));
    // `block 2` resolves against the SNAPSHOT → span [2,3] → replace
    // "line1","line2"; recovery 3-way-merges the change onto the live file.
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace block 2:\n+NEW"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    assert_eq!(result.sections[0].op, PatchOp::Update);
    assert_eq!(
        result.sections[0].after,
        "line0\nNEW\nline3\nline4\nline5\n"
    );
    assert!(
        result.sections[0]
            .warnings
            .iter()
            .any(|w| w.contains("Recovered"))
    );
}

#[tokio::test]
async fn rejects_block_edit_whose_tag_was_never_recorded_for_path() {
    let text = "line0\nline1\nline2\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), text.to_string())]);
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let live = compute_file_hash(text);
    let bogus = if live == "FFFF" { "0000" } else { "FFFF" };
    let check_fs = fs.clone();
    let mut patcher = Patcher::new(fs, store, Some(stub_resolver as BlockResolver));
    let patch = Patch::parse(
        &format!("¶{PATH}#{bogus}\nreplace block 2:\n+NEW"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .downcast_ref::<MismatchError>()
            .is_some()
    );
    assert_eq!(check_fs.get(PATH).unwrap(), text);
}

#[tokio::test]
async fn patcher_returns_error_when_resolver_returns_null() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), text.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, text);
    let _check_fs = fs.clone();
    let mut patcher = Patcher::new(fs, store, Some(null_resolver as BlockResolver));
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace block 2:\n+X"),
        &SplitOptions::default(),
    )
    .unwrap();
    // A clean Err carrying the diagnostic — never a panic (the patcher's
    // apply path has no catch_unwind; a panic would kill the agent loop).
    let msg = patcher.apply(&patch).await.unwrap_err().to_string();
    assert!(
        msg.contains("could not resolve a syntactic block"),
        "{msg}"
    );
}

#[test]
fn parses_delete_block_into_block_edit_with_no_payloads() {
    let (edits, _) = parse_patch("delete block 2").unwrap();

    assert_eq!(edits.len(), 1);
    let edit = &edits[0];
    assert!(matches!(edit, Edit::Block { .. }));
    if let Edit::Block {
        anchor, payloads, ..
    } = edit
    {
        assert_eq!(anchor.line, 2);
        assert!(payloads.is_empty());
    }
}

#[test]
fn rejects_body_rows_under_delete_block() {
    let result = parse_patch("delete block 2\n+X");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("delete block N` does not take body rows")
    );
}

#[test]
fn resolve_block_edits_expands_delete_block_into_pure_deletes() {
    let (edits, _) = parse_patch("delete block 2").unwrap();
    let resolved = resolve_block_edits(
        &edits,
        "ignored",
        PATH,
        Some(stub_resolver as BlockResolver),
        None,
    )
    .unwrap();

    assert!(resolved.iter().all(|e| matches!(e, Edit::Delete { .. })));
    let delete_lines: Vec<u32> = resolved
        .iter()
        .filter_map(|e| {
            if let Edit::Delete { anchor, .. } = e {
                Some(anchor.line)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(delete_lines, vec![2, 3]);
}

#[test]
fn apply_to_deletes_resolved_block_span() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\ndelete block 2"),
        &SplitOptions::default(),
    )
    .unwrap();
    // stub span [2,3] → drop "  if (y) {" and "  }".
    let result = section
        .apply_to(text, Some(stub_resolver as BlockResolver))
        .expect("apply_to should succeed");
    assert_eq!(result.text, "function x() {\n}\n");
}

#[test]
fn apply_partial_to_drops_unresolvable_delete_block_edit() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let section = Patch::parse_single(
        &format!("¶{PATH}#1A2B\ndelete block 2"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = section
        .apply_partial_to(text, None)
        .expect("apply_partial_to should succeed");
    assert_eq!(result.text, text);
}

#[tokio::test]
async fn patcher_applies_delete_block_edit_on_hash_match_path() {
    let text = "function x() {\n  if (y) {\n  }\n}\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), text.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, text);
    let mut patcher = Patcher::new(fs, store, Some(stub_resolver as BlockResolver));
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\ndelete block 2"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    assert_eq!(result.sections[0].op, PatchOp::Update);
    assert_eq!(result.sections[0].after, "function x() {\n}\n");
}
