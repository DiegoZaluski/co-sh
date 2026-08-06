//! Tests for the single-owner LLM-free SYNCHRONOUS context manager.
//!
//! Coverage: in-place pipeline compression (1 item = 1 message), protected
//! user prompts, the native `tool_call → tool` structural chain, `LoopClosure`
//! promotion + guard, the 80/40 four-phase compaction (pipeline → gradual
//! draft eviction → tools → closures), message rendering (positions + roles),
//! save/restore round-trip, display info, and integration with the harness
//! payload.

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

#[test]
fn two_most_recent_user_prompts_are_protected() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user("first prompt");
    cm.add_user("second prompt");
    // Both are protected — and with no worker, nothing is compressed either.
    assert!(cm.items.iter().all(|it| {
        matches!(
            it,
            ContextItem::User {
                protected: true,
                compressed: None,
                ..
            }
        )
    }));

    // A third prompt un-protects the FIRST one — the session anchor, which is
    // verbatim and therefore NEVER compressed by the pipeline (it stays raw).
    cm.add_user("third prompt");
    let protected = cm
        .items
        .iter()
        .filter(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(protected, 2, "exactly two protected prompts");
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                protected: false,
                verbatim: true,
                compressed: None,
                ..
            }
        ),
        "the session anchor is un-protected but stays verbatim — never compressed"
    );

    // A fourth prompt un-protects the SECOND one — a normal (non-verbatim)
    // prompt — which the pipeline WILL compress at the next overflow. Fire it
    // with a big compressible draft: the pipeline summarizes the normal
    // prompt while the verbatim anchor stays raw.
    cm.add_user("fourth prompt");
    cm.add_assistant(&prose_copies(40), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    cm.run();
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::User {
                protected: false,
                compressed: Some(_),
                ..
            }
        ),
        "a non-verbatim un-protected prompt compresses via the pipeline"
    );
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                compressed: None,
                ..
            }
        ),
        "the verbatim anchor is never compressed"
    );
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

// Proof: the duplicate-input guard rests on a real invariant
//
// `build_messages` skips appending `current_input` when the last rendered
// message is a user with the same content, while `run_agent_loop` skips
// `add_user` via `last_user_equals` (which compares the original text). These
// two guards are consistent ONLY because the last user prompt in the CM is
// ALWAYS protected (and therefore rendered verbatim, never compressed). This
// test proves that invariant holds even after older prompts get compressed.
#[test]
fn last_user_prompt_is_always_protected_and_verbatim() {
    let mut cm = cm(10_000);
    for i in 0..5 {
        cm.add_user(&format!("prompt {i}"));
    }

    // The last user prompt is protected and uncompressed (verbatim).
    let last_user = cm
        .items
        .iter()
        .rev()
        .find(|it| matches!(it, ContextItem::User { .. }))
        .expect("there is a user prompt");
    assert!(matches!(
        last_user,
        ContextItem::User {
            protected: true,
            compressed: None,
            ..
        }
    ));

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

// Proof: restore keeps the protected bookkeeping consistent
//
// After a save/restore of a session with protected prompts, a NEW user prompt
// must un-protect the OLDEST protected prompt (not a compressed one) — exactly
// as it would without the restore. The oldest prompt is the session anchor,
// so it stays verbatim; a subsequent prompt un-protects a normal prompt which
// IS compressed, proving the bookkeeping stayed consistent.
#[test]
fn restore_then_new_user_unprotects_oldest_prompt() {
    let mut manager = cm(1000);
    manager.add_user("first prompt"); // session anchor
    manager.add_user("second prompt");
    let state = manager.save_state();

    let mut restored = cm(1000);
    restored.restore_state(&state);
    assert_eq!(restored.items.len(), 2);
    assert_eq!(restored.user_prompts.len(), 2);
    assert!(
        matches!(restored.anchor, Some(PromptAnchor { id: 1 })),
        "the restored session keeps its anchor"
    );

    // A new prompt arrives on the restored session → un-protects the OLDEST
    // protected prompt (the session anchor) — verbatim, so it stays raw.
    restored.add_user("third prompt");
    assert!(
        matches!(
            &restored.items[0],
            ContextItem::User {
                protected: false,
                verbatim: true,
                compressed: None,
                ..
            }
        ),
        "the restored anchor is un-protected but verbatim (never compressed)"
    );
    let protected = restored
        .items
        .iter()
        .filter(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        protected, 2,
        "exactly two protected prompts after the transition"
    );

    // A fourth prompt un-protects the SECOND prompt (a normal one). With no
    // worker, compression waits for the next 80% overflow — fire it with a
    // big compressible draft and the pipeline summarizes the normal prompt:
    // restore kept the bookkeeping consistent.
    restored.add_user("fourth prompt");
    restored.add_assistant(&prose_copies(40), true);
    assert!(
        restored.total_tokens() >= 800,
        "precondition: over the trigger"
    );
    restored.run();
    assert!(
        matches!(
            &restored.items[1],
            ContextItem::User {
                protected: false,
                compressed: Some(_),
                ..
            }
        ),
        "the non-verbatim prompt compresses exactly as without a restore"
    );
}

// build_messages input handling

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

// Compaction phases

#[test]
fn evict_tools_removes_tools_middle_out_and_preserves_chains() {
    let mut cm = cm(1000);
    for i in 0..3 {
        cm.add_tool_call(&format!("c{i}"), "read_file", &"x".repeat(100));
        cm.add_tool_result(&format!("c{i}"), &"payload ".repeat(300));
    }
    assert_eq!(cm.items.len(), 6);
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "the tool eviction must drop the context below the 80% trigger, got {}",
        cm.total_tokens()
    );
    assert!(cm.items.len() < 6);
    // Chain integrity: no tool item survives without its call/result partner.
    let snapshot: Vec<ContextItem> = cm.items.iter().cloned().collect();
    for item in &snapshot {
        match item {
            ContextItem::ToolCall { call_id, .. } => {
                assert!(snapshot.iter().any(|it| {
                    matches!(it, ContextItem::ToolResult { call_id: c, .. } if c == call_id)
                }));
            }
            ContextItem::ToolResult { call_id, .. } => {
                assert!(snapshot.iter().any(|it| {
                    matches!(it, ContextItem::ToolCall { call_id: c, .. } if c == call_id)
                }));
            }
            _ => {}
        }
    }
}

