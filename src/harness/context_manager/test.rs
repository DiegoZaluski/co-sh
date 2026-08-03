//! Tests for the single-owner LLM-free asynchronous context manager.
//!
//! Coverage: in-place async compression (1 item = 1 message), protected user
//! prompts, the native `tool_call → tool` structural chain, `LoopClosure`
//! promotion + guard, the 80/40 compaction phases (chain-aware), message
//! rendering (positions + roles), save/restore round-trip, display info, and
//! integration with the harness payload.

use super::*;

fn cm(max_tokens: usize) -> ContextManager {
    ContextManager::new(max_tokens)
}

/// Poll until every item that is EXPECTED to compress has done so (with a
/// generous deadline), so tests that rely on the async compression resolve
/// deterministically.
///
/// Only un-protected user prompts and compressible assistant texts are waited
/// on — protected prompts and non-compressible (tool-carrying) assistants
/// legitimately keep `compressed: None` forever, so waiting on them would spin
/// the full deadline.
fn wait_for_compression(cm: &mut ContextManager) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cm.items.iter().any(|it| {
        matches!(
            it,
            ContextItem::User {
                protected: false,
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

    // No poll has run since the job was sent, so the item is deterministically
    // still raw — even if the worker already finished, its result is sitting in
    // the channel unconsumed. Its raw rendering therefore costs the original.
    assert!(matches!(
        &cm.items[1],
        ContextItem::Assistant {
            compressed: None,
            ..
        }
    ));
    assert_eq!(cm.items[1].tokens(), estimate_tokens(&text));

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

    // A third prompt un-protects the first one and submits it.
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
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User {
                compressed: Some(_),
                ..
            }
        )),
        "the oldest protected prompt should have been compressed"
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
        cm.items[0].tokens() == estimate_tokens("Let me check the file first"),
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
// must un-protect the OLDEST protected prompt (not a compressed one) and
// submit it for compression — exactly as it would without the restore.
#[test]
fn restore_then_new_user_unprotects_oldest_prompt() {
    let mut manager = cm(10_000);
    manager.add_user("first prompt");
    manager.add_user("second prompt");
    let state = manager.save_state();

    let mut restored = cm(10_000);
    restored.restore_state(&state);
    assert_eq!(restored.items.len(), 2);
    assert_eq!(restored.user_prompts.len(), 2);

    // A new prompt arrives on the restored session.
    restored.add_user("third prompt");
    wait_for_compression(&mut restored);

    // Exactly two protected prompts remain, and the oldest is now un-protected
    // and compressed (it was submitted by the transition).
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
    assert!(matches!(
        &restored.items[0],
        ContextItem::User {
            protected: false,
            compressed: Some(_),
            ..
        }
    ));
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
fn phase1_removes_tools_middle_out_and_preserves_chains() {
    let mut cm = cm(1000);
    for i in 0..3 {
        cm.add_tool_call(&format!("c{i}"), "read_file", &"x".repeat(100));
        cm.add_tool_result(&format!("c{i}"), &"payload ".repeat(300));
    }
    assert_eq!(cm.items.len(), 6);
    cm.run();
    assert!(
        cm.total_tokens() < 800,
        "Phase 1 must drop the context below the 80% trigger, got {}",
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

// The middle-out ORDER matters: Phase 1 must remove the middle tool chains
// first and preserve the NEWEST tool (the most relevant recent context). A
// wrong order that removed the newest tool first would pass the generic
// `phase1_removes_tools_middle_out_and_preserves_chains` test (any order gets
// under the trigger), so this test calls `phase1` directly.
//
// Asserting only that the NEWEST chain survives does NOT distinguish middle-out
// from oldest-first (both preserve the newest). Asserting that the MIDDLE chain
// (c2, the first removal target) is gone does: oldest-first would keep c2 and
// drop c0/c1 instead. Together the two asserts pin the exact middle-out order.
#[test]
fn phase1_preserves_the_newest_tool_chain() {
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

    cm.phase1();

    assert!(
        cm.total_tokens() < 800,
        "phase1 must drop below the 80% trigger, got {}",
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
fn phase2_keeps_loop_closures_and_protected_prompts() {
    let mut cm = cm(1000);
    // 50 (protected) + 50 (loop) + 400 (draft a) + 400 (draft b) = 900,
    // which fires the 80% trigger (800). After Phase 2 removes both drafts
    // the floor is 100 ≤ 40% (400), so Phase 3 never fires and the
    // LoopClosure + protected prompt survive.
    cm.items.push_back(ContextItem::User {
        id: 1,
        protected: true,
        original: "x ".repeat(50),
        compressed: None,
    });
    cm.items.push_back(ContextItem::LoopClosure {
        id: 2,
        content: "x ".repeat(50),
    });
    cm.items.push_back(ContextItem::Assistant {
        id: 3,
        original: "draft ".repeat(400),
        compressed: None,
        compressible: true,
    });
    cm.items.push_back(ContextItem::Assistant {
        id: 4,
        original: "draft ".repeat(400),
        compressed: None,
        compressible: true,
    });
    cm.run();
    assert!(
        cm.total_tokens() <= 400,
        "after Phase 2 total must be <= 40%, got {}",
        cm.total_tokens()
    );
    assert!(
        cm.items.iter().any(|it| it.is_loop()),
        "LoopClosure must survive Phase 2"
    );
    assert!(
        cm.items.iter().any(|it| matches!(
            it,
            ContextItem::User {
                protected: true,
                ..
            }
        )),
        "protected user prompt must survive Phase 2"
    );
}

#[test]
fn phase3_removes_oldest_loop_closure_when_closures_hold_40_percent() {
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
        "after Phase 3 total must be < 40%, got {}",
        cm.total_tokens()
    );
    assert_eq!(cm.items.len(), 1, "only the newest closure should survive");
    let ContextItem::LoopClosure { content, .. } = &cm.items[0] else {
        panic!("expected LoopClosure");
    };
    assert!(content.starts_with("new "), "newest closure survives");
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
