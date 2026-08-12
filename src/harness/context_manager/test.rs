//! Tests for the single-owner LLM-free SYNCHRONOUS context manager.
//!
//! Coverage: in-place pipeline compression (1 item = 1 message), protected
//! user prompts, the native `tool_call → tool` structural chain, `LoopClosure`
//! promotion + guard, the pipeline-first 80% compaction funnel (phase 1 always
//! leads; phase 2 only when it fails, with the LoopClosure as the shared
//! checkpoint — eviction structurally never touches unsummarized prose), the
//! useless tool-chain sweep, the LLM compaction fallback (phase 3, outside the
//! funnel), message rendering (positions + roles), save/restore round-trip,
//! display info, and integration with the harness payload.

use super::*;
use crate::util::estimate_tokens;

fn cm(max_tokens: usize) -> ContextManager {
    ContextManager::new(max_tokens)
}

/// ~16-word varied prose sentence with a unique index; `n` copies give a
/// text of roughly `n` × 85 chars. Each copy is unique so the
/// TF-IDF → LSA → MMR pipeline has real signal to compress (identical
/// repeated sentences collapse to a degenerate matrix and barely shrink).
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

// ── Ingestion + in-place compression ─────────────────────────────────────

#[test]
fn assistant_text_compresses_in_place_at_the_same_position() {
    let mut cm = cm(1000); // trigger = 800
    let text = prose_copies(40);
    cm.add_user("before");
    cm.add_assistant(&text, true);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: the raw draft must fire the trigger"
    );

    // The pipeline (phase 1 of the compaction) compresses SYNCHRONOUSLY on
    // the agent loop's thread — there is no worker thread anymore. The copy is
    // swapped IN PLACE: same position, smaller content.
    cm.run();

    let ContextItem::Assistant { compressed, .. } = &cm.items[1] else {
        panic!("expected Assistant at index 1");
    };
    let compressed_text = compressed.clone().expect("the pipeline resolved");
    assert!(
        compressed_text.len() < text.len(),
        "compressed copy must be smaller than the original"
    );

    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 2, "position preserved after the swap");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content.as_deref(), Some(compressed_text.as_str()));
}

#[test]
fn user_prompts_keep_the_native_user_role() {
    let mut cm = cm(10_000);
    cm.add_user("ping");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content.as_deref(), Some("ping"));
}

// EVERY user prompt is protected by construction: never compressed by the
// pipeline and never evicted by the draft pass. The old "two most recent are
// protected, the older ones are pipeline material" bookkeeping (and the whole
// anchor system) is gone — the original text stays verbatim until the LLM
// compaction folds it into the general summary.
#[test]
fn all_user_prompts_are_protected_and_never_compressed() {
    let mut cm = cm(1000); // trigger = 800
    for i in 0..5 {
        cm.add_user(&format!("prompt {i}"));
    }
    cm.add_assistant(&prose_copies(40), true); // fires the trigger
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    // No user prompt was compressed or evicted: every one renders verbatim.
    let msgs = cm.build_messages("");
    for (i, msg) in msgs.iter().enumerate() {
        if i < 5 {
            assert_eq!(msg.role, "user", "every user prompt keeps its role");
            assert_eq!(
                msg.content.as_deref(),
                Some(format!("prompt {i}").as_str()),
                "the ORIGINAL user text is delivered verbatim"
            );
        }
    }
    // Only the assistant draft was compressed.
    assert!(
        matches!(
            &cm.items[5],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "the pipeline compressed the assistant draft in place"
    );
    assert_eq!(cm.items.len(), 6, "nothing was evicted");
}

// Proof: the duplicate-input guard rests on a real invariant — the trailing
// user prompt is ALWAYS delivered verbatim (never compressed), so the
// `build_messages` "same text → don't append" guard and the harness's
// `last_user_equals` guard agree.
#[test]
fn last_user_prompt_is_always_delivered_verbatim() {
    let mut cm = cm(10_000);
    for i in 0..5 {
        cm.add_user(&format!("prompt {i}"));
    }
    // current_input equal to the last prompt is not duplicated; it renders once.
    let msgs = cm.build_messages("prompt 4");
    assert_eq!(msgs.len(), 5, "no extra message appended");
    assert_eq!(msgs.last().unwrap().content.as_deref(), Some("prompt 4"));
    assert_eq!(
        msgs.iter()
            .filter(|m| m.content.as_deref() == Some("prompt 4"))
            .count(),
        1,
        "the last prompt must be sent exactly once"
    );
}

#[test]
fn build_messages_appends_current_input_unless_duplicate() {
    let mut cm = cm(10_000);
    cm.add_user("hello");

    // Same text as the trailing user turn → not duplicated.
    let msgs = cm.build_messages("hello");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content.as_deref(), Some("hello"));

    // A different steering input is appended as a final user message.
    let msgs = cm.build_messages("please continue");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].role, "user");
    assert_eq!(msgs[1].content.as_deref(), Some("please continue"));
}

// ── Abandoned input cleanup (Esc before any LLM output) ──────────────────

// The user sends input A, cancels with Esc before the LLM produced anything,
// then sends input B: the timeline ends with two consecutive User turns with
// no output between. The abandoned A is dropped — only B stays.
#[test]
fn remove_abandoned_inputs_drops_the_earlier_consecutive_turn() {
    let mut cm = cm(10_000);
    cm.add_user("input one (abandoned)");
    cm.add_user("input two (fresh)");

    assert!(cm.remove_abandoned_inputs(), "an abandoned turn is removed");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content.as_deref(), Some("input two (fresh)"));
}

// A turn that produced ANY output (assistant text promoted to a LoopClosure)
// is never removed — the LLM already answered, so the user did not give up
// on the input.
#[test]
fn remove_abandoned_inputs_keeps_a_turn_that_produced_output() {
    let mut cm = cm(10_000);
    cm.add_user("answered question");
    cm.add_assistant("Here is the answer.", true);
    cm.close_loop(); // → LoopClosure
    cm.add_user("follow-up");

    assert!(
        !cm.remove_abandoned_inputs(),
        "output between the turns means nothing was abandoned"
    );
    assert_eq!(cm.items_snapshot().len(), 3);
}

// Tool work between the two inputs counts as output — nothing is removed.
#[test]
fn remove_abandoned_inputs_keeps_a_turn_with_tool_work_after_it() {
    let mut cm = cm(10_000);
    cm.add_user("do it");
    cm.add_tool_call("c1", "fs_read", "{}");
    cm.add_tool_result("c1", "contents");
    cm.add_user("next");

    assert!(!cm.remove_abandoned_inputs());
    assert_eq!(cm.items_snapshot().len(), 4);
}

// Three consecutive abandoned inputs: all but the newest are dropped.
#[test]
fn remove_abandoned_inputs_keeps_only_the_newest_of_a_consecutive_run() {
    let mut cm = cm(10_000);
    cm.add_user("a");
    cm.add_user("b");
    cm.add_user("c");

    assert!(cm.remove_abandoned_inputs());
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content.as_deref(), Some("c"));
}

// No-op on an empty timeline or a single user turn.
#[test]
fn remove_abandoned_inputs_is_a_noop_without_a_consecutive_pair() {
    let mut cm = cm(10_000);
    assert!(!cm.remove_abandoned_inputs(), "empty timeline");
    cm.add_user("single");
    assert!(!cm.remove_abandoned_inputs(), "one user turn only");
}

// The scheduling bias (draft cursor / segment frontier) is reset after the
// removal, so the next overflow starts fresh.
#[test]
fn remove_abandoned_inputs_resets_the_scheduling_state() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"u ".repeat(50));
    cm.add_assistant(&"A ".repeat(500), false);
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(500));
    cm.run(); // evicts draft A and parks the cursor on the chain
    let chain_id = cm
        .items
        .iter()
        .find(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1"))
        .map(ContextItem::id)
        .unwrap();
    assert_eq!(
        cm.draft_cursor,
        Some(chain_id),
        "precondition: cursor parked on the chain"
    );

    cm.add_user("abandoned");
    cm.add_user("fresh");
    assert!(cm.remove_abandoned_inputs());
    assert_eq!(cm.draft_cursor, None, "scheduling bias is reset");
    assert_eq!(cm.segment_start, None, "segment frontier is reset");
}

// Structural tool layer (native chain)

#[test]
fn tool_call_and_result_render_native_chain() {
    let mut cm = cm(10_000);
    cm.add_user("do it");
    cm.add_tool_call("call_1", "read_file", r#"{"path":"/a"}"#);
    cm.add_tool_result("call_1", "file contents here");

    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role, "user");
    // Tool call → assistant message with native tool_calls.
    assert_eq!(msgs[1].role, "assistant");
    let tc = msgs[1].tool_calls.as_ref().expect("tool_calls present");
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].id, "call_1");
    assert_eq!(tc[0].function.name, "read_file");
    assert!(tc[0].function.arguments.contains("\"path\""));
    // Tool result → tool message with the matching id.
    assert_eq!(msgs[2].role, "tool");
    assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(msgs[2].content.as_deref(), Some("file contents here"));
}

#[test]
fn tool_call_with_signature_round_trips_through_messages() {
    let mut cm_sig = cm(10_000);
    cm_sig.add_user("read it");
    // Gemini 3.x native functionCall carrying its thought signature.
    cm_sig.add_tool_call_with_signature(
        "fc_1",
        "fs_read",
        r#"{"targets":[{"path":"/a"}]}"#,
        "sig_roundtrip_123",
    );
    cm_sig.add_tool_result("fc_1", "content");

    let msgs = cm_sig.build_messages("");
    let tc = msgs[1].tool_calls.as_ref().expect("tool_calls present");
    assert_eq!(
        tc[0].thought_signature.as_deref(),
        Some("sig_roundtrip_123"),
        "the thought signature must survive into the native tool_calls message"
    );
    // A plain call (no signature) must render None — not an empty string —
    // so non-Gemini providers never see the key on their wire format.
    let mut cm_plain = cm(10_000);
    cm_plain.add_user("read it");
    cm_plain.add_tool_call("fc_2", "fs_read", "{}");
    let msgs_plain = cm_plain.build_messages("");
    let tc_plain = msgs_plain[1]
        .tool_calls
        .as_ref()
        .expect("tool_calls present");
    assert_eq!(tc_plain[0].thought_signature, None);
}