// The middle-out ORDER matters: the tool eviction must remove the middle tool
// chains first and preserve the NEWEST tool (the most relevant recent
// context). A wrong order that removed the newest tool first would pass the
// generic `evict_tools_removes_tools_middle_out_and_preserves_chains` test
// (any order gets under the trigger), so this test calls `evict_tools`
// directly.
//
// Asserting only that the NEWEST chain survives does NOT distinguish
// middle-out from oldest-first (both preserve the newest). Asserting that the
// MIDDLE chain (c2, the first removal target) is gone does: oldest-first
// would keep c2 and drop c0/c1 instead. Together the two asserts pin the
// exact middle-out order.
#[test]
fn evict_tools_preserves_the_newest_tool_chain() {
    let mut cm = cm(1000); // trigger = 800
    // 4 small chains (~153 tokens) + 1 big newest chain (~403 tokens) = 1015.
    for i in 0..5 {
        let size = if i == 4 {
            "w ".repeat(400)
        } else {
            "x ".repeat(150)
        };
        cm.add_tool_call(&format!("c{i}"), "read_file", "{}");
        cm.add_tool_result(&format!("c{i}"), &size);
    }
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.evict_tools();

    assert!(
        cm.total_tokens() < 800,
        "evict_tools must drop below the 80% trigger, got {}",
        cm.total_tokens()
    );
    // Middle-out: the NEWEST chain survives (call + result both present) …
    assert!(
        cm.items
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c4") }),
        "the newest tool call must survive middle-out removal"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolResult { call_id, .. } if call_id == "c4") }),
        "the newest tool result must survive middle-out removal"
    );
    // … and the MIDDLE chain (first removal target) is gone — the property
    // that separates middle-out from oldest-first removal.
    assert!(
        !cm.items
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c2") }),
        "the middle chain must be removed FIRST by middle-out removal"
    );
}

