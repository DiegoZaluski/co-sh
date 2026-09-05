use super::*;
use crate::harness::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};

fn staged_harness() -> Harness {
    let mut harness = Harness::new_test();
    harness.context_manager = ContextManager::new(2_000);
    harness.context_manager.add_user(&"source ".repeat(2_000));
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
async fn real_map_stream_reports_usage_and_omits_tools() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"map result\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15,\"cost\":0.01}}\n\ndata: [DONE]\n\n";
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
        "system".into(),
        "prompt".into(),
        None,
        tx,
    )
    .await
    .unwrap();
    assert_eq!(result, "map result");
    server.await.unwrap();
    assert!(
        matches!(rx.recv().await, Some(HarnessEvent::Usage { reported_cost: Some(cost), .. }) if cost == 0.01)
    );
}