#[test]
fn tool_items_are_never_prose_compressed() {
    let mut cm = cm(10_000);
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"huge tool payload ".repeat(100));
    assert!(
        cm.items.iter().all(|it| matches!(
            it,
            ContextItem::ToolCall { .. } | ContextItem::ToolResult { .. }
        )),
        "tool items must stay structural"
    );
}

#[test]
fn assistant_output_with_tool_call_is_never_compressed() {
    let mut cm = cm(10_000);
    // An output that carried a tool call stays in the structural layer.
    cm.add_assistant("Let me check the file first", false);
    cm.add_tool_call("c1", "fs_read", "{}");
    cm.add_tool_result("c1", "contents");
    assert!(
        cm.items[0].tokens(TokenEncoding::Cl100k) == estimate_tokens("Let me check the file first"),
        "non-compressible assistant output must stay raw"
    );
    assert_eq!(
        cm.build_messages("")[0].content.as_deref(),
        Some("Let me check the file first"),
        "the structural assistant text is delivered verbatim"
    );
    // And it can never be promoted to a LoopClosure.
    cm.close_loop();
    assert!(!cm.items.back().unwrap().is_loop());
}

#[test]
fn leading_orphaned_tool_results_are_skipped_in_messages() {
    let mut cm = cm(10_000);
    // A lone tool result with no preceding tool call (defensive restore case).
    // id 999 avoids colliding with the ids add_user() will allocate (1, 2, ...).
    cm.items.push_back(ContextItem::ToolResult {
        id: 999,
        call_id: "orphan".to_string(),
        content: "stray".to_string(),
        useless: false,
    });
    cm.add_user("hi");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1, "the orphan tool result must be skipped");
    assert_eq!(msgs[0].role, "user");
}

// The useless marker is metadata for the sweep only: it is recorded on the
// flagged result, defaults to false for the plain ingestion path, and never
// changes what is delivered to the provider (the tool message renders the
// content verbatim).
#[test]
fn tool_result_useless_flag_is_recorded_but_not_rendered() {
    let mut cm = cm(10_000);
    cm.add_tool_call("c1", "find_grep", "{}");
    cm.add_tool_result_flagged("c1", "no matches", true);
    cm.add_tool_call("c2", "fs_read", "{}");
    cm.add_tool_result("c2", "file contents");

    let items = cm.items_snapshot();
    assert_eq!(
        items
            .iter()
            .filter(|it| matches!(it, ContextItem::ToolResult { useless: true, .. }))
            .count(),
        1,
        "exactly the flagged result is marked useless"
    );
    assert_eq!(
        items
            .iter()
            .filter(|it| matches!(it, ContextItem::ToolResult { useless: false, .. }))
            .count(),
        1,
        "the plain ingestion path records useless=false"
    );

    // Both results still render as ordinary tool messages.
    let msgs = cm.build_messages("");
    let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tool_msgs.len(), 2, "both tool results must render");
    assert_eq!(
        tool_msgs[0].content.as_deref(),
        Some("no matches"),
        "useless must not leak into the delivered content"
    );
}

// LoopClosure promotion + guard

#[test]
fn close_loop_promotes_last_assistant_text() {
    let mut cm = cm(10_000);
    cm.add_user("do the thing");
    cm.add_tool_call("c1", "fs_read", "{}");
    cm.add_tool_result("c1", "hello");
    cm.add_assistant("Done. The file contains hello.", true);

    cm.close_loop();
    assert_eq!(cm.items.len(), 4);
    assert!(
        cm.items.back().unwrap().is_loop(),
        "last item becomes a LoopClosure"
    );
    let ContextItem::LoopClosure { content, .. } = cm.items.back().unwrap() else {
        panic!("expected LoopClosure");
    };
    assert_eq!(content, "Done. The file contains hello.");
    // The LoopClosure keeps its position and role in the messages array.
    let msgs = cm.build_messages("");
    assert_eq!(msgs[3].role, "assistant");
    assert_eq!(
        msgs[3].content.as_deref(),
        Some("Done. The file contains hello.")
    );
}

#[test]
fn close_loop_guard_skips_when_last_output_was_a_tool_call() {
    let mut cm = cm(10_000);
    cm.add_user("do the thing");
    cm.add_tool_call("c1", "fs_read", "{}");
    cm.add_tool_result("c1", "hello");
    cm.close_loop();
    assert!(
        !cm.items.back().unwrap().is_loop(),
        "a tool result cannot become a LoopClosure"
    );
}

// ── Useless tool-chain sweep (outside the toggle) ────────────────────────

// Dead-weight cleanup: chains whose RESULT is marked `useless` (e.g. a
// zero-match find_grep) are removed automatically at the start of `run` —
// even under the budget — while the NEWEST chain (which the model has not
// seen yet) survives and useful chains are never touched.
#[test]
fn sweep_useless_chains_removes_dead_chains_but_keeps_the_newest() {
    let mut cm = cm(10_000); // far below the trigger — the sweep runs anyway
    cm.add_tool_call("t0", "find_grep", "{}");
    cm.add_tool_result_flagged("t0", "no matches", true); // dead
    cm.add_tool_call("t1", "fs_read", "{}");
    cm.add_tool_result("t1", "file contents"); // useful
    cm.add_tool_call("t2", "find_glob", "{}");
    cm.add_tool_result_flagged("t2", "no files", true); // dead, but NEWEST

    cm.run();

    assert!(
        !cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t0")),
        "the old useless chain is swept automatically"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t1")),
        "a useful chain is never touched"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t2")),
        "the NEWEST chain survives — the model has not seen it yet"
    );
}

#[test]
fn sweep_keeps_useful_chains_intact() {
    let mut cm = cm(10_000);
    cm.add_tool_call("t0", "fs_read", "{}");
    cm.add_tool_result("t0", "a");
    cm.add_tool_call("t1", "fs_write", "{}");
    cm.add_tool_result("t1", "b");
    let before = cm.items_snapshot().len();

    cm.run();
    assert_eq!(cm.items_snapshot().len(), before, "nothing is swept");
}

// ── Context-window overflow recovery ────────────────────────────────────

fn has_chain(cm: &ContextManager, call_id: &str) -> bool {
    cm.items.iter().any(|it| match it {
        ContextItem::ToolCall { call_id: c, .. } | ContextItem::ToolResult { call_id: c, .. } => {
            c == call_id
        }
        _ => false,
    })
}

/// The harness drains a provider-side context-window overflow ONE chain per
/// call ("1 por vez"): useless chains first, then the LARGEST, then the rest
/// — always the call+result PAIR together (a lone half would break the native
/// tool-call format) — and never a user prompt, assistant draft, LoopClosure
/// or compaction summary.
#[test]
fn evict_tool_chain_for_overflow_removes_one_chain_at_a_time() {
    let mut cm = cm(10_000);
    cm.add_user("protected user prompt");
    cm.add_assistant("plain draft", true);
    cm.add_tool_call("t0", "find_grep", "{}");
    cm.add_tool_result_flagged("t0", "no matches", true); // useless
    cm.add_tool_call("t1", "fs_read", "{}");
    cm.add_tool_result("t1", "file contents"); // useful, small
    cm.add_tool_call("t2", "bash_run", "{}");
    cm.add_tool_result("t2", &"big payload ".repeat(200)); // useful, LARGE
    cm.add_assistant("final answer", true);
    cm.close_loop(); // → LoopClosure, protected

    // Useless chain goes first.
    assert!(cm.evict_tool_chain_for_overflow());
    assert!(!has_chain(&cm, "t0"), "useless chain is drained first");
    assert!(has_chain(&cm, "t1") && has_chain(&cm, "t2"));

    // Then the largest (most tokens freed per removal).
    assert!(cm.evict_tool_chain_for_overflow());
    assert!(!has_chain(&cm, "t2"), "the largest chain goes second");
    assert!(has_chain(&cm, "t1"));

    // Then the last one.
    assert!(cm.evict_tool_chain_for_overflow());
    assert!(!has_chain(&cm, "t1"));

    // Nothing left — the harness must stop draining.
    assert!(!cm.evict_tool_chain_for_overflow());

    // Protected items survived untouched.
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::User { .. })),
        "user prompts are never drained"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::LoopClosure { .. })),
        "LoopClosures are never drained"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::Assistant { .. })),
        "assistant drafts are never drained"
    );
}

/// The pair (call + result) is removed TOGETHER — the native tool-call
/// format rejects a lone half.
#[test]
fn evict_tool_chain_for_overflow_removes_both_halves() {
    let mut cm = cm(10_000);
    cm.add_tool_call("t0", "fs_write", "{}");
    cm.add_tool_result("t0", "written");

    assert!(cm.evict_tool_chain_for_overflow());
    assert!(
        !cm.items.iter().any(|it| it.is_tool()),
        "both halves of the chain are gone"
    );
}

/// The stuck-overflow lifecycle: marked per provider, cleared on provider
/// switch (model change) or on a successful compaction, and persisted with
/// the snapshot so a stuck session stays notified across turns.
#[test]
fn overflow_stuck_state_lifecycle() {
    let mut cm = cm(10_000);
    assert!(!cm.overflow_stuck("openai"));

    cm.mark_overflow("openai");
    assert!(cm.overflow_stuck("openai"));
    assert!(!cm.overflow_stuck("anthropic"));

    // The harness syncs the active provider at loop start: a switch clears.
    cm.sync_provider("anthropic");
    assert!(!cm.overflow_stuck("openai"));

    // Same provider keeps the stuck state.
    cm.mark_overflow("anthropic");
    cm.sync_provider("anthropic");
    assert!(cm.overflow_stuck("anthropic"));

    // A successful compaction proves the provider accepts the context again.
    cm.apply_llm_summary("## Objective\n- done".to_string());
    assert!(!cm.overflow_stuck("anthropic"));

    // The stuck state survives save/restore (persisted notification).
    cm.mark_overflow("openai");
    let state = cm.save_state();
    let mut restored = ContextManager::new(10_000);
    restored.restore_state(&state);
    assert!(restored.overflow_stuck("openai"));
}

