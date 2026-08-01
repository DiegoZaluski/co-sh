//! Tests for context expansion functionality (expand_slot, collapse_context)

use crate::harness::context_manager::ContextManager;
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// --- Expansion tests ---

// Verifies that collapse_context() clears the expand list.
#[test]
fn collapse_context_clears_list() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.expand_slot(10);
    cm.expand_slot(20);
    assert_eq!(cm.expand.len(), 2);

    cm.collapse_context();

    assert!(
        cm.expand.is_empty(),
        "expand list should be empty after collapse"
    );
}

// Verifies that format_context does NOT contain "[EXPANDED]" when
// no expand_slot has been called.
#[test]
fn format_context_without_expand_no_expanded() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("first".to_string(), None, 1, 2, Some(1));
    cm.build_context("second".to_string(), None, 1, 3, Some(2));

    let output = cm.format_context().unwrap();

    assert!(
        !output.contains("[EXPANDED]"),
        "no [EXPANDED] without expand call"
    );
    assert!(output.contains("first"));
    assert!(output.contains("second"));
}

// Verifies that format_context includes expanded parent content when
// the model sends the target hash (exhibition << 24 | lv_desejado).
#[test]
fn format_context_with_expand_includes_parent_content() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context(
        "detailed original content".to_string(),
        None,
        1,
        100,
        Some(1),
    );
    cm.build_context("compressed".to_string(), None, 2, 50, Some(1));
    let lv2_hash = cm.queue.back().unwrap().hash_id; // = (1<<24)|2

    cm.layers.insert(lv2_hash, vec![cm.queue[0].clone()]);

    // Model expands target hash for LV1: expand_slot((1 << 24) | 1)
    cm.expand_slot((1 << 24) | 1);

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("[EXPANDED]"),
        "expanded content should show [EXPANDED] marker"
    );
    assert!(
        output.contains("detailed original content"),
        "original parent content should appear when expanded"
    );
    assert!(
        !output.contains("compressed"),
        "compressed content should be replaced by expanded in place"
    );
}

// Verifies that expanding one hash does not affect other entries.
// Only the entry whose exhibition slot matches gets expanded.
#[test]
fn expand_only_affects_matching_entry() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // Entry A (LV1 at ex=1)
    cm.build_context("content A original".to_string(), None, 1, 100, Some(1));
    cm.build_context("compressed A".to_string(), None, 2, 50, Some(1));
    let hash_a2 = (1 << 24) | 2; // LV2 hash

    // Entry B (LV1 at ex=2)
    cm.build_context("content B original".to_string(), None, 1, 100, Some(2));
    cm.build_context("compressed B".to_string(), None, 2, 50, Some(2));
    let hash_b2 = (2 << 24) | 2; // LV2 hash

    // Populate layers
    cm.layers.insert(hash_a2, vec![cm.queue[0].clone()]);
    cm.layers.insert(hash_b2, vec![cm.queue[2].clone()]);

    // Expand A's slot to LV1: target hash = (1<<24)|1
    cm.expand_slot((1 << 24) | 1);

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("content A original"),
        "entry marked for expansion should show its parent"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "expanded entry should show [EXPANDED] marker"
    );
    assert!(
        !output.contains("content B original"),
        "entry NOT marked for expansion should NOT show its parent"
    );
    assert!(
        output.contains("compressed B"),
        "unexpanded entry should show its compressed form"
    );
    assert!(
        !output.contains("compressed A"),
        "expanded entry should NOT show compressed form"
    );
}

// Verifies that expanding a hash for a non-existent exhibition slot
// produces no [EXPANDED] and does not crash.
#[test]
fn expand_nonexistent_hash_does_not_crash() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("hello".to_string(), None, 1, 5, Some(1));

    // Expand a hash for a slot that doesn't exist
    cm.expand_slot(999_999);

    // Must not panic
    let output = cm.format_context().unwrap();

    assert!(
        !output.contains("[EXPANDED]"),
        "no [EXPANDED] for non-existent hash"
    );
    assert!(output.contains("hello"));
}

// Verifies that expanded content displays the correct parent level.
#[test]
fn expanded_shows_correct_parent_level() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("original text".to_string(), None, 1, 100, Some(1));
    cm.build_context("compressed text".to_string(), None, 2, 50, Some(1));
    let lv2_hash = cm.queue.back().unwrap().hash_id; // = (1<<24)|2

    cm.layers.insert(lv2_hash, vec![cm.queue[0].clone()]);

    // Target LV1: ex=1, lv=1 → (1<<24)|1
    cm.expand_slot((1 << 24) | 1);

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("original text"),
        "expanded content should show original (parent) content"
    );
    assert!(
        !output.contains("compressed text"),
        "compressed content should be replaced by expanded in place"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "expanded entry should show [EXPANDED] marker"
    );
}
