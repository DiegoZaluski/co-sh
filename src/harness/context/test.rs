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
    // Append-only: the abandoned turn stays in the timeline, hidden.
    assert_eq!(cm.items_snapshot().len(), 2);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1, "only the current input reaches the model");
    assert_eq!(msgs[0].content.as_deref(), Some("current"));
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

#[test]
fn remove_abandoned_inputs_drops_a_partial_runs_orphan_tool_call() {
    // A run cancelled mid-execution: the tool result never arrived. The new
    // input follows the dangling call, so the orphan is removed and the older
    // prompt is then recognized as abandoned too.
    let mut cm = cm(10_000);
    cm.add_user("abandoned");
    cm.add_tool_call("orphan", "fs_read", "{}");
    cm.add_user("current");
    assert!(cm.remove_abandoned_inputs());
    // Append-only: the abandoned turn + the orphan call stay, hidden.
    assert_eq!(cm.items_snapshot().len(), 3);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1, "only the newest input reaches the model");
    assert_eq!(msgs[0].content.as_deref(), Some("current"));
}

#[test]
fn remove_abandoned_inputs_drops_parallel_orphan_calls() {
    let mut cm = cm(10_000);
    cm.add_user("abandoned");
    cm.add_tool_call("orphan-a", "fs_read", "{}");
    cm.add_tool_call("orphan-b", "find_grep", "{}");
    cm.add_user("current");
    assert!(cm.remove_abandoned_inputs());
    // Append-only: 5 items stay (2 users + 2 orphan calls + ... the newest
    // user); the orphans and the abandoned turn are hidden from the model.
    assert_eq!(cm.items_snapshot().len(), 4);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content.as_deref(), Some("current"));
}

#[test]
fn remove_abandoned_inputs_drops_the_orphan_in_a_mixed_parallel_run() {
    // Cancellation with SOME results already arrived: TC(y) completed, TC(x)
    // never got its result. The orphan must go while the completed chain
    // (and the output it represents) stays whole.
    let mut cm = cm(10_000);
    cm.add_user("abandoned");
    cm.add_tool_call("orphan", "fs_read", "{}");
    cm.add_tool_call("done", "find_grep", "{}");
    cm.add_tool_result("done", "3 matches");
    cm.add_user("current");
    assert!(cm.remove_abandoned_inputs());
    // Append-only: everything stays in the timeline; the orphan call is
    // hidden, the completed chain and the prompt that produced it (a turn
    // WITH output is real work, never abandoned) stay visible.
    assert_eq!(cm.items_snapshot().len(), 5);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].content.as_deref(), Some("abandoned"));
    assert_eq!(msgs[1].role, "assistant", "the tool call of the done chain");
    assert_eq!(msgs[2].role, "tool");
    assert_eq!(msgs[2].tool_call_id.as_deref(), Some("done"));
    assert_eq!(msgs[3].content.as_deref(), Some("current"));
}

#[test]
fn remove_abandoned_inputs_keeps_completed_chains_and_pending_calls() {
    // A completed chain (call WITH result) followed by a new input is real
    // work — never removed…
    let mut cm = cm(10_000);
    cm.add_user("first");
    cm.add_tool_call("done", "fs_read", "{}");
    cm.add_tool_result("done", "contents");
    cm.add_user("second");
    assert!(!cm.remove_abandoned_inputs());
    assert_eq!(cm.items_snapshot().len(), 4);
}

#[test]
fn remove_abandoned_inputs_never_touches_a_pending_live_call() {
    // A call at the very END of the timeline (result still pending, no new
    // input yet) is a live run — never touched.
    let mut cm = cm(10_000);
    cm.add_user("live");
    cm.add_tool_call("pending", "fs_read", "{}");
    assert!(!cm.remove_abandoned_inputs());
    assert_eq!(cm.items_snapshot().len(), 2);
}

#[test]
fn a_fresh_input_after_a_compaction_with_nothing_produced_between_is_abandoned() {
    // [Compaction, User(a), User(b)]: a Compaction anchor holds the answer to
    // the work that came BEFORE it. A user turn AFTER the anchor with NO
    // output between it and the next input produced nothing (the run was
    // cancelled before the LLM answered) — the abandoned-input rule applies
    // and only the newest input survives.
    let mut cm = cm(10_000);
    cm.add_user("real task");
    let _ = cm.apply_llm_summary("## Objective\n- do the task".into());
    cm.add_user("abandoned retry");
    cm.add_user("next input");
    assert!(cm.remove_abandoned_inputs());
    // Append-only: all four items stay; the abandoned retry is HIDDEN.
    assert_eq!(cm.items_snapshot().len(), 4);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 2, "summary + newest input reach the model");
    assert_eq!(
        msgs[0].content.as_deref(),
        Some("## Objective\n- do the task")
    );
    assert_eq!(msgs[1].content.as_deref(), Some("next input"));
}