// ── Compaction phases ────────────────────────────────────────────────────

// The pipeline (phase 1) compresses compressible assistant drafts IN PLACE
// while user prompts and LoopClosures survive untouched.
#[test]
fn pipeline_compresses_drafts_and_keeps_protected_items() {
    let mut cm = cm(1000);
    // Protected user (50) + closure (50) + two compressible prose drafts
    // (~850 total) = ~950, which fires the 80% trigger (800). The pipeline
    // (phase 1) SUMMARIZES the drafts in place — nothing is evicted.
    cm.items.push_back(ContextItem::User {
        id: 1,
        original: "x ".repeat(50),
    });
    cm.items.push_back(ContextItem::LoopClosure {
        id: 2,
        content: "x ".repeat(50),
    });
    cm.items.push_back(ContextItem::Assistant {
        id: 3,
        original: prose_copies(20),
        compressed: None,
        compressible: true,
    });
    cm.items.push_back(ContextItem::Assistant {
        id: 4,
        original: prose_copies(20),
        compressed: None,
        compressible: true,
    });
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "after the pipeline total must be below the 80% trigger, got {}",
        cm.total_tokens()
    );
    assert_eq!(
        cm.items.len(),
        4,
        "the pipeline summarizes the drafts — nothing is evicted"
    );
    assert!(
        cm.items.iter().all(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            } | ContextItem::User { .. }
                | ContextItem::LoopClosure { .. }
        )),
        "both drafts are compressed in place"
    );
    assert!(
        cm.items.iter().any(|it| it.is_loop()),
        "LoopClosure must survive the pipeline"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::User { .. })),
        "user prompts must survive the pipeline"
    );
}

// The funnel is pipeline-first: phase 1 always leads; phase 2 (the draft
// pass) runs only when the pipeline cannot resolve, resuming from its
// persistent cursor across overflows and yielding at a LoopClosure (or the
// end of the timeline still over the trigger). Drafts here are
// non-compressible (tool-carrying) assistant outputs so the pipeline has
// nothing to summarize and falls through to the draft pass.
#[test]
fn run_evicts_drafts_and_resumes_from_the_cursor() {
    let mut cm = cm(1000); // trigger = 800

    // Trigger 1 content: protected user (50) + drafts A/B (250 each) + tool
    // chain c1 (~503) = ~1053 ≥ 800.
    cm.add_user(&"u ".repeat(50));
    cm.add_assistant(&"A ".repeat(250), false); // draft A — carried a tool call
    cm.add_assistant(&"B ".repeat(250), false); // draft B
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(500));
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Trigger 1 — the drafts are the only eviction target; tool chains are
    // protected now. A (250) is removed → 803 ≥ 800, so B goes too → 553 <
    // 800, and the pass stops with the cursor parked on c1.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("A ") || original.starts_with("B ")
        )),
        "the drafts are evicted by phase 2"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "the tool chain is protected — never evicted by the draft pass"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::User { .. })),
        "the user prompt is untouchable"
    );

    // Regrow above the trigger: drafts X/Y + a new tool chain c2 at the end
    // (~303): 553 + 250 + 250 + 303 = ~1356 ≥ 800.
    cm.add_assistant(&"X ".repeat(250), false); // draft X
    cm.add_assistant(&"Y ".repeat(250), false); // draft Y
    cm.add_tool_call("c2", "read_file", "{}");
    cm.add_tool_result("c2", &"t ".repeat(300));
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    // Trigger 2 — phase 2 RESUMES from the cursor: tool chains are skipped,
    // X (→ 1106) and Y (→ 856) are removed, and the end of the timeline is
    // reached still over 800 with NO draft left. The retention tier then
    // trims the OLD chain's result content in place (c1 is no longer the
    // newest chain), bringing the total under the trigger WITHOUT an LLM
    // call. The NEWEST chain (c2) keeps its full result; both chains'
    // structural halves survive — trimming never breaks the
    // `tool_call → tool` pairing.
    let outcome = cm.run();
    assert_eq!(
        outcome,
        RunOutcome::Resolved,
        "the retention trim resolves the overflow instead of the LLM compaction"
    );
    assert!(
        cm.total_tokens() < 800,
        "the retention trim brought the total under the trigger"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "the oldest tool chain survives — retention trims content, never the chain"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c2")),
        "the newest tool chain survives too"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(
                it,
                ContextItem::ToolResult { call_id, content, .. }
                    if call_id == "c1" && content.starts_with(TOOL_RESULT_TRIM_MARKER)
            )),
        "the old chain's result is trimmed in place"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(
                it,
                ContextItem::ToolResult { call_id, content, .. }
                    if call_id == "c2" && content.starts_with("t ")
            )),
        "the newest chain's result stays full"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("X ") || original.starts_with("Y ")
        )),
        "phase 2 continued from its cursor and removed the next drafts"
    );
}

// A huge (protected) user prompt can push the total over the trigger when the
// only other content is LoopClosures — the deterministic phases have nothing
// to compress and nothing to evict. The grind is exhausted immediately: the
// LLM compaction is the ONLY relief, so `run` requests it.
#[test]
fn run_requests_llm_compaction_when_only_protected_items_remain() {
    let mut cm = cm(1000); // trigger = 800

    // Two user prompts (~700 tokens) + three LoopClosures (~600 tokens):
    // total ~1300 — over the trigger AND over the budget.
    cm.add_user(&"p ".repeat(300));
    cm.add_user(&"P ".repeat(400));
    for label in ["old", "mid", "new"] {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::LoopClosure {
            id,
            content: format!("{label} ").repeat(200),
        });
    }
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    assert!(
        cm.total_tokens() > 1000,
        "precondition: over the real budget"
    );

    let outcome = cm.run();

    assert_eq!(
        outcome,
        RunOutcome::NeedsLlmCompaction,
        "protected-only over the trigger requests the LLM compaction"
    );
    // Nothing was removed: every protected item survives untouched.
    assert_eq!(cm.items.len(), 5);
    assert!(cm.items.iter().all(|it| matches!(
        it,
        ContextItem::User { .. } | ContextItem::LoopClosure { .. }
    )));
}

// First-trigger behavior with NO tool calls: the draft eviction alone must
// converge the pass below the 80% trigger, removing drafts oldest-first —
// the newest draft and every LoopClosure/user prompt survive.
#[test]
fn first_trigger_without_tools_is_handled_by_draft_eviction() {
    let mut cm = cm(1000); // trigger = 800

    // User (50) + oldest draft A (500) + closure (100) + newest draft B (100)
    // + closure (100) = 850. NO tool items anywhere.
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(100),
    });
    cm.add_assistant(&"B ".repeat(100), false); // newest draft
    let closure2 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure2,
        content: "M ".repeat(100),
    });
    assert_eq!(
        cm.items.iter().filter(|it| it.is_tool()).count(),
        0,
        "precondition: no tool calls on the first breach"
    );
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert!(
        cm.total_tokens() < 800,
        "the draft eviction alone must converge below the 80% trigger, got {}",
        cm.total_tokens()
    );
    // Only the minimum was removed: exactly one item (the oldest draft) is
    // gone — the newest draft, both closures and the prompt survive.
    assert_eq!(cm.items.len(), 4, "only the oldest draft is removed");
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the OLDEST draft must be removed first"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the NEWEST draft survives — recent context is never hit before older"
    );
    // LoopClosures and user prompts are untouchable.
    assert_eq!(
        cm.items.iter().filter(|it| it.is_loop()).count(),
        2,
        "both LoopClosures survive"
    );
    assert_eq!(
        cm.items
            .iter()
            .filter(|it| matches!(it, ContextItem::User { .. }))
            .count(),
        1,
        "the user prompt survives"
    );
}

// Distribution across content types across two triggers: the draft pass
// removes the drafts OLDEST-FIRST with the minimum decompaction, and pass 2
// RESUMES from the cursor. Removal never hits recent context to start the
// next phase, and tool chains survive both passes.
#[test]
fn run_distributes_eviction_without_hitting_recent_first() {
    let mut cm = cm(1000); // trigger = 800

    // Pass 1. User (50) + draft A (200) + old tool T0 (~403) + draft B (90) +
    // new tool T1 (~153) + draft C (90) = ~986.
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(200), false);
    cm.add_tool_call("t0", "read_file", "{}");
    cm.add_tool_result("t0", &"T0 ".repeat(200));
    cm.add_assistant(&"B ".repeat(90), false);
    cm.add_tool_call("t1", "read_file", "{}");
    cm.add_tool_result("t1", &"T1 ".repeat(75));
    cm.add_assistant(&"C ".repeat(90), false);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: pass 1 over the trigger"
    );

    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "pass 1 must drop below the trigger"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the oldest draft is the first eviction target"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ") || original.starts_with("C ")
        )),
        "the newer drafts survive pass 1"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "t0" || call_id == "t1"
        )),
        "ALL tool chains survive pass 1 — they are protected"
    );

    // Pass 2. Regrow with drafts D/E then a brand-new tool chain T2 at the
    // very end (~184): 791 + 91 + 91 + 184 = ~1157.
    cm.add_assistant(&"D ".repeat(90), false);
    cm.add_assistant(&"E ".repeat(90), false);
    cm.add_tool_call("t2", "read_file", "{}");
    cm.add_tool_result("t2", &"T2 ".repeat(90));
    assert!(
        cm.total_tokens() >= 800,
        "precondition: pass 2 over the trigger"
    );

    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "pass 2 must drop below the trigger"
    );
    // Phase 2 RESUMES from its cursor (parked on t0 after pass 1): tool
    // chains are skipped, then B (→ 1066), C (→ 975), D (→ 884), E (→ 793)
    // are removed one at a time until the total crosses below the trigger.
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ")
                    || original.starts_with("C ")
                    || original.starts_with("D ")
                    || original.starts_with("E ")
        )),
        "phase 2 resumes from the cursor and removes the next drafts"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. }
                if call_id == "t0" || call_id == "t1" || call_id == "t2"
        )),
        "ALL tool chains survive — they are protected until the LLM compaction"
    );
}

