use super::*;
use cosh_sdk::connector::Connector;
use std::collections::VecDeque;

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

// Verifies that Context struct fields are accessible and store values correctly.
#[test]
fn context_struct_fields() {
    let ctx = Context {
        hash_id: 42,
        lv: 1,
        user: String::new(),
        assistant: "hello".to_string(),
        tokens: 5,
        exhibition: 1,
    };
    assert_eq!(ctx.hash_id, 42);
    assert_eq!(ctx.lv, 1);
    assert_eq!(ctx.assistant, "hello");
    assert_eq!(ctx.tokens, 5);
}

// Verifies that Role struct works correctly.
#[test]
fn role_struct_works() {
    let r = Role::user("user text");
    assert_eq!(r.text(), "user text");
    assert_eq!(r.label(), "user");
    assert!(r.assistant.is_none());
    assert!(r.user.is_some());

    let r = Role::assistant("assistant text");
    assert_eq!(r.text(), "assistant text");
    assert_eq!(r.label(), "assistant");
    assert!(r.assistant.is_some());
    assert!(r.user.is_none());

    let r = Role {
        assistant: None,
        user: None,
    };
    assert_eq!(r.text(), "");
    assert_eq!(r.label(), "");
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

    // Model navigation arithmetic:
    // From (hash=(1<<24)|2, lv=2), target LV1: ((1<<24)|2) - 2 + 1 = (1<<24)|1 ✅
    assert_eq!(
        ((1 << 24) | 2) - 2 + 1,
        (1 << 24) | 1,
        "model can navigate: hash - lv + 1 = LV1 hash"
    );
    // From (hash=(1<<24)|3, lv=3), target LV1: ((1<<24)|3) - 3 + 1 = (1<<24)|1 ✅
    assert_eq!(
        ((1 << 24) | 3) - 3 + 1,
        (1 << 24) | 1,
        "model can navigate: hash - lv + 1 = LV1 hash"
    );
    // Target LV1 for ex=2: ((2<<24)|1) - 1 + 1 = (2<<24)|1 (lv=1, stays same)
    assert_eq!(
        ((2 << 24) | 1) - 1 + 1,
        (2 << 24) | 1,
        "LV1 hash minus lv=1 plus 1 is itself"
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
        buffer_chunks: Vec::new(),
        checkpoint_counter: 0,
        next_slot_id: 0,
    });

    assert!(cm.queue.is_empty());
    assert!(cm.fixed_contexts.is_empty());
}

// Verifies that format_context preserves exhibition order after simulated compression.
// When a context is "compressed" (rebuilt at the same exhibition slot), its visual
// position in the formatted output must not move — only the content/lv updates.
#[test]
fn format_context_preserves_exhibition_order() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("alpha".to_string(), None, 1, 2, Some(1));
    cm.build_context("beta".to_string(), None, 1, 3, Some(2));
    cm.build_context("gamma".to_string(), None, 1, 2, Some(3));

    // Simulate compression: re-build at exhibition slots 1 and 3
    cm.build_context("ALPHA".to_string(), Some(1), 2, 1, Some(1));
    cm.build_context("GAMMA".to_string(), Some(3), 2, 1, Some(3));

    let output = cm.format_context().unwrap();

    // Slot-1 content must appear before slot-2, which must appear before slot-3
    let p1 = output.find("ALPHA").unwrap();
    let p2 = output.find("beta").unwrap();
    let p3 = output.find("GAMMA").unwrap();
    assert!(p1 < p2, "exhibition slot 1 must appear before slot 2");
    assert!(p2 < p3, "exhibition slot 2 must appear before slot 3");

    // Compressed slots should hold the new content, not the old
    assert!(
        !output.contains("alpha"),
        "slot 1 should be 'ALPHA', not 'alpha'"
    );
    assert!(
        !output.contains("gamma"),
        "slot 3 should be 'GAMMA', not 'gamma'"
    );
}

// --- Expansion tests ---

// Verifies that expand_slot() stores a hash_id in the expand list.
#[test]
fn expand_slot_stores_hash() {
    let mut cm = ContextManager::new(test_connector(), 3);

    assert!(cm.expand.is_empty(), "expand list should start empty");

    cm.expand_slot(42);

    assert_eq!(cm.expand.len(), 1);
    assert_eq!(cm.expand[0], 42);
}

// Verifies that expand_slot() can store multiple hashes.
#[test]
fn expand_slot_multiple_hashes() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.expand_slot(100);
    cm.expand_slot(200);
    cm.expand_slot(300);

    assert_eq!(cm.expand.len(), 3);
    assert_eq!(cm.expand, vec![100, 200, 300]);
}

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

