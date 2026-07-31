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
// against MAX_CONTEXT_TOKENS (10_000). The buffer is only drained by
// `compress_fresh_up_to` (the model-invoked `force_compress` tool); `run()`
// only rotates the circular `queue`, which stays EMPTY in normal sessions.
// The bar therefore hits 100% within a few tool results and never moves.

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
    assert_eq!(info.budget_pct, 100, "precondition: buffer alone exceeds the budget");

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

// Correct behavior: when `compress_oldest` pops the oldest queue entry and its
// LLM compression fails, the entry must NOT be lost — it must be re-queued or
// moved to fixed_contexts. Today the failure path returns early after
// `pop_front`, silently dropping the entry (context loss without any signal).
//
// Isolation note: this test drives `compress_oldest` DIRECTLY instead of via
// `run()`. `run()` drains the over-budget fresh buffer first, which would
// leave `compress_oldest` unreachable and let the test pass without
// exercising the compression-failure path (masking the bug).
//
// Environment note: the real compression path
// (`compress_via_llm` → `connector.chat_with_system`) is used. It reproduces
// the bug whenever compression fails — e.g. `OLLAMA_API_KEY` unset, which
// fails fast with `MissingApiKey` (no network). On a machine where ollama is
// running, compression may succeed and the entry is legitimately re-queued or
// fixed, so the assertion holds and the test passes silently (false negative,
// not false positive). After the fix, the entry is restored on BOTH outcomes.
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
    cm.compress_oldest(None).await;

    let after = cm.queue.len() + cm.fixed_contexts.len();
    assert!(
        after >= before,
        "BUG: compress_oldest dropped the oldest queue entry when compression failed (entries {} → {}) — context is silently lost",
        before, after
    );
}

// Correct behavior: the deterministic compression pipeline must stay bounded
// for huge inputs. The O(n²) SVD/MMR cost is capped by batching — a giant
// buffer (e.g. `force_compress` on a long session) must not stall the agent
// loop. Exercises the public `compress_fresh_up_to` path end-to-end.
//
// Uses genuinely DIVERSE content — every sentence is distinct (unique index
// plus rotated word order), so the TF-IDF vocabulary stays healthy and the
// MMR selection is real. A repeated paragraph (or repeated word) would have
// its common bigrams filtered by max_df, collapsing the output and passing
// any assertion trivially. Sized just above the batching threshold so the
// batched path runs while the ~40% MMR selection still fits the budget.
#[test]
fn compress_fresh_up_to_huge_buffer_stays_bounded() {
    let mut cm = ContextManager::new(test_connector(), 3);

    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
        "india", "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
        "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey",
        "xray", "yankee", "zulu", "amber", "beacon", "cypress", "dune", "ember",
        "falcon", "granite", "harbor", "ivory", "juniper", "kestrel", "lagoon",
        "mosaic", "nimbus",
    ];
    let mut text = String::new();
    for i in 0..700 {
        text.push_str(&format!("Record {i} mentions "));
        let base = (i * 7) % words.len();
        for k in 0..12 {
            text.push_str(words[(base + k * 3) % words.len()]);
            text.push(' ');
        }
        text.push_str("at coordinates.");
        text.push(' ');
    }
    let input_tokens = crate::util::token_counter::estimate_tokens(&text);
    assert!(
        input_tokens > 10_000,
        "precondition: input must exceed the batching threshold (got {input_tokens})"
    );

    // Split into several fresh chunks so the buffer itself is multi-entry.
    let third = text.len() / 3;
    cm.add_buffer_context(Role::assistant(&text[..third]));
    cm.add_buffer_context(Role::assistant(&text[third..2 * third]));
    cm.add_buffer_context(Role::assistant(&text[2 * third..]));
    assert_eq!(cm.buffer_chunks.len(), 3);

    // Must complete promptly (bounded SVD/MMR via batching) and produce a
    // compressed entry, not a stall or a panic.
    let last = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last);

    assert!(result.is_ok(), "huge buffer must compress, got: {:?}", result);
    assert!(
        cm.buffer_chunks.is_empty(),
        "all fresh chunks must be consumed"
    );
    assert_eq!(cm.queue.len(), 1, "exactly one compressed entry created");
    assert!(
        !cm.queue[0].assistant.is_empty(),
        "compression of diverse content must produce a non-empty selection"
    );
    assert!(
        cm.queue[0].tokens <= input_tokens,
        "compression must never grow the content (input={input_tokens}, out={})",
        cm.queue[0].tokens
    );
    assert!(
        cm.queue[0].tokens <= 10_000,
        "compressed entry must respect the budget (tokens={})",
        cm.queue[0].tokens
    );
}