// ── LLM compaction (phase 3 — the last-resort fallback) ──────────────────

// The manager builds the request with the ENTIRE remaining context serialized
// opencode-style: user prompts, assistant texts, tool calls, tool results
// (verbatim — never truncated), LoopClosures and previous summaries all
// appear in the prompt.
#[test]
fn llm_compaction_request_serializes_the_whole_context() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user("what is the weather"); // [User]
    cm.add_assistant("Let me search.", false); // [Assistant]
    cm.add_tool_call("c1", "find_grep", r#"{"query":"weather"}"#);
    let huge_payload = "payload ".repeat(1_000); // must NOT be truncated
    cm.add_tool_result("c1", &huge_payload);
    cm.add_assistant("No matches found.", true);
    cm.close_loop(); // → [Assistant]: No matches found.

    // Over the trigger, no draft left → the request is materialized.
    let request = cm
        .llm_compaction_request()
        .expect("an over-trigger context produces a request");
    assert!(
        request.prompt.contains("[User]: what is the weather"),
        "user prompts are serialized"
    );
    assert!(
        request.prompt.contains("[Assistant]: Let me search."),
        "assistant texts are serialized"
    );
    assert!(
        request.prompt.contains("[Assistant tool call]: find_grep"),
        "tool calls are serialized"
    );
    assert!(
        request.prompt.contains("[Tool result]: payload"),
        "tool results are serialized"
    );
    assert!(
        request.prompt.contains(&huge_payload),
        "tool results are passed VERBATIM — a huge payload is never truncated"
    );
    assert!(
        !request.prompt.contains("[truncated]"),
        "no truncation marker: the summarizer sees the full tool payloads"
    );
    assert!(
        request.prompt.contains("[Assistant]: No matches found."),
        "LoopClosures are serialized as assistant text"
    );
    // Create mode (no previous summary).
    assert!(
        request.prompt.contains("Create a new anchored summary"),
        "no previous summary → create mode"
    );
    assert!(
        request.prompt.contains("## Objective"),
        "the opencode-style template is embedded"
    );
}

// Update mode: when a previous Compaction item exists, the prompt asks the
// model to UPDATE the anchored summary instead of creating one from scratch.
// The previous summary is NOT duplicated: it appears exactly once, as the
// `[Previous summary]:` line at the top of the transcript, and the instruction
// references it by that label (no `<previous-summary>` block anymore).
#[test]
fn llm_compaction_request_uses_update_mode_with_previous_summary() {
    let mut cm = cm(1000);
    cm.items.push_back(ContextItem::Compaction {
        id: 1,
        summary: "## Objective\n- Refactor the context manager".to_string(),
    });
    cm.add_user(&"p ".repeat(300));
    cm.add_user(&"P ".repeat(400));
    cm.add_assistant(&"A ".repeat(500), false); // the last removable draft
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(200),
    });
    // 20 + 700 + 500 + 200 ≈ 1420 ≥ 800; after the draft is evicted the
    // protected items alone (920) still hold the trigger — LLM compaction.
    assert_eq!(cm.run(), RunOutcome::NeedsLlmCompaction);

    let request = cm.llm_compaction_request().unwrap();
    assert!(
        request
            .prompt
            .contains("Update the summary labeled [Previous summary]"),
        "a previous summary switches the prompt to update mode"
    );
    assert!(
        !request.prompt.contains("<previous-summary>"),
        "the instruction block is gone — the summary is never embedded twice"
    );
    assert!(
        request.prompt.contains("[Previous summary]: ## Objective"),
        "the previous summary appears once, as the first line of the transcript"
    );
    assert_eq!(
        request
            .prompt
            .matches("Refactor the context manager")
            .count(),
        1,
        "the previous summary content is delivered EXACTLY once in the prompt"
    );
}

#[test]
fn llm_compaction_request_is_none_below_the_trigger_or_empty() {
    let empty = cm(1000);
    assert!(
        empty.llm_compaction_request().is_none(),
        "empty → no request"
    );

    let mut small = cm(10_000);
    small.add_user("hello");
    small.add_assistant("hi", true);
    assert!(
        small.llm_compaction_request().is_none(),
        "below the trigger → nothing to compact"
    );
}

// `apply_llm_summary` replaces the ENTIRE timeline with a single Compaction
// item (the continuation summary) and resets the scheduling state. The
// summary renders as an assistant message so the session continues from it.
#[test]
fn apply_llm_summary_replaces_everything_and_resets_scheduling() {
    let mut cm = cm(1000);
    cm.add_user(&"p ".repeat(300));
    cm.add_user(&"P ".repeat(400));
    cm.add_assistant(&"A ".repeat(900), false); // over the trigger
    cm.add_tool_call("c1", "find_grep", "{}");
    cm.add_tool_result("c1", &"t ".repeat(500));
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Drive the compaction so the cursor parks somewhere, then reset it.
    cm.run();
    let outcome = cm.run();
    assert_eq!(outcome, RunOutcome::NeedsLlmCompaction);

    let summary = "## Objective\n- Keep the refactoring going".to_string();
    assert!(
        cm.apply_llm_summary(summary.clone()),
        "the summary must drop the total below the trigger"
    );

    // The whole timeline became a single Compaction item.
    let items = cm.items_snapshot();
    assert_eq!(items.len(), 1, "everything is replaced by the summary");
    assert!(matches!(
        &items[0],
        ContextItem::Compaction { summary: s, .. } if *s == summary
    ));
    assert_eq!(
        cm.draft_cursor, None,
        "the draft cursor is reset — the timeline is new"
    );

    // The summary renders as an assistant message; a new prompt resumes the
    // session.
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "assistant");
    assert_eq!(msgs[0].content.as_deref(), Some(summary.as_str()));
    cm.add_user("continue");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].content.as_deref(), Some("continue"));
}

// While drafts remain, the deterministic grind is NOT exhausted: `run`
// returns Resolved and never requests the LLM prematurely — the funnel
// grinds through the segments until the total drops below the trigger or
// every draft is gone (tudo se repete).
#[test]
fn run_returns_resolved_while_drafts_remain() {
    let mut cm = cm(1000);
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(200), false); // small oldest draft
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"B ".repeat(900), false); // next segment's draft
    // 50 + 200 + 50 + 900 = 1200 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // One run(): the pipeline has nothing compressible, phase 2 evicts A,
    // yields at the closure checkpoint still over the trigger — the funnel
    // REPEATS into the next segment (tudo se repete) and evicts B in the
    // SAME call, landing below the trigger. Resolved, never the LLM.
    let first = cm.run();
    assert_eq!(first, RunOutcome::Resolved);
    assert!(cm.total_tokens() < 800, "converged within one call");
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the next segment's draft was evicted in the same call (tudo se repete)"
    );

    // The drain stays granular across overflows when ONE removal is enough:
    // a fresh draft is removed by the very next call, below the trigger.
    cm.add_assistant(&"C ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    let second = cm.run();
    assert_eq!(second, RunOutcome::Resolved);
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("C ")
        )),
        "the new draft was removed"
    );
}

// The draft cursor must survive the useless sweep removing the very item it
// points at: the resume finds the first item with id >= the removed id.
#[test]
fn draft_cursor_survives_useless_sweep_removing_its_item() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    cm.add_tool_call("t0", "find_grep", "{}");
    cm.add_tool_result_flagged("t0", &"t ".repeat(500), true); // ~500-token useless chain
    // 50 + 500 + ~500 = ~1050 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Round 1: the draft pass removes A (→ below 800) and parks the cursor ON
    // the t0 chain (the next item). The sweep kept t0 alive (it is the newest
    // chain — the model has not seen it yet).
    cm.run();
    assert!(cm.total_tokens() < 800);
    let t0_id = cm
        .items
        .iter()
        .find(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t0"))
        .map(ContextItem::id)
        .unwrap();
    assert_eq!(
        cm.draft_cursor,
        Some(t0_id),
        "the cursor parks on the chain"
    );

    // Round 2: a NEWER chain (t1) arrives, so t0 is no longer the newest —
    // the sweep removes it (the cursor's item). The resume must find the
    // first item with id >= t0 (the new draft B) and continue evicting.
    cm.add_assistant(&"B ".repeat(800), false);
    cm.add_tool_call("t1", "fs_read", "{}");
    cm.add_tool_result("t1", "file contents");
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    cm.run();

    assert!(
        !cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t0")),
        "the sweep removed the useless chain the cursor pointed at"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t1")),
        "the newest (useful) chain survives"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the draft pass resumed past the removed item and kept evicting"
    );
}

// ── The funnel: pipeline-first by construction ────────────────────────────
//
// Phase 1 (the pipeline) ALWAYS leads every overflow; phase 2 (eviction)
// only runs when the pipeline could not resolve. Because of that order,
// eviction structurally never touches prose the pipeline could still
// summarize — a fresh compressible draft is always compressed first, never
// evicted raw. The LoopClosure remains the shared checkpoint: the pipeline
// checks the budget at its boundaries and eviction yields at them so the
// next overflow's pipeline takes the next segment. The LLM compaction is
// OUTSIDE the funnel (a last-resort fallback requested via RunOutcome).

// Overflow 2 proves the funnel is pipeline-first EVERY overflow: a fresh raw
// compressible draft is COMPRESSED by the pipeline (under a draft-led toggle
// it would have been evicted raw), and eviction only removes the older
// non-compressible chunk when compression alone cannot resolve.
#[test]
fn pipeline_leads_every_overflow_and_compresses_fresh_drafts() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&prose_copies(40), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Overflow 1: the pipeline leads and compresses the raw draft in place.
    cm.run();
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "overflow 1: the pipeline compresses the raw draft"
    );

    // Overflow 2: add an older non-compressible chunk and a fresh raw
    // compressible draft. The pipeline leads AGAIN: it compresses the fresh
    // draft (never evicted raw), and only when compression alone cannot
    // resolve does eviction remove the older chunk.
    cm.add_assistant(&"B ".repeat(1200), false);
    cm.add_assistant(&prose_copies(30), true);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    cm.run();
    assert!(cm.total_tokens() < 800);
    assert_eq!(cm.items.len(), 2, "only the older chunk was evicted");
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "the fresh draft was COMPRESSED by the pipeline — eviction never \
         touches unsummarized prose"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the older non-compressible chunk was evicted instead"
    );
}

