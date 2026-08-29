//! Tests for the single-owner LLM-free SYNCHRONOUS context manager.
//!
//! Coverage: ingestion (user/assistant/tool call/tool result), `Closure`
//! promotion + guard, the native `tool_call → tool` structural chain, the
//! useless tool-chain sweep, the 80% budget trigger, the LLM compaction
//! fallback (request + apply), message rendering (positions + roles),
//! save/restore round-trip, display info, manual `/compact`, context-window
//! discovery, and the split-and-concatenate contingency.

use super::*;
use cosh_sdk::connector::effective_context_window;

fn cm(max_tokens: usize) -> ContextManager {
    ContextManager::new(max_tokens)
}

/// ~16-word varied prose sentence with a unique index; `n` copies give a
/// text of roughly `n` × 85 chars.
fn prose_copies(n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "Paragraph {i} discusses the interplay between rivers, mountains \
                 and the changing weather patterns that define the region. "
            )
        })
        .collect()
}

// ── Ingestion + rendering ─────────────────────────────────────────────────

#[test]
fn user_prompts_keep_the_native_user_role() {
    let mut cm = cm(10_000);
    cm.add_user("hello there");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content.as_deref(), Some("hello there"));
}

#[test]
fn assistant_text_renders_as_assistant() {
    let mut cm = cm(10_000);
    cm.add_user("hi");
    cm.add_assistant("hello back", true);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content.as_deref(), Some("hello back"));
}

#[test]
fn build_messages_appends_current_input_unless_duplicate() {
    let mut cm = cm(10_000);
    cm.add_user("first");
    let msgs = cm.build_messages("continue");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].content.as_deref(), Some("continue"));

    // A duplicate trailing user turn is not re-appended.
    cm.add_user("dupe");
    let msgs = cm.build_messages("dupe");
    assert_eq!(msgs.len(), 2, "the duplicate steering input is skipped");
    assert_eq!(msgs[1].content.as_deref(), Some("dupe"));
}

#[test]
fn tool_call_and_result_render_native_chain() {
    let mut cm = cm(10_000);
    cm.add_user("run it");
    cm.add_tool_call("call-1", "read", r#"{"path":"a"}"#);
    cm.add_tool_result("call-1", "content");
    let msgs = cm.build_messages("");
    // user, tool call, tool result
    assert_eq!(msgs.len(), 3);
    assert!(msgs[1].tool_calls.is_some(), "tool call renders natively");
    assert_eq!(msgs[2].role, "tool");
    assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call-1"));
}

#[test]
fn tool_call_thinking_blocks_round_trip_through_messages() {
    use cosh_sdk::connector::ClaudeThinkingBlock;
    let mut cm = cm(10_000);
    cm.add_user("go");
    let blocks = vec![ClaudeThinkingBlock {
        thinking: "thought".to_string(),
        signature: "sig".to_string(),
    }];
    cm.add_tool_call_with_thinking("call-1", "read", "{}", "", blocks.clone());
    let msgs = cm.build_messages("");
    let msg = &msgs[1];
    assert_eq!(
        msg.thinking_blocks.as_ref().map(|b| b.len()),
        Some(1),
        "thinking blocks travel with the tool call"
    );
}

#[test]
fn leading_orphaned_tool_results_are_skipped_in_messages() {
    let mut cm = cm(10_000);
    cm.add_tool_result("orphan", "no call");
    cm.add_user("real");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1, "the orphaned tool result is skipped");
    assert_eq!(msgs[0].role, "user");
}

#[test]
fn close_loop_promotes_last_assistant_text() {
    let mut cm = cm(10_000);
    cm.add_user("task");
    cm.add_assistant("the final answer", true);
    cm.close_loop();
    let items = cm.items_snapshot();
    assert!(matches!(
        items.last(),
        Some(ContextItem::Closure { content, .. }) if content == "the final answer"
    ));
    assert_eq!(cm.final_answer().as_deref(), Some("the final answer"));
}

#[test]
fn close_loop_guard_skips_when_last_output_was_a_tool_call() {
    let mut cm = cm(10_000);
    cm.add_user("task");
    cm.add_assistant("narrative", false); // carried a tool call → not closable
    cm.close_loop();
    assert!(matches!(
        cm.items_snapshot().last(),
        Some(ContextItem::Assistant { .. })
    ));
    assert_eq!(cm.final_answer(), None);
}