// ── Useless tool-chain sweep ──────────────────────────────────────────────

#[test]
fn correction_block_renders_at_the_tail_before_the_todo_block() {
    use cosh_tools::plan::types::{TodoItem, TodoList, TodoStatus};
    let mut cm = cm(10_000);
    cm.add_user("task");
    cm.set_correction_block("## Correction History\n\n- bad call".into());
    cm.todo.sync(TodoList {
        items: vec![TodoItem {
            id: "1".into(),
            description: "do it".into(),
            status: TodoStatus::Pending,
            depends_on: Vec::new(),
        }],
    });

    let msgs = cm.build_messages("");
    let last = msgs.last().unwrap();
    assert_eq!(
        last.role, "user",
        "blocks merge into the trailing user turn"
    );
    let content = last.content.as_deref().unwrap();
    let corrections = content
        .find("Correction History")
        .expect("corrections rendered");
    let todo = content.find("Tool TODOs").expect("todo block rendered");
    assert!(corrections < todo, "corrections come BEFORE the TODO block");
    // The steering prompt trails both blocks.
    assert!(content.ends_with("task"));
    // The block counts toward the budget (never free for the trigger).
    let mut bare = ContextManager::new(10_000);
    bare.add_user("task");
    assert!(
        cm.total_tokens() > bare.total_tokens(),
        "the correction block counts toward the budget"
    );
}

#[test]
fn an_empty_correction_block_renders_nothing() {
    let mut cm = cm(10_000);
    cm.add_user("task");
    cm.set_correction_block(String::new());
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1, "no correction block message");
    assert!(
        !msgs[0]
            .content
            .as_deref()
            .unwrap()
            .contains("Correction History")
    );
}

#[test]
fn sweep_useless_chains_hides_dead_chains_but_keeps_the_newest() {
    let mut cm = cm(100_000);
    cm.add_user("task");
    cm.add_tool_call("dead", "find_glob", "{}");
    cm.add_tool_result_flagged("dead", "no matches", true);
    cm.add_tool_call("alive", "find_glob", "{}");
    cm.add_tool_result("alive", "3 matches");

    cm.sweep_useless_chains();
    // Append-only: the dead chain STAYS in the timeline, hidden from the
    // model; the useful chain stays visible.
    assert_eq!(cm.items_snapshot().len(), 5);
    let msgs = cm.build_messages("");
    assert!(
        !msgs.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("no matches"))),
        "the dead chain's result never reaches the model"
    );
    assert!(
        msgs.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("3 matches"))),
        "the useful chain's result survives"
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

#[test]
fn model_requested_masking_replaces_the_payload_and_keeps_the_source_recoverable() {
    let mut cm = cm(1_000);
    cm.add_user("inspect both files");
    cm.add_tool_call("old", "fs_read", r#"{"path":"old.rs"}"#);
    let old_payload = prose_copies(120);
    cm.add_tool_result("old", &old_payload);
    cm.add_assistant("The old file establishes the baseline.", false);
    cm.add_tool_call("recent", "fs_read", r#"{"path":"recent.rs"}"#);
    cm.add_tool_result("recent", "fresh contents");

    // The model masks the two results one after another (newest first).
    assert_eq!(cm.mask_newest_tool_result(), Some(6));
    assert_eq!(cm.mask_newest_tool_result(), Some(3));
    assert_eq!(cm.mask_newest_tool_result(), None, "idempotent when done");
    let state = cm.save_state();
    assert_eq!(state.masked, HashSet::from([3, 6]));
    // Raw payloads stay in the immutable timeline (rehydration-ready).
    assert!(matches!(
        &state.items[2],
        ContextItem::ToolResult { content, .. } if content == &old_payload
    ));

    let messages = cm.build_messages("");
    assert!(messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("source context item #3"))
    }));
    assert!(messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("source context item #6"))
    }));
    assert!(
        !messages
            .iter()
            .any(|message| { message.content.as_deref() == Some(old_payload.as_str()) })
    );
    assert!(
        !messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("fresh contents") })
    );
}