// The invariant the funnel guarantees structurally: a fresh compressible
// draft arriving between overflows is ALWAYS compressed by the pipeline (it
// leads), never evicted raw — even when the draft pass is mid-grind.
#[test]
fn fresh_compressible_drafts_are_compressed_not_evicted_raw() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(900), false); // non-compressible oldest draft

    // Overflow 1: the pipeline has nothing compressible; eviction removes A.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the non-compressible draft is evicted by phase 2"
    );

    // Overflow 2: a fresh compressible draft arrives. The pipeline-first
    // funnel guarantees it is COMPRESSED in place (never evicted raw) and the
    // total drops below the trigger.
    cm.add_assistant(&prose_copies(60), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert_eq!(cm.items.len(), 2, "the fresh draft survives");
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "the fresh draft was compressed by the pipeline, not evicted raw"
    );
}

// The draft pass grinds segment by segment INSIDE one call (tudo se repete):
// when phase 2 reaches the LoopClosure checkpoint still over the trigger, the
// funnel repeats — the pipeline takes the next segment, phase 2 drains it,
// until the total drops below the trigger. Across overflows, the persistent
// cursor keeps the drain granular when ONE removal suffices.
#[test]
fn eviction_grinds_across_overflows_until_the_loop_closure_checkpoint() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Round 1: the pipeline has nothing compressible → the draft pass removes
    // A and stops below the trigger.
    cm.run();
    assert!(cm.total_tokens() < 800);

    // Round 2: the cursor resumes and removes the new chunk.
    cm.add_assistant(&"B ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    cm.run();
    assert!(cm.total_tokens() < 800);

    // Round 3: a new segment starts with a LoopClosure after the draft. The
    // draft pass reaches the checkpoint still over the trigger → the funnel
    // REPEATS into the next segment (tudo se repete): the new segment's
    // draft is evicted in the SAME call, below the trigger.
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"C ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    let outcome = cm.run();
    assert_eq!(
        outcome,
        RunOutcome::Resolved,
        "the funnel converges within one call"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("C ")
        )),
        "the next segment's draft is evicted in the same call (tudo se repete)"
    );
    assert!(cm.total_tokens() < 800);
}

// Phase 2 yields at a LoopClosure boundary (the checkpoint): the oldest
// segment's drafts are exhausted and the funnel REPEATS into the next segment
// in the same call (tudo se repete). When every draft is gone and only
// protected items (tool chains, closures, prompts) remain over the trigger,
// the deterministic grind is exhausted: NeedsLlmCompaction. Tool chains are
// protected and survive.
#[test]
fn eviction_yields_at_a_loop_closure_for_the_next_segment() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"B ".repeat(100), false); // newest draft (next segment)
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(1200)); // ~1200-token chain
    // 50 + 500 + 50 + 100 + ~1203 = ~1903 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    let outcome = cm.run();

    // The funnel grinds BOTH segments in one call: A and B are evicted, the
    // pass yields at the closure boundary, and the tool chain survives. Only
    // protected items remain (~50 + ~50 + ~1203 = ~1303 — clearly over the
    // trigger, wide margin), so the deterministic grind is exhausted →
    // NeedsLlmCompaction.
    assert_eq!(outcome, RunOutcome::NeedsLlmCompaction);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the oldest draft was evicted by phase 2"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the next segment's draft was evicted in the same call (tudo se repete)"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "tool chains are protected"
    );
}

// The core "tudo se repete" interplay IN ONE CALL: segment 1's drafts are
// non-compressible (drained by phase 2), the funnel repeats past the
// checkpoint, and segment 2's COMPRESSIBLE drafts are compressed by the
// pipeline in the SAME run() — no LLM, everything below the trigger.
#[test]
fn segment_interplay_evicts_then_compresses_in_one_call() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(900), false); // non-compressible oldest draft
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&prose_copies(60), true); // compressible, next segment
    // 50 + 900 + 50 + ~prose = ~1900 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    let outcome = cm.run();

    // Segment 1: the pipeline has nothing compressible → phase 2 evicts A.
    // Still over → the funnel repeats into segment 2 → the pipeline
    // COMPRESSES the compressible draft (never evicted raw).
    assert_eq!(outcome, RunOutcome::Resolved);
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "segment 1's non-compressible draft was evicted by phase 2"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        )),
        "segment 2's compressible draft was compressed by the pipeline in the same call"
    );
}

// Persistence

#[test]
fn save_restore_roundtrip_preserves_items_and_resumes_compression() {
    let mut manager = cm(1000);
    manager.add_user("hello");
    manager.add_assistant("some assistant text to compress", true);
    let state = manager.save_state();
    assert_eq!(state.items.len(), 2);

    let mut restored = cm(1000);
    restored.restore_state(&state);
    assert_eq!(restored.items.len(), 2);
    // Nothing is compressed until the budget overflows — fire the pipeline on
    // the restored session and it summarizes the compressible items exactly
    // as in a fresh session (no re-submission machinery needed).
    restored.add_assistant(&prose_copies(40), true);
    assert!(
        restored.total_tokens() >= 800,
        "precondition: over the trigger"
    );
    restored.run();
    assert!(
        restored.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        )),
        "the pipeline compresses restored items on the next overflow"
    );
}

// User prompts are protected by construction, so a restore can never surface
// a compressed user prompt: the delivered text is always the original.
#[test]
fn restore_never_compresses_user_prompts() {
    let mut manager = cm(10_000);
    manager.add_user(&"first prompt that must stay verbatim ".repeat(30));
    manager.add_user(&"second prompt that must stay verbatim ".repeat(30));
    let state = manager.save_state();

    let mut restored = cm(10_000);
    restored.restore_state(&state);

    let msgs = restored.build_messages("");
    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().all(|m| m.role == "user"));
    let expected = "first prompt that must stay verbatim ".repeat(30);
    assert_eq!(
        msgs[0].content.as_deref(),
        Some(expected.as_str()),
        "user prompts render their ORIGINAL text after a restore"
    );
}

// The persistence path the TUI ACTUALLY uses: the `Done`/`Stopped` handlers
// bincode-serialize `save_state()` into the `.ctx` companion file, and
// resuming deserializes those bytes into `restore_state`. The plain in-memory
// save_state/restore_state clone in the other tests can't catch a bincode
// break (field order, type change, missing `#[serde(default)]`), so the real
// serialization format is round-tripped here.
#[test]
fn bincode_roundtrip_preserves_compression_state_and_bookkeeping() {
    let mut m = cm(1000);
    m.add_user("task one"); // id 1
    m.add_assistant(&prose_copies(40), true); // id 2 — gets compressed by run()
    m.run(); // pipeline compresses the draft in place
    m.add_user("task two"); // id 3
    m.add_tool_call("c1", "search", "{\"q\":\"x\"}"); // id 4
    m.add_tool_result_flagged("c1", "no matches", true); // id 5 — useless

    let bytes = bincode::serialize(&m.save_state()).unwrap();
    let state: ContextManagerState = bincode::deserialize(&bytes).unwrap();

    let mut r = cm(1000);
    r.restore_state(&state);

    // Items, the useless flag and the compressed copy all survive.
    assert_eq!(r.items_snapshot().len(), 5);
    assert!(
        r.items_snapshot()
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolResult { useless: true, .. }) })
    );
    assert!(
        r.items_snapshot().iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant {
                    compressed: Some(_),
                    ..
                }
            )
        }),
        "the compressed copy itself is persisted"
    );

    // Restored ids stay monotonic and never collide with new items.
    r.add_assistant("continuation", true);
    let ids: Vec<u64> = r.items_snapshot().iter().map(ContextItem::id).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ids stay monotonic after a bincode round trip"
    );
    assert_eq!(
        *ids.last().unwrap(),
        6,
        "next_id resumes after the last saved id"
    );
}

// The exact "recover the interrupted session" scenario: the snapshot is taken
// mid-loop (`Stopped`), so the timeline ends with a trailing user prompt (the
// in-flight input), a raw assistant draft and a live tool chain. Restoring it
// must resume perfectly: the harness `last_user_equals` guard skips re-adding
// the input, ids continue monotonically, the raw draft is still pipeline
// material, and `close_loop` still promotes the ORIGINAL final text.
#[test]
fn restored_interrupted_session_resumes_exactly_where_it_stopped() {
    let mut m = cm(3000); // trigger = 2400
    m.add_user("q1"); // 1
    m.add_assistant("completed answer one", true); // 2
    m.close_loop(); // → LoopClosure (id 2)
    m.add_user("q2"); // 3
    m.add_assistant("draft two, still raw", true); // 4
    m.add_tool_call("c1", "read_file", "{\"path\":\"a.rs\"}"); // 5
    m.add_tool_result_flagged("c1", "file contents here", true); // 6 — useless
    m.add_user("q3"); // 7 — the in-flight input when the loop was stopped

    let bytes = bincode::serialize(&m.save_state()).unwrap();
    let state: ContextManagerState = bincode::deserialize(&bytes).unwrap();

    let mut r = cm(3000);
    r.restore_state(&state);

    // The resumed input is already the trailing user turn — the harness guard
    // must not re-add it.
    assert!(
        r.last_user_equals("q3"),
        "the guard sees the resumed trailing prompt"
    );
    assert_eq!(
        r.items_snapshot().len(),
        7,
        "the full interrupted timeline is restored"
    );

    // Resume the loop: the final answer arrives and ids stay monotonic.
    let final_answer = prose_copies(150);
    r.add_assistant(&final_answer, true);
    let ids: Vec<u64> = r.items_snapshot().iter().map(ContextItem::id).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ids stay monotonic after restore"
    );
    assert_eq!(*ids.last().unwrap(), 8);

    // Rendering is 1:1 and the resumed input is delivered exactly once (with
    // the empty steering input the harness uses on the first iteration,
    // nothing is appended on top of the restored trailing prompt).
    let msgs = r.build_messages("");
    assert_eq!(msgs.len(), 8, "1 item = 1 message after restore");
    assert_eq!(
        msgs.iter()
            .filter(|m| m.content.as_deref() == Some("q3"))
            .count(),
        1,
        "the resumed input is delivered exactly once"
    );

    // The compaction phases work on the restored timeline exactly as on a
    // fresh one: the pipeline compresses the raw drafts and pulls the total
    // below the trigger.
    assert!(r.total_tokens() >= 2400, "precondition: over the trigger");
    r.run();
    assert!(
        r.total_tokens() < 2400,
        "compaction converged below the trigger"
    );

    // The loop ends: the FINAL answer is promoted in its ORIGINAL form, even
    // though the pipeline may have compressed the raw draft.
    r.close_loop();
    let last = r.items_snapshot().last().cloned();
    assert!(
        matches!(
            last,
            Some(ContextItem::LoopClosure { content, .. }) if content == final_answer
        ),
        "the final answer is promoted verbatim after a restored session"
    );
}