// The `useless` marker inverts the middle-out priority: a chain whose RESULT
// is marked `useless` (e.g. a zero-match search) is the FIRST eviction target
// of the tool pass — even when it is the NEWEST chain, which pure middle-out
// would otherwise preserve. On this layout middle-out removes the middle chain
// (c2) first and keeps the newest (c4); useless-first removes c4 and keeps c2,
// so asserting the exact opposite survival pins the useless-first order.
#[test]
fn evict_tools_prefers_useless_chains_first_even_when_newest() {
    let mut cm = cm(1000); // trigger = 800
    // Four small normal chains (~123 tokens each, "x " ≈ 1 token/2 chars)
    // + one big USELESS newest chain (~352) ≈ 844 — over the trigger, but
    // removing JUST the useless chain drops below it (so nothing else can be
    // evicted — proving the useless chain went first by choice, not by order).
    for i in 0..4 {
        cm.add_tool_call(&format!("c{i}"), "read_file", "{}");
        cm.add_tool_result(&format!("c{i}"), &"x ".repeat(120));
    }
    cm.add_tool_call("c4", "read_file", "{}");
    cm.add_tool_result_flagged("c4", &"y ".repeat(350), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.evict_tools();

    assert!(
        cm.total_tokens() < 800,
        "evict_tools must drop below the trigger, got {}",
        cm.total_tokens()
    );
    // The useless NEWEST chain is gone first …
    assert!(
        !cm.items
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c4") }),
        "the useless chain must be evicted even though it is the newest"
    );
    // … while a non-useless MIDDLE chain survives — the opposite of pure
    // middle-out, which would evict c2 first and preserve c4.
    assert!(
        cm.items
            .iter()
            .any(|it| { matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c2") }),
        "the non-useless middle chain must survive the useless-first eviction"
    );
    // Chain integrity: no tool item survives without its call/result partner.
    let snapshot: Vec<ContextItem> = cm.items.iter().cloned().collect();
    for item in &snapshot {
        match item {
            ContextItem::ToolCall { call_id, .. } => {
                assert!(snapshot.iter().any(|it| {
                    matches!(it, ContextItem::ToolResult { call_id: c, .. } if c == call_id)
                }));
            }
            ContextItem::ToolResult { call_id, .. } => {
                assert!(snapshot.iter().any(|it| {
                    matches!(it, ContextItem::ToolCall { call_id: c, .. } if c == call_id)
                }));
            }
            _ => {}
        }
    }
}

// The useless marker is metadata for the eviction pass only: it is recorded on
// the flagged result, defaults to false for the plain ingestion path, and never
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

#[test]
fn pipeline_compresses_drafts_and_keeps_loop_closures_and_protected_prompts() {
    let mut cm = cm(1000);
    // 50 (protected) + 50 (loop) + ~850 (two compressible prose drafts) =
    // ~950, which fires the 80% trigger (800). The pipeline (phase 1)
    // SUMMARIZES the drafts in place — the cheapest decompaction — instead of
    // removing them: nothing is evicted, and trim_loop_closures never fires
    // (closures < 40%).
    cm.items.push_back(ContextItem::User {
        id: 1,
        protected: true,
        verbatim: false,
        original: "x ".repeat(50),
        compressed: None,
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
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User {
                protected: true,
                ..
            }
        )),
        "protected user prompt must survive the pipeline"
    );
}

#[test]
fn trim_loop_closures_removes_oldest_loop_closure_when_closures_hold_40_percent() {
    let mut cm = cm(1000);
    // Three ~300-token closures = 900 ≥ 800 trigger; closures alone ≥ 40%
    // (400). Oldest-first: removing the first drops 900 → 600 (still ≥ 400),
    // removing the second drops to 300 (< 400), so exactly one survives.
    for label in ["old", "mid", "new"] {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::LoopClosure {
            id,
            content: format!("{label} ").repeat(300),
        });
    }
    cm.run();
    assert!(
        cm.total_tokens() < 400,
        "after trim_loop_closures total must be < 40%, got {}",
        cm.total_tokens()
    );
    assert_eq!(cm.items.len(), 1, "only the newest closure should survive");
    let ContextItem::LoopClosure { content, .. } = &cm.items[0] else {
        panic!("expected LoopClosure");
    };
    assert!(content.starts_with("new "), "newest closure survives");
}

// The compaction runs on a TOGGLE: the pipeline leads the first overflow;
// the draft pass (phase 2) is the sticky lead that resumes from its
// persistent cursor across overflows and only hands the lead to the tool
// pass once it reaches a LoopClosure (or the end of the timeline still over
// the trigger). Drafts here are non-compressible (tool-carrying) assistant
// outputs so the pipeline (the initial toggle lead) has nothing to summarize
// and falls through to the draft pass.
#[test]
fn run_evicts_drafts_before_tools_and_resumes_from_the_cursor() {
    let mut cm = cm(1000); // trigger = 800

    // Trigger 1 content: protected user (50) + drafts A/B (250 each) + tool
    // chain c1 (~503) = ~1053 ≥ 800.
    cm.add_user(&"u ".repeat(50));
    cm.add_assistant(&"A ".repeat(250), false); // draft A — carried a tool call
    cm.add_assistant(&"B ".repeat(250), false); // draft B
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(500));
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Trigger 1 — phase 2 (drafts) before phase 3 (tools): the drafts are the
    // first eviction target and the tool chain survives — the task's tool
    // data lives one more cycle. A (250) is removed → 803 ≥ 800, so B goes
    // too → 553 < 800, and the pass stops with the cursor parked on c1.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("A ") || original.starts_with("B ")
        )),
        "the drafts are evicted first — phase 2 before phase 3"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "the tool chain survives the draft pass — tool data lives one more cycle"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User {
                protected: true,
                ..
            }
        )),
        "the protected prompt is untouchable"
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

    // Trigger 2 — phase 2 RESUMES from the cursor (parked on c1): tool chains
    // are skipped, X (→ 1106) and Y (→ 856) are removed, and the end of the
    // timeline ends phase 2's jurisdiction (still over 800). Phase 3 finally
    // fires: middle-out removes the OLDEST tool chain c1 (→ 353 < 800), so
    // the newest chain c2 survives.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "c1"
        )),
        "phase 3 evicts the oldest tool chain once phase 2 has exhausted the drafts"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c2")),
        "the newest tool chain survives the middle-out eviction"
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
// only other content is LoopClosures — the tool eviction has no tools and the
// draft eviction has nothing removable (every item is protected or a
// closure). The rotation must still reach trim_loop_closures: it is the ONLY
// eviction that can relieve the budget, and it trims closures one at a time
// until their SUM is < 40% — the newest summary survives. (Regression guard
// for a design where a big user prompt would have skipped the closure
// trimming entirely.)
#[test]
fn run_reaches_loop_closure_trimming_when_protected_prompts_alone_overflow() {
    let mut cm = cm(1000); // trigger = 800, target = 400, budget = 1000

    // Two protected user prompts (the "big prompt" scenario): ~700 tokens.
    cm.add_user(&"p ".repeat(300));
    cm.add_user(&"P ".repeat(400));
    // Three accumulated LoopClosures: ~600 tokens. Total ~1300 — over the
    // trigger AND over the budget, with nothing removable but the closures.
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

    cm.run();

    // trim_loop_closures trims closures one at a time until their SUM drops
    // below 40% (400): the two oldest closures go, the newest (200) survives.
    // The 40% limit applies to the closures, not the whole context.
    assert!(
        cm.total_tokens() <= 1000,
        "trim_loop_closures must fire to bring the total back under the budget, got {}",
        cm.total_tokens()
    );
    let survivors: Vec<&ContextItem> = cm.items.iter().filter(|it| it.is_loop()).collect();
    assert_eq!(
        survivors.len(),
        1,
        "trim_loop_closures must stop once the SUM of closures is < 40% — the newest summary survives"
    );
    let ContextItem::LoopClosure { content, .. } = survivors[0] else {
        panic!("expected LoopClosure");
    };
    assert!(content.starts_with("new "), "newest closure survives");
    assert_eq!(
        cm.items.iter().filter(|it| it.is_protected()).count(),
        2,
        "protected prompts are untouchable"
    );
}

// First-trigger behavior with NO tool calls: the tool eviction no-ops on an
// empty tool scan and the draft eviction alone must converge the pass below
// the 80% trigger, removing drafts oldest-first — the newest draft and every
// LoopClosure/protected prompt survive. Proves the no-tools path can never
// stall or skip the cleanup.
#[test]
fn first_trigger_without_tools_is_handled_by_draft_eviction() {
    let mut cm = cm(1000); // trigger = 800, target = 400

    // Protected user (50) + oldest draft A (500) + closure (100) + newest
    // draft B (100) + closure (100) = 850. NO tool items anywhere.
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
    // Oldest-first: the oldest draft is gone, the newest draft survives.
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
    // LoopClosures and protected prompts are untouchable.
    assert_eq!(
        cm.items.iter().filter(|it| it.is_loop()).count(),
        2,
        "both LoopClosures survive the draft eviction (skipped) and trim_loop_closures (closures < 40%)"
    );
    assert_eq!(
        cm.items.iter().filter(|it| it.is_protected()).count(),
        1,
        "the protected prompt survives"
    );
}

// Distribution across content types across two triggers: the draft pass
// removes the drafts OLDEST-FIRST with the minimum decompaction (pass 1
// removes only the oldest draft and every tool chain survives — the tool data
// lives on), and pass 2 RESUMES from the cursor, still removing drafts before
// any tool chain is touched. Removal never hits recent context to start the
// next phase.
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
    // Only the OLDEST draft was removed — every tool chain AND the newer
    // drafts survive, so the pass removed the minimum and never the recent
    // context.
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
        "ALL tool chains survive pass 1 — the tool data lives on"
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
    // are removed one at a time until the total finally crosses below the
    // trigger — so the pass stops there. No tool chain was touched: the draft
    // pass always runs to completion before the tool pass.
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
        "ALL tool chains survive — phase 3 never fires while drafts remain removable"
    );
}