#[test]
fn masking_only_touches_the_newest_visible_result_and_skips_hidden_ones() {
    let mut cm = cm(10_000);
    cm.add_user("task");
    // Hidden result: never a masking target.
    cm.add_tool_call("dead", "find_glob", "{}");
    cm.add_tool_result_flagged("dead", "no matches", true);
    // Newest result: the target.
    cm.add_tool_call("alive", "fs_read", "{}");
    cm.add_tool_result("alive", "big payload");
    cm.sweep_useless_chains();
    assert_eq!(cm.mask_newest_tool_result(), Some(5));
    assert_eq!(
        cm.mask_newest_tool_result(),
        None,
        "the hidden useless result is never a target"
    );
    let msgs = cm.build_messages("");
    assert!(
        !msgs.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("no matches"))),
        "the dead chain stays hidden"
    );
    assert!(
        !msgs.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("big payload"))),
        "the newest payload is masked"
    );
}

#[test]
fn masking_the_newest_useless_chain_lets_the_sweep_hide_it() {
    let mut cm = cm(10_000);
    cm.add_user("task");
    cm.add_tool_call("q", "find_grep", "{}");
    cm.add_tool_result_flagged("q", "no matches", true);
    cm.sweep_useless_chains();
    assert_eq!(
        cm.build_messages("").len(),
        3,
        "the newest useless chain is kept until the model reacts"
    );
    assert_eq!(cm.mask_newest_tool_result(), Some(3));
    // The explicit mask IS the reaction: the sweep may now hide even the
    // newest chain.
    cm.sweep_useless_chains();
    assert_eq!(
        cm.build_messages("").len(),
        1,
        "the masked newest chain is hidden by the sweep"
    );
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
        request
            .prompt
            .contains("[Previous summary]: previous anchor"),
        "the previous summary stays in the timeline"
    );
    assert!(
        request
            .prompt
            .contains("Update the summary labeled [Previous summary]"),
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
fn apply_llm_summary_hides_everything_behind_the_summary() {
    // Append-only: the summary is appended and the pre-existing timeline is
    // hidden behind the boundary — kept on disk for revert/fork, never shown
    // to the model.
    let mut cm = cm(10_000);
    cm.add_user("a");
    cm.add_assistant("b", true);
    let ok = cm.apply_llm_summary("## Objective\n- summarized".into());
    assert!(ok);
    let items = cm.items_snapshot();
    assert_eq!(items.len(), 3, "nothing is deleted");
    assert!(
        matches!(&items[2], ContextItem::Compaction { summary, .. } if summary == "## Objective\n- summarized")
    );
    // The model sees ONLY the summary.
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].content.as_deref(),
        Some("## Objective\n- summarized")
    );
    // The token budget reflects the visible-only view (the hidden items left
    // the total).
    let visible_only = cm.display_info().total_tokens;
    let brute_all: usize = items.iter().map(|it| it.tokens(cm.encoding)).sum();
    assert!(
        visible_only < brute_all,
        "hidden items must leave the budget ({visible_only} vs {brute_all})"
    );
}

#[test]
fn automatic_checkpoint_preserves_a_recent_raw_tail_and_records_exact_ranges() {
    let mut cm = cm(1_000);
    cm.add_user(&prose_copies(120)); // id 1: old, large source
    cm.add_assistant("old conclusion", true); // id 2: recent raw budget starts here
    cm.add_user("latest instruction"); // id 3
    cm.add_assistant("working on it", false); // id 4

    let request = cm.llm_compaction_request().expect("checkpoint is required");
    assert!(request.prompt.contains("Paragraph 0 discusses"));
    assert!(!request.prompt.contains("latest instruction"));
    assert!(cm.apply_llm_summary("## Objective\n- continue safely".into()));

    let items = cm.items_snapshot();
    let ContextItem::Compaction {
        covered_ranges,
        summary,
        ..
    } = items.last().expect("checkpoint appended")
    else {
        panic!("last item must be a checkpoint");
    };
    assert_eq!(summary, "## Objective\n- continue safely");
    assert_eq!(
        covered_ranges,
        &[ContextItemRange {
            start_id: 1,
            end_id: 1,
        }]
    );

    let messages = cm.build_messages("");
    assert_eq!(messages[0].content.as_deref(), Some(summary.as_str()));
    assert!(
        messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("latest instruction") })
    );
    assert!(
        messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("working on it") })
    );
}

