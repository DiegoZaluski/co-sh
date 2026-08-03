//! Tests for the single-owner LLM-free asynchronous context manager.
//!
//! Coverage: in-place async compression (1 item = 1 message), protected user
//! prompts, the native `tool_call → tool` structural chain, `LoopClosure`
//! promotion + guard, the 80/40 compaction phases (chain-aware), message
//! rendering (positions + roles), save/restore round-trip, display info, and
//! integration with the harness payload.

use super::*;
use crate::util::estimate_tokens;

fn cm(max_tokens: usize) -> ContextManager {
    ContextManager::new(max_tokens)
}

/// Poll until every item that is EXPECTED to compress has done so (with a
/// generous deadline), so tests that rely on the async compression resolve
/// deterministically.
///
/// Only un-protected, NON-verbatim user prompts and compressible assistant
/// texts are waited on — protected prompts, the task anchors (verbatim) and
/// non-compressible (tool-carrying) assistants legitimately keep
/// `compressed: None` forever, so waiting on them would spin the full
/// deadline.
fn wait_for_compression(cm: &mut ContextManager) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cm.items.iter().any(|it| {
        matches!(
            it,
            ContextItem::User {
                protected: false,
                verbatim: false,
                compressed: None,
                ..
            } | ContextItem::Assistant {
                compressible: true,
                compressed: None,
                ..
            }
        )
    }) && std::time::Instant::now() < deadline
    {
        cm.poll();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    cm.poll();
}

// ── Ingestion + in-place compression ─────────────────────────────────────

#[test]
fn assistant_text_compresses_in_place_at_the_same_position() {
    let mut cm = cm(10_000);
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(40);
    cm.add_user("before");
    cm.add_assistant(&text, true);

    // The job is DEFERRED (the turn might be the loop's final output), so the
    // item is deterministically still raw — nothing has been submitted yet.
    assert!(matches!(
        &cm.items[1],
        ContextItem::Assistant {
            compressed: None,
            ..
        }
    ));
    assert_eq!(
        cm.items[1].tokens(TokenEncoding::Cl100k),
        estimate_tokens(&text)
    );

    // The loop advancing (any subsequent poll) flushes the pending job; the
    // worker compresses it and the copy is swapped in place.
    wait_for_compression(&mut cm);

    // The compressed copy was swapped IN PLACE: same position, smaller content.
    let ContextItem::Assistant { compressed, .. } = &cm.items[1] else {
        panic!("expected Assistant at index 1");
    };
    let compressed_text = compressed.clone().expect("compression resolved");
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
    let mut cm = cm(10_000);
    cm.add_user("first prompt");
    cm.add_user("second prompt");
    wait_for_compression(&mut cm);
    // Both are protected: neither was submitted for compression.
    assert!(cm.items.iter().all(|it| {
        matches!(
            it,
            ContextItem::User {
                protected: true,
                ..
            }
        )
    }));

    // A third prompt un-protects the FIRST one — the session anchor, which is
    // verbatim and therefore NEVER submitted to the pipeline (it stays raw).
    cm.add_user("third prompt");
    wait_for_compression(&mut cm);
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
    // prompt — which IS submitted and compresses asynchronously.
    cm.add_user("fourth prompt");
    wait_for_compression(&mut cm);
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::User {
                protected: false,
                compressed: Some(_),
                ..
            }
        ),
        "a non-verbatim un-protected prompt compresses normally"
    );
}

// Structural tool layer (native chain)