// ── The task anchor (drafts-first + anchor) ──────────────────────────────

// The session anchor (the first user prompt) carries the user's original
// intent: it is NEVER submitted to the compression pipeline (stays verbatim)
// and is protected from draft eviction even after it loses its "protected"
// status (a third prompt un-protects it). While the anchor is current, draft
// eviction skips it; a non-anchor un-protected prompt still compresses
// normally — proving the exclusion is by verbatim anchor, not by "unprotected
// prompts never compress".
#[test]
fn anchor_survives_draft_eviction_and_stays_verbatim() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"F ".repeat(100)); // session anchor — verbatim
    cm.add_user(&"S ".repeat(50));
    cm.add_user(&"T ".repeat(50)); // un-protects the anchor
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                protected: false,
                verbatim: true,
                compressed: None,
                ..
            }
        ),
        "the anchor is un-protected but verbatim — never submitted"
    );

    // Heavy drafts fire the trigger.
    cm.add_assistant(&"da ".repeat(300), false);
    cm.add_assistant(&"db ".repeat(300), false);
    cm.add_assistant(&"dc ".repeat(300), false);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    // The anchor survives the draft eviction while the oldest drafts drop.
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                protected: false,
                verbatim: true,
                compressed: None,
                original,
                ..
            } if original.starts_with("F ")
        ),
        "the anchor must survive draft eviction, verbatim"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("da ") || original.starts_with("db ")
        )),
        "the oldest drafts are the first eviction target"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("dc ")
        )),
        "the newest draft survives"
    );
    assert!(
        matches!(cm.anchor, Some(PromptAnchor { id: 1 })),
        "the anchor is still the first prompt"
    );
    assert_eq!(
        cm.total_tokens(),
        estimate_tokens(&"F ".repeat(100))
            + estimate_tokens(&"S ".repeat(50))
            + estimate_tokens(&"T ".repeat(50))
            + estimate_tokens(&"dc ".repeat(300)),
        "anchor (100) + 50 + 50 + 300 (newest draft) after the eviction"
    );

    // A new user prompt arrives; the anchor stays the current task anchor and
    // is never compressed — and with no overflow the pipeline does not fire
    // at all, so everything stays raw (the verbatim anchor is never a
    // candidate regardless).
    cm.add_user(&"V ".repeat(50));
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                compressed: None,
                ..
            }
        ),
        "the anchor is still raw — never submitted to the pipeline"
    );
}

// The anchor is not eternal: it lives for one task segment. When a new
// segment starts — the first user prompt AFTER a LoopClosure — the anchor
// moves to the new prompt, and the OLD anchor becomes an ordinary removable
// prompt (it can finally be evicted by draft compaction, though it is still
// never compressed).
#[test]
fn anchor_moves_to_new_segment_after_loop_closure() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"one ".repeat(100)); // id 1 — session anchor
    cm.add_assistant(&"done with task one. ".repeat(30), true); // id 2
    cm.close_loop(); // id 2 → LoopClosure — closes segment 1
    cm.add_user(&"two ".repeat(100)); // id 3 — NEW anchor (after a LoopClosure)
    cm.add_user(&"three ".repeat(100)); // id 4 — un-protects the old anchor (id 1)
    assert!(
        matches!(cm.anchor, Some(PromptAnchor { id: 3 })),
        "the anchor moved to the new segment"
    );
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                protected: false,
                verbatim: true,
                original,
                ..
            } if original.starts_with("one ")
        ),
        "the old anchor is un-protected but stays verbatim (never compressed)"
    );

    // Heavy drafts fire the trigger: the OLD anchor is now a plain prompt and
    // is the first eviction target; the NEW anchor (task two) is untouched.
    cm.add_assistant(&"da ".repeat(300), false);
    cm.add_assistant(&"db ".repeat(300), false);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } if original.starts_with("one ")
        )),
        "the OLD anchor becomes an ordinary prompt and is evicted once the anchor moves"
    );
    assert!(
        matches!(cm.anchor, Some(PromptAnchor { id: 3 })),
        "the new anchor is still current"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } if original.starts_with("two ")
        )),
        "the NEW anchor (task two) survives the draft eviction"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("db ")
        )),
        "the newest draft survives"
    );
}

// ── Proof: the pipeline is idle in tool-heavy loops ─────────────────────
//
// The compression pipeline only ever consumes compressible assistant texts
// and un-protected user prompts. A realistic agent loop is dominated by tool
// calls + tool results, which are structural and NEVER compressed. So in a
// tool-heavy session the pipeline processes NOTHING — the raw tool content
// grows until the 80% compaction evicts chains (a drop, not a TF-IDF → LSA →
// MMR pass).
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
                ContextItem::User {
                    compressed: Some(_),
                    ..
                } | ContextItem::Assistant {
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

// ── Proof: normal chat (no tools) DOES compress dialogue + user input ────
//
// For a conversational session the pipeline (phase 1) is fed: user prompts
// beyond the two most recent, and every assistant text that carried no tool
// call. The two most recent prompts stay protected and the session anchor
// stays verbatim. This test builds a plain multi-turn chat, fires the 80%
// overflow, and asserts exactly that coverage.
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

    // The two most recent user prompts are protected (raw)...
    let protected = cm
        .items
        .iter()
        .filter(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        protected, 2,
        "exactly the two most recent prompts stay protected"
    );
    // ...the session anchor (first prompt) stays verbatim/raw — never
    // compressed — while the older non-verbatim prompt IS compressed.
    assert!(
        matches!(
            &cm.items[0],
            ContextItem::User {
                compressed: None,
                ..
            }
        ),
        "the session anchor is never compressed"
    );
    assert!(
        cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: false,
                    compressed: Some(_),
                    ..
                }
            )
        }),
        "older user input must be compressed"
    );
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

// ── Phase 2 gradualism ─────────────────────────────────────────────────
//
// The gradual draft eviction removes ONE chunk per overflow, checks the
// budget after each removal, and parks a persistent cursor — so the next
// overflow resumes exactly where this one stopped. The context lives one
// overflow at a time instead of being decoupled wholesale.
#[test]
fn draft_eviction_removes_one_chunk_per_overflow_and_resumes() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor (verbatim) — never evicted
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