#[test]
fn successive_checkpoints_carry_transitive_coverage_and_hide_old_anchors() {
    let mut cm = cm(1_000);
    cm.add_user(&prose_copies(120)); // id 1
    assert!(cm.apply_llm_summary("## Objective\n- first checkpoint".into())); // id 2
    cm.add_user(&prose_copies(120)); // id 3
    cm.add_assistant("recent raw tail", false); // id 4

    assert!(cm.apply_llm_summary("## Objective\n- merged checkpoint".into())); // id 5
    let items = cm.items_snapshot();
    let ContextItem::Compaction { covered_ranges, .. } = items.last().unwrap() else {
        panic!("expected merged checkpoint");
    };
    assert_eq!(
        covered_ranges,
        &[ContextItemRange {
            start_id: 1,
            end_id: 3,
        }]
    );

    let messages = cm.build_messages("");
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content.as_deref(),
        Some("## Objective\n- merged checkpoint")
    );
    assert_eq!(messages[1].content.as_deref(), Some("recent raw tail"));
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
    assert!(!cm.overflow_stuck("model-a"));
    cm.mark_overflow("model-a");
    assert!(cm.overflow_stuck("model-a"));
    assert!(!cm.overflow_stuck("model-b"));
    cm.clear_overflow();
    assert!(!cm.overflow_stuck("model-a"));
}

#[test]
fn overflow_stuck_is_keyed_by_model_not_provider() {
    // The stuck constraint is the model's WINDOW, not the provider's API: a
    // model switch inside the same provider must clear the stuck state.
    let mut cm = cm(10_000);
    cm.mark_overflow("gpt-4o");
    cm.sync_model("gpt-4o");
    assert!(cm.overflow_stuck("gpt-4o"), "same model → still stuck");
    cm.sync_model("gpt-4o-mini");
    assert!(
        !cm.overflow_stuck("gpt-4o"),
        "a model switch clears the stuck state"
    );
    assert!(!cm.overflow_stuck("gpt-4o-mini"));
}

#[test]
fn apply_llm_summary_keeps_the_stuck_state_when_the_verdict_fails() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&prose_copies(200));
    cm.mark_overflow("gpt-4o");
    let before = cm.items_snapshot().len();
    // A degenerate summary that would land above the trigger reports failure,
    // keeps the stuck state, and leaves the committed model view unchanged.
    let ok = cm.apply_llm_summary(prose_copies(200));
    assert!(!ok);
    assert_eq!(cm.items_snapshot().len(), before);
    assert!(
        cm.overflow_stuck("gpt-4o"),
        "a failed compaction must not clear the stuck overflow"
    );
    // A second (small) summary succeeds and clears the stuck state.
    assert!(cm.apply_llm_summary("tiny".into()));
    assert!(!cm.overflow_stuck("gpt-4o"));
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
        // The cache tracks the VISIBLE total only — hidden items (append-only
        // history) left it.
        let brute: usize = cm
            .items
            .iter()
            .filter(|it| !cm.is_hidden(it))
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
    cm.add_tool_call_with_thinking("c2", "find", "{}", "sig", Vec::new());
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

// The window→budget mapping is exercised OFFLINE through the pure
// `from_window` constructor; the network path (`with_discovered_context`) is
// a thin delegation kept for production and covered by an ignored smoke test.

#[test]
fn from_window_resizes_a_real_discovered_window() {
    let cm = ContextManager::from_window(Some(128_000));
    assert_eq!(
        cm.display_info().max_tokens,
        effective_context_window(128_000),
        "Budget should be the effective context of the discovered 128k window"
    );
}

#[test]
fn from_window_falls_back_to_the_default_without_discovery() {
    let cm = ContextManager::from_window(None);
    assert_eq!(
        cm.display_info().max_tokens,
        MAX_CONTEXT_TOKENS,
        "No discovery → the default MAX_CONTEXT_TOKENS"
    );
}

#[test]
fn from_window_creates_a_valid_context_manager() {
    let mut cm = ContextManager::from_window(Some(128_000));
    assert_eq!(cm.total_tokens(), 0, "New context manager should be empty");
    cm.add_user("test message");
    assert!(
        cm.total_tokens() > 0,
        "Should track tokens after adding content"
    );
}

