//! Tests for ContextManager initialization, construction, and state management

use crate::harness::context_manager::{Context, ContextManager, ContextManagerState};
use cosh_sdk::connector::Connector;
use std::collections::{HashMap, VecDeque};

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

fn sample_context(hash_id: u64, lv: u64, text: &str) -> Context {
    Context {
        hash_id,
        lv,
        user: String::new(),
        assistant: text.to_string(),
        tokens: crate::util::token_counter::estimate_tokens(text),
        exhibition: 1,
    }
}

// Verifies that ContextManager::new() initialises everything to empty/zero state.
#[test]
fn new_context_manager_is_empty() {
    let cm = ContextManager::new(test_connector(), 3);
    assert!(cm.queue.is_empty(), "queue should start empty");
    assert!(
        cm.fixed_contexts.is_empty(),
        "fixed_contexts should start empty"
    );
    assert!(!cm.warning, "warning should start false");
    assert_eq!(cm.ctxt.hash_id, 0, "ctxt hash_id should be 0");
    assert_eq!(cm.ctxt.lv, 0, "ctxt lv should be 0");
    assert!(
        cm.ctxt.assistant.is_empty(),
        "ctxt assistant should be empty"
    );
    assert!(cm.ctxt.user.is_empty(), "ctxt user should be empty");
    assert_eq!(cm.ctxt.tokens, 0, "ctxt tokens should be 0");
}

// Verifies that build_context() pushes one entry onto the queue with the correct fields.
#[test]
fn build_context_lv1_pushes_to_queue() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("hello world".to_string(), None, 1, 2, Some(1));

    assert_eq!(cm.queue.len(), 1, "should have one entry");
    let entry = &cm.queue[0];
    assert_eq!(entry.lv, 1, "lv should be 1");
    assert_eq!(entry.assistant, "hello world");
    assert_eq!(entry.tokens, 2);
    assert_ne!(entry.hash_id, 0, "hash should be non-zero");
}

// Verifies that the same LV1 content always produces the same hash (determinism).
#[test]
fn build_context_lv1_hash_is_deterministic() {
    let mut cm1 = ContextManager::new(test_connector(), 3);
    let mut cm2 = ContextManager::new(test_connector(), 3);

    cm1.build_context("same content".to_string(), None, 1, 2, Some(1));
    cm2.build_context("same content".to_string(), None, 1, 2, Some(1));

    assert_eq!(
        cm1.queue[0].hash_id, cm2.queue[0].hash_id,
        "same LV1 content must produce identical hash",
    );
}

// Verifies that different exhibition numbers produce different hashes
// (same lv, different ex → different hash).
#[test]
fn build_context_different_exhibition_different_hash() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("any".to_string(), None, 1, 2, Some(1));
    cm.build_context("any".to_string(), None, 1, 2, Some(2));

    assert_ne!(
        cm.queue[0].hash_id, cm.queue[1].hash_id,
        "different exhibition slots must produce different hashes",
    );
}

// Verifies the bit-packed id formula: hash = (exhibition << 24) | lv
#[test]
fn build_context_hash_is_linear() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("x".to_string(), None, 1, 1, Some(1)); // ex=1, lv=1 → hash = (1<<24)|1
    cm.build_context("x".to_string(), None, 2, 1, Some(1)); // ex=1, lv=2 → hash = (1<<24)|2
    cm.build_context("x".to_string(), None, 3, 1, Some(1)); // ex=1, lv=3 → hash = (1<<24)|3
    cm.build_context("x".to_string(), None, 1, 1, Some(2)); // ex=2, lv=1 → hash = (2<<24)|1

    assert_eq!(
        cm.queue[0].hash_id,
        (1 << 24) | 1,
        "ex=1 lv=1 → hash=(1<<24)|1"
    );
    assert_eq!(
        cm.queue[1].hash_id,
        (1 << 24) | 2,
        "ex=1 lv=2 → hash=(1<<24)|2"
    );
    assert_eq!(
        cm.queue[2].hash_id,
        (1 << 24) | 3,
        "ex=1 lv=3 → hash=(1<<24)|3"
    );
    assert_eq!(
        cm.queue[3].hash_id,
        (2 << 24) | 1,
        "ex=2 lv=1 → hash=(2<<24)|1"
    );
}