#[test]
fn remove_abandoned_inputs_drops_the_earlier_consecutive_turn() {
    let mut cm = cm(10_000);
    cm.add_user("abandoned");
    cm.add_user("current");
    assert!(cm.remove_abandoned_inputs());
    let items = cm.items_snapshot();
    assert_eq!(items.len(), 1);
    assert!(matches!(
        &items[0],
        ContextItem::User { original, .. } if original == "current"
    ));
}

#[test]
fn remove_abandoned_inputs_keeps_a_turn_that_produced_output() {
    let mut cm = cm(10_000);
    cm.add_user("first");
    cm.add_assistant("answer", true);
    cm.add_user("second");
    assert!(!cm.remove_abandoned_inputs());
    assert_eq!(cm.items_snapshot().len(), 3);
}

// ── Useless tool-chain sweep ──────────────────────────────────────────────

#[test]
fn sweep_useless_chains_removes_dead_chains_but_keeps_the_newest() {
    let mut cm = cm(100_000);
    cm.add_user("task");
    cm.add_tool_call("dead", "find_glob", "{}");
    cm.add_tool_result_flagged("dead", "no matches", true);
    cm.add_tool_call("alive", "find_glob", "{}");
    cm.add_tool_result("alive", "3 matches");

    cm.sweep_useless_chains();
    let items = cm.items_snapshot();
    assert!(
        !items.iter().any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "dead")),
        "the useless chain is removed"
    );
    assert!(
        items.iter().any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "alive")),
        "the useful chain survives"
    );
}

#[test]
fn sweep_keeps_useful_chains_intact() {
    let mut cm = cm(100_000);
    cm.add_user("task");
    cm.add_tool_call("c1", "find_glob", "{}");
    cm.add_tool_result("c1", "matches");
    cm.sweep_useless_chains();
    assert_eq!(cm.items_snapshot().len(), 3, "nothing is removed");
}

// ── Budget trigger + LLM compaction ───────────────────────────────────────

#[test]
fn run_resolves_below_the_trigger_and_requests_llm_above() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user("short");
    assert_eq!(cm.run(), RunOutcome::Resolved);

    cm.add_user(&prose_copies(200)); // way over the trigger
    assert_eq!(cm.run(), RunOutcome::NeedsLlmCompaction);
}

#[test]
fn llm_compaction_request_serializes_the_whole_context() {
    let mut cm = cm(1000);
    cm.add_user("a small prompt");
    cm.add_assistant("an answer", true);
    cm.add_tool_call("c1", "read", "{}");
    cm.add_tool_result("c1", "file contents");
    cm.add_user(&prose_copies(200)); // over the trigger

    let request = cm.llm_compaction_request().expect("request exists");
    assert!(request.system.contains("summarizer"));
    assert!(request.prompt.contains("[User]: a small prompt"));
    assert!(request.prompt.contains("[Assistant tool call]: read"));
    assert!(request.prompt.contains("[Tool result]: file contents"));
}

#[test]
fn llm_compaction_request_uses_update_mode_with_previous_summary() {
    let mut cm = cm(1000);
    cm.add_user(&prose_copies(200));
    let ok = cm.apply_llm_summary("previous anchor".into());
    assert!(ok);
    cm.add_user(&prose_copies(200));

    let request = cm.llm_compaction_request().expect("request exists");
    assert!(
        request.prompt.contains("[Previous summary]: previous anchor"),
        "the previous summary stays in the timeline"
    );
    assert!(
        request.prompt.contains("Update the summary labeled [Previous summary]"),
        "update mode is selected"
    );
}

#[test]
fn llm_compaction_request_is_none_below_the_trigger_or_empty() {
    let mut cm = cm(100_000);
    assert!(cm.llm_compaction_request().is_none(), "empty timeline");
    cm.add_user("short");
    assert!(
        cm.llm_compaction_request().is_none(),
        "below the trigger → none"
    );
}

#[test]
fn apply_llm_summary_replaces_everything() {
    let mut cm = cm(10_000);
    cm.add_user("a");
    cm.add_assistant("b", true);
    let ok = cm.apply_llm_summary("## Objective\n- summarized".into());
    assert!(ok);
    let items = cm.items_snapshot();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ContextItem::Compaction { summary, .. } if summary == "## Objective\n- summarized"));
}

// ── Save / restore ────────────────────────────────────────────────────────