// Verifies that format_context handles an empty exhibitions array gracefully.
#[test]
fn format_context_empty_exhibitions_returns_some_empty() {
    let mut cm = ContextManager::new(test_connector(), 3);

    let output = cm.format_context();

    assert!(
        output.is_some(),
        "format_context should return Some even empty"
    );
    let text = output.unwrap();
    assert!(
        text.is_empty() || text.trim().is_empty(),
        "empty exhibitions should produce empty output"
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

// ── FreshContext compression (compress_fresh_up_to) ──

// Verifies that an empty buffer returns an error.
#[test]
fn compress_fresh_up_to_empty_buffer_returns_err() {
    let mut cm = ContextManager::new(test_connector(), 3);
    assert!(cm.buffer_chunks.is_empty());

    let result = cm.compress_fresh_up_to(1);
    assert!(result.is_err(), "empty buffer should return an error");
}

// Verifies that checkpoint 0 returns an error regardless of buffer content.
#[test]
fn compress_fresh_up_to_zero_checkpoint_returns_err() {
    let mut cm = ContextManager::new(test_connector(), 3);
    cm.add_buffer_context(Role::user("some content"));

    let result = cm.compress_fresh_up_to(0);
    assert!(result.is_err(), "checkpoint 0 should return an error");
    assert_eq!(cm.buffer_chunks.len(), 1, "buffer should be untouched");
}

// Verifies that compressing a single buffer chunk removes it and creates a Context.
#[test]
fn compress_fresh_up_to_single_chunk() {
    let mut cm = ContextManager::new(test_connector(), 3);
    cm.add_buffer_context(Role::user("Hello world. This is a test."));

    assert_eq!(cm.buffer_chunks.len(), 1);
    assert_eq!(cm.checkpoint_counter, 1);

    let result = cm.compress_fresh_up_to(1);

    assert!(result.is_ok(), "should compress the single chunk");
    assert!(
        result.as_ref().unwrap().contains("fresh chunk"),
        "should confirm compression: {:?}",
        result
    );

    // Buffer must be empty now
    assert!(
        cm.buffer_chunks.is_empty(),
        "buffer should be empty after compression"
    );

    // A Context entry must have been created (LV=1)
    assert_eq!(cm.queue.len(), 1, "queue should have one entry");
    assert_eq!(cm.queue[0].lv, 1, "compressed entry should be LV=1");

    // fresh must be reset
    assert_eq!(cm.fresh.checkpoints, 0, "fresh checkpoints should be 0");
    assert_eq!(cm.checkpoint_counter, 0, "global counter should be 0");
}

// Verifies that compressing N of M chunks removes N and re-indexes the rest.
#[test]
fn compress_fresh_up_to_partial_compression_reindexes() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.add_buffer_context(Role::assistant("first chunk")); // checkpoints=1
    cm.add_buffer_context(Role::assistant("second chunk")); // checkpoints=2
    cm.add_buffer_context(Role::assistant("third chunk")); // checkpoints=3
    cm.add_buffer_context(Role::assistant("fourth chunk")); // checkpoints=4

    assert_eq!(cm.buffer_chunks.len(), 4);

    // Compress up to checkpoint 2 → consumes chunks 1 and 2
    let result = cm.compress_fresh_up_to(2);

    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    assert!(
        result.as_ref().unwrap().contains("fresh chunk"),
        "expected 'fresh chunk' in: {:?}",
        result
    );

    // Buffer should have 2 remaining chunks
    assert_eq!(cm.buffer_chunks.len(), 2, "two chunks should remain");

    // Remaining chunks must be re-indexed: new checkpoint 1 = old chunk 3
    assert_eq!(
        cm.buffer_chunks[0].checkpoints, 1,
        "first remaining should be checkpoint 1"
    );
    assert_eq!(cm.buffer_chunks[0].role.text(), "third chunk");
    assert_eq!(
        cm.buffer_chunks[1].checkpoints, 2,
        "second remaining should be checkpoint 2"
    );
    assert_eq!(cm.buffer_chunks[1].role.text(), "fourth chunk");

    // fresh must reflect remaining chunks
    assert_eq!(
        cm.fresh.checkpoints, 2,
        "fresh checkpoints should be max remaining"
    );
    assert_eq!(
        cm.checkpoint_counter, 2,
        "global counter should match fresh"
    );

    // A single Context entry must exist (the compressed batch)
    assert_eq!(cm.queue.len(), 1, "queue should have one compressed entry");
    assert_eq!(cm.queue[0].lv, 1, "compressed entry should be LV=1");
}

// Verifies that compressing ALL chunks empties the buffer entirely.
#[test]
fn compress_fresh_up_to_all_chunks_empties_buffer() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.add_buffer_context(Role::user("alpha"));
    cm.add_buffer_context(Role::user("beta"));
    cm.add_buffer_context(Role::user("gamma"));

    // Compress up to checkpoint 3 (= the last one)
    let result = cm.compress_fresh_up_to(3);

    assert!(result.is_ok());
    assert!(
        cm.buffer_chunks.is_empty(),
        "buffer should be fully drained"
    );
    assert_eq!(cm.checkpoint_counter, 0, "counter should reset to 0");
    assert_eq!(cm.queue.len(), 1, "one compressed context entry created");
}