#[test]
fn tool_call_and_result_render_native_chain() {
    let mut cm = cm(10_000);
    cm.add_user("do it");
    cm.add_tool_call("call_1", "read_file", r#"{"path":"/a"}"#);
    cm.add_tool_result("call_1", "file contents here");
    wait_for_compression(&mut cm);

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
    wait_for_compression(&mut cm);
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
    wait_for_compression(&mut cm);
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
    wait_for_compression(&mut cm);

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
    let mut manager = cm(10_000);
    manager.add_user("first prompt"); // session anchor
    manager.add_user("second prompt");
    let state = manager.save_state();

    let mut restored = cm(10_000);
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

    // A fourth prompt un-protects the SECOND prompt (a normal one) — it IS
    // submitted and compresses: restore kept the bookkeeping consistent.
    restored.add_user("fourth prompt");
    wait_for_compression(&mut restored);
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

#[test]
fn evict_drafts_keeps_loop_closures_and_protected_prompts() {
    let mut cm = cm(1000);
    // 50 (protected) + 50 (loop) + 400 (old draft) + 400 (new draft) = 900,
    // which fires the 80% trigger (800). The draft eviction targets the
    // TRIGGER, not 40%: it removes only the OLDEST draft (the minimum needed
    // to drop below 800) and the NEWEST draft survives. trim_loop_closures
    // never fires
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
        original: "old ".repeat(400),
        compressed: None,
        compressible: true,
    });
    cm.items.push_back(ContextItem::Assistant {
        id: 4,
        original: "new ".repeat(400),
        compressed: None,
        compressible: true,
    });
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "after the draft eviction total must be below the 80% trigger, got {}",
        cm.total_tokens()
    );
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("old ")
        )),
        "the OLDEST draft is removed — the minimum needed to drop below 80%"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. } if original.starts_with("new ")
        )),
        "the NEWEST draft survives — the draft eviction no longer halves the window"
    );
    assert!(
        cm.items.iter().any(|it| it.is_loop()),
        "LoopClosure must survive the draft eviction"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User {
                protected: true,
                ..
            }
        )),
        "protected user prompt must survive the draft eviction"
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

// The compaction lead rotates on every trigger: the DEFAULT pass opens with
// the draft eviction (the task anchor and the tool data live one more cycle,
// since drafts go first); the next trigger opens with the tool eviction
// (tool chains are the first eviction target, middle-out). Both evictions
// only decompact the MINIMUM needed to drop below the 80% trigger — no single
// content class is always the first victim and the window is never halved.
// Drafts here are non-compressible (tool-carrying) assistant outputs so the
// async worker cannot race the assertions.
#[test]
fn run_rotates_eviction_lead_between_tools_and_drafts() {
    let mut cm = cm(1000); // trigger = 800, target = 400

    // Trigger 1 content: protected user (50) + drafts A/B (250 each) + tool
    // chain c1 (~503) = ~1053 ≥ 800.
    cm.add_user(&"u ".repeat(50));
    cm.add_assistant(&"A ".repeat(250), false); // draft A — carried a tool call
    cm.add_assistant(&"B ".repeat(250), false); // draft B
    cm.add_tool_call("c1", "read_file", "{}");
    cm.add_tool_result("c1", &"t ".repeat(500));
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // Trigger 1 — the DRAFT eviction LEADS (the default, compact_lead starts
    // false): the drafts are the first eviction target and the tool chain
    // survives — the task's tool data lives one more cycle.
    cm.run();
    assert!(cm.compact_lead, "lead must toggle after a trigger");
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("A ") || original.starts_with("B ")
        )),
        "drafts-led pass evicts the drafts first"
    );
    assert!(
        cm.items
            .iter()
            .any(|it| matches!(it, ContextItem::ToolCall { call_id, .. } if call_id == "c1")),
        "the tool chain survives the drafts-led pass — tool data lives one more cycle"
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

    // Trigger 2 — the TOOL eviction LEADS: the tool chains are the first
    // eviction target (middle-out) and the drafts are spared this round.
    cm.run();
    assert!(!cm.compact_lead, "lead must toggle back");
    assert!(cm.total_tokens() < 800);
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. }
                if call_id == "c1" || call_id == "c2"
        )),
        "tools-led pass evicts the tool chains first"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("X ") || original.starts_with("Y ")
        )),
        "the newest drafts survive the tools-led pass"
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
        cm.compact_lead,
        "the lead still toggles with no tools present"
    );
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