#[test]
fn save_restore_roundtrip_preserves_items_and_bookkeeping() {
    let mut cm = cm(10_000);
    cm.add_user("one");
    cm.add_assistant("two", true);
    cm.close_loop();
    cm.add_user("three");

    let state = cm.save_state();
    let mut restored = ContextManager::new(10_000);
    restored.restore_state(&state);

    assert_eq!(restored.items_snapshot().len(), 3);
    assert_eq!(restored.max_tokens(), 10_000);
    // The id counter continues past the restored items.
    restored.add_user("four");
    let items = restored.items_snapshot();
    let ids: Vec<u64> = items.iter().map(|it| it.id()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(ids.len(), sorted.len(), "ids stay unique after restore");
}

#[test]
fn overflow_stuck_state_lifecycle() {
    let mut cm = cm(10_000);
    assert!(!cm.overflow_stuck("provider-a"));
    cm.mark_overflow("provider-a");
    assert!(cm.overflow_stuck("provider-a"));
    assert!(!cm.overflow_stuck("provider-b"));
    cm.clear_overflow();
    assert!(!cm.overflow_stuck("provider-a"));
}

// ── Display info ──────────────────────────────────────────────────────────

#[test]
fn display_info_reports_tokens_and_budget() {
    let mut cm = cm(100_000);
    cm.add_user("hello world");
    let info = cm.display_info();
    assert!(info.total_tokens > 0);
    assert_eq!(info.max_tokens, 100_000);
    assert_eq!(info.budget_pct, 0, "far below the budget");
}

// ── Cached token total stays in sync ──────────────────────────────────────

#[test]
fn cached_token_total_stays_in_sync_across_all_mutations() {
    let check = |cm: &ContextManager| {
        let brute: usize = cm
            .items
            .iter()
            .map(|it| it.tokens(cm.encoding))
            .sum::<usize>()
            .saturating_add(cm.todo.tokens(cm.encoding));
        assert_eq!(
            cm.total_tokens(),
            brute,
            "cache drifted: cached={} brute={} (items={})",
            cm.total_tokens(),
            brute,
            cm.items.len()
        );
    };

    let mut cm = cm(10_000);
    check(&cm);

    cm.add_user(&"user ".repeat(50));
    check(&cm);
    cm.add_assistant(&"draft ".repeat(200), true);
    check(&cm);
    cm.add_tool_call("c1", "fs_edit", r#"{"path":"a"}"#);
    check(&cm);
    cm.add_tool_result_flagged("c1", &"result ".repeat(300), false);
    check(&cm);
    cm.add_tool_call_with_signature("c2", "find", "{}", "sig");
    check(&cm);
    cm.add_tool_result("c2", &"r2 ".repeat(100));
    check(&cm);

    cm.close_loop();
    check(&cm);

    cm.add_tool_call("dead", "grep", "{}");
    cm.add_tool_result_flagged("dead", &"junk ".repeat(50), true);
    check(&cm);
    cm.run();
    check(&cm);

    cm.add_user(&"abandoned ".repeat(20));
    cm.add_user(&"current ".repeat(20));
    check(&cm);
    assert!(cm.remove_abandoned_inputs());
    check(&cm);

    assert!(cm.apply_llm_summary("## Objective\n- done".to_string()));
    check(&cm);

    cm.set_model(Some("gpt-4o"));
    check(&cm);
    cm.set_model(Some("claude-sonnet-4-6"));
    check(&cm);

    let saved = cm.save_state();
    cm.add_user(&"extra ".repeat(10));
    check(&cm);
    cm.restore_state(&saved);
    check(&cm);
}

// ── Context window discovery ───────────────────────────────────────────────

#[tokio::test]
async fn with_discovered_context_uses_discovered_value() {
    let cm = ContextManager::with_discovered_context("openai/gpt-4o").await;
    assert_eq!(
        cm.display_info().max_tokens,
        effective_context_window(128_000),
        "Budget should be the effective context of the discovered 128k window"
    );
}

#[tokio::test]
async fn with_discovered_context_falls_back_to_default() {
    let cm = ContextManager::with_discovered_context("invalid-model-xyz-12345").await;
    assert_eq!(
        cm.display_info().max_tokens,
        MAX_CONTEXT_TOKENS,
        "Invalid model should fall back to default MAX_CONTEXT_TOKENS"
    );
}

#[tokio::test]
async fn with_discovered_context_works_with_claude_models() {
    let cm = ContextManager::with_discovered_context("claude-opus-5").await;
    assert_eq!(
        cm.display_info().max_tokens,
        effective_context_window(1_000_000),
        "Budget should be the effective context of the discovered 1M window"
    );
}

#[tokio::test]
async fn with_discovered_context_creates_valid_context_manager() {
    let mut cm = ContextManager::with_discovered_context("openai/gpt-4o").await;
    assert_eq!(cm.total_tokens(), 0, "New context manager should be empty");
    cm.add_user("test message");
    assert!(cm.total_tokens() > 0, "Should track tokens after adding content");
}

// ── Split-and-concatenate (the context-window contingency) ─────────────────

#[test]
fn split_chunks_whole_items_in_order_with_continuity_and_commits_atomically() {
    let mut cm = cm(10_000);
    // The first item alone vastly exceeds the chunk budget (window 10k →
    // budget 2k) — it must still be included whole as the first chunk.
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_assistant("short a", true); // id 2
    cm.add_assistant("short b", true); // id 3
    cm.close_loop(); // id 3 → Closure
    cm.add_user("short c"); // id 4

    cm.begin_split(10_000);
    let before = cm.items_snapshot().len();

    // Chunk 1 = the giant first item alone (whole), historical order.
    let first = cm.split_next_chunk().expect("chunk 1");
    assert!(first.prompt.contains("[Conversation chunk to summarize]"));
    assert!(first.prompt.contains("abcd efgh"), "the first item is included");
    assert!(first.prompt.contains("## Objective"), "the first chunk embeds the anchored template");
    assert!(!first.prompt.contains("[Continuation context"), "no continuity on the very first chunk");
    assert!(!first.prompt.contains("short a"), "the chunk boundary never splits an item: item 2 stays out");

    cm.advance_split("## Objective\n- the giant first part", first.chunk_end);
    assert!(!cm.split_all_consumed(), "more items remain");
    assert_eq!(cm.items_snapshot().len(), before, "the timeline is untouched while the split is in progress");

    // Chunk 2 = the remaining whole items + the continuity tail to glue on.
    let second = cm.split_next_chunk().expect("chunk 2");
    assert!(second.prompt.contains("[Continuation context"), "continuation chunks carry the tail of the previous summary");
    assert!(second.prompt.contains("the giant first part"));
    assert!(second.prompt.contains("short a") && second.prompt.contains("short c"));
    assert!(!second.prompt.contains("abcd efgh"), "summarized items are never re-sent");
    assert!(!second.prompt.contains("Output exactly the Markdown structure"), "continuation chunks do not re-embed the template instruction");
    assert!(!second.prompt.contains("## Work State"), "continuation chunks do not restart the template skeleton");

    cm.advance_split("### Active\n- the rest", second.chunk_end);
    assert!(cm.split_all_consumed(), "every item has been summarized");

    // The atomic commit replaces the WHOLE timeline with the buffer.
    assert!(cm.commit_split(), "the commit succeeds");
    let items = cm.items_snapshot();
    assert_eq!(items.len(), 1, "one Compaction anchor after the commit");
    let ContextItem::Compaction { summary, .. } = &items[0] else {
        panic!("expected a single Compaction anchor");
    };
    assert!(summary.contains("the giant first part"));
    assert!(summary.contains("the rest"));
    assert!(!cm.split_active(), "staging is cleared after the commit");
}

#[test]
fn split_warns_only_when_the_buffer_accumulation_is_concerning() {
    let mut cm = cm(10_000);
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_user("second prompt"); // id 2
    cm.begin_split(10_000); // ceiling = 4000, warn_at = 3200

    let projection = cm.split_projection();
    assert_eq!(projection.ceiling, 4000, "40% of the window is the ceiling");
    assert_eq!(projection.warn_at, 3200, "80% of the ceiling is the warning point");
    assert!(!projection.should_warn, "an empty buffer never warns");

    let first = cm.split_next_chunk().expect("chunk 1");
    assert!(!first.prompt.contains("TOKEN LIMIT"), "the model is left to act naturally while the buffer is small");
    // A bloated first summary crosses the threshold: the next chunk must ask
    // the summarizer to be terse, with a computed target.
    cm.advance_split(&"summary content ".repeat(3000), first.chunk_end);
    assert!(cm.split_projection().should_warn, "a bloated buffer triggers the limit message");
    let second = cm.split_next_chunk().expect("chunk 2");
    assert!(second.prompt.contains("TOKEN LIMIT"));
    assert!(second.prompt.contains("AT MOST ~"), "the limit carries a target");
    cm.advance_split("concise summary", second.chunk_end);
    assert!(cm.split_all_consumed());
}

#[test]
fn split_abort_leaves_the_timeline_untouched() {
    let mut cm = cm(10_000);
    cm.add_user("keep me");
    cm.add_assistant("draft", true);
    let before = cm.items_snapshot();

    cm.begin_split(10_000);
    let request = cm.split_next_chunk().expect("a chunk");
    cm.advance_split("partial summary", request.chunk_end);
    assert!(cm.split_active());

    cm.abort_split();
    assert!(!cm.split_active(), "staging is dropped");
    assert_eq!(cm.items_snapshot().len(), before.len(), "the timeline is untouched");
    assert!(
        cm.items_snapshot()
            .iter()
            .any(|it| matches!(it, ContextItem::User { original, .. } if original == "keep me"))
    );
}

#[test]
fn commit_split_requires_the_buffer_to_fit_the_window() {
    let mut cm = cm(10_000);
    cm.add_user("keep me verbatim");
    cm.begin_split(100); // tiny window: the hard limit is tiny too
    let request = cm.split_next_chunk().expect("a chunk");
    cm.advance_split(&"overshoot ".repeat(200), request.chunk_end);
    assert!(cm.split_all_consumed());

    assert!(!cm.commit_split(), "an anchor larger than the window must not commit");
    assert_eq!(cm.items_snapshot().len(), 1, "the timeline stays exactly as it was");
    assert!(
        cm.items_snapshot().iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } if original == "keep me verbatim"
        ))
    );
}

