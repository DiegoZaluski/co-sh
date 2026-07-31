//! Tests for advanced expansion functionality (expand_to, multi-level expansion)

use crate::harness::context_manager::ContextManager;
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// Verifies model can expand directly to LV3 from LV5.
#[test]
fn expand_directly_to_mid_level() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("level 1 original".to_string(), None, 1, 100, Some(1));
    cm.build_context("level 2 summary".to_string(), None, 2, 80, Some(1));
    cm.build_context("level 3 summary".to_string(), None, 3, 60, Some(1));
    cm.build_context("level 4 summary".to_string(), None, 4, 40, Some(1));
    cm.build_context("level 5 seed".to_string(), None, 5, 20, Some(1));

    let hashes: Vec<u64> = cm.queue.iter().map(|c| c.hash_id).collect();
    // hashes: [(1<<24)|1, (1<<24)|2, (1<<24)|3, (1<<24)|4, (1<<24)|5] for ex=1, lv=1..5
    cm.layers.insert(hashes[4], vec![cm.queue[3].clone()]);
    cm.layers.insert(hashes[3], vec![cm.queue[2].clone()]);
    cm.layers.insert(hashes[2], vec![cm.queue[1].clone()]);
    cm.layers.insert(hashes[1], vec![cm.queue[0].clone()]);

    // Target LV3: (1<<24) | 3
    cm.expand_slot((1 << 24) | 3);

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("level 3 summary"),
        "should expand to level 3"
    );
    assert!(!output.contains("level 5 seed"), "should NOT show level 5");
    assert!(
        !output.contains("level 1 original"),
        "should NOT show level 1"
    );
    assert!(
        !output.contains("level 4 summary"),
        "should NOT show level 4"
    );
    assert!(
        !output.contains("level 2 summary"),
        "should NOT show level 2"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "should show [EXPANDED] marker"
    );
}

// Verifies that expand_to(hash, target_lv) resolves correctly — simulates
// what the model-facing tool would call: the visible hash + desired level.
#[test]
fn expand_to_resolves_target_level() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("original content".to_string(), None, 1, 100, Some(1));
    cm.build_context("compressed lv2".to_string(), None, 2, 80, Some(1));
    cm.build_context("compressed lv3".to_string(), None, 3, 60, Some(1));

    let hashes: Vec<u64> = cm.queue.iter().map(|c| c.hash_id).collect();
    // hashes: [(1<<24)|1, (1<<24)|2, (1<<24)|3] for ex=1, lv=1..3
    cm.layers.insert(hashes[2], vec![cm.queue[1].clone()]);
    cm.layers.insert(hashes[1], vec![cm.queue[0].clone()]);

    let visible_hash = hashes[2]; // (1<<24)|3, current LV3

    // Expand to LV1 via expand_to(visible_hash, 1) — zero arithmetic for model
    cm.expand_to(visible_hash, 1).unwrap();

    let output = cm.format_context().unwrap();
    assert!(
        output.contains("original content"),
        "expand_to LV1 should show original"
    );
    assert!(
        !output.contains("compressed lv2"),
        "should NOT show intermediate level"
    );
    assert!(
        !output.contains("compressed lv3"),
        "should NOT show current level"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "should show [EXPANDED] marker"
    );

    // Clear and expand to LV2
    cm.collapse_context();
    cm.expand_to(visible_hash, 2).unwrap();

    let output = cm.format_context().unwrap();
    assert!(
        output.contains("compressed lv2"),
        "expand_to LV2 should show lv2 content"
    );
    assert!(
        !output.contains("original content"),
        "should NOT show original"
    );
    assert!(
        !output.contains("compressed lv3"),
        "should NOT show current level"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "should show [EXPANDED] marker"
    );

    // Expand to current level (LV3) — no-op, no [EXPANDED]
    cm.collapse_context();
    cm.expand_to(visible_hash, 3).unwrap();

    let output = cm.format_context().unwrap();
    assert!(
        !output.contains("[EXPANDED]"),
        "target_lv == current lv should not show [EXPANDED]"
    );
    assert!(
        output.contains("compressed lv3"),
        "should show current level content"
    );
}

// Verifies that expand_to works across different exhibition slots.
#[test]
fn expand_to_works_across_slots() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // Slot 1: lv1 → lv2
    cm.build_context("slot1 original".to_string(), None, 1, 100, Some(1));
    cm.build_context("slot1 compressed".to_string(), None, 2, 50, Some(1));

    // Slot 2: lv1 → lv2 → lv3
    cm.build_context("slot2 original".to_string(), None, 1, 100, Some(2));
    cm.build_context("slot2 mid".to_string(), None, 2, 60, Some(2));
    cm.build_context("slot2 seed".to_string(), None, 3, 20, Some(2));

    let h1: Vec<u64> = cm
        .queue
        .iter()
        .filter(|c| c.exhibition == 1)
        .map(|c| c.hash_id)
        .collect();
    let h2: Vec<u64> = cm
        .queue
        .iter()
        .filter(|c| c.exhibition == 2)
        .map(|c| c.hash_id)
        .collect();

    cm.layers.insert(h1[1], vec![cm.queue[0].clone()]);
    cm.layers.insert(h2[2], vec![cm.queue[3].clone()]);
    cm.layers.insert(h2[1], vec![cm.queue[2].clone()]);

    // Expand slot 1 to LV1 using its LV2 hash
    cm.expand_to(h1[1], 1).unwrap();
    // Expand slot 2 to LV1 using its LV3 hash
    cm.expand_to(h2[2], 1).unwrap();

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("slot1 original"),
        "slot1 should show original"
    );
    assert!(
        output.contains("slot2 original"),
        "slot2 should show original"
    );
    assert!(
        !output.contains("slot1 compressed"),
        "slot1 should NOT show compressed"
    );
    assert!(!output.contains("slot2 mid"), "slot2 should NOT show mid");
    assert!(!output.contains("slot2 seed"), "slot2 should NOT show seed");
}