// Back-compat boundary for the persistence format: bincode 1.x is positional
// and does NOT honor `#[serde(default)]` — a snapshot missing ANY field
// (mid-stream like `useless`, or the stream-final `anchor`) fails the whole
// deserialization with an error rather than defaulting the field. The TUI's
// `if let Ok(state)` then falls back to the JSONL history: the conversation
// is always recovered, only the compression state is lost — the accepted
// dev-stage tradeoff. This test pins that boundary for the layout BEFORE the
// protection/anchor removal: a legacy snapshot with the old `User` fields and
// the `user_prompts`/`anchor` state is rejected by the new reader.
#[test]
fn snapshot_from_before_the_protection_removal_is_rejected_gracefully() {
    // Only the variants needed to mirror the historical layout are built.
    #[allow(dead_code)]
    #[derive(serde::Serialize)]
    enum LegacyItem {
        // Pre-refactor User: protected/verbatim/compressed fields are gone now.
        User {
            id: u64,
            protected: bool,
            verbatim: bool,
            original: String,
            compressed: Option<String>,
        },
        Assistant {
            id: u64,
            original: String,
            compressed: Option<String>,
            compressible: bool,
        },
        ToolCall {
            id: u64,
            call_id: String,
            name: String,
            arguments: String,
        },
        ToolResult {
            id: u64,
            call_id: String,
            content: String,
            useless: bool,
        },
        LoopClosure {
            id: u64,
            content: String,
        },
    }
    #[derive(serde::Serialize)]
    struct LegacyState {
        items: std::collections::VecDeque<LegacyItem>,
        next_id: u64,
        user_prompts: std::collections::VecDeque<u64>,
        max_tokens: usize,
        anchor: Option<u64>,
    }

    let legacy = LegacyState {
        items: std::collections::VecDeque::from([
            LegacyItem::User {
                id: 1,
                protected: true,
                verbatim: true,
                original: "old prompt".to_string(),
                compressed: None,
            },
            LegacyItem::ToolResult {
                id: 2,
                call_id: "c1".to_string(),
                content: "old result".to_string(),
                useless: true,
            },
        ]),
        next_id: 3,
        user_prompts: std::collections::VecDeque::from([1]),
        max_tokens: 1000,
        anchor: Some(1),
    };
    let bytes = bincode::serialize(&legacy).unwrap();

    // Never panics; the .ctx is discarded and the JSONL fallback takes over.
    assert!(
        bincode::deserialize::<ContextManagerState>(&bytes).is_err(),
        "a legacy snapshot is rejected (JSONL fallback)"
    );
}

#[test]
fn display_info_reports_tokens_and_budget() {
    let mut cm = cm(1000);
    cm.add_user("hello");
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"some tool result payload ".repeat(60));
    let info = cm.display_info();
    assert!(info.total_tokens > 0);
    assert!(info.budget_pct > 0 && info.budget_pct <= 100);
    assert_eq!(
        info.budget_pct,
        ((info.total_tokens as f64 / 1000.0) * 100.0).min(100.0) as u8
    );
}

// Integration: no duplication in the LLM payload

// The system prompt (`build_chat_context`) NEVER contains conversation
// content anymore — the whole conversation lives in the context manager and
// is delivered once, via the messages array.
#[test]
fn assistant_history_turn_is_not_duplicated_system_and_messages() {
    let marker = "UNIQUE_ASSISTANT_MARKER_XYZ";
    let mut h = crate::harness::Harness::new_test().with_history(&[
        ("user".into(), "question".into()),
        ("assistant".into(), marker.into()),
    ]);

    let system_ctx = h.build_chat_context_for_test();
    let msgs = h.build_messages_for_test("");

    // NEGATIVE: conversation content must never leak into the system prompt.
    assert!(
        !system_ctx.contains(marker),
        "conversation content must not leak into the system prompt"
    );

    // POSITIVE: whatever the assistant turn delivers (raw or compressed) must
    // appear in the messages array EXACTLY ONCE — a content-drop would fail
    // this, and a double-send would too.
    let delivered = h
        .context_manager
        .items_snapshot()
        .iter()
        .find_map(|it| match it {
            ContextItem::Assistant {
                original,
                compressed,
                ..
            } => Some(compressed.clone().unwrap_or_else(|| original.clone())),
            _ => None,
        })
        .expect("loaded assistant turn exists");
    let count = msgs
        .iter()
        .filter(|m| m.role == "assistant" && m.content.as_deref() == Some(delivered.as_str()))
        .count();
    assert_eq!(
        count, 1,
        "the loaded assistant turn must be delivered exactly once"
    );
}

// A LoopClosure is delivered exactly once (as an assistant message) and never
// duplicated in the system prompt.
#[test]
fn loop_closure_is_delivered_exactly_once() {
    let marker = "FINAL_TURN_MARKER_XYZ";
    let mut h = crate::harness::Harness::new_test().with_history(&[
        ("user".into(), "question".into()),
        ("assistant".into(), marker.into()),
    ]);
    h.context_manager.close_loop();
    assert!(
        h.context_manager.items.back().unwrap().is_loop(),
        "precondition: the final assistant turn becomes a LoopClosure"
    );

    let system_ctx = h.build_chat_context_for_test();
    let msgs = h.build_messages_for_test("");
    let joined: String = msgs
        .iter()
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let in_system = system_ctx.contains(marker);
    let in_messages = joined.contains(marker);
    // POSITIVE: the LoopClosure must actually be delivered (a silent content
    // drop would otherwise pass a `!(both)` check).
    assert!(
        in_messages,
        "the LoopClosure must be delivered via the messages array"
    );
    assert!(
        !(in_system && in_messages),
        "BUG: the loop's final turn is sent TWICE — as a raw assistant message \
         (in_messages={in_messages}) AND in the system prompt (in_system={in_system})."
    );
    assert!(
        !in_system,
        "LoopClosure content must not leak into the system prompt"
    );
}

// A large loaded history never ships a raw-buffer/truncation system message:
// the single-owner CM carries it in the messages array once.
#[test]
fn loaded_history_produces_no_system_truncation_messages() {
    let marker = "UNIQUE_RAW_BUFFER_MARKER_XYZ";
    let big = format!("{marker} ") + &"word ".repeat(55_000);
    let mut h = crate::harness::Harness::new_test().with_history(&[("user".into(), big.clone())]);

    let msgs = h.build_messages_for_test("hi");
    let system_text: String = msgs
        .iter()
        .filter(|m| m.role == "system")
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !system_text.contains(marker),
        "the messages array must not contain a raw-buffer system message"
    );
    let user_count = msgs
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref() == Some(big.as_str()))
        .count();
    assert_eq!(user_count, 1, "the loaded turn is delivered exactly once");
}

// ── The pipeline is synchronous: compression happens in-phase ────────────
//
// There is no worker thread anymore — the TF-IDF → LSA → MMR pipeline runs on
// the agent loop's thread as phase 1 of the compaction. A panic inside the
// deterministic pipeline degrades to keeping the draft raw (the eviction
// phase remains the fallback); it can never stall or crash the loop.
#[test]
fn pipeline_compresses_drafts_synchronously_and_never_stalls() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    // Four compressible drafts (varied prose so the pipeline has signal).
    for _ in 0..4 {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::Assistant {
            id,
            original: prose_copies(16),
            compressed: None,
            compressible: true,
        });
    }
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // The compaction completes synchronously — no waiting, no stall: the
    // pipeline summarizes every compressible draft in-phase and drops the
    // total below the trigger without evicting a single item.
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "compression alone must drop below the trigger, got {}",
        cm.total_tokens()
    );
    assert_eq!(
        cm.items
            .iter()
            .filter(|it| matches!(
                it,
                ContextItem::Assistant {
                    compressed: Some(_),
                    ..
                }
            ))
            .count(),
        4,
        "every compressible draft is summarized by the pipeline"
    );
    assert_eq!(
        cm.items.len(),
        5,
        "nothing was evicted — the summaries sufficed"
    );

    // The manager is still fully usable: ingest, render and snapshot work.
    cm.add_user("keep going");
    let msgs = cm.build_messages("");
    assert_eq!(msgs.len(), 6, "rendering still works after the pass");
    assert!(cm.display_info().total_tokens > 0);
}

// The pipeline (phase 1) compresses SEGMENT BY SEGMENT: the budget is checked
// at each LoopClosure boundary. When the first segment's drafts alone drop
// the total below 80%, the pipeline stops there and the NEXT segment's drafts
// stay raw — the minimum decompaction.
#[test]
fn pipeline_compresses_drafts_segment_by_segment() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // user prompt — never compressed
    // Segment 1: one big compressible draft (~1300 tokens raw → ~40% after
    // compression), then a small closure. Segment 2: a smaller compressible
    // draft that must stay raw if segment 1 alone drops below the trigger.
    cm.add_assistant(&prose_copies(60), true);
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&prose_copies(5), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    // Compressing segment 1's draft drops the total below 800 at the closure
    // boundary → the pipeline stops; segment 2's draft is never touched.
    assert!(
        cm.total_tokens() < 800,
        "compression alone must drop below the trigger, got {}",
        cm.total_tokens()
    );
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "segment 1's draft is compressed by the pipeline"
    );
    assert!(
        matches!(
            &cm.items[3],
            ContextItem::Assistant {
                compressed: None,
                ..
            }
        ),
        "segment 2's draft stays raw — the pipeline stopped at the closure boundary"
    );
}