// Distribution across content types across two triggers: with the minimum
// decompaction, a drafts-led pass removes ONLY the oldest draft (every tool
// chain survives — the tool data lives on); the following tools-led pass
// evicts the oldest tool chain first while every draft survives. Removal
// never hits recent context to start the next phase.
#[test]
fn run_distributes_eviction_without_hitting_recent_first() {
    let mut cm = cm(1000); // trigger = 800, target = 400

    // Pass 1 — drafts-led (the default). User (50) + draft A (200) + old tool
    // T0 (~403 — "T0" is 2 tokens) + draft B (90) + new tool T1 (~153) +
    // draft C (90) = ~986.
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
        cm.compact_lead,
        "pass 1 is drafts-led → lead toggles to true"
    );
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
        "the oldest draft is the first eviction target of the drafts-led pass"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ") || original.starts_with("C ")
        )),
        "the newer drafts survive the drafts-led pass"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "t0" || call_id == "t1"
        )),
        "ALL tool chains survive the drafts-led pass — the tool data lives on"
    );

    // Pass 2 — tools-led. Regrow with drafts D/E then a brand-new tool chain
    // T2 at the very end (~183): 786 + 90 + 90 + 183 = ~1149.
    cm.add_assistant(&"D ".repeat(90), false);
    cm.add_assistant(&"E ".repeat(90), false);
    cm.add_tool_call("t2", "read_file", "{}");
    cm.add_tool_result("t2", &"T2 ".repeat(90));
    assert!(
        cm.total_tokens() >= 800,
        "precondition: pass 2 over the trigger"
    );

    cm.run();
    assert!(!cm.compact_lead, "pass 2 is tools-led → lead toggles back");
    assert!(
        cm.total_tokens() < 800,
        "pass 2 must drop below the trigger"
    );
    // The oldest tool chain goes first; the newer chains and EVERY draft
    // survive untouched.
    assert!(
        !cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "t0"
        )),
        "the OLDEST tool chain is the first eviction target of the tools-led pass"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::ToolCall { call_id, .. } if call_id == "t1" || call_id == "t2"
        )),
        "the newer tool chains survive the tools-led pass"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant { original, .. }
                if original.starts_with("B ")
                    || original.starts_with("C ")
                    || original.starts_with("D ")
                    || original.starts_with("E ")
        )),
        "EVERY draft survives the tools-led pass — only the minimum was removed"
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

    // A fourth prompt un-protects a NORMAL prompt (not the anchor) — it IS
    // submitted and compresses, proving the anchor exclusion is by verbatim.
    cm.add_user(&"V ".repeat(50));
    wait_for_compression(&mut cm);
    assert!(
        matches!(
            &cm.items[1],
            ContextItem::User {
                protected: false,
                compressed: Some(_),
                ..
            }
        ),
        "a normal un-protected prompt compresses normally"
    );
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

// ── Proof: the async pipeline is starved in tool-heavy loops ─────────────
//
// The TF-IDF → LSA → MMR worker thread IS genuinely asynchronous (dedicated
// thread, non-blocking send/poll), but it only ever receives compressible
// assistant texts and un-protected user prompts. A realistic agent loop is
// dominated by tool calls + tool results, which are structural and NEVER
// submitted. So during the exact phase that gets timed (between tool calls)
// the pipeline processes NOTHING — the raw tool content grows until the 80%
// compaction REMOVES chains wholesale (a drop, not a TF-IDF → LSA → MMR pass).
#[test]
fn tool_loop_submits_zero_jobs_and_compression_stays_idle() {
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

    // Give the worker every chance to run (drain until stable).
    wait_for_compression(&mut cm);

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
        "a tool loop must submit ZERO compression jobs — the pipeline is idle"
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

    // Contrast: a compressible final answer DOES compress — proving the worker
    // runs fine and the exclusion is strictly item-type based.
    cm.add_assistant("The implementation is complete and all tests pass.", true);
    wait_for_compression(&mut cm);
    assert!(
        cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant {
                    compressible: true,
                    compressed: Some(_),
                    ..
                }
            )
        }),
        "compressible items DO compress asynchronously"
    );
}