#[test]
fn restore_never_compresses_protected_user_prompts() {
    let mut manager = cm(10_000);
    manager.add_user(&"first prompt that must stay verbatim ".repeat(30));
    manager.add_user(&"second prompt that must stay verbatim ".repeat(30));
    // Both are protected and nothing has been compressed (no overflow yet).
    assert!(manager.items.iter().all(|it| {
        matches!(
            it,
            ContextItem::User {
                protected: true,
                compressed: None,
                ..
            }
        )
    }));
    let state = manager.save_state();

    let mut restored = cm(10_000);
    restored.restore_state(&state);

    // Without a budget overflow nothing is compressed — and protected prompts
    // are never pipeline candidates anyway.
    assert!(
        restored.items.iter().all(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: true,
                    compressed: None,
                    ..
                }
            )
        }),
        "BUG: restore_state compressed protected user prompts — the active \
         session anchors must stay verbatim"
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
    m.add_user("task one"); // id 1, session anchor
    m.add_assistant(&prose_copies(40), true); // id 2 — gets compressed by run()
    m.run(); // pipeline compresses the draft in place
    m.add_user("task two"); // id 3, protected (no segment boundary → anchor stays 1)
    m.add_tool_call("c1", "search", "{\"q\":\"x\"}"); // id 4
    m.add_tool_result_flagged("c1", "no matches", true); // id 5 — useless

    let bytes = bincode::serialize(&m.save_state()).unwrap();
    let state: ContextManagerState = bincode::deserialize(&bytes).unwrap();

    let mut r = cm(1000);
    r.restore_state(&state);

    // Items, anchor, the useless flag and the compressed copy all survive.
    assert_eq!(r.items_snapshot().len(), 5);
    assert!(matches!(r.anchor, Some(PromptAnchor { id: 1 })));
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
    m.add_user("q1"); // 1 — session anchor
    m.add_assistant("completed answer one", true); // 2
    m.close_loop(); // → LoopClosure (id 2)
    m.add_user("q2"); // 3 — anchor of the new segment
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
// is always recovered, only the compression/closure state is lost — the
// accepted dev-stage tradeoff (documented in commit 9837ad6). These two
// tests pin that boundary, so a future format fix (e.g. a versioned envelope)
// has concrete regression coverage.
#[test]
fn snapshot_without_anchor_field_is_rejected_gracefully() {
    // Only the variants needed to mirror the historical layout are built.
    #[allow(dead_code)]
    #[derive(serde::Serialize)]
    enum NoAnchorItem {
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
        // Current layout — only the stream-final `anchor` is missing.
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
    struct NoAnchorState {
        items: std::collections::VecDeque<NoAnchorItem>,
        next_id: u64,
        user_prompts: std::collections::VecDeque<u64>,
        max_tokens: usize,
        // No trailing `anchor` field (pre-9837ad6 layout).
    }

    let legacy = NoAnchorState {
        items: std::collections::VecDeque::from([
            NoAnchorItem::User {
                id: 1,
                protected: true,
                verbatim: true,
                original: "old prompt".to_string(),
                compressed: None,
            },
            NoAnchorItem::ToolResult {
                id: 2,
                call_id: "c1".to_string(),
                content: "old result".to_string(),
                useless: true,
            },
        ]),
        next_id: 3,
        user_prompts: std::collections::VecDeque::from([1]),
        max_tokens: 1000,
    };
    let bytes = bincode::serialize(&legacy).unwrap();

    // Never panics; the .ctx is discarded and the JSONL fallback takes over.
    assert!(
        bincode::deserialize::<ContextManagerState>(&bytes).is_err(),
        "a snapshot missing the trailing anchor is rejected (JSONL fallback)"
    );
}

#[test]
fn snapshot_before_the_useless_flag_is_rejected_gracefully() {
    #[allow(dead_code)]
    #[derive(serde::Serialize)]
    enum PreUselessItem {
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
        // Pre-c728c93 layout: no trailing `useless` in the variant — the
        // derived reader consumes the next byte (the low byte of next_id,
        // 0x02) as the bool and fails with an invalid-bool encoding error.
        ToolResult {
            id: u64,
            call_id: String,
            content: String,
        },
        LoopClosure {
            id: u64,
            content: String,
        },
    }
    #[derive(serde::Serialize)]
    struct PreUselessState {
        items: std::collections::VecDeque<PreUselessItem>,
        next_id: u64,
        user_prompts: std::collections::VecDeque<u64>,
        max_tokens: usize,
    }

    let legacy = PreUselessState {
        items: std::collections::VecDeque::from([PreUselessItem::ToolResult {
            id: 1,
            call_id: "c1".to_string(),
            content: "old result".to_string(),
        }]),
        next_id: 2,
        user_prompts: std::collections::VecDeque::new(),
        max_tokens: 1000,
    };
    let bytes = bincode::serialize(&legacy).unwrap();

    // Returns Err (never panics) → the .ctx is discarded and the JSONL
    // fallback takes over.
    assert!(
        bincode::deserialize::<ContextManagerState>(&bytes).is_err(),
        "pre-useless snapshots are rejected (JSONL fallback)"
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
//
// NOTE: the loaded assistant turn is compressible (`with_history` calls
// `add_assistant(text, true)`), but with the synchronous pipeline nothing is
// compressed below the 80% overflow — the delivered text is the raw marker.
// The delivery check still uses the ACTUAL delivered text (raw or compressed)
// so the assertion is robust either way.
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
// phases remain the fallback); it can never stall or crash the loop.
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
    cm.add_user(&"U ".repeat(50)); // verbatim anchor — never compressed
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

// Phase 2 yields to phase 3 at a LoopClosure: the closure is the segment
// boundary — once the oldest segment's drafts are exhausted (and the total is
// still over 80%), the toggle proceeds to the tool eviction. The cursor parks
// just past the closure so the next draft session starts on the NEXT segment.
#[test]
fn draft_eviction_yields_to_tools_at_a_loop_closure() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor (verbatim)
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"B ".repeat(100), false); // newest draft (next segment)
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(700)); // ~700-token chain
    // 50 + 500 + 50 + 100 + ~703 = ~1403 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    // Phase 2 removes A (→ 903, still over) and then hits the LoopClosure:
    // it yields. Phase 3 fires and evicts the only tool chain (→ 203 < 800).
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the oldest draft is evicted before the closure boundary"
    );
    assert!(
        !cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "phase 3 evicts the tool chain once phase 2 reaches the LoopClosure"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the draft in the NEXT segment survives this pass"
    );
    let b_id = cm
        .items
        .iter()
        .find(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. } if original.starts_with("B ")
            )
        })
        .map(ContextItem::id)
        .unwrap();
    assert_eq!(
        cm.draft_cursor,
        Some(b_id),
        "the cursor parks just past the closure — the next session starts on the next segment"
    );
}