// The eviction pass removes ONE chunk per overflow, checks the budget after
// each removal, and parks a persistent cursor — so the next overflow resumes
// exactly where this one stopped. The context lives one overflow at a time
// instead of being decoupled wholesale.
#[test]
fn draft_eviction_removes_one_chunk_per_overflow_and_resumes() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // protected — never evicted
    cm.add_assistant(&"A ".repeat(250), false);
    cm.add_assistant(&"B ".repeat(250), false);
    cm.add_assistant(&"C ".repeat(400), false);
    // 50 + 250 + 250 + 400 = 950 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Trigger 1: exactly ONE chunk (the oldest draft A) is removed — the
    // total (700) drops below 80% and the cursor parks on B.
    let b_id = cm.items[2].id();
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "exactly the oldest chunk is removed"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )) && cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("C ")
        )),
        "B and C survive trigger 1 — only the minimum was decoupled"
    );
    assert_eq!(
        cm.draft_cursor,
        Some(b_id),
        "the cursor parks on the next item to consider"
    );

    // Regrow over the trigger: add D (400) → 50 + 250 + 400 + 400 = 1100.
    cm.add_assistant(&"D ".repeat(400), false);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    // Trigger 2 resumes from the cursor (B), NOT from the start: B (→ 850)
    // then C (→ 450) are removed before stopping below the trigger.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ") || original.starts_with("C ")
        )),
        "phase 2 resumes from the cursor and removes the next chunks"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("D ")
        )),
        "the newest draft survives"
    );
}

// ── Proof: the pipeline is idle in tool-heavy loops ─────────────────────
//
// The compression pipeline only ever consumes compressible assistant texts.
// A realistic agent loop is dominated by tool calls + tool results, which are
// structural and NEVER compressed. So in a tool-heavy session the pipeline
// processes NOTHING — the raw tool content grows until the LLM compaction
// folds it (a summary, not a TF-IDF → LSA → MMR pass).
#[test]
fn tool_loops_leave_the_pipeline_idle() {
    let mut cm = cm(100_000);
    // Exact item sequence `run_agent_loop` produces for a tool-heavy session:
    // user → (assistant-with-tool-call → tool_call → tool_result) × N.
    cm.add_user("implement the feature");
    for i in 0..10 {
        cm.add_assistant(&format!("I will inspect and edit round {i}."), false); // carried a tool call
        cm.add_tool_call(
            &format!("c{i}"),
            "fs_edit",
            r#"{"targets":[{"path":"src/lib.rs"}]}"#,
        );
        cm.add_tool_result(
            &format!("c{i}"),
            &format!("applied edit {i}: {}", "let x = i32;\n".repeat(40)),
        );
    }

    // Far below the 100k trigger → run() must not compress anything.
    cm.run();

    // NOTHING was compressed — every item is still raw/verbatim.
    let compressed_count = cm
        .items
        .iter()
        .filter(|it| {
            matches!(
                it,
                ContextItem::Assistant {
                    compressed: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        compressed_count, 0,
        "a tool loop compresses NOTHING — the pipeline stays idle"
    );

    // The delivered payload is byte-for-byte the raw tool content.
    let msgs = cm.build_messages("");
    assert!(
        msgs.iter().any(|m| {
            m.role == "tool"
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("let x = i32;"))
        }),
        "tool results must be delivered verbatim"
    );
}

// ── Proof: normal chat (no tools) DOES compress dialogue ─────────────────
//
// For a conversational session the pipeline (phase 1) is fed every assistant
// text that carried no tool call. User prompts are protected — never
// compressed, never evicted. This test builds a plain multi-turn chat, fires
// the 80% overflow, and asserts exactly that coverage.
#[test]
fn normal_chat_compresses_dialogue_and_user_input() {
    let mut cm = cm(10_000); // trigger = 8000
    for i in 0..4 {
        cm.add_user(&format!("user question {i}: {}", prose_copies(60)));
        cm.add_assistant(&format!("answer {i}: {}", prose_copies(60)), true);
    }
    assert!(
        cm.total_tokens() >= 8000,
        "precondition: over the 80% trigger"
    );

    cm.run();

    // Every user prompt survives — none compressed, none evicted: the
    // rendered messages carry the ORIGINAL prompt text.
    assert_eq!(
        cm.items
            .iter()
            .filter(|it| matches!(it, ContextItem::User { .. }))
            .count(),
        4,
        "all user prompts survive"
    );
    let msgs = cm.build_messages("");
    for (i, msg) in msgs.iter().filter(|m| m.role == "user").enumerate() {
        assert!(
            msg.content
                .as_deref()
                .is_some_and(|c| c.starts_with(&format!("user question {i}:"))),
            "the ORIGINAL user prompt {i} is delivered verbatim"
        );
    }
    // Every assistant dialogue turn (no tool call) IS compressed.
    let compressed_assistants = cm
        .items
        .iter()
        .filter(|it| {
            matches!(
                it,
                ContextItem::Assistant {
                    compressible: true,
                    compressed: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        compressed_assistants, 4,
        "all plain-chat assistant turns must be compressed"
    );
}

// ── Proof: the loop's final output is never compressed ─────────────────
//
// The last assistant turn of each loop is a CANDIDATE final output: the
// harness only knows it is final when it closes the loop. `close_loop()`
// promotes the ORIGINAL text verbatim into a LoopClosure, so the final
// answer keeps its original form — and with the pipeline only firing on an
// 80% overflow, no CPU was spent summarizing it.
#[test]
fn close_loop_promotes_the_final_output_verbatim() {
    let mut cm = cm(10_000);
    let text = "The final summary of the completed task. ".repeat(60);
    cm.add_user("task");
    cm.add_assistant(&text, true); // candidate final output

    cm.close_loop(); // harness signals the loop is done
    let ContextItem::LoopClosure { content, .. } = &cm.items[1] else {
        panic!("expected LoopClosure");
    };
    assert_eq!(content, &text, "the ORIGINAL text is promoted verbatim");

    // No overflow ever fired → the final output was never compressed.
    assert_eq!(
        cm.items[1].tokens(TokenEncoding::Cl100k),
        estimate_tokens(&text),
        "the final output stays raw forever — it was never compressed"
    );
}

// The final answer is promoted verbatim even when a pipeline pass had already
// compressed the raw draft: `close_loop` promotes the ORIGINAL text.
#[test]
fn close_loop_promotes_original_even_after_the_pipeline_compressed_it() {
    let mut cm = cm(1000); // trigger = 800
    let text = prose_copies(40); // fires the trigger when added
    cm.add_user("task");
    cm.add_assistant(&text, true);

    // Overflow fires BEFORE the loop closes (the budget is exceeded
    // mid-loop): the pipeline compresses the draft…
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    cm.run();
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "the pipeline compressed the candidate final draft"
    );

    // …and close_loop still promotes the ORIGINAL text.
    cm.close_loop();
    let ContextItem::LoopClosure { content, .. } = &cm.items[1] else {
        panic!("expected LoopClosure");
    };
    assert_eq!(content, &text, "the final answer is never compressed");
}

// ── Compaction observer (TUI live feedback) ──────────────────────────────

/// Collect compaction events through the observer. `Arc<Mutex>` because the
/// observer is `Send`-bounded (the harness moves it into a tokio task).
fn collect_events() -> (
    ContextManager,
    std::sync::Arc<std::sync::Mutex<Vec<CompactionEvent>>>,
) {
    let mut cm = cm(1000);
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = sink.clone();
    cm.set_compaction_observer(move |e| sink.lock().unwrap().push(e));
    (cm, events)
}

#[test]
fn compaction_observer_reports_pipeline_lifecycle() {
    let (mut cm, events) = collect_events();

    // A compressible draft that overflows the trigger — the pipeline runs and
    // resolves by itself.
    cm.add_user("before");
    cm.add_assistant(&prose_copies(40), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            CompactionEvent::PipelineStarted,
            CompactionEvent::PipelineFinished
        ],
        "the stopwatch pair fires exactly once around the compression work"
    );
}

#[test]
fn compaction_observer_is_silent_without_compressible_work() {
    let (mut cm, events) = collect_events();

    // A single protected prompt cannot be compressed and never overflows
    // alone — `run` is a no-op and must not surface a stopwatch line.
    cm.add_user("protected prompt");
    cm.run();
    assert!(
        events.lock().unwrap().is_empty(),
        "no work → no pipeline events"
    );
}

#[test]
fn compaction_observer_reports_draft_eviction() {
    let (mut cm, events) = collect_events();

    // Protected user prompts + a compressible draft followed by a big
    // LoopClosure. The pipeline compresses the draft in place but the total
    // stays over (the closure dominates), so phase 2 evicts the draft — and
    // with no draft left, the LLM compaction (phase 3) is requested.
    cm.add_user("q1");
    cm.add_user("q2");
    cm.add_assistant(&prose_copies(40), true); // compressible draft
    cm.add_assistant(&prose_copies(60), true); // becomes the closure
    cm.close_loop();
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    let outcome = cm.run();

    assert_eq!(
        outcome,
        RunOutcome::NeedsLlmCompaction,
        "after the draft is evicted only protected items remain over the trigger"
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            CompactionEvent::PipelineStarted,
            CompactionEvent::PipelineFinished,
            CompactionEvent::DraftsEvicted,
        ],
        "the funnel reports every phase that actually did work"
    );
}

// The LLM compaction request is OUTSIDE the observer: `run` only requests it
// (no event — the harness drives the model call and emits its own lifecycle
// events), and `apply_llm_summary` swaps the timeline silently.
#[test]
fn llm_compaction_is_invisible_to_the_sync_observer() {
    let (mut cm, events) = collect_events();
    // Protected-only timeline over the trigger: no deterministic phase has
    // anything to do, yet `run` still requests the LLM compaction — and the
    // request + the apply are completely silent to the sync observer (the
    // harness drives the model call and emits its own lifecycle events).
    cm.add_user(&"p ".repeat(300));
    cm.add_user(&"P ".repeat(400));
    let id = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id,
        content: "L ".repeat(600),
    });
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    assert_eq!(cm.run(), RunOutcome::NeedsLlmCompaction);
    cm.apply_llm_summary("## Objective\n- continue".to_string());

    assert!(
        events.lock().unwrap().is_empty(),
        "the sync observer reports nothing for the LLM compaction"
    );
}