#[test]
fn commit_split_without_work_returns_false() {
    let mut cm = cm(10_000);
    cm.add_user("hi");
    assert!(!cm.commit_split(), "no active split → false");
    cm.begin_split(10_000);
    assert!(!cm.commit_split(), "an empty buffer must not wipe the timeline");
    assert_eq!(cm.items_snapshot().len(), 1, "nothing was committed");
}

#[test]
fn split_staging_survives_save_restore_and_resumes() {
    let mut cm = cm(10_000);
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_assistant(&"second part ".repeat(10_000), true); // id 2
    cm.add_assistant("third part", true); // id 3
    cm.begin_split(10_000);

    let first = cm.split_next_chunk().expect("chunk 1");
    cm.advance_split("summary of part one", first.chunk_end);
    assert!(!cm.split_all_consumed());

    // Persist mid-split and restore into a fresh manager.
    let state = cm.save_state();
    let mut restored = ContextManager::new(10_000);
    restored.restore_state(&state);

    assert!(restored.split_active(), "staging is restored");
    assert_eq!(
        restored.split_projection().buffer_tokens,
        cm.split_projection().buffer_tokens
    );
    let next = restored.split_next_chunk().expect("resumes where it stopped");
    assert!(!next.prompt.contains("abcd efgh"), "the summarized chunk is not re-sent after a restore");
    assert!(next.prompt.contains("second part"));
    assert!(next.prompt.contains("[Continuation context"));
    restored.advance_split("summary of part two", next.chunk_end);

    let last = restored.split_next_chunk().expect("the final chunk");
    assert!(last.prompt.contains("third part"));
    restored.advance_split("summary of part three", last.chunk_end);
    assert!(restored.split_all_consumed());
    assert!(restored.commit_split());
    let ContextItem::Compaction { summary, .. } = &restored.items_snapshot()[0] else {
        panic!("expected the committed anchor");
    };
    assert!(summary.contains("part one") && summary.contains("part three"));
}

// ── Manual /compact (trigger override) ────────────────────────────────────

#[test]
fn manual_compaction_requests_llm_below_the_normal_trigger() {
    let mut cm = cm(100_000); // normal trigger = 80k
    cm.add_user("explore the repo");
    cm.add_assistant(&prose_copies(10), true);
    assert!(cm.total_tokens() < cm.trigger(), "precondition: below the normal trigger");
    assert_eq!(cm.run(), RunOutcome::Resolved);

    cm.begin_manual_compaction();
    assert!(matches!(cm.run(), RunOutcome::NeedsLlmCompaction));
    assert!(cm.llm_compaction_request().is_some(), "the summarizer request must exist");
    let ok = cm.apply_llm_summary("## Objective\n- compacted".into());
    cm.end_manual_compaction();
    assert!(ok);
    assert_eq!(cm.items.len(), 1, "the timeline folds into one anchor");
}

#[test]
fn has_compactable_content_rejects_a_lone_previous_summary() {
    let mut cm = cm(1000);
    assert!(!cm.has_compactable_content(), "empty timeline");
    let _ = cm.apply_llm_summary("previous anchor".into());
    assert!(!cm.has_compactable_content(), "a lone previous summary is not compactable content");
}
