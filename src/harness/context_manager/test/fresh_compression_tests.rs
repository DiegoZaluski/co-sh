//! Tests for fresh buffer compression (compress_fresh_up_to)

use crate::harness::context_manager::{ContextManager, Role};
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
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

// Correct behavior: the confirmation message must report the COMPRESSED
// output size, not the raw input size. The input here is ~16k tokens while
// the deterministic output is a fraction of that — the message must match
// the entry actually pushed to the queue.
#[test]
fn compress_fresh_up_to_reports_output_tokens() {
    let mut cm = ContextManager::new(test_connector(), 3);

    for _ in 0..4 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(4000)));
    }

    let msg = cm.compress_fresh_up_to(4).unwrap();
    let actual = cm.queue.back().map(|e| e.tokens).unwrap_or(0);
    let reported = msg
        .split('(')
        .nth(2)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    assert!(
        actual > 0,
        "precondition: compression should produce a non-empty queue entry"
    );
    assert_eq!(
        reported, actual,
        "BUG: message reported {reported} tokens but the compressed entry has {actual} tokens"
    );
}