/// Regression band for the 1M-class budget: a healthy 1M discovery must land
/// in 200k–209,715 (the 20% effective floor over the 1M–1,048,576 raw range).
/// A report that lands BELOW the band means a non-token number (e.g. a 413
/// body's byte payload) leaked into the window pipeline.
fn assert_one_million_budget(max_tokens: usize) {
    assert!(
        (200_000..=209_715).contains(&max_tokens),
        "1M model budget must be 200k–209,715, got {max_tokens}"
    );
}

#[test]
fn a_one_million_model_never_lands_below_the_200k_band() {
    assert_one_million_budget(
        ContextManager::from_window(Some(1_048_576))
            .display_info()
            .max_tokens,
    );
    assert_one_million_budget(
        ContextManager::from_window(Some(1_000_000))
            .display_info()
            .max_tokens,
    );
}

#[test]
fn provider_window_report_of_a_1m_model_lands_in_the_200k_band() {
    // The overflow-report path re-sizes the budget exactly like a discovery.
    // Persistence is skipped under cfg(test), so this never touches the
    // real error catalog.
    let mut c = cm(MAX_CONTEXT_TOKENS);
    c.record_provider_window(Some("gemini-3-pro"), 1_048_576);
    assert_one_million_budget(c.display_info().max_tokens);
}

#[tokio::test]
#[ignore = "hits the real discovery APIs (network) — run explicitly"]
async fn with_discovered_context_resolves_a_known_model() {
    let cm = ContextManager::with_discovered_context("openai/gpt-4o").await;
    assert_eq!(
        cm.display_info().max_tokens,
        effective_context_window(128_000)
    );
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
    assert!(
        first.prompt.contains("abcd efgh"),
        "the first item is included"
    );
    assert!(
        first.prompt.contains("## Objective"),
        "the first chunk embeds the anchored template"
    );
    assert!(
        !first.prompt.contains("[Continuation context"),
        "no continuity on the very first chunk"
    );
    assert!(
        !first.prompt.contains("short a"),
        "the chunk boundary never splits an item: item 2 stays out"
    );

    cm.advance_split("## Objective\n- the giant first part", first.chunk_end);
    assert!(!cm.split_all_consumed(), "more items remain");
    assert_eq!(
        cm.items_snapshot().len(),
        before,
        "the timeline is untouched while the split is in progress"
    );

    // Chunk 2 = the remaining whole items + the continuity tail to glue on.
    let second = cm.split_next_chunk().expect("chunk 2");
    assert!(
        second.prompt.contains("[Continuation context"),
        "continuation chunks carry the tail of the previous summary"
    );
    assert!(second.prompt.contains("the giant first part"));
    assert!(second.prompt.contains("short a") && second.prompt.contains("short c"));
    assert!(
        !second.prompt.contains("abcd efgh"),
        "summarized items are never re-sent"
    );
    assert!(
        !second
            .prompt
            .contains("Output exactly the Markdown structure"),
        "continuation chunks do not re-embed the template instruction"
    );
    assert!(
        !second.prompt.contains("## Work State"),
        "continuation chunks do not restart the template skeleton"
    );

    cm.advance_split("### Active\n- the rest", second.chunk_end);
    assert!(cm.split_all_consumed(), "every item has been summarized");

    // The atomic commit appends the buffer as ONE Compaction anchor and hides
    // the whole pre-existing timeline behind it (append-only — nothing is
    // deleted).
    assert!(cm.commit_split(), "the commit succeeds");
    let items = cm.items_snapshot();
    assert!(
        items.len() > 1,
        "the pre-split timeline is retained behind the anchor"
    );
    let ContextItem::Compaction { summary, .. } = &items[items.len() - 1] else {
        panic!("the anchor is appended as the newest item");
    };
    assert!(summary.contains("the giant first part"));
    assert!(summary.contains("the rest"));
    assert!(!cm.split_active(), "staging is cleared after the commit");
    // The model sees only the anchor.
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0]
            .content
            .as_deref()
            .is_some_and(|c| c.contains("the rest"))
    );
}

