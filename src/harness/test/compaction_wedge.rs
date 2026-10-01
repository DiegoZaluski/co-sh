//! Reproducer for the "silent summarization death" report: the budget
//! crosses 100% (way past the 80% trigger) and the LLM compaction never
//! runs again; `/compact` is equally dead; only an app reboot restores it.
//!
//! Two coupled mechanisms, both proven here:
//!
//! 1. THE TURN LATCH. Any single failed compaction attempt — including the
//!    silent `Interrupted` classification — sets
//!    `automatic_compaction_failed` in `run_agent_loop_inner`, which skips
//!    the 80% trigger for the REST OF THE TURN. Tool dispatches keep growing
//!    the context every iteration, so the budget climbs past 80%, 85% … and
//!    over 100% with zero further summarization attempts.
//! 2. THE ESC-WEDGED STOP FLAG. The TUI flips the shared `stop_signal` on
//!    ESC/Interrupt and the harness side never cleared it — only
//!    `start_agent_loop` did, on the direct-send path. `/compact` cloned
//!    the flag WITHOUT clearing it (`start_manual_compaction` →
//!    `compact_on_demand`), so a flag wedged by an earlier ESC handed the
//!    manual pass a still-set signal. Every summarizer entry point races
//!    that flag BEFORE connecting (`tokio::select!` against
//!    `wait_for_stop_signal`, 50 ms poll): an already-set flag won the race
//!    immediately → `CompactionErr::Interrupted` → the manual pass failed no
//!    matter how often it was retried. A REBOOT created a fresh flag —
//!    exactly why the mechanism came back after restarting the app.
//!
//!    THE FIX, covered here: both sides of the handoff now clear the flag
//!    before the compaction starts — the TUI in `start_manual_compaction`
//!    (its own copy) and the harness in `compact_on_demand` (whatever
//!    arrives through the API), mirroring the `start_agent_loop` contract.
//!    `fixed_compact_on_demand_clears_the_wedged_flag` proves the manual
//!    pass now survives a flag wedged by an earlier ESC.
//!
//! The race is proven against the REAL SDK streaming path (a local
//! HTTP/SSE server): the test-only mock response short-circuits before the
//! select, so mocks cannot reproduce it — production always can, because
//! the connect future is pending while the wait arm is ready on first poll.
//!
//! `repro_summarizer_dies_silently_to_orphan_stop_signal` proves the wedged
//! loop entry; `repro_budget_climbs_past_100pct_after_one_silent_death`
//! proves the latch half of the budget blowout;
//! `fixed_compact_on_demand_clears_the_wedged_flag` proves the `/compact`
//! fix; `repro_fresh_flag_after_reboot_revives_summarization`
//! proves a cleared flag (the reboot) is all it takes to revive everything.

use super::super::core::{Harness, INTERRUPTED_MARKER, ManualCompactionOutcome};
use super::super::events::HarnessEvent;
use crate::harness::context::{ContextItem, ContextManager};
use crate::harness::events::LlmCompactionEvent;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Over-trigger history with nothing for the useless-chain sweep to remove,
/// so `run()` keeps demanding LLM compaction after every dispatch.
fn over_trigger_harness() -> Harness {
    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    h
}

/// A real local HTTP/SSE summarizer endpoint. The response is delayed so the
/// `wait_for_stop_signal` arm of the pre-connect `tokio::select!` — ready on
/// FIRST poll when the shared flag is already set — deterministically wins
/// the race, exactly like a wedged production session.
async fn streaming_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    // Drain the request HEADERS *and BODY* before answering.
                    // Stopping at the header terminator (the old logic) left
                    // the ~9 KB summarizer prompt mid-flight: the socket
                    // closed under the client, which got an RST and failed
                    // the request with "error sending request" — before the
                    // 200 ms reply window below ever mattered.
                    let header_end = bytes
                        .windows(4)
                        .position(|part| part == b"\r\n\r\n")
                        .map(|i| i + 4);
                    if let Some(end) = header_end {
                        let content_length = String::from_utf8_lossy(&bytes[..end])
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.trim()
                                    .eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + content_length {
                            break;
                        }
                    }
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    }
                }
                // The wedge wins the race before this delay elapses.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"## Objective\\n- compacted\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    (address, server)
}

