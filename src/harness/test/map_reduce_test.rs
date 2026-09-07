use super::*;
use crate::harness::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};
use std::time::Instant;

// This fixture tests orchestration and transport retention, not model quality.
// Scripted replies are an oracle; no claim about a real model's recall follows.
const TRACE_FACTS: [&str; 5] = [
    "CONSTRAINT: never publish credentials",
    "DECISION: use append-only history",
    "VERIFIED: parser tests passed",
    "INVALIDATION: old.rs changed; reread before editing",
    "OPEN: verify migration on the child branch",
];

fn long_trace() -> ContextManager {
    let mut manager = ContextManager::new(8_000);
    manager.add_user(&TRACE_FACTS[..2].join("\n"));
    manager.add_assistant("STALE_CONTENT: old.rs revision zero is current", false);
    for turn in 0..24 {
        match turn {
            4 => manager.add_assistant(TRACE_FACTS[2], false),
            12 => manager.add_user(TRACE_FACTS[3]),
            20 => manager.add_user(TRACE_FACTS[4]),
            _ => {}
        }
        let call = format!("read-{turn}");
        manager.add_tool_call(&call, "fs_read", r#"{"path":"old.rs"}"#);
        manager.add_tool_result(&call, &"historical file observation ".repeat(400));
        manager.add_assistant(
            &format!("Inspected revision {turn}; work remains open."),
            false,
        );
    }
    manager.add_user("Latest steering must remain verbatim.");
    manager
}

fn trace_view(manager: &ContextManager) -> String {
    manager
        .build_messages("")
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn offline_long_trace_compares_five_compaction_paths_without_claiming_model_quality() {
    let original = long_trace();
    let original_state = original.save_state();
    let original_items = serde_json::to_value(&original_state.items).unwrap();
    let before = original.display_info().total_tokens;
    let restore = || {
        let mut manager = ContextManager::new(8_000);
        manager.restore_state(&original_state);
        manager
    };
    let oracle = format!(
        "## Objective\nContinue safely\n## Important Details\n{}\n## Next Move\nReread old.rs",
        TRACE_FACTS.join("\n")
    );
    let check_facts = |manager: &ContextManager| {
        let view = trace_view(manager);
        for fact in TRACE_FACTS {
            assert!(view.contains(fact), "lost scripted fact: {fact}");
        }
    };
    let assert_original_prefix = |manager: &ContextManager| {
        let items = manager.save_state().items;
        let prefix: Vec<_> = items.iter().take(original_state.items.len()).collect();
        assert_eq!(serde_json::to_value(prefix).unwrap(), original_items);
    };

    let mut masking = restore();
    masking.run();
    check_facts(&masking);
    assert_original_prefix(&masking);
    assert!(masking.display_info().total_tokens < before);
    println!(
        "offline masking: before={before}, after={}, requests=0",
        masking.display_info().total_tokens
    );

    let mut one_shot = restore();
    let request = one_shot.llm_compaction_request().unwrap();
    let encoding = crate::util::TokenEncoding::for_model(None);
    let one_shot_input = encoding.estimate(&request.system) + encoding.estimate(&request.prompt);
    assert!(one_shot.apply_llm_summary(oracle.clone()));
    check_facts(&one_shot);
    assert_original_prefix(&one_shot);
    assert!(!trace_view(&one_shot).contains("STALE_CONTENT:"));
    println!(
        "offline one-shot oracle: input={one_shot_input}, fits_window={}, after={}, scripted_requests=1",
        one_shot_input < 8_000,
        one_shot.display_info().total_tokens
    );

    let mut legacy = restore();
    legacy.begin_split(8_000);
    let mut legacy_calls = 0;
    let mut legacy_peak_input = 0;
    while let Some(request) = legacy.split_next_chunk() {
        legacy_peak_input = legacy_peak_input
            .max(encoding.estimate(&request.system) + encoding.estimate(&request.prompt));
        let summary = if legacy_calls == 0 {
            oracle.as_str()
        } else {
            "Additional historical inspection completed."
        };
        legacy.advance_split(summary, request.chunk_end);
        legacy_calls += 1;
        assert!(legacy_calls < 100);
    }
    assert!(legacy.commit_split());
    check_facts(&legacy);
    assert_original_prefix(&legacy);
    println!(
        "offline legacy oracle: peak_input={legacy_peak_input}, after={}, scripted_requests={legacy_calls}",
        legacy.display_info().total_tokens
    );

    let mut views = Vec::new();
    for parallel in [false, true] {
        let mut harness = Harness::new_test();
        harness.context_manager = restore();
        assert!(harness.context_manager.begin_map_reduce(8_000));
        if !parallel {
            harness.context_manager.disable_parallel_mapping();
        }
        let requests = harness.context_manager.pending_map_requests();
        let maps = requests.len();
        assert!(maps > MAP_CONCURRENCY);
        let mut peak_input = 0;
        let mut mapped_facts = Vec::new();
        for request in requests {
            peak_input = peak_input
                .max(encoding.estimate(&request.system) + encoding.estimate(&request.prompt));
            assert!(!request.prompt.contains("[Continuation context"));
            let facts: Vec<_> = TRACE_FACTS
                .iter()
                .filter(|fact| request.prompt.contains(**fact))
                .copied()
                .collect();
            mapped_facts.extend(facts.iter().copied());
            let reply = if facts.is_empty() {
                "Historical inspection; no new task decision.".into()
            } else {
                facts.join("\n")
            };
            harness.mock_chat_queue.push_back(Ok(reply));
        }
        for fact in TRACE_FACTS {
            assert!(
                mapped_facts.contains(&fact),
                "scripted maps lost source fact: {fact}"
            );
        }
        harness.mock_chat_queue.push_back(Ok(oracle.clone()));
        harness.mock_chat_queue.push_back(Ok("PASS".into()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let started = Instant::now();
        assert!(harness.drive_map_reduce(&tx).await.unwrap());
        let elapsed = started.elapsed();
        assert!(harness.mock_chat_queue.is_empty());
        check_facts(&harness.context_manager);
        assert_original_prefix(&harness.context_manager);
        let view = trace_view(&harness.context_manager);
        assert!(view.contains("Latest steering must remain verbatim."));
        assert!(!view.contains("STALE_CONTENT:"));
        let snapshots = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| matches!(event, HarnessEvent::ContextSnapshot { .. }))
            .count();
        assert_eq!(snapshots, maps + 2);
        println!(
            "offline MapReduce parallel={parallel}: peak_map_input={peak_input}, after={}, scripted_requests={}, local_elapsed_us={}",
            harness.context_manager.display_info().total_tokens,
            maps + 2,
            elapsed.as_micros()
        );
        views.push(view);
    }
    assert_eq!(views[0], views[1], "scheduling must not change the handoff");
}

fn staged_harness() -> Harness {
    let mut harness = Harness::new_test();
    harness.context_manager = ContextManager::new(2_000);
    for index in 0..24 {
        harness
            .context_manager
            .add_user(&format!("source {index}: {}", "detail ".repeat(100)));
        harness.context_manager.add_assistant("acknowledged", true);
    }
    harness
        .context_manager
        .add_assistant("recent raw answer", true);
    assert!(harness.context_manager.begin_map_reduce(600));
    harness
}

#[tokio::test]
async fn parallel_maps_persist_out_of_order_results_by_stable_ordinal() {
    let mut harness = staged_harness().with_mock_map_delays(&[80, 1, 20, 30]);
    let requests = harness.context_manager.pending_map_requests();
    for request in &requests {
        harness
            .mock_chat_queue
            .push_back(Ok(format!("map {}", request.ordinal)));
    }
    let original = serde_json::to_value(harness.context_manager.save_state().items).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    harness.run_parallel_maps(requests, &tx).await.unwrap();
    let state = harness.context_manager.save_state();
    let staging = state.map_reduce.unwrap();
    for segment in &staging.segments {
        assert_eq!(segment.summary, Some(format!("map {}", segment.ordinal)));
    }
    assert_eq!(serde_json::to_value(state.items).unwrap(), original);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let snapshots: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            HarnessEvent::ContextSnapshot { context } => context.map_reduce.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots.len(), staging.segments.len());
    assert!(snapshots[0].segments[0].summary.is_none());
    assert!(snapshots[0].segments[1].summary.is_some());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, HarnessEvent::LlmCompactionToken { .. }))
    );
}