#[test]
fn split_warns_only_when_the_buffer_accumulation_is_concerning() {
    let mut cm = cm(10_000);
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_user("second prompt"); // id 2
    cm.begin_split(10_000); // ceiling = 4000, warn at 3200 (80% of the ceiling)

    let projection = cm.split_projection();
    assert_eq!(projection.ceiling, 4000, "40% of the window is the ceiling");
    assert!(!projection.should_warn, "an empty buffer never warns");

    let first = cm.split_next_chunk().expect("chunk 1");
    assert!(
        !first.prompt.contains("TOKEN LIMIT"),
        "the model is left to act naturally while the buffer is small"
    );
    // A bloated first summary crosses the threshold: the next chunk must ask
    // the summarizer to be terse, with a computed target.
    cm.advance_split(&"summary content ".repeat(3000), first.chunk_end);
    assert!(
        cm.split_projection().should_warn,
        "a bloated buffer triggers the limit message"
    );
    let second = cm.split_next_chunk().expect("chunk 2");
    assert!(second.prompt.contains("TOKEN LIMIT"));
    assert!(
        second.prompt.contains("AT MOST ~"),
        "the limit carries a target"
    );
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
    assert_eq!(
        cm.items_snapshot().len(),
        before.len(),
        "the timeline is untouched"
    );
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

    assert!(
        !cm.commit_split(),
        "an anchor larger than the window must not commit"
    );
    assert_eq!(
        cm.items_snapshot().len(),
        1,
        "the timeline stays exactly as it was"
    );
    assert!(cm.items_snapshot().iter().any(|it| matches!(
        it,
        ContextItem::User { original, .. } if original == "keep me verbatim"
    )));
}

#[test]
fn commit_split_without_work_returns_false() {
    let mut cm = cm(10_000);
    cm.add_user("hi");
    assert!(!cm.commit_split(), "no active split → false");
    cm.begin_split(10_000);
    assert!(
        !cm.commit_split(),
        "an empty buffer must not wipe the timeline"
    );
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
    let next = restored
        .split_next_chunk()
        .expect("resumes where it stopped");
    assert!(
        !next.prompt.contains("abcd efgh"),
        "the summarized chunk is not re-sent after a restore"
    );
    assert!(next.prompt.contains("second part"));
    assert!(next.prompt.contains("[Continuation context"));
    restored.advance_split("summary of part two", next.chunk_end);

    let last = restored.split_next_chunk().expect("the final chunk");
    assert!(last.prompt.contains("third part"));
    restored.advance_split("summary of part three", last.chunk_end);
    assert!(restored.split_all_consumed());
    assert!(restored.commit_split());
    let items = restored.items_snapshot();
    let ContextItem::Compaction { summary, .. } = &items[items.len() - 1] else {
        panic!("expected the committed anchor (appended last)");
    };
    assert!(summary.contains("part one") && summary.contains("part three"));
    // The restored split resumes on the VISIBLE timeline: the pre-boundary
    // items are never re-summarized.
    assert_eq!(restored.build_messages("").len(), 1);
}

// ── Manual /compact (trigger override) ────────────────────────────────────

#[test]
fn manual_compaction_requests_llm_below_the_normal_trigger() {
    let mut cm = cm(100_000); // normal trigger = 80k
    cm.add_user("explore the repo");
    cm.add_assistant(&prose_copies(10), true);
    assert!(
        cm.total_tokens() < cm.trigger(),
        "precondition: below the normal trigger"
    );
    assert_eq!(cm.run(), RunOutcome::Resolved);

    cm.begin_manual_compaction();
    assert!(matches!(cm.run(), RunOutcome::NeedsLlmCompaction));
    assert!(
        cm.llm_compaction_request().is_some(),
        "the summarizer request must exist"
    );
    let ok = cm.apply_llm_summary("## Objective\n- compacted".into());
    cm.end_manual_compaction();
    assert!(ok);
    // Append-only: the timeline is NOT folded into one anchor — everything
    // stays, hidden behind the boundary; only the summary is visible.
    assert_eq!(cm.items.len(), 3);
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].content.as_deref(),
        Some("## Objective\n- compacted")
    );
}

#[test]
fn has_compactable_content_rejects_a_lone_previous_summary() {
    let mut cm = cm(1000);
    assert!(!cm.has_compactable_content(), "empty timeline");
    let _ = cm.apply_llm_summary("previous anchor".into());
    assert!(
        !cm.has_compactable_content(),
        "a lone previous summary is not compactable content"
    );
}