// Verifies that build_context() can be called multiple times in sequence.
#[test]
fn build_context_can_be_called_multiple_times() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("first".to_string(), None, 1, 1, Some(1));
    cm.build_context("second".to_string(), None, 1, 1, Some(1));

    assert_eq!(cm.queue.len(), 2);
    assert_eq!(cm.queue[0].assistant, "first");
    assert_eq!(cm.queue[1].assistant, "second");
    // Same exhibition+lv → same hash (already verified deterministic)
    assert_eq!(cm.queue[0].hash_id, cm.queue[1].hash_id);
}

// Verifies that enqueue() preserves insertion order.
#[test]
fn enqueue_preserves_insertion_order() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.enqueue(sample_context(10, 1, "first"));
    cm.enqueue(sample_context(20, 2, "second"));

    assert_eq!(cm.queue.len(), 2);
    assert_eq!(cm.queue[0].hash_id, 10);
    assert_eq!(cm.queue[0].assistant, "first");
    assert_eq!(cm.queue[0].lv, 1);
    assert_eq!(cm.queue[1].hash_id, 20);
    assert_eq!(cm.queue[1].assistant, "second");
    assert_eq!(cm.queue[1].lv, 2);
}

// Verifies interleaving build_context() and enqueue().
#[test]
fn enqueue_mixed_with_build_context() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("built".to_string(), None, 1, 5, Some(1));
    cm.enqueue(sample_context(99, 2, "enqueued"));

    assert_eq!(cm.queue.len(), 2);
    assert_eq!(cm.queue[0].assistant, "built");
    assert_eq!(cm.queue[0].lv, 1);
    assert_eq!(cm.queue[1].assistant, "enqueued");
    assert_eq!(cm.queue[1].lv, 2);
}

// Verifies that restore_state restores both queue and fixed_contexts.
#[test]
fn restore_restores_queue_and_fixed() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("live".to_string(), None, 1, 1, Some(1));
    cm.enqueue(sample_context(1, 2, "extra"));

    cm.fixed_contexts.push(sample_context(100, 3, "seed"));

    let state = cm.save_state();

    let mut restored = ContextManager::new(test_connector(), 3);
    restored.restore_state(&state);

    assert_eq!(restored.queue.len(), 2, "queue should have 2 entries");
    assert_eq!(restored.queue[0].assistant, "live");
    assert_eq!(restored.queue[1].hash_id, 1);

    assert_eq!(
        restored.fixed_contexts.len(),
        1,
        "fixed should have 1 entry"
    );
    assert_eq!(restored.fixed_contexts[0].hash_id, 100);
    assert_eq!(restored.fixed_contexts[0].assistant, "seed");
}

// Verifies restore_state with empty lists.
#[test]
fn restore_empty_state() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.restore_state(&ContextManagerState {
        queue: VecDeque::new(),
        fixed_contexts: Vec::new(),
        layers: HashMap::new(),
        buffer_chunks: Vec::new(),
        checkpoint_counter: 0,
        next_slot_id: 0,
    });

    assert!(cm.queue.is_empty());
    assert!(cm.fixed_contexts.is_empty());
}

// Verifies that the `layers` parent-content map survives a save/restore
// roundtrip. Compressed parents are popped from the queue before being stored
// in `layers`, so they cannot be reconstructed from queue relationships after
// restore — the map must be persisted as-is.
#[test]
fn restore_preserves_layers() {
    let mut cm = ContextManager::new(test_connector(), 3);

    let parent = sample_context((1 << 24) | 1, 1, "original content");
    let child = sample_context((1 << 24) | 2, 2, "summary");
    cm.queue.push_back(child.clone());
    cm.layers.insert(child.hash_id, vec![parent.clone()]);

    let state = cm.save_state();
    let mut restored = ContextManager::new(test_connector(), 3);
    restored.restore_state(&state);

    assert_eq!(
        restored.layers.len(),
        1,
        "layers should be preserved across save/restore"
    );
    let parents = restored
        .layers
        .get(&child.hash_id)
        .expect("child hash should map to its parent");
    assert_eq!(parents[0].assistant, "original content");
    assert_eq!(parents[0].hash_id, parent.hash_id);
}