/// A connector pointed at the local server — the production streaming path
/// (`stream_chat_with_system_no_tools`) with no test mocks in the way.
fn local_connector(address: std::net::SocketAddr) -> cosh_sdk::connector::Connector {
    cosh_sdk::connector::Connector::new("openrouter")
        .unwrap()
        .with_base_url(format!("http://{address}/v1"))
        .with_api_key("test-only")
        .with_model("glm-5.3-flash")
        .with_tool_call_mode(cosh_sdk::connector::ToolCallMode::Inline)
}

/// PROVES the ESC-wedged loop entry: with the shared stop flag ALREADY SET,
/// the summarizer request is aborted at the pre-connect race (the real SDK
/// path, no mocks), classified `Interrupted`, gets NO toast and no
/// explanation, and the turn ends with the context still pinned over the
/// trigger and no checkpoint committed.
#[tokio::test]
async fn repro_summarizer_dies_silently_to_orphan_stop_signal() {
    let (address, server) = streaming_server().await;
    let mut h = over_trigger_harness();
    h = h.with_connector(local_connector(address));
    h = h.with_mock_stream(Ok(vec!["final answer"]));

    // The ESC wedge: the flag was flipped and never cleared by the harness.
    let stop_signal = Arc::new(AtomicBool::new(true));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal),
    )
    .await
    .expect("the agent loop must terminate");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    server.abort();

    // Silent death: the summarizer never produced a response (no tokens were
    // streamed to the TUI — the call died before the server ever answered)…
    assert!(
        !events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the wedged flag must abort the summarizer call before any response; events={events:?}"
    );
    // …the attempt opened the Summarizing box and failed silently…
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started
            }
        )),
        "the attempt was started; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "and died at the race; events={events:?}"
    );
    // …and NOTHING tells the user why: no toast of any variant. That is the
    // "sem motivo óbvio" of the report — an Interrupted classification emits
    // no toast on the automatic path.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Toast { .. })),
        "the death must be unexplained — that is the bug; events={events:?}"
    );
    // The turn does not recover the context: still over the trigger, no
    // checkpoint committed.
    assert!(
        !h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(item, ContextItem::Compaction { .. })),
        "no summary may be committed while wedged"
    );
    assert!(
        h.context_manager.display_info().budget_pct >= 80,
        "the context remains pinned at/over the trigger (budget_pct={})",
        h.context_manager.display_info().budget_pct
    );
}

/// PROVES the turn-level latch: once one compaction attempt failed (here the
/// silent `Interrupted` classification — e.g. ESC during the Summarizing
/// box), the 80% trigger is abandoned for the REST of the turn — even when a
/// perfectly working summarizer is queued. Repeated dispatches push the
/// budget past 100% with zero further attempts: "o budget passou dos 100%".
#[tokio::test]
async fn repro_budget_climbs_past_100pct_after_one_silent_death() {
    let mut h = over_trigger_harness();
    // First summarizer attempt fails as Interrupted; everything after would
    // succeed — but the turn is already poisoned.
    h = h
        .with_mock_chats(vec![
            Err(INTERRUPTED_MARKER),
            Ok("## Objective\n- compacted"),
            Ok("## Objective\n- compacted"),
        ])
        .with_test_tool(
            "test_tool",
            serde_json::json!({
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x"]
            }),
        );
    // Two tool dispatches follow: each pushes the total further past the
    // trigger. Without the latch, the second attempt would succeed.
    h.mock_stream_queue.push_back(Ok(vec![format!(
        r#"{{"name": "test_tool", "arguments": {{"x": "{}"}}}}"#,
        "a".repeat(4_000)
    )]));
    h.mock_stream_queue.push_back(Ok(vec![format!(
        r#"{{"name": "test_tool", "arguments": {{"x": "{}"}}}}"#,
        "b".repeat(4_000)
    )]));
    h.mock_stream_queue
        .push_back(Ok(vec!["final answer".to_string()]));

    let stop_signal = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal),
    )
    .await
    .expect("the agent loop must terminate");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // Exactly ONE summarizer call was made (the aborted one). The later
    // over-trigger dispatches — where a working summarizer was queued —
    // never ran: the turn-level latch ate them.
    assert_eq!(
        h.mock_compaction_calls().len(),
        1,
        "after one silent failure the trigger must stay dead for the turn; calls={:?}",
        h.mock_compaction_calls()
    );
    // No committed checkpoint: the budget bar keeps climbing…
    assert!(
        !h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(item, ContextItem::Compaction { .. })),
        "no summary may be committed while the latch is set"
    );
    // …past 100% of the budget, exactly as reported.
    let info = h.context_manager.display_info();
    assert!(
        info.total_tokens > info.max_tokens,
        "the budget must blow past 100% (total={}, max={})",
        info.total_tokens,
        info.max_tokens
    );
    assert_eq!(
        info.budget_pct, 100,
        "the bar clamps at 100% while overflowing"
    );
    // The only compaction event is the one dead call (Failed) — nothing
    // ongoing explains the climbing budget.
    let failures = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Failed
                }
            )
        })
        .count();
    assert_eq!(
        failures, 1,
        "one dead attempt, then silence; events={events:?}"
    );
}