#[test]
fn an_api_error_is_display_only_and_never_reaches_the_model() {
    let mut ctx = cm(10_000);
    ctx.add_user("fix the build");
    ctx.add_error("Error: HTTP 401 - unauthorized");

    // The item is in the timeline (so the transcript persists it) …
    let items = ctx.items_snapshot();
    assert!(matches!(
        items.last(),
        Some(ContextItem::Error { content, .. }) if content == "Error: HTTP 401 - unauthorized"
    ));
    // … but it is invisible to the model and costs no budget.
    let msgs = ctx.build_messages("");
    assert_eq!(msgs.len(), 1, "the error never becomes a model message");
    assert_eq!(msgs[0].role, "user");
    let mut bare = cm(10_000);
    bare.add_user("fix the build");
    assert_eq!(
        ctx.display_info().total_tokens,
        bare.display_info().total_tokens,
        "errors add zero tokens to the budget"
    );

    // A summarizer prompt never sees the failure either. The budget is tiny
    // on purpose so the compaction guard (`total >= trigger`) actually fires
    // and the request is really built — a larger budget would return `None`
    // and the assertion below would never run.
    let long_task = "please ".repeat(60) + "fix the build";
    let mut ctx = cm(50);
    ctx.add_user(&long_task);
    ctx.add_error("Error: HTTP 401 - unauthorized");
    let req = ctx
        .llm_compaction_request()
        .expect("the tiny budget must fire the compaction request");
    assert!(
        !req.prompt.contains("HTTP 401"),
        "the compaction prompt must not embed API errors"
    );

    // A trailing error does not turn the user prompt into "abandoned input":
    // the run failed before producing anything, but the prompt stays so the
    // user can see what they asked (and resend).
    let mut ctx = cm(10_000);
    ctx.add_user("fix the build");
    ctx.add_error("Error: HTTP 401 - unauthorized");
    assert!(!ctx.remove_abandoned_inputs());
    assert_eq!(ctx.build_messages("").len(), 1);
}

#[test]
fn an_error_survives_a_state_roundtrip() {
    let mut ctx = cm(10_000);
    ctx.add_user("task");
    ctx.add_error("Error: provider down");
    let state = ctx.save_state();

    let mut restored = cm(10_000);
    restored.restore_state(&state);
    let msgs = restored.build_messages("");
    assert_eq!(msgs.len(), 1, "the restored error stays out of the context");
    let items = restored.items_snapshot();
    assert!(matches!(items.last(), Some(ContextItem::Error { .. })));
}

// ── summary annotator hook (handoff/tool-set consistency check) ───────────

#[test]
fn checkpoint_budget_includes_the_annotation_that_will_be_committed() {
    let mut manager = cm(1_000);
    manager.add_user(&prose_copies(200));
    manager.begin_manual_compaction();
    manager.set_summary_annotator(Box::new(|summary| {
        format!("{summary} {}", "notice ".repeat(1_000))
    }));
    let before = serde_json::to_value(manager.save_state()).unwrap();
    assert!(!manager.apply_llm_summary("small handoff".into()));
    assert_eq!(serde_json::to_value(manager.save_state()).unwrap(), before);
}

#[test]
fn legacy_commit_checks_annotated_output_and_preserves_rejected_staging() {
    let mut manager = cm(10_000);
    manager.add_user("source evidence");
    manager.begin_split(1_000);
    let chunk = manager.split_next_chunk().unwrap();
    manager.advance_split("small handoff", chunk.chunk_end);
    manager.set_summary_annotator(Box::new(|summary| {
        format!("{summary} {}", "notice ".repeat(1_000))
    }));
    let before = serde_json::to_value(manager.save_state()).unwrap();
    assert!(!manager.commit_split());
    assert_eq!(serde_json::to_value(manager.save_state()).unwrap(), before);
}

#[test]
fn legacy_commit_cannot_cover_items_that_have_not_been_summarized() {
    let mut manager = cm(10_000);
    for _ in 0..3 {
        manager.add_user(&"source ".repeat(150));
    }
    manager.begin_split(200);
    let chunk = manager.split_next_chunk().unwrap();
    manager.advance_split("first chunk only", chunk.chunk_end);
    assert!(!manager.split_all_consumed());
    let before = serde_json::to_value(manager.save_state()).unwrap();
    assert!(!manager.commit_split());
    assert_eq!(serde_json::to_value(manager.save_state()).unwrap(), before);
}