// ── Proof: normal chat (no tools) DOES compress dialogue + user input ────
//
// For a conversational session the pipeline is fed: user prompts beyond the
// two most recent, and every assistant text that carried no tool call. The
// active (most recent) user prompt stays protected/verbatim. This test builds
// a plain multi-turn chat and asserts exactly that coverage.
#[test]
fn normal_chat_compresses_dialogue_and_user_input() {
    let mut cm = cm(10_000);
    for i in 0..4 {
        cm.add_user(&format!(
            "user question {i}: {}",
            "why does the sky change color at sunset? ".repeat(10)
        ));
        cm.add_assistant(
            &format!(
                "answer {i}: {}",
                "because of Rayleigh scattering and atmospheric optics. ".repeat(10)
            ),
            true,
        );
    }
    wait_for_compression(&mut cm);

    // The two most recent user prompts are protected (verbatim anchors)...
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
    // ...older user prompts ARE compressed asynchronously.
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

// ── Proof: the loop's final output is never submitted to the pipeline ─────
//
// The last assistant turn of each loop is a CANDIDATE final output: the
// harness only knows it is final when it closes the loop. The manager parks
// the job in a deferred slot instead of submitting it, so `close_loop()` can
// simply drop it — the final answer keeps its ORIGINAL text and zero work was
// started (nothing to cancel, nothing wasted). Only a turn the loop provably
// advances past is flushed (via the next poll) and compressed.
#[test]
fn close_loop_never_submits_the_final_output() {
    let mut cm = cm(10_000);
    let text = "The final summary of the completed task. ".repeat(60);
    cm.add_user("task");
    cm.add_assistant(&text, true); // candidate final → job deferred, not sent

    cm.close_loop(); // harness signals the loop is done
    let ContextItem::LoopClosure { content, .. } = &cm.items[1] else {
        panic!("expected LoopClosure");
    };
    assert_eq!(content, &text, "the ORIGINAL text is promoted verbatim");

    // Drain the worker for a while: nothing was ever submitted, so the item
    // cannot change and no CPU was spent compressing the final answer.
    for _ in 0..50 {
        cm.poll();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        cm.items[1].tokens(TokenEncoding::Cl100k),
        estimate_tokens(&text),
        "the final output stays raw forever — it was never compressed"
    );
}

// The counterpart: an intermediate turn IS flushed as soon as the loop
// advances (a new item triggers a poll) and compresses normally — the deferred
// slot only shields the true final output.
#[test]
fn pending_candidate_is_flushed_when_the_loop_advances() {
    let mut cm = cm(10_000);
    let text = "intermediate reply that is not the final output. ".repeat(40);
    cm.add_user("task");
    cm.add_assistant(&text, true); // candidate final → deferred

    // The loop advances with a new item → the previous turn is provably old.
    cm.add_user("follow-up");
    wait_for_compression(&mut cm);

    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        )),
        "an intermediate turn must be submitted and compressed once the loop advances"
    );
}

// Persistence

#[test]
fn save_restore_roundtrip_resubmits_pending() {
    let mut manager = cm(10_000);
    manager.add_user("hello");
    manager.add_assistant("some assistant text to compress", true);
    let state = manager.save_state();
    assert_eq!(state.items.len(), 2);

    let mut restored = cm(10_000);
    restored.restore_state(&state);
    assert_eq!(restored.items.len(), 2);
    wait_for_compression(&mut restored);
    assert!(
        restored.items.iter().any(|it| matches!(
            it,
            ContextItem::Assistant {
                compressed: Some(_),
                ..
            }
        )),
        "pending items must be re-submitted on restore"
    );
}