// ── Compression is bounded ──────────────────────────────────────────────

// `compress_text` batches by sentence-chunk count (≤ DETERMINISTIC_MAX_CHUNKS
// per deterministic pass), so a single pass can never run ONE full SVD +
// O(n³) MMR over the whole text and stall the agent loop (the pipeline now
// runs on the agent loop's thread). This guards the batching: an input that
// spans many batches (~800 chunks, well under the old 100k-token single-pass
// threshold) must compress quickly — a regression to a single whole-text pass
// would blow through the bound by orders of magnitude. (Timing guard,
// generous bound; the structural guarantee is the 200-chunk cap per pass.)
#[test]
fn compress_text_batches_large_inputs_and_stays_fast() {
    // ~800 sentence-chunks ≈ 38k estimated tokens — the exact input that used
    // to hit the single-pass path (≤ 100k tokens) and stall the agent loop.
    let mut text = String::new();
    for i in 0..800 {
        text.push_str(&format!(
            "Paragraph {i} contains several sentences with unique vocabulary. \
             The quick brown fox jumps over the lazy dog near the river. \
             Consider the implications of this distinct content item and its neighbors. "
        ));
    }
    assert!(text.len() > 40_000, "precondition: spans multiple batches");

    let start = std::time::Instant::now();
    let out = compress_text(&text);
    let elapsed = start.elapsed();

    assert!(!out.is_empty(), "compression must produce output");
    assert!(
        out.len() < text.len(),
        "compression must shrink the input, got {} -> {} chars",
        text.len(),
        out.len()
    );
    eprintln!(
        "[perf] compress_text ~800 chunks: {elapsed:?} ({} -> {} chars)",
        text.len(),
        out.len()
    );
    assert!(
        elapsed.as_secs_f64() < 30.0,
        "compress_text must stay bounded per batch (batched passes), took {elapsed:?}"
    );
}

// ── Adversarial edge cases: cursor & phase interplay ────────────────────
//
// The gradual draft eviction's persistent cursor is an ITEM ID. The other
// phases (tool eviction, closure trimming) can remove the very item the
// cursor points at between two overflows. The resume must degrade gracefully:
// the first item with `id >= cursor` is the next candidate — never a panic,
// never a stall, never a re-removal of newer content before older content is
// exhausted.

// trim_loop_closures (phase 4) removes the LoopClosure the draft cursor
// parked on (a removal that dropped the total below the trigger right before
// a closure). The next overflow must resume past the removed closure and
// still converge.
#[test]
fn draft_cursor_survives_closure_trimming_removing_its_item() {
    let mut cm = cm(1000); // trigger = 800, target = 400
    cm.add_user(&"U ".repeat(50)); // anchor (verbatim)
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "c1 ".repeat(150), // 300 tokens
    });
    let closure2 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure2,
        content: "c2 ".repeat(150), // 300 tokens
    });
    // 50 + 500 + 300 + 300 = 1150 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();
    // Phase 2 removed A (→ 650 < 800), parking the cursor on closure1. Phase 4
    // then trims the closures (600 ≥ 400): the OLDEST closure — the very item
    // the cursor points at — is removed.
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the oldest draft is removed by phase 2"
    );
    assert!(
        !cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::LoopClosure { id, .. } if *id == closure1)),
        "phase 4 removed the closure the cursor points at"
    );
    assert_eq!(
        cm.draft_cursor,
        Some(closure1),
        "the cursor still points at the removed id"
    );

    // Regrow over the trigger and run twice: the resume must find the first
    // item with id >= the removed closure's id (closure2 — also a boundary,
    // yielding), and the following run must then remove the new draft.
    cm.add_assistant(&"B ".repeat(900), false);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );
    cm.run();
    cm.run();
    assert!(cm.total_tokens() < 800, "the pass must still converge");
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the new draft is removed on the resumed pass"
    );
}

// Phase 3 (tool eviction) can remove the TOOL CHAIN the draft cursor parked
// on (phase 2 yielded at a closure with the cursor just past it). The next
// overflow must resume past the removed chain — not re-scan from the start.
#[test]
fn draft_cursor_survives_tool_eviction_removing_its_item() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor (verbatim)
    cm.add_assistant(&"A ".repeat(500), false); // oldest draft
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(50),
    });
    cm.add_tool_call("t0", "read_file", "{}");
    cm.add_tool_result("t0", &"t ".repeat(700)); // ~700-token chain
    cm.add_assistant(&"B ".repeat(100), false); // newest draft (after the tools)
    let t0_call_id = cm
        .items
        .iter()
        .find(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t0"))
        .map(ContextItem::id)
        .unwrap();
    // 50 + 500 + 50 + ~703 + 100 = ~1403 ≥ 800.
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();
    // Phase 2: U skip, A remove (→ 903 ≥ 800) → closure → yield with the
    // cursor parked on t0's CALL. Phase 3 evicts the t0 chain (→ 200 < 800) —
    // the very item the cursor points at is now gone.
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "t0")),
        "phase 3 removed the tool chain the cursor pointed at"
    );
    assert_eq!(
        cm.draft_cursor,
        Some(t0_call_id),
        "the cursor still points at the removed tool call"
    );

    // Regrow: B (100) + new draft C (800) = 50 + 50 + 100 + 800 = 1000 ≥ 800.
    cm.add_assistant(&"C ".repeat(800), false);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );
    cm.run();
    // The resume must find B (the first id >= the removed t0 call), not
    // re-scan: B (→ 900) then C (→ 100 < 800) are removed in order.
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ") || original.starts_with("C ")
        )),
        "the resumed pass removes the drafts in order"
    );
}

