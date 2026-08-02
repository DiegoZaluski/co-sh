//! Regression tests for specific bugs

use crate::harness::context_manager::{ContextManager, Role};
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// ── BUG-2026-07-31-CTX: budget bar pins at 100% and the fresh buffer is
// never auto-compressed ──
//
// `display_info()` sums the RAW token count of every `buffer_chunks` entry
// against MAX_CONTEXT_TOKENS (10_000). The buffer is only drained inside
// `run()` once it exceeds the budget; before the fix, `run()` only rotated
// the circular `queue`, which stayed EMPTY in normal sessions. The bar
// therefore hit 100% within a few tool results and never moved.

// Correct behavior: `run()` — the per-iteration context tick the agent loop
// calls after every dispatch — must compress the fresh buffer once it exceeds
// the budget, so `budget_pct` drops back below 100.
#[tokio::test]
async fn run_does_not_lower_budget_pct_when_buffer_over_budget() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // ~4 chunks × 4k tokens = 16k raw tokens — well above MAX_CONTEXT_TOKENS.
    for _ in 0..4 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(4000)));
    }
    let info = cm.display_info();
    assert_eq!(
        info.budget_pct, 100,
        "precondition: buffer alone exceeds the budget"
    );

    // This is exactly what run_agent_loop does after each tool dispatch:
    // `self.context_manager.run().await`.
    cm.run().await;

    let after = cm.display_info();
    assert!(
        after.budget_pct < 100,
        "BUG: budget_pct stayed at {}% after run() with a {}-token fresh buffer — the buffer is never auto-compressed, so the bar is stuck at 100%",
        after.budget_pct,
        after.total_tokens
    );
}

// Correct behavior: the per-iteration tick must drain the over-budget fresh
// buffer into the compressed queue (the design intent of `run()`).
#[tokio::test]
async fn run_never_drains_fresh_buffer() {
    let mut cm = ContextManager::new(test_connector(), 3);

    for _ in 0..4 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(4000)));
    }
    let chunks_before = cm.buffer_chunks.len();
    assert_eq!(chunks_before, 4);

    cm.run().await;

    assert!(
        cm.buffer_chunks.len() < chunks_before,
        "BUG: run() left all {} fresh buffer chunks in place (over budget) — compress_oldest only rotates the empty queue, so the raw buffer is never compressed",
        chunks_before
    );
}

// Correct behavior: `compress_oldest` must never drop the entry it pops.
// Compression is infallible — `compress_if_needed` always falls back to the
// deterministic pipeline when the LLM call fails — so the popped entry always
// survives as either a compressed re-queue or a `fixed_contexts` entry. This
// test guards against a regression that reintroduces a failure path which
// returns early after `pop_front` (silent context loss).
//
// Isolation note: this test drives `compress_oldest` DIRECTLY instead of via
// `run()`. `run()` drains the over-budget fresh buffer first, which would
// leave `compress_oldest` unreachable and let the test pass without
// exercising the compression path.
//
// Environment note: the real compression path
// (`compress_via_llm` → `connector.chat_with_system`) is used. When ollama is
// unavailable it fails fast with `MissingApiKey` (no network) and the entry is
// compressed deterministically; when it succeeds the entry is compressed via
// the LLM. Either way the entry survives, so the assertion holds on both
// outcomes.
#[tokio::test]
async fn run_does_not_drop_queue_entry_when_compression_fails() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // A queue entry above SEED_MAX_TOKENS so compression is actually attempted.
    cm.build_context(
        "queued summary content that must survive a failed compression attempt".to_string(),
        None,
        1,
        500,
        Some(1),
    );
    let before = cm.queue.len() + cm.fixed_contexts.len();
    assert_eq!(before, 1, "precondition: one queue entry exists");

    // Drive compress_oldest directly — isolated from run()'s buffer drain.
    cm.compress_oldest().await;

    let after = cm.queue.len() + cm.fixed_contexts.len();
    assert!(
        after >= before,
        "BUG: compress_oldest dropped the oldest queue entry when compression failed (entries {} → {}) — context is silently lost",
        before,
        after
    );
}

// Correct behavior: `run()` must only rotate the queue when the buffer drain
// alone could not bring the context back under budget. Rotating an
// already-compressed queue entry wastes an LLM/deterministic pass and
// degrades summaries unnecessarily.
#[tokio::test]
async fn run_does_not_rotate_queue_when_drain_suffices() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // Over-budget fresh buffer (~16k tokens); the deterministic drain
    // compresses it to ~6.4k tokens, which alone fits the budget.
    for _ in 0..4 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(4000)));
    }
    // A queue entry above SEED_MAX_TOKENS that must survive the tick.
    cm.build_context(
        "queued summary content that must not be re-compressed".to_string(),
        None,
        1,
        500,
        Some(2),
    );
    let entry_hash = (2 << 24) | 1;
    assert!(
        cm.display_info().budget_pct == 100,
        "precondition: buffer alone exceeds the budget"
    );

    cm.run().await;

    let preserved = cm.queue.iter().any(|e| e.hash_id == entry_hash)
        || cm.fixed_contexts.iter().any(|e| e.hash_id == entry_hash);
    assert!(
        preserved,
        "BUG: run() re-compressed a queue entry even though the buffer drain already brought the context under budget"
    );
    assert!(
        cm.display_info().budget_pct < 100,
        "BUG: run() left the context over budget"
    );
}

// Correct behavior: `run()` must surface a residual over-budget state via the
// `warning` flag (plus a log line) when one rotation per tick is not enough to
// fall under the budget, instead of silently returning an over-budget context.
#[tokio::test]
async fn run_sets_warning_when_still_over_budget() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // Two drains leave two large queue entries; one rotation per tick cannot
    // bring the total under budget in a single run().
    for _ in 0..5 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(6000)));
    }
    cm.run().await;
    for _ in 0..5 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(6000)));
    }
    cm.run().await;

    let info = cm.display_info();
    assert!(
        info.total_tokens > info.max_budget,
        "precondition: residual over-budget after one rotation per tick (total={})",
        info.total_tokens
    );
    assert!(
        cm.warning,
        "BUG: run() did not flag the residual over-budget context (total={})",
        info.total_tokens
    );
}

// Correct behavior: once back under budget, the warning flag clears.
#[tokio::test]
async fn run_clears_warning_when_under_budget() {
    let mut cm = ContextManager::new(test_connector(), 3);
    for _ in 0..4 {
        cm.add_buffer_context(Role::assistant(&"word ".repeat(4000)));
    }
    cm.run().await;

    let info = cm.display_info();
    assert!(
        info.total_tokens <= info.max_budget,
        "precondition: drain alone fits the budget (total={})",
        info.total_tokens
    );
    assert!(
        !cm.warning,
        "BUG: run() flagged an under-budget context as over budget"
    );
}