#[test]
fn restore_never_compresses_protected_user_prompts() {
    let mut manager = cm(10_000);
    manager.add_user(&"first prompt that must stay verbatim ".repeat(30));
    manager.add_user(&"second prompt that must stay verbatim ".repeat(30));
    // Both are protected and neither has been submitted in normal operation.
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

    // Give any (buggy) restore-submitted jobs time to complete before asserting.
    for _ in 0..50 {
        restored.poll();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

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
        "BUG: restore_state submitted protected user prompts for compression — \
         the active session anchors must stay verbatim"
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
// `add_assistant(text, true)`), so the async worker may replace the raw marker
// with its summary before we render. The delivery check therefore uses the
// ACTUAL delivered text (raw or compressed), never the raw marker — otherwise
// this test would be a race with the worker thread.
#[test]
fn assistant_history_turn_is_not_duplicated_system_and_messages() {
    let marker = "UNIQUE_ASSISTANT_MARKER_XYZ";
    let mut h = crate::harness::Harness::new_test().with_history(&[
        ("user".into(), "question".into()),
        ("assistant".into(), marker.into()),
    ]);
    // Resolve the async compression deterministically so the delivered content
    // is stable before we render.
    wait_for_compression(&mut h.context_manager);

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

// ── Failure-mode simulations: delayed / dead compression pipeline ─────────
//
// The TF-IDF → LSA → MMR worker is genuinely async (dedicated thread,
// non-blocking unbounded send, try_recv poll), so the agent loop NEVER waits
// on it. But what happens when the pipeline is DELAYED (weak machine, or a
// single huge job monopolising the FIFO worker — full SVD + O(n³) MMR on the
// whole sentence set, no chunk cap below 100k tokens) or DEAD? These three
// tests simulate the failure modes deterministically (bypassing the worker
// and injecting results through a swapped channel) and pin the actual
// behaviour: degradation by synchronous DROP, never a stall or a break.

// Simulates the pipeline never returning: compressible drafts stay raw at
// full cost, the 80% trigger fires, and the phases degrade by REMOVAL
// (oldest-first, minimum decompaction) — synchronously, without waiting on
// the worker. The manager remains fully usable afterwards.
#[test]
fn dead_pipeline_degrades_by_drop_and_never_stalls() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    // Four compressible drafts whose compression never arrives (worker
    // unavailable / pathologically slow). Pushed raw, they stay raw.
    for label in ["A ", "B ", "C ", "D "] {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::Assistant {
            id,
            original: label.repeat(250),
            compressed: None,
            compressible: true,
        });
    }
    assert_eq!(
        cm.total_tokens(),
        estimate_tokens(&"U ".repeat(50)) + 4 * estimate_tokens(&"A ".repeat(250)),
        "precondition: raw full cost"
    );
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // The compaction must complete synchronously and degrade by REMOVAL:
    // the two oldest raw drafts are dropped, the newest survive.
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "must drop below the trigger without waiting on the pipeline, got {}",
        cm.total_tokens()
    );
    assert!(
        !cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. }
                    if original.starts_with("A ") || original.starts_with("B ")
            )
        }),
        "the OLDEST raw drafts are dropped whole — the loss is the full text, not a summary"
    );
    assert!(
        cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. }
                    if original.starts_with("C ") || original.starts_with("D ")
            )
        }),
        "the NEWEST drafts survive the degraded pass"
    );

    // The manager is still fully usable: ingest, render and snapshot work.
    cm.add_user("keep going");
    assert_eq!(cm.items.len(), 4, "the manager keeps accepting new content");
    let msgs = cm.build_messages("");
    assert_eq!(
        msgs.len(),
        4,
        "rendering still works after the degraded pass"
    );
    assert!(cm.display_info().total_tokens > 0);
}