#[tokio::test]
async fn parallel_failure_drains_successes_then_retries_only_missing_maps_sequentially() {
    let mut harness = staged_harness().with_mock_map_delays(&[1, 20, 30, 40]);
    let count = harness.context_manager.pending_map_requests().len();
    assert!(count > MAP_CONCURRENCY);
    harness
        .mock_chat_queue
        .push_back(Err("429 concurrent request limit".into()));
    for ordinal in 1..MAP_CONCURRENCY {
        harness
            .mock_chat_queue
            .push_back(Ok(format!("accepted {ordinal}")));
    }
    harness.mock_chat_response = Some(Ok("recovered map".into()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let requests = harness.context_manager.pending_map_requests();
    harness.run_parallel_maps(requests, &tx).await.unwrap();
    assert!(!harness.context_manager.parallel_mapping_enabled());
    assert_eq!(
        harness.context_manager.map_progress(),
        (MAP_CONCURRENCY - 1, count)
    );
    let saved = harness.context_manager.save_state();
    harness.context_manager.restore_state(&saved);
    assert!(!harness.context_manager.parallel_mapping_enabled());
    let pending = harness.context_manager.pending_map_requests().len();
    harness.mock_chat_response = None;
    for _ in 0..pending {
        harness
            .mock_chat_queue
            .push_back(Ok("recovered map".into()));
    }
    harness
        .mock_chat_queue
        .push_back(Ok("validated checkpoint".into()));
    harness.mock_chat_queue.push_back(Ok("PASS".into()));
    assert!(harness.drive_map_reduce(&tx).await.unwrap());
    assert!(harness.mock_chat_queue.is_empty());
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(
        event,
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Progress {
                phase: LlmCompactionPhase::SequentialFallback,
                ..
            }
        }
    )));
    let last_text = events.iter().rev().find_map(|event| match event {
        HarnessEvent::LlmCompactionToken { text } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(last_text, Some("validated checkpoint"));
}

#[tokio::test]
async fn cancellation_preserves_completed_maps_without_committing_a_checkpoint() {
    let mut harness = staged_harness().with_mock_map_delays(&[1, 5_000, 5_000, 5_000]);
    harness.mock_chat_response = Some(Ok("accepted map".into()));
    let stop = Arc::new(AtomicBool::new(false));
    harness.stop_signal = Some(stop.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let requests = harness.context_manager.pending_map_requests();
    let stopper = async {
        while let Some(event) = rx.recv().await {
            if matches!(event, HarnessEvent::ContextSnapshot { .. }) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    };
    let (result, ()) = tokio::join!(harness.run_parallel_maps(requests, &tx), stopper);
    assert!(matches!(result, Err(CompactionErr::Interrupted)));
    assert!(harness.context_manager.map_progress().0 >= 1);
    assert!(
        !harness
            .context_manager
            .save_state()
            .items
            .iter()
            .any(|item| matches!(item, super::super::context::ContextItem::Compaction { .. }))
    );
}

#[tokio::test]
async fn segment_overflow_repartitions_without_losing_accepted_maps() {
    let mut harness = staged_harness().with_mock_map_delays(&[1, 20, 30, 40]);
    harness
        .mock_chat_queue
        .push_back(Err(format!("{CONTEXT_WINDOW_MARKER}:400")));
    for ordinal in 1..MAP_CONCURRENCY {
        harness
            .mock_chat_queue
            .push_back(Ok(format!("accepted {ordinal}")));
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let requests = harness.context_manager.pending_map_requests();
    harness.run_parallel_maps(requests, &tx).await.unwrap();
    let saved = harness.context_manager.save_state();
    let state = saved.map_reduce.unwrap();
    assert_eq!(state.window, 400);
    assert_eq!(state.repartitions, 1);
    assert!(!state.parallel_disabled);
    for ordinal in 1..MAP_CONCURRENCY {
        assert!(
            state
                .segments
                .iter()
                .any(|segment| segment.ordinal == ordinal
                    && segment.summary == Some(format!("accepted {ordinal}")))
        );
    }
}

#[tokio::test]
async fn real_map_stream_sends_an_intact_item_above_the_soft_target_and_reports_usage() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut manager = ContextManager::new(4_000);
    let source = format!("START_OF_ITEM {} END_OF_ITEM", "evidence áβ ".repeat(900));
    manager.add_user(&source);
    manager.add_assistant("recent raw tail", true);
    assert!(manager.begin_map_reduce(4_000));
    let staging = manager.save_state().map_reduce.unwrap();
    assert_eq!(staging.segments.len(), 1);
    assert_eq!(staging.segments[0].item_ids, vec![1]);
    let request = manager.pending_map_requests().remove(0);
    let encoding = crate::util::TokenEncoding::for_model(Some("test-model"));
    assert!(encoding.estimate(&source) > staging.map_target_tokens);
    assert!(
        encoding.estimate(&request.system) + encoding.estimate(&request.prompt) + 64 + 400 < 4_000
    );
    let expected_prompt = request.prompt.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse().ok())
                    })
                    .unwrap();
                if bytes.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        let request = String::from_utf8(bytes).unwrap();
        assert!(!request.contains("\"tools\":"));
        assert!(request.contains("\"max_tokens\":400"));
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|message| { message["content"].as_str() == Some(expected_prompt.as_str()) })
        );
        assert!(expected_prompt.contains(&source));
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"map result\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15,\"cost\":0.01}}\n\ndata: [DONE]\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let connector = Connector::new("openrouter")
        .unwrap()
        .with_base_url(format!("http://{address}/v1"))
        .with_api_key("test-only")
        .with_model("test-model")
        .with_tools(vec![]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = Harness::summarize_map_with_connector(
        connector,
        request.system,
        request.prompt,
        None,
        tx,
        4_000,
    )
    .await
    .unwrap();
    assert_eq!(result, "map result");
    server.await.unwrap();
    assert!(
        matches!(rx.recv().await, Some(HarnessEvent::Usage { reported_cost: Some(cost), .. }) if cost == 0.01)
    );
}