// Verifies that compressing with a checkpoint larger than any existing
// checkpoint still compresses everything (saturating semantics).
#[test]
fn compress_fresh_up_to_checkpoint_beyond_last_compresses_all() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.add_buffer_context(Role::user("chunk one")); // checkpoints=1
    cm.add_buffer_context(Role::user("chunk two")); // checkpoints=2

    let result = cm.compress_fresh_up_to(99);

    assert!(
        result.is_ok(),
        "should compress everything when checkpoint is past the end"
    );
    assert!(cm.buffer_chunks.is_empty(), "all chunks should be consumed");
    assert_eq!(cm.queue.len(), 1, "one context entry created");
}

// Verifies that compress_fresh_up_to can be called multiple times,
// progressively consuming the buffer.
#[test]
fn compress_fresh_up_to_progressive_compression() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.add_buffer_context(Role::assistant("batch A part 1"));
    cm.add_buffer_context(Role::assistant("batch A part 2"));
    cm.add_buffer_context(Role::assistant("batch A part 3"));

    // First call: compress up to 2
    let _ = cm.compress_fresh_up_to(2);
    assert_eq!(cm.buffer_chunks.len(), 1, "one chunk should remain");
    assert_eq!(cm.queue.len(), 1, "first context entry created");

    // Add more data
    cm.add_buffer_context(Role::assistant("batch B part 1"));
    cm.add_buffer_context(Role::assistant("batch B part 2"));

    assert_eq!(cm.buffer_chunks.len(), 3, "should have 3 chunks now");

    // Second call: compress up to 1
    let _ = cm.compress_fresh_up_to(1);
    assert_eq!(
        cm.buffer_chunks.len(),
        2,
        "two chunks should remain after second compression"
    );
    assert_eq!(cm.queue.len(), 2, "second context entry created");

    // Remaining chunks should be re-indexed from 1
    assert_eq!(cm.buffer_chunks[0].checkpoints, 1);
    assert!(cm.buffer_chunks[0].role.text().contains("batch B"));
    assert_eq!(cm.buffer_chunks[1].checkpoints, 2);
}

// Verifies that format_context still works after compress_fresh_up_to,
// showing remaining buffer chunks correctly and no longer showing
// the compressed chunk in the buffer section.
#[test]
fn format_context_after_compress_fresh_up_to() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.add_buffer_context(Role::user("FIRST_CHUNK_SEGMENT")); // checkpoints=1
    cm.add_buffer_context(Role::user("SECOND_CHUNK_SEGMENT")); // checkpoints=2

    let _ = cm.compress_fresh_up_to(1);

    let output = cm.format_context().unwrap();

    // Remaining buffer must show the second chunk at checkpoint 1.
    assert!(
        output.contains("(checkpoint 1) user:\nSECOND_CHUNK_SEGMENT"),
        "remaining buffer chunk should appear in the buffer section"
    );

    // Isolate the buffer section: buffer content appears before the first
    // exhibition entry (which starts with "(" after \n\n).
    if let Some(buf_start) = output.find("(checkpoint") {
        let from_buffer = &output[buf_start..];
        // Buffer section ends at the newline before "(hash..."
        let buf_end = from_buffer.find("\n\n(").unwrap_or(from_buffer.len());
        let buf_section = &from_buffer[..buf_end];

        // The compressed chunk must NOT be in the buffer section anymore
        assert!(
            !buf_section.contains("FIRST_CHUNK_SEGMENT"),
            "compressed content should NOT appear in buffer section, got: {:?}",
            buf_section
        );
    }
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

// --- Retry configuration tests ---

// Verifies that the default max_retries is 3 as defined by DEFAULT_MAX_RETRIES.
#[test]
fn default_max_retries_is_three() {
    let cm = ContextManager::new(test_connector(), 3);
    assert_eq!(cm.max_retries(), 3, "default max_retries should be 3");
}

// Verifies that with_max_retries overrides the retry limit and persists.
#[test]
fn with_max_retries_changes_value() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.with_max_retries(5);
    assert_eq!(cm.max_retries(), 5, "should reflect the new value");

    cm.with_max_retries(0);
    assert_eq!(
        cm.max_retries(),
        0,
        "zero is a valid retry count (no retries)"
    );

    cm.with_max_retries(100);
    assert_eq!(cm.max_retries(), 100, "large values should work too");
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