/// PROVES the `/compact` fix: even with the TUI's shared stop flag still
/// set from an earlier ESC, `compact_on_demand` now clears it before the
/// summarizer's pre-connect race, so the manual pass reaches the real SDK
/// streaming path and commits the checkpoint. "Ao tentar forçar a
/// sumarização usando o /compact, a sumarização não funcionava" — agora
/// funciona na primeira tentativa, sem depender do chamador ter limpado.
#[tokio::test]
async fn fixed_compact_on_demand_clears_the_wedged_flag() {
    let (address, server) = streaming_server().await;
    let mut h = over_trigger_harness();
    h = h.with_connector(local_connector(address));
    h = h.with_mock_stream(Ok(vec!["final answer"]));

    // The TUI's shared flag — still set from the ESC wedge. The TUI clears
    // its copy in `start_manual_compaction`; this test passes it NOT cleared
    // to pin the harness-side half of the contract on its own.
    let stop_signal = Arc::new(AtomicBool::new(true));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.compact_on_demand(&tx, stop_signal),
    )
    .await
    .expect("compact_on_demand must terminate");
    server.abort();

    // The slash command succeeds despite the wedged flag…
    assert_eq!(outcome, ManualCompactionOutcome::Compacted);
    // …and the summarizer reached the real endpoint and committed the
    // checkpoint.
    assert!(
        h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(item, ContextItem::Compaction { .. })),
        "the manual pass must survive a flag wedged by an earlier ESC"
    );
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the manual pass reports its completed call; events={events:?}"
    );
    assert!(
        h.context_manager.display_info().budget_pct < 80,
        "the budget drops back under the trigger"
    );
}

/// PROVES the reboot is what revives the mechanism: a FRESH flag (what an
/// app restart produces) makes the very same over-trigger history compact
/// successfully through the same real streaming path. The wedge was never
/// about the context or the model — it was the orphaned process-global flag.
#[tokio::test]
async fn repro_fresh_flag_after_reboot_revives_summarization() {
    let (address, server) = streaming_server().await;
    let mut h = over_trigger_harness();
    h = h.with_connector(local_connector(address));
    h = h.with_mock_stream(Ok(vec!["final answer"]));

    // The reboot: a brand-new flag, cleared — exactly what a process restart
    // hands the next turn.
    let stop_signal = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal),
    )
    .await
    .expect("the agent loop must terminate");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    server.abort();

    // The summarizer ran against the real endpoint and committed the
    // checkpoint.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the compaction completes; events={events:?}"
    );
    assert!(
        h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(item, ContextItem::Compaction { .. })),
        "the checkpoint is committed after the reboot-equivalent fresh start"
    );
    assert!(
        h.context_manager.display_info().budget_pct < 80,
        "the budget drops back under the trigger"
    );
}

/// PROVES the harness contract that makes the TUI wedge dangerous: ANY loop
/// or compaction entry that starts with the shared flag already set runs
/// summarizer-dead from its first millisecond — no user keypress anywhere
/// near it. Today `start_manual_compaction` is exactly such an entry (it
/// clones the flag without clearing it); the contract pins what every entry
/// point must guarantee.
#[tokio::test]
async fn repro_entry_born_with_the_flag_set_is_summarizer_dead() {
    let (address, server) = streaming_server().await;
    let mut h = over_trigger_harness();
    h = h.with_connector(local_connector(address));
    h = h.with_mock_stream(Ok(vec!["final answer"]));

    // The previous interaction left the flag set; this entry does not clear
    // it before running.
    let stop_signal = Arc::new(AtomicBool::new(true));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.run_agent_loop("queued follow-up", tx, answer_rx, perm_rx, stop_signal),
    )
    .await
    .expect("the agent loop must terminate");
    while let Ok(_event) = rx.try_recv() {}
    server.abort();

    assert!(
        !h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(item, ContextItem::Compaction { .. })),
        "an entry born with the flag set has a dead summarizer from the start"
    );
    assert!(
        h.context_manager.display_info().budget_pct >= 80,
        "the entry runs its whole lifetime over the trigger with no relief"
    );
}