#[tokio::test]
async fn undersized_window_is_rejected_before_contacting_provider() {
    let connector = Connector::new("openrouter")
        .unwrap()
        .with_base_url("http://127.0.0.1:1/v1")
        .with_api_key("test-only")
        .with_model("test-model");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = Harness::summarize_map_with_connector(
        connector,
        "system instructions ".repeat(100),
        "prompt".into(),
        None,
        tx,
        100,
    )
    .await;
    assert!(matches!(
        result,
        Err(CompactionErr::ContextWindow {
            window_tokens: Some(100)
        })
    ));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn whole_item_exceeding_the_actual_window_fails_without_slicing_or_committing() {
    let mut manager = ContextManager::new(4_000);
    let source = "immutable evidence áβ ".repeat(4_000);
    manager.add_user(&source);
    manager.add_assistant("recent raw tail", true);
    let before = serde_json::to_value(manager.save_state().items).unwrap();
    assert!(manager.begin_map_reduce(4_000));
    let requests = manager.pending_map_requests();
    assert_eq!(requests.len(), 1);
    let request = requests.into_iter().next().unwrap();
    assert!(request.prompt.contains(&source));
    let connector = Connector::new("openrouter")
        .unwrap()
        .with_base_url("http://127.0.0.1:1/v1")
        .with_api_key("test-only")
        .with_model("test-model");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = Harness::summarize_map_with_connector(
        connector,
        request.system,
        request.prompt,
        None,
        tx,
        4_000,
    )
    .await;
    assert!(matches!(
        result,
        Err(CompactionErr::ContextWindow {
            window_tokens: Some(4_000)
        })
    ));
    assert!(rx.try_recv().is_err());
    assert!(!manager.repartition_incomplete_maps(Some(4_000)));
    assert!(!manager.commit_map_reduce());
    assert_eq!(
        before,
        serde_json::to_value(manager.save_state().items).unwrap()
    );
    assert_eq!(manager.map_progress(), (0, 1));
}
#[tokio::test]
async fn resumed_plan_rehydrates_tools_before_the_first_loop() {
    use cosh_tools::plan::types::TodoList;
    let plan: TodoList = serde_json::from_value(serde_json::json!({"groups": [{
        "title": "Release", "tests_verified": true, "items": [{
            "id": "task-1", "description": "Keep API compatibility",
            "status": "InProgress", "depends_on": []
        }]
    }]}))
    .unwrap();
    let mut harness = Harness::new_test().with_mock_stream(Ok(vec!["Ready"]));
    harness.cosh_tools = Some(super::CoshTools::new("."));
    harness.context_manager.set_todo_list(plan.clone());
    let state = harness.context_manager.save_state();
    harness.context_manager.restore_state(&state);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    harness
        .run_agent_loop(
            "continue",
            tx,
            answer_rx,
            perm_rx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    assert_eq!(
        serde_json::to_value(harness.cosh_tools.as_ref().unwrap().todo_list()).unwrap(),
        serde_json::to_value(&plan).unwrap()
    );
    assert_eq!(
        serde_json::to_value(harness.context_manager.save_state().todo).unwrap(),
        serde_json::to_value(Some(plan)).unwrap()
    );

    // Reuse the harness after reverting to a point before any plan existed.
    let mut earlier = state;
    earlier.todo = None;
    harness.context_manager.restore_state(&earlier);
    harness = harness.with_mock_stream(Ok(vec!["Ready again"]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    harness
        .run_agent_loop(
            "continue",
            tx,
            answer_rx,
            perm_rx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    assert!(
        harness
            .cosh_tools
            .as_ref()
            .unwrap()
            .todo_list()
            .groups
            .is_empty()
    );
}
/// A real local SSE response exercises the SDK and both summarization consumers.
async fn summary_fixture(reason: Option<&str>) -> (Connector, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reason = reason.map(str::to_owned);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:")?.trim().parse().ok())
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let chunk = serde_json::json!({"choices":[{"delta":{"content":"partial checkpoint"},"finish_reason":reason}]});
        let body = format!(
            "data: {chunk}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}}}\n\ndata: [DONE]\n\n"
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    (
        Connector::new("openrouter")
            .unwrap()
            .with_base_url(format!("http://{address}/v1"))
            .with_api_key("test-only")
            .with_model("test-model"),
        server,
    )
}

#[tokio::test]
async fn incomplete_summary_streams_are_rejected_by_both_consumers() {
    for reason in [
        Some("length"),
        Some("max_tokens"),
        Some("MAX_TOKENS"),
        Some("content_filter"),
        Some("pause_turn"),
        Some("incomplete"),
        None,
    ] {
        let (connector, server) = summary_fixture(reason).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Harness::summarize_map_with_connector(
            connector,
            "summarize".into(),
            "source".into(),
            None,
            tx,
            4_000,
        )
        .await;
        server.await.unwrap();
        assert!(result.is_err(), "accepted incomplete map: {reason:?}");
        assert!(matches!(rx.try_recv(), Ok(HarnessEvent::Usage { .. })));

        let (connector, server) = summary_fixture(reason).await;
        let mut harness = Harness::new_test();
        harness.connector = connector;
        let result = harness
            .stream_summarize_for_compaction("summarize", "source", |_| {})
            .await;
        server.await.unwrap();
        assert!(result.is_err(), "accepted incomplete one-shot: {reason:?}");
    }
}

#[tokio::test]
async fn natural_summary_termination_is_accepted() {
    for reason in ["stop", "STOP", "end_turn"] {
        let (connector, server) = summary_fixture(Some(reason)).await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Harness::summarize_map_with_connector(
            connector,
            "summarize".into(),
            "source".into(),
            None,
            tx,
            4_000,
        )
        .await
        .unwrap();
        assert_eq!(result, "partial checkpoint");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn thinking_output_reservation_is_checked_before_summary_dispatch() {
    for model in ["claude-sonnet-4-5", "claude-sonnet-4-6"] {
        let connector = Connector::new("claude")
            .unwrap()
            .with_model(model)
            .with_reasoning_effort("high")
            .with_api_key("test-only")
            .with_base_url("http://127.0.0.1:1")
            .with_retry(false);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Harness::summarize_map_with_connector(
            connector.clone(),
            "summarize".into(),
            "source".into(),
            None,
            tx,
            8_000,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(CompactionErr::ContextWindow {
                    window_tokens: Some(8_000)
                })
            ),
            "request should fail preflight, not contact the provider: {result:?}"
        );
        let mut harness = Harness::new_test().with_discovered_window(8_000);
        harness.connector = connector;
        let result = harness
            .stream_summarize_for_compaction("summarize", "source", |_| {})
            .await;
        assert!(matches!(
            result,
            Err(CompactionErr::ContextWindow {
                window_tokens: Some(8_000)
            })
        ));
    }
}

#[tokio::test]
async fn incomplete_one_shot_never_commits_or_hides_source() {
    let (connector, server) = summary_fixture(Some("length")).await;
    let mut harness = Harness::new_test();
    harness.connector = connector.with_retry(false);
    harness
        .context_manager
        .add_user("Never change the public API");
    harness
        .context_manager
        .add_assistant("Integration tests remain open", true);
    harness.context_manager.begin_manual_compaction();
    let before = serde_json::to_value(harness.context_manager.save_state()).unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(!harness.llm_compact(&tx).await);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(harness.context_manager.save_state()).unwrap(),
        before
    );
}

// CM-05: adversarial omission fixtures. The scripted replies are an oracle
// for ORCHESTRATION behavior only: they show what the pipeline can detect
// with deterministic inputs, never what a real model would recall.
#[tokio::test]
async fn map_omissions_are_invisible_to_the_audit_and_silently_committed() {
    let mut manager = long_trace();
    assert!(manager.begin_map_reduce(8_000));
    // Every map deliberately drops the constraint and the open task; the
    // reducer stays consistent with what the maps reported.
    for request in manager.pending_map_requests() {
        let kept: Vec<_> = TRACE_FACTS
            .iter()
            .filter(|fact| **fact != TRACE_FACTS[0] && **fact != TRACE_FACTS[4])
            .filter(|fact| request.prompt.contains(**fact))
            .copied()
            .collect();
        let reply = if kept.is_empty() {
            "Historical inspection; nothing to report.".to_string()
        } else {
            kept.join("\n")
        };
        assert!(
            manager.accept_map_summary(request.ordinal, &reply),
            "map {ordinal} reply must be accepted",
            ordinal = request.ordinal
        );
    }
    while let Some(request) = manager.next_reduce_request() {
        let summary = format!(
            "## Objective\nContinue safely\n## Important Details\n{}\n## Next Move\nReread old.rs",
            [TRACE_FACTS[1], TRACE_FACTS[2], TRACE_FACTS[3]].join("\n")
        );
        assert!(manager.accept_reduce_summary(&request, &summary));
    }
    // The structural gap: the validator's inputs are the candidate and the
    // map summaries only. The raw source items that hold the dropped facts
    // never enter the comparison, so even a perfect auditor cannot flag
    // what it never sees. This is the orchestration boundary CM-05 records.
    let validation = manager.next_validation_request().unwrap();
    assert!(
        validation
            .prompt
            .contains("DECISION: use append-only history")
    );
    assert!(!validation.prompt.contains(TRACE_FACTS[0]));
    assert!(!validation.prompt.contains(TRACE_FACTS[4]));
    assert!(manager.accept_validation(&validation, "PASS"));
    assert!(manager.commit_map_reduce());
    let view = trace_view(&manager);
    assert!(view.contains(TRACE_FACTS[1]));
    assert!(!view.contains(TRACE_FACTS[0]), "dropped constraint is lost");
    assert!(!view.contains(TRACE_FACTS[4]), "dropped open task is lost");
}

#[tokio::test]
async fn audit_recovers_reducer_omissions_within_the_correction_budget() {
    let mut harness = Harness::new_test();
    harness.context_manager = long_trace();
    assert!(harness.context_manager.begin_map_reduce(8_000));
    // Every map keeps every fact it saw.
    for request in harness.context_manager.pending_map_requests() {
        let facts: Vec<_> = TRACE_FACTS
            .iter()
            .filter(|fact| request.prompt.contains(**fact))
            .copied()
            .collect();
        let reply = if facts.is_empty() {
            "Historical inspection; nothing to report.".to_string()
        } else {
            facts.join("\n")
        };
        harness.mock_chat_queue.push_back(Ok(reply));
    }
    // The reducer drops the constraint and the open task.
    harness.mock_chat_queue.push_back(Ok(format!(
        "## Objective\nContinue safely\n## Important Details\n{}\n## Next Move\nReread old.rs",
        [TRACE_FACTS[1], TRACE_FACTS[2], TRACE_FACTS[3]].join("\n")
    )));
    // The audit compares against the map summaries and flags both losses.
    harness.mock_chat_queue.push_back(Ok(format!(
        "Restore the dropped constraint '{}' and the open task '{}'.",
        TRACE_FACTS[0], TRACE_FACTS[4]
    )));
    // The correction rebuilds the full oracle and passes re-audit.
    harness.mock_chat_queue.push_back(Ok(format!(
        "## Objective\nContinue safely\n## Important Details\n{}\n## Next Move\nReread old.rs",
        TRACE_FACTS.join("\n")
    )));
    harness.mock_chat_queue.push_back(Ok("PASS".into()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(harness.drive_map_reduce(&tx).await.unwrap());
    assert!(harness.mock_chat_queue.is_empty());
    let view = trace_view(&harness.context_manager);
    for fact in TRACE_FACTS {
        assert!(view.contains(fact), "correction must restore: {fact}");
    }
}

// CM-06: chronological precedence and cross-range audit scope. Scripted
// replies remain an orchestration oracle, never a model-quality claim.
#[tokio::test]
async fn late_invalidation_supersedes_earlier_state_in_the_reduction() {
    let mut manager = long_trace();
    assert!(manager.begin_map_reduce(8_000));
    for request in manager.pending_map_requests() {
        let facts: Vec<_> = TRACE_FACTS
            .iter()
            .filter(|fact| request.prompt.contains(**fact))
            .copied()
            .collect();
        let reply = if facts.is_empty() {
            "Historical inspection; nothing to report.".to_string()
        } else {
            facts.join("\n")
        };
        assert!(manager.accept_map_summary(request.ordinal, &reply));
    }
    // The reducer is explicitly instructed to apply chronological precedence.
    let reduce = manager.next_reduce_request().unwrap();
    assert!(reduce.prompt.contains("Preserve chronological precedence"));
    let oracle = format!(
        "## Objective\nContinue safely\n## Important Details\n{}\n## Next Move\nReread old.rs",
        TRACE_FACTS.join("\n")
    );
    while let Some(request) = manager.next_reduce_request() {
        assert!(manager.accept_reduce_summary(&request, &oracle));
    }
    while let Some(validation) = manager.next_validation_request() {
        assert!(manager.accept_validation(&validation, "PASS"));
    }
    assert!(manager.commit_map_reduce());
    let view = trace_view(&manager);
    for fact in TRACE_FACTS {
        assert!(view.contains(fact));
    }
    assert!(!view.contains("STALE_CONTENT:"));
}

#[tokio::test]
async fn cross_group_audits_cannot_see_another_groups_invalidation() {
    let mut manager = ContextManager::new(2_000);
    for turn in 0..8 {
        if turn < 4 {
            manager.add_user(&format!(
                "early turn {turn}: DECISION: use SQLite for storage. {}",
                "history ".repeat(600)
            ));
        } else {
            manager.add_user(&format!(
                "late turn {turn}: INVALIDATION: SQLite replaced by Postgres. {}",
                "history ".repeat(600)
            ));
        }
        manager.add_assistant("acknowledged", false);
    }
    let source_before = serde_json::to_value(manager.save_state().items).unwrap();
    assert!(manager.begin_map_reduce(2_000));
    for request in manager.pending_map_requests() {
        let reply = if request.prompt.contains("replaced by Postgres") {
            format!(
                "INVALIDATION: SQLite replaced by Postgres\n{}",
                "note ".repeat(450)
            )
        } else if request.prompt.contains("use SQLite") {
            format!("DECISION: use SQLite for storage\n{}", "note ".repeat(450))
        } else {
            format!("Historical inspection. {}", "note ".repeat(450))
        };
        assert!(manager.accept_map_summary(request.ordinal, &reply));
    }
    // The candidate correctly applies precedence: it keeps the invalidation
    // and drops the superseded SQLite decision.
    let candidate = "## Objective\n- continue on Postgres\n## Important Details\n\
                     INVALIDATION: SQLite replaced by Postgres\n## Next Move\nmigrate the schema";
    while let Some(request) = manager.next_reduce_request() {
        assert!(manager.accept_reduce_summary(&request, candidate));
    }
    // The first audit group holds only the early summaries: it sees the stale
    // decision but never the later invalidation that justifies dropping it.
    // A compliant auditor must therefore flag the correct drop as an
    // omission — cross-range context never reaches grouped audits.
    let early = manager.next_validation_request().unwrap();
    let early_summaries = early
        .prompt
        .split("[Map summaries to audit]")
        .last()
        .unwrap();
    assert!(early_summaries.contains("DECISION: use SQLite for storage"));
    assert!(
        !early_summaries.contains("replaced by Postgres"),
        "the early audit group must not see the later invalidation"
    );
    assert!(manager.accept_validation(
        &early,
        "Restore the SQLite decision; the candidate omits it."
    ));
    // Intermediate groups stay early-era; keep accepting them until a group
    // carrying the invalidation arrives.
    let mut saw_late_group = false;
    while let Some(group) = manager.next_validation_request() {
        let summaries = group
            .prompt
            .split("[Map summaries to audit]")
            .last()
            .unwrap();
        if summaries.contains("replaced by Postgres") {
            assert!(group.start > early.start);
            saw_late_group = true;
        }
        assert!(manager.accept_validation(&group, "PASS"));
    }
    assert!(saw_late_group);
    // The false correction round-trips until the bounded budget is exhausted:
    // two corrections are the limit, then no further correction is offered
    // and nothing commits — the source stays intact for retry or abort.
    assert!(manager.correction_request().is_some());
    assert!(manager.accept_correction(candidate));
    assert!(manager.next_validation_request().unwrap().start == 0);
    assert!(manager.accept_validation(
        &manager.next_validation_request().unwrap(),
        "Restore the SQLite decision; the candidate omits it."
    ));
    while let Some(late) = manager.next_validation_request() {
        assert!(manager.accept_validation(&late, "PASS"));
    }
    assert!(manager.correction_request().is_some());
    assert!(manager.accept_correction(candidate));
    assert!(manager.correction_request().is_none());
    assert!(!manager.commit_map_reduce());
    manager.abort_map_reduce();
    assert_eq!(
        serde_json::to_value(manager.save_state().items).unwrap(),
        source_before
    );
}

// CM-07: the correction request combines the candidate with every audit from
// every validation group. Unlike reduce and validation inputs, nothing bounds
// that combination, so a many-group audit round can exceed the model window.
#[tokio::test]
async fn oversized_correction_input_fails_preflight_and_preserves_staging() {
    let mut harness = Harness::new_test().with_discovered_window(2_000);
    harness.context_manager = ContextManager::new(2_000);
    for turn in 0..8 {
        harness
            .context_manager
            .add_user(&format!("turn {turn}: {}", "history ".repeat(600)));
        harness.context_manager.add_assistant("acknowledged", false);
    }
    let source_before = serde_json::to_value(harness.context_manager.save_state().items).unwrap();
    assert!(harness.context_manager.begin_map_reduce(2_000));
    for request in harness.context_manager.pending_map_requests() {
        assert!(harness.context_manager.accept_map_summary(
            request.ordinal,
            &format!("inspection\n{}", "note ".repeat(450))
        ));
    }
    let candidate = "## Objective\n- continue\n## Next Move\nwrap up";
    while let Some(request) = harness.context_manager.next_reduce_request() {
        assert!(
            harness
                .context_manager
                .accept_reduce_summary(&request, candidate)
        );
    }
    // Every validation group flags, so the audits accumulate unbounded.
    let audit = format!("Restore the omitted details. {}", "audit ".repeat(600));
    let mut groups = 0;
    while let Some(validation) = harness.context_manager.next_validation_request() {
        assert!(
            harness
                .context_manager
                .accept_validation(&validation, &audit)
        );
        groups += 1;
    }
    assert!(groups > 1, "fixture needs several audit groups");
    let correction = harness.context_manager.correction_request().unwrap();
    let encoding = crate::util::TokenEncoding::for_model(None);
    let input = encoding.estimate(&correction.system) + encoding.estimate(&correction.prompt) + 64;
    assert!(
        input + 2_00 >= 2_000,
        "reproduced: correction input {input} plus the output reservation exceeds the window"
    );
    // The preflight rejects the dispatch before any provider contact.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let result = harness.drive_map_reduce(&tx).await;
    assert!(
        matches!(&result, Err(CompactionErr::ContextWindow { .. })),
        "correction overflow must fail in preflight, not dispatch: {result:?}"
    );
    assert_eq!(
        serde_json::to_value(harness.context_manager.save_state().items).unwrap(),
        source_before
    );
    assert!(
        harness.context_manager.save_state().map_reduce.is_some(),
        "staging stays resumable after the rejected correction"
    );
    harness.context_manager.abort_map_reduce();
    assert!(harness.context_manager.begin_map_reduce(2_000));
    assert!(!harness.context_manager.pending_map_requests().is_empty());
}