// The worker returns results for items that are already GONE (removed by the
// compaction phases) or PROMOTED (an assistant turn became a LoopClosure).
// Late results must be discarded safely: no crash, no item resurrection, no
// corruption of the promoted closure. Simulated deterministically by swapping
// the result channel for a test-controlled one and injecting the results.
#[test]
fn late_results_for_removed_and_promoted_items_are_discarded_safely() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    // Two raw drafts, both with in-flight (never-returned) compression.
    let a_id = cm.next_id();
    cm.items.push_back(ContextItem::Assistant {
        id: a_id,
        original: "A ".repeat(400),
        compressed: None,
        compressible: true,
    });
    let b_id = cm.next_id();
    cm.items.push_back(ContextItem::Assistant {
        id: b_id,
        original: "B ".repeat(400),
        compressed: None,
        compressible: true,
    });
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    // The compaction fires while the pipeline is delayed: the oldest draft
    // (A) is DROPPED synchronously while its compression is still in flight.
    cm.run();
    assert!(
        !cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. } if original.starts_with("A ")
            )
        }),
        "the oldest raw draft is dropped while its compression is in flight"
    );

    // Now the (delayed) worker finally delivers — swap in a channel we can
    // inject into, and drop the real one (its worker exits harmlessly on the
    // next failed result send; nothing is in flight).
    let (late_tx, late_rx) = std::sync::mpsc::channel::<CompressResult>();
    cm.result_rx = late_rx;

    // (a) A result for the item that was already REMOVED by the compaction.
    late_tx
        .send(CompressResult {
            id: a_id,
            compressed: "late ".repeat(10),
        })
        .unwrap();
    // (b) A result for an id that never existed.
    late_tx
        .send(CompressResult {
            id: 999_999,
            compressed: "ghost ".repeat(10),
        })
        .unwrap();
    cm.poll();
    assert_eq!(
        cm.items.len(),
        2,
        "stale results must not resurrect or duplicate items"
    );
    assert!(
        cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. } if original.starts_with("B ")
            )
        }),
        "the surviving draft is untouched by stale results"
    );

    // (c) Promote the surviving draft to a LoopClosure, then deliver a late
    // result for its id: it must be dropped, the verbatim text preserved.
    cm.close_loop();
    let ContextItem::LoopClosure { id, content } = &cm.items[1] else {
        panic!("expected LoopClosure at index 1");
    };
    let closure_id = *id;
    let verbatim = content.clone();
    late_tx
        .send(CompressResult {
            id: closure_id,
            compressed: "garbage ".repeat(20),
        })
        .unwrap();
    cm.poll();
    let ContextItem::LoopClosure { content, .. } = &cm.items[1] else {
        panic!("expected LoopClosure at index 1");
    };
    assert_eq!(
        content, &verbatim,
        "a late result must never overwrite the promoted closure"
    );
}

// Counterfactual: the SAME content with the pipeline WORKING (compressed
// copies already swapped in → ~1 token each) stays under the trigger and is
// untouched; with the pipeline DELAYED (raw copies → full cost) the trigger
// fires and a draft is DROPPED whole. This is the aggressive-degradation
// mechanism: a slow pipeline inflates `total_tokens`, fires the 80% trigger
// earlier, and evicts items entirely instead of summarising them.
#[test]
fn delayed_compression_inflates_raw_costs_and_forces_drops() {
    // Pipeline WORKING: the three drafts already have their summaries.
    let mut ok = cm(1000);
    ok.add_user(&"U ".repeat(50));
    for label in ["A ", "B ", "C "] {
        let id = ok.next_id();
        ok.items.push_back(ContextItem::Assistant {
            id,
            original: label.repeat(300),
            compressed: Some("short ".to_string()),
            compressible: true,
        });
    }
    let expected_working = estimate_tokens(&"U ".repeat(50)) + 3 * estimate_tokens("short ");
    assert_eq!(
        ok.total_tokens(),
        expected_working,
        "compressed drafts cost their summary tokens"
    );
    ok.run();
    assert_eq!(
        ok.items.len(),
        4,
        "working pipeline: the context stays under the trigger, nothing is removed"
    );
    assert_eq!(ok.total_tokens(), expected_working);

    // Pipeline DELAYED: identical content, but the drafts are still raw —
    // full cost → the trigger fires and the oldest draft is dropped whole
    // (the summary that would have saved it never landed in time).
    let mut delayed = cm(1000);
    delayed.add_user(&"U ".repeat(50));
    for label in ["A ", "B ", "C "] {
        let id = delayed.next_id();
        delayed.items.push_back(ContextItem::Assistant {
            id,
            original: label.repeat(300),
            compressed: None,
            compressible: true,
        });
    }
    assert_eq!(
        delayed.total_tokens(),
        estimate_tokens(&"U ".repeat(50)) + 3 * estimate_tokens(&"A ".repeat(300)),
        "raw drafts cost their full size while the pipeline is delayed"
    );
    delayed.run();
    assert!(
        delayed.total_tokens() < 800,
        "delayed pipeline: the trigger fires and the pass drops below 80%"
    );
    assert_eq!(
        delayed.items.len(),
        3,
        "delayed pipeline: the oldest draft is dropped — information is lost, not summarized"
    );
    assert!(
        delayed.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. }
                    if original.starts_with("B ") || original.starts_with("C ")
            )
        }),
        "the newest drafts survive in both worlds"
    );
}