// Repeated overflows in a tool-heavy session must converge every time —
// phase 1 compresses the compressible chatter, phase 2 exhausts the drafts,
// phase 3 evicts tool chains middle-out — with no infinite loop and no
// runaway removal of the newest context.
#[test]
fn repeated_overflows_converge_in_a_tool_heavy_session() {
    let mut cm = cm(1000); // trigger = 800
    for i in 0..20 {
        cm.add_user(&"U ".repeat(50));
        cm.add_assistant(&"Working. ".repeat(5), true); // compressible chatter
        cm.add_tool_call(&format!("c{i}"), "fs_edit", "{}");
        cm.add_tool_result(
            &format!("c{i}"),
            &"the quick brown fox jumps over the lazy dog near the riverbank. ".repeat(30),
        );
        cm.run();
        assert!(
            cm.total_tokens() < 800,
            "each overflow must converge below the trigger, round {i}: {}",
            cm.total_tokens()
        );
    }
    // The manager is still fully usable and renders.
    assert!(cm.display_info().total_tokens > 0);
    let msgs = cm.build_messages("");
    assert!(
        !msgs.is_empty(),
        "rendering still works after repeated overflows"
    );
}

// The pipeline (phase 1) can compress the most recent compressible assistant
// BEFORE the loop closes (if the budget overflows mid-loop). close_loop must
// still promote the ORIGINAL text verbatim — the final answer is never
// delivered compressed.
#[test]
fn close_loop_promotes_original_even_after_the_pipeline_compressed_it() {
    let mut cm = cm(1000); // trigger = 800
    let text = prose_copies(40);
    cm.add_user("task");
    cm.add_assistant(&text, true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // The pipeline compresses the assistant in place…
    cm.run();
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        ),
        "the pipeline compressed the candidate final output"
    );

    // …and close_loop still promotes the ORIGINAL text.
    cm.close_loop();
    let ContextItem::LoopClosure { content, .. } = &cm.items[1] else {
        panic!("expected LoopClosure");
    };
    assert_eq!(content, &text, "the final answer keeps its original text");
}

// Phase 2 yields at a LoopClosure (the segment boundary) even when the total
// is still over the trigger; the drafts in the NEXT segment wait for the next
// overflow. This is the documented transient — the following run() resumes
// past the closure and converges.
#[test]
fn budget_overflows_self_heal_on_the_next_run_after_a_closure_yield() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor (verbatim)
    cm.add_assistant(&"A ".repeat(600), false); // oldest draft (segment 1)
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"B ".repeat(100), false); // draft in segment 2
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Run 1: A is removed (→ ~200 < 800) and the pass stops below the trigger.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("A ")
        )),
        "the oldest draft is removed"
    );

    // Regrow: a huge draft in segment 2 pushes the total back over the
    // trigger.
    cm.add_assistant(&"C ".repeat(900), false);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    // Run 2: phase 2 resumes at the closure → yields immediately (B, C are in
    // the NEXT segment) and the budget stays over — the documented transient.
    cm.run();
    // Run 3: phase 2 resumes past the closure and removes B, then C → below.
    cm.run();
    assert!(cm.total_tokens() < 800, "the self-heal converges");
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ") || original.starts_with("C ")
        )),
        "the drafts in the next segment are removed on the resumed pass"
    );
}

// ── The toggle: which phase leads the next overflow ──────────────────────
//
// The toggle rotates pipeline → drafts → tools → pipeline. Phase 2 is the
// STICKY lead: it stays across consecutive overflows (one chunk per stop)
// until it reaches a LoopClosure checkpoint; phase 3 completing resets the
// cycle to the pipeline. The funnel inside a call follows the toggle cycle,
// wrapping to the pipeline if needed. Phase 4 is outside the toggle.

// Overflow 1 leads with the pipeline (the initial toggle) and resolves by
// compressing; the lead then hands to the draft pass. Overflow 2 must be
// DRAFT-led: a fresh raw compressible draft survives UNCOMPRESSED (the
// pipeline would have summarized it) while the older chunks are evicted
// oldest-first.
#[test]
fn toggle_advances_pipeline_to_drafts_after_the_first_overflow() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&prose_copies(40), true);
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
        "overflow 1: the pipeline leads and compresses the raw draft"
    );
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Drafts,
        "the pipeline resolving hands the lead to the draft pass"
    );

    // Overflow 2: add an older removable chunk and a fresh raw compressible
    // draft. The draft pass evicts the older chunk; the fresh draft survives
    // UNCOMPRESSED — proof the pipeline did not lead.
    cm.add_assistant(&"B ".repeat(1200), false);
    cm.add_assistant(&prose_copies(30), true);
    assert!(
        cm.total_tokens() >= 800,
        "precondition: regrown over the trigger"
    );

    cm.run();
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Drafts,
        "drafts resolved mid-timeline → stays sticky"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: None,
                ..
            }
        )),
        "the fresh raw draft survived the draft pass UNCOMPRESSED"
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("B ")
        )),
        "the older chunk was evicted instead"
    );
}

// The draft pass is the STICKY lead: it stays across consecutive overflows
// while it resolves mid-timeline, and only hands the lead to the tool pass
// when it reaches a LoopClosure — the shared checkpoint of phases 1 and 2.
#[test]
fn toggle_keeps_drafts_leading_until_the_loop_closure_checkpoint() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor
    cm.add_assistant(&"A ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Round 1: the pipeline leads (no eligible drafts → falls through), the
    // draft pass removes A and stops below with no checkpoint ahead → sticky.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Drafts,
        "round 1: drafts resolved mid-timeline → stays"
    );

    // Round 2: drafts lead again, remove the new chunk, still no checkpoint.
    cm.add_assistant(&"B ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Drafts,
        "round 2: still sticky — no LoopClosure yet"
    );

    // Round 3: a new segment starts with a LoopClosure after the draft. The
    // draft pass reaches the checkpoint → its jurisdiction over the oldest
    // segment ends → the lead hands to the tool pass. The NEW segment's draft
    // waits for the next pass.
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    cm.add_assistant(&"C ".repeat(900), false);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    cm.run();
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Tools,
        "the LoopClosure checkpoint ends phase 2's jurisdiction"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("C ")
        )),
        "the next segment's draft waits for the next pass"
    );
}