#[test]
fn summary_annotator_runs_on_every_commit_path_and_never_accumulates() {
    let mut manager = cm(10_000);
    manager.add_user(&prose_copies(200));
    manager.set_summary_annotator(Box::new(|summary| {
        crate::harness::core::annotate_summary_tool_set(summary, &["fs_edit".to_string()])
    }));

    // First compaction: the summary references a removed tool → one notice.
    let summary = "## Objective\n- use plan_todo_cross_off next".to_string();
    assert!(manager.apply_llm_summary(summary));
    let committed = manager
        .items
        .iter()
        .filter_map(|item| match item {
            ContextItem::Compaction { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        committed.contains("[Tool set notice]") && committed.contains("plan_todo_cross_off"),
        "the committed handoff carries the notice: {committed}"
    );

    // Re-compaction in update mode: the summarizer carries the old notice
    // line into the new summary — the annotator must strip it and append
    // exactly one fresh notice instead of stacking.
    let carried = format!("{committed}\n\nmore context");
    assert!(manager.apply_llm_summary(carried));
    let latest = manager
        .items
        .iter()
        .filter_map(|item| match item {
            ContextItem::Compaction { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .next_back()
        .expect("a compaction item exists");
    assert_eq!(
        latest.matches("[Tool set notice]").count(),
        1,
        "the newest checkpoint carries exactly one notice: {latest}"
    );
    assert!(
        latest.contains("more context"),
        "the new content survives: {latest}"
    );
}

// ── Transcript export (Markdown) ────────────────────────────────────────────

#[test]
fn export_markdown_mirrors_the_model_view_and_skips_display_only_items() {
    let mut cm = cm(10_000);
    cm.add_user("the prompt");
    cm.add_tool_call("call-1", "fs_read", "{\"path\":\"a.rs\"}");
    cm.add_tool_result("call-1", "file contents");
    cm.add_assistant("the answer", true);
    cm.add_error("Error: HTTP 500");

    let md = cm.export_markdown();
    assert!(md.contains("## User\n\nthe prompt"));
    assert!(md.contains("### Tool call: `fs_read`"));
    assert!(md.contains("```json\n{\"path\":\"a.rs\"}\n```"));
    assert!(md.contains("### Tool result\n\n```text\nfile contents\n```"));
    assert!(md.contains("## Assistant\n\nthe answer"));
    // Display-only error line: the model never sees it, so neither does the export.
    assert!(!md.contains("HTTP 500"));
}

#[test]
fn export_markdown_hides_swept_and_masked_items_like_build_messages() {
    let mut cm = cm(10_000);
    cm.add_user("prompt");
    cm.add_tool_call("call-1", "fs_read", "{}");
    cm.add_tool_result("call-1", "payload");
    cm.add_assistant("answer", true);
    assert!(cm.mask_newest_tool_result().is_some());

    let md = cm.export_markdown();
    assert!(
        md.contains("source context item #"),
        "a masked result renders its typed reference: {md}"
    );
    assert!(!md.contains("payload"), "the masked payload stays out: {md}");

    // Hiding (debris sweep) removes the item entirely from the view.
    let answer_id = cm
        .items
        .iter()
        .find(|item| matches!(item, ContextItem::Assistant { .. }))
        .map(ContextItem::id)
        .unwrap();
    cm.hidden.insert(answer_id);
    let md = cm.export_markdown();
    assert!(!md.contains("the answer") && !md.contains("answer"));
}

#[test]
fn export_markdown_composes_checkpoints_before_the_raw_tail() {
    let mut cm = cm(10_000);
    cm.add_user("old prompt");
    cm.add_assistant("old answer", true);
    cm.begin_manual_compaction();
    assert!(cm.apply_llm_summary("## Objective\n- summarized".to_string()));
    cm.add_user("new prompt");

    let md = cm.export_markdown();
    let checkpoint = md.find("## Compaction checkpoint").unwrap();
    let raw_tail = md.find("new prompt").unwrap();
    assert!(checkpoint < raw_tail, "checkpoints lead, raw tail follows: {md}");
    // The folded sources are hidden behind the checkpoint coverage.
    assert!(!md.contains("old prompt"));
    assert!(!md.contains("old answer"));
}

#[test]
fn push_fenced_never_breaks_out_on_embedded_backtick_runs() {
    let mut out = String::new();
    // A payload whose line starts with four backticks must get a five-wide fence.
    push_fenced(&mut out, "text", "before\n````closed?\nafter");
    assert!(out.starts_with("`````text\n"), "fence outgrows the payload: {out}");
    assert!(out.ends_with("\n`````\n"), "closing fence matches: {out}");
    // The default fence is the CommonMark minimum of three.
    let mut plain = String::new();
    push_fenced(&mut plain, "json", "{}");
    assert_eq!(plain, "```json\n{}\n```\n");
}