// ── The two fixes ────────────────────────────────────────────────────────

// FIX 1 — per-job work is bounded: `compress_text` batches by sentence-chunk
// count (≤ DETERMINISTIC_MAX_CHUNKS per deterministic pass), so a single job
// can never run ONE full SVD + O(n³) MMR over the whole text and monopolise
// the FIFO worker. This guards the batching: an input that spans many batches
// (~800 chunks, well under the old 100k-token single-pass threshold) must
// compress quickly — a regression to a single whole-text pass would blow
// through the bound by orders of magnitude. (Timing guard, generous bound;
// the structural guarantee is the 200-chunk cap per pass.)
#[test]
fn compress_text_batches_large_inputs_and_stays_fast() {
    // ~800 sentence-chunks ≈ 38k estimated tokens — the exact input that used
    // to hit the single-pass path (≤ 100k tokens) and stall the worker.
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

// FIX 2 — the draft eviction evicts drafts whose compression has ALREADY
// landed first (their content is already reduced to a summary — the cheapest
// loss). Raw drafts with compression still in flight get a reprieve: their
// summaries are on the way, and evicting them whole would lose the full text.
// Under the old oldest-first-only order, the raw drafts would be dropped
// instead — so the assertions below discriminate the fix.
#[test]
fn evict_drafts_prefers_already_compressed_drafts_over_in_flight_raw() {
    let mut cm = cm(1000); // trigger = 800
    cm.add_user(&"U ".repeat(50));
    // A, B: raw drafts with compression IN FLIGHT (never returned).
    for label in ["A ", "B "] {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::Assistant {
            id,
            original: label.repeat(250),
            compressed: None,
            compressible: true,
        });
    }
    // C, D: drafts whose compressed summaries ALREADY landed.
    for label in ["C ", "D "] {
        let id = cm.next_id();
        cm.items.push_back(ContextItem::Assistant {
            id,
            original: label.repeat(400),
            compressed: Some(label.repeat(250)),
            compressible: true,
        });
    }
    assert_eq!(
        cm.total_tokens(),
        estimate_tokens(&"U ".repeat(50))
            + 2 * estimate_tokens(&"A ".repeat(250))
            + 2 * estimate_tokens(&"C ".repeat(250)),
        "precondition: raw drafts at full cost, compressed at summary cost"
    );
    assert!(cm.total_tokens() >= 800, "precondition: over the trigger");

    cm.run();

    // The compressed drafts (C, D) were evicted to cross below the trigger…
    assert!(
        !cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. }
                    if original.starts_with("C ") || original.starts_with("D ")
            )
        }),
        "already-compressed drafts are the first eviction target"
    );
    // …while the in-flight raw drafts (A, B) survived with their summaries on
    // the way — evicting them whole would have lost the full text.
    assert!(
        cm.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::Assistant { original, .. }
                    if original.starts_with("A ") || original.starts_with("B ")
            )
        }),
        "in-flight raw drafts get a reprieve — their summaries are still coming"
    );
    assert!(
        cm.total_tokens() < 800,
        "minimum decompaction: the pass stops right below the trigger"
    );
}