// The tool pass completing — as the lead or reached via the funnel — resets
// the toggle to the pipeline: the cycle restarts.
#[test]
fn toggle_resets_to_pipeline_after_the_tool_pass_completes() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50)); // anchor
    cm.add_assistant(&"A ".repeat(500), false);
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    cm.add_tool_call("t0", "read_file", "{}");
    cm.add_tool_result("t0", &"t ".repeat(700));
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Pipeline (no eligible) → drafts: A removed, still over → closure
    // boundary → funnel to the tool pass → the t0 chain is evicted and the
    // budget resolves. The cycle then restarts at the pipeline.
    cm.run();
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "t0"
        )),
        "the tool pass evicted the chain"
    );
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Pipeline,
        "the tool pass completing resets the cycle to the pipeline"
    );
}

// The funnel follows the TOGGLE cycle, not a fixed pipeline-first order: with
// the toggle on the tool pass but no tool chains present, it wraps to the
// pipeline ("rotate to 1 again") which compresses the raw draft.
#[test]
fn funnel_wraps_to_the_pipeline_when_tools_lead_and_cannot_resolve() {
    let mut cm = cm(1000); // trigger = 800
    // Round 1: force the toggle onto the tool pass via a closure checkpoint.
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(900), false);
    let closure = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure,
        content: "L ".repeat(50),
    });
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");
    cm.run();
    assert_eq!(
        cm.toggle_for_test(),
        ToggleLead::Tools,
        "the closure checkpoint handed the lead to the tool pass"
    );

    // Round 2: the tool pass leads but there are NO tool chains → the funnel
    // wraps to the pipeline, which compresses the fresh raw draft.
    cm.add_assistant(&prose_copies(40), true);
    assert!(cm.total_tokens() >= 800, "precondition: regrown");
    cm.run();
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        )),
        "the funnel wrapped Tools → Pipeline and the pipeline compressed the raw draft"
    );
}

// Phase 4 (LoopClosure trimming) is OUTSIDE the toggle: even when the funnel
// already resolved below the trigger, it still runs last on its own condition
// (closures alone ≥ 40% of the budget).
#[test]
fn phase_four_trims_closures_regardless_of_the_toggle() {
    let mut cm = cm(1000); // trigger = 800, target = 400
    cm.add_user(&"U ".repeat(50));
    cm.add_assistant(&"A ".repeat(500), false);
    let closure1 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure1,
        content: "c1 ".repeat(150), // 300 tokens
    });
    let closure2 = cm.next_id();
    cm.items.push_back(ContextItem::LoopClosure {
        id: closure2,
        content: "c2 ".repeat(150), // 300 tokens
    });
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();
    // Drafts removed A (→ below the trigger, next item is the closure) → the
    // funnel resolved at the checkpoint and the toggle advanced to Tools — and
    // phase 4 STILL trimmed the closures (600 ≥ 400) afterwards.
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::LoopClosure { id, .. } if *id == closure1
        )),
        "phase 4 trimmed the oldest closure even though the funnel already resolved"
    );
    assert_eq!(cm.toggle_for_test(), ToggleLead::Tools);
}

// ── Compaction observer (TUI live feedback) ──────────────────────────────

/// Collect compaction events through the observer. `Arc<Mutex>` because the
/// observer is `Send`-bounded (the harness moves it into a tokio task).
fn collect_events() -> (
    ContextManager,
    std::sync::Arc<std::sync::Mutex<Vec<CompactionEvent>>>,
) {
    let cm = cm(1000); // trigger = 800, target = 400
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut cm = cm;
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

    // Segment 1: a removable draft (the first prompt stops being the anchor
    // once a new segment starts) + a big LoopClosure. Segment 2: the new
    // anchor prompt + a compressible draft. After the pipeline compresses the
    // segment-2 draft the total stays over, so phase 2 evicts the segment-1
    // prompt — and phase 4 then trims the big closure.
    cm.add_user("q1");
    cm.add_user("q2");
    cm.add_assistant(&prose_copies(60), true); // becomes the closure
    cm.close_loop();
    cm.add_user("q3");
    cm.add_assistant(&prose_copies(40), true);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            CompactionEvent::PipelineStarted,
            CompactionEvent::PipelineFinished,
            CompactionEvent::DraftsEvicted,
            CompactionEvent::ClosuresTrimmed,
        ],
        "the funnel reports every phase that actually did work"
    );
}

#[test]
fn compaction_observer_reports_tools_eviction() {
    let (mut cm, events) = collect_events();

    // Tool chains are structural (never prose-compressed): the pipeline has
    // nothing to compress and the draft pass nothing to evict, so the funnel
    // reaches the tool pass, which removes the chain.
    cm.add_user("task");
    cm.add_tool_call("call_1", "find_grep", "{}");
    cm.add_tool_result_flagged("call_1", &prose_copies(80), false);
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert_eq!(
        *events.lock().unwrap(),
        vec![CompactionEvent::ToolsEvicted],
        "only the tool pass reports — it removed the whole chain"
    );
}

#[test]
fn compaction_observer_reports_closure_trimming() {
    let (mut cm, events) = collect_events();

    // A single big LoopClosure: no pipeline work, no removable drafts, no
    // tools — only phase 4 fires (closures alone hold ≥ 40% of the budget).
    cm.add_user("task");
    cm.add_assistant(&prose_copies(60), true);
    cm.close_loop();
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    assert_eq!(
        *events.lock().unwrap(),
        vec![CompactionEvent::ClosuresTrimmed],
        "only the closure trimming reports"
    );
}