// ── Context window discovery ───────────────────────────────────────────────

#[tokio::test]
async fn with_discovered_context_uses_discovered_value() {
    // Test with a known OpenRouter model that should have a context window
    let cm = ContextManager::with_discovered_context("openai/gpt-4o").await;

    // The discovered context window should be greater than the default
    assert!(
        cm.display_info().max_tokens > MAX_CONTEXT_TOKENS,
        "Discovered context window should be greater than default"
    );
}

#[tokio::test]
async fn with_discovered_context_falls_back_to_default() {
    // Test with an invalid model name that won't be found
    let cm = ContextManager::with_discovered_context("invalid-model-xyz-12345").await;

    // Should fall back to the default MAX_CONTEXT_TOKENS
    assert_eq!(
        cm.display_info().max_tokens,
        MAX_CONTEXT_TOKENS,
        "Invalid model should fall back to default MAX_CONTEXT_TOKENS"
    );
}

#[tokio::test]
async fn with_discovered_context_works_with_claude_models() {
    // Test with a Claude model (should try Anthropic API)
    let cm = ContextManager::with_discovered_context("claude-opus-5").await;

    // Either discover the real context window or fall back to default
    // The important thing is that it doesn't crash
    let max_tokens = cm.display_info().max_tokens;
    assert!(
        max_tokens >= MAX_CONTEXT_TOKENS,
        "Context window should be at least the default value"
    );
}

#[tokio::test]
async fn with_discovered_context_creates_valid_context_manager() {
    // Test that the created ContextManager is valid and functional
    let mut cm = ContextManager::with_discovered_context("openai/gpt-4o").await;

    // Verify basic functionality works
    assert_eq!(cm.total_tokens(), 0, "New context manager should be empty");
    assert!(
        cm.items.is_empty(),
        "New context manager should have empty items"
    );

    // Add some content and verify it works
    cm.add_user("test message");
    assert!(
        cm.total_tokens() > 0,
        "Should track tokens after adding content"
    );
    assert!(
        !cm.items.is_empty(),
        "Should have items after adding content"
    );
}

// ── Split-and-concatenate (the known-window contingency) ─────────────────
//
// The split processes the timeline in SEQUENTIAL CHUNKS of whole items in
// historical order; each chunk's summary is staged into a buffer; only when
// EVERY item has been consumed does commit_split replace the timeline with
// the concatenated buffer — atomically. The timeline is untouched during the
// whole process.

// A single item larger than the chunk budget is still included WHOLE (never
// cut mid-item); the next chunk starts after it, carrying the continuity
// snippet so the final buffer reads seamlessly.
#[test]
fn split_chunks_whole_items_in_order_with_continuity_and_commits_atomically() {
    let mut cm = cm(10_000);
    // The first item alone vastly exceeds the chunk budget (window 10k →
    // budget 2k) — it must still be included whole as the first chunk.
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_assistant("short a", true); // id 2
    cm.add_assistant("short b", true); // id 3
    cm.close_loop(); // id 3 → LoopClosure
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
    // The template is NOT restarted: the unique template instruction and its
    // skeleton sections never reappear — only the continuity tail (which may
    // legitimately start with "## Objective") is carried over.
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

// The limit message is SILENT until the buffer accumulation becomes
// concerning (the projection crosses the warning threshold); from then on it
// carries a proportional target so the final buffer stays under the ceiling.
#[test]
fn split_warns_only_when_the_buffer_accumulation_is_concerning() {
    let mut cm = cm(10_000);
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    cm.add_user("second prompt"); // id 2
    cm.begin_split(10_000); // ceiling = 4000, warn_at = 3200

    let projection = cm.split_projection();
    assert_eq!(projection.ceiling, 4000, "40% of the window is the ceiling");
    assert_eq!(
        projection.warn_at, 3200,
        "80% of the ceiling is the warning point"
    );
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

// Atomicity: aborting a split leaves the timeline EXACTLY as it was — the
// buffer and cursor are dropped, nothing is removed.
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
        cm.items_snapshot().iter().any(|it| {
            matches!(it, ContextItem::User { original, .. } if original == "keep me")
        })
    );
}

// The commit's hard limit is the KNOWN WINDOW (the ceiling is only the
// planning target): a pathologically bloated anchor that overshoots the
// window must NOT commit — the timeline stays exactly as it was (atomicity).
#[test]
fn commit_split_requires_the_buffer_to_fit_the_window() {
    let mut cm = cm(10_000);
    cm.add_user("keep me verbatim");
    cm.begin_split(100); // tiny window: the hard limit is tiny too
    let request = cm.split_next_chunk().expect("a chunk");
    // A pathologically bloated summary overshoots the 100-token window.
    cm.advance_split(&"overshoot ".repeat(200), request.chunk_end);
    assert!(cm.split_all_consumed());

    assert!(
        !cm.commit_split(),
        "an anchor larger than the window must not commit"
    );
    // Atomicity: nothing was replaced — the original timeline is intact.
    assert_eq!(cm.items_snapshot().len(), 1);
    assert!(
        cm.items_snapshot().iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } if original == "keep me verbatim"
        )),
        "the timeline stays exactly as it was"
    );
}

// Committing without a staged split — or with an empty buffer (nothing was
// summarized) — is a no-op that leaves everything as it is.
#[test]
fn commit_split_without_work_returns_false() {
    let mut cm = cm(10_000);
    cm.add_user("hi");
    assert!(!cm.commit_split(), "no active split → false");
    cm.begin_split(10_000);
    assert!(
        !cm.commit_split(),
        "an empty buffer (nothing summarized) must not wipe the timeline"
    );
    assert_eq!(cm.items_snapshot().len(), 1, "nothing was committed");
}

// The staging is persisted with the snapshot: an interrupted split resumes
// EXACTLY where it stopped — the summarized chunk is never re-sent.
#[test]
fn split_staging_survives_save_restore_and_resumes() {
    let mut cm = cm(10_000);
    cm.add_user(&"abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(2000)); // id 1
    // Huge enough (≫ any chunk budget below the 10k window, whatever the
    // overhead reserve is) to fill chunk 2 on its own, so the small third
    // part forms a real final chunk.
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
    let ContextItem::Compaction { summary, .. } = &restored.items_snapshot()[0] else {
        panic!("expected the committed anchor");
    };
    assert!(summary.contains("part one") && summary.contains("part three"));
}

// ── Retention: old tool results are trimmed under budget pressure ────────
//
// A tool-heavy session accumulates every full tool output forever (tool
// chains are protected structural items), so long sessions grow unbounded
// until the LLM compaction. The retention pass trims the CONTENT of OLD tool
// results in place — the chain stays a valid `tool` message with its
// `call_id` — leaves the NEWEST chain full, and resolves the overflow without
// an LLM call. Already-trimmed results are never re-trimmed.
#[test]
fn trim_stale_tool_results_bounds_old_results_under_pressure() {
    let mut cm = cm(4000); // trigger = 3200
    let huge = "A ".repeat(100_000); // far over the trigger on its own
    let tiny = "B ".to_string();
    cm.add_user("short prompt");
    cm.add_tool_call("c1", "fs_read", "{}");
    cm.add_tool_result("c1", &huge);
    cm.add_tool_call("c2", "fs_read", "{}");
    cm.add_tool_result("c2", &tiny);
    // No compressible drafts (protected user prompt + structural tool chains):
    // the funnel can only go pipeline → eviction → retention.
    assert_eq!(cm.run(), RunOutcome::Resolved);

    let items = cm.items_snapshot();
    let c1 = items
        .iter()
        .find_map(|it| match it {
            ContextItem::ToolResult { call_id, content, .. } if call_id == "c1" => Some(content),
            _ => None,
        })
        .expect("chain 1 result still present");
    let c2 = items
        .iter()
        .find_map(|it| match it {
            ContextItem::ToolResult { call_id, content, .. } if call_id == "c2" => Some(content),
            _ => None,
        })
        .expect("chain 2 result still present");
    assert!(
        c1.starts_with(TOOL_RESULT_TRIM_MARKER),
        "the OLD result is trimmed in place; c1={c1:?}"
    );
    assert_eq!(c2, &tiny, "the NEWEST chain stays full");
    // The structural halves of BOTH chains are intact: the native
    // `tool_call → tool` pairing the providers require never breaks.
    for cid in ["c1", "c2"] {
        assert!(cm.items_snapshot().iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == cid
        )), "call {cid} still present");
    }

    // Idempotency + fall-through: push the total back over the trigger with a
    // protected (incompressible) user prompt. The second `run()` finds the old
    // result already trimmed (no re-trim), the newest chain still full and
    // protected → only the LLM compaction can resolve it.
    cm.add_user(&"P ".repeat(100_000));
    assert_eq!(cm.run(), RunOutcome::NeedsLlmCompaction);

    let items = cm.items_snapshot();
    let c1 = items
        .iter()
        .find_map(|it| match it {
            ContextItem::ToolResult { call_id, content, .. } if call_id == "c1" => Some(content),
            _ => None,
        })
        .unwrap();
    let c2 = items
        .iter()
        .find_map(|it| match it {
            ContextItem::ToolResult { call_id, content, .. } if call_id == "c2" => Some(content),
            _ => None,
        })
        .unwrap();
    assert!(
        c1.starts_with(TOOL_RESULT_TRIM_MARKER),
        "the already-trimmed result is not re-trimmed; c1={c1:?}"
    );
    assert_eq!(c2, &tiny, "the newest chain is still full");
}
