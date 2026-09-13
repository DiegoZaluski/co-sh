use super::super::core::Harness;
use super::super::events::HarnessEvent;
use crate::harness::context::ContextManager;
use crate::harness::events::LlmCompactionEvent;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Exercise the real SDK stream and completion validation. This provider has
/// ample input space, but needs more than 2,000 output tokens for the handoff.
async fn output_limited_summarizer(
    required_output: u64,
) -> (
    cosh_sdk::connector::Connector,
    tokio::sync::mpsc::UnboundedReceiver<Option<u64>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut buffer = [0; 8192];
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
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + length],
                        )
                        .unwrap();
                    }
                }
            };
            let limit = request["max_tokens"].as_u64();
            tx.send(limit).unwrap();
            let complete =
                required_output != u64::MAX && limit.is_none_or(|limit| limit >= required_output);
            let summary = if complete {
                format!(
                    "## Objective\n{}\n## Next Move\n- Run integration tests.",
                    "- Preserve verified evidence.\n".repeat(4_000)
                )
            } else {
                format!(
                    "## Objective\n{}",
                    "- Preserve verified evidence.\n".repeat(250)
                )
            };
            let chunk = serde_json::json!({"choices":[{
                "delta":{"content":summary},
                "finish_reason":if complete { "stop" } else { "length" }
            }]});
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        }
    });
    (
        cosh_sdk::connector::Connector::new("openrouter")
            .unwrap()
            .with_base_url(format!("http://{address}/v1"))
            .with_api_key("test-only")
            .with_model("glm-5.3-flash")
            .with_tool_call_mode(cosh_sdk::connector::ToolCallMode::Inline),
        rx,
        server,
    )
}

fn queue_small_work(h: &mut Harness) {
    for x in ["first", "second"] {
        h.mock_stream_queue.push_back(Ok(vec![format!(
            r#"{{"name":"test_tool","arguments":{{"x":"{x}"}}}}"#
        )]));
    }
    h.mock_stream_queue.push_back(Ok(vec!["done".into()]));
}

async fn run_small_work(h: &mut Harness) -> Vec<HarnessEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        h.run_agent_loop(
            "continue",
            tx,
            answer_rx,
            perm_rx,
            Arc::new(AtomicBool::new(false)),
        ),
    )
    .await
    .expect("the agent loop must terminate");
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, HarnessEvent::Done { .. }))
    );
    events
}

fn work_harness(budget: usize) -> Harness {
    let mut h = Harness::new_test().with_test_tool(
        "test_tool",
        serde_json::json!({
            "type":"object", "properties":{"x":{"type":"string"}}, "required":["x"]
        }),
    );
    h.context_manager = ContextManager::new(budget);
    h = h.with_history(&[
        ("user".into(), "u ".repeat(budget / 2)),
        ("assistant".into(), "a ".repeat(budget / 2)),
    ]);
    h.context_manager.close_loop();
    queue_small_work(&mut h);
    h
}

#[tokio::test]
async fn automatic_summary_has_room_to_finish_in_a_million_token_window() {
    let (connector, mut requests, server) = output_limited_summarizer(32_000).await;
    let mut h = work_harness(205_000); // automatic trigger = 164k
    h.connector = connector.clone();
    h.discovered_window = Some(1_000_000);
    let source = h.context_manager.save_state();
    let events = run_small_work(&mut h).await;

    // The slash command constructs a fresh harness without discovery. The
    // same history and provider must succeed through either entry point.
    let mut manual = Harness::new_test();
    manual.connector = connector;
    manual.context_manager.restore_state(&source);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        manual
            .compact_on_demand(&tx, Arc::new(AtomicBool::new(false)))
            .await,
        crate::harness::core::ManualCompactionOutcome::Compacted
    );
    server.abort();
    let mut limits = Vec::new();
    while let Ok(limit) = requests.try_recv() {
        limits.push(limit);
    }
    let started = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Started
                }
            )
        })
        .count();
    let failed = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Failed
                }
            )
        })
        .count();
    let toasts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            HarnessEvent::Toast { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        failed, 0,
        "started={started}; output limits={limits:?}; toasts={toasts:?}"
    );
    assert_eq!(
        started, 1,
        "small tool results must not retrigger compaction"
    );
    assert_eq!(
        limits.len(),
        2,
        "one automatic request and one manual request"
    );
    assert_eq!(
        limits,
        vec![None, None],
        "normal compaction must not install a contingency output ceiling"
    );
    assert!(h.context_manager.display_info().total_tokens < 164_000);
    assert!(h.context_manager.build_messages("").iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|text| text.contains("Run integration tests."))
    }));
}

#[tokio::test]
async fn exhausted_automatic_summary_retries_are_not_renewed_after_each_tool() {
    let mut h = work_harness(2_000).with_mock_chat(Err(
        "summarizer response is incomplete (finish reason: length)",
    ));
    h.discovered_window = Some(1_000_000);
    let events = run_small_work(&mut h).await;
    let started = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Started
                }
            )
        })
        .count();
    assert_eq!(
        started, 1,
        "an exhausted failure must defer automatic compaction for this turn"
    );
    assert_eq!(h.mock_compaction_models.len(), 1);
    assert!(h.context_manager.display_info().total_tokens >= 1_600);

    // A new user turn has a fresh chance; a failure must not disable
    // compaction permanently or mark a large provider window as stuck.
    h = h.with_mock_chat(Ok("## Objective\n- recovered"));
    queue_small_work(&mut h);
    let events = run_small_work(&mut h).await;
    assert!(events.iter().any(|event| matches!(
        event,
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Finished
        }
    )));
}

#[tokio::test]
async fn failure_after_first_tool_defers_further_automatic_compaction() {
    let mut h = work_harness(2_000).with_mock_chat(Err("provider unavailable"));
    h.context_manager = ContextManager::new(2_000);
    h.mock_stream_queue.push_front(Ok(vec![format!(
        r#"{{"name":"test_tool","arguments":{{"x":"{}"}}}}"#,
        "evidence ".repeat(3_000)
    )]));
    let events = run_small_work(&mut h).await;
    let started = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Started
                }
            )
        })
        .count();
    assert_eq!(started, 1);
    assert_eq!(
        h.mock_compaction_models.len(),
        super::MAX_COMPACTION_RETRIES
    );
    let first_agent = events
        .iter()
        .position(|event| matches!(event, HarnessEvent::BeginAssistant))
        .unwrap();
    let first_summary = events
        .iter()
        .position(|event| {
            matches!(
                event,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Started
                }
            )
        })
        .unwrap();
    assert!(first_agent < first_summary, "failure must happen mid-turn");
}

#[tokio::test]
async fn rejected_completed_summary_is_reported_once_and_manual_retry_still_works() {
    let oversized = "summary ".repeat(2_000);
    let mut h = work_harness(2_000).with_mock_chat(Ok(&oversized));
    let events = run_small_work(&mut h).await;
    assert_eq!(h.mock_compaction_models.len(), 1);
    assert!(events.iter().any(|event| matches!(event,
        HarnessEvent::Toast { message, .. }
            if message.contains("summary and retained context exceed the compaction budget"))));
    assert!(
        !h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(
                item,
                crate::harness::context::ContextItem::Compaction { .. }
            ))
    );

    h = h.with_mock_chat(Ok("## Objective\n- recovered manually"));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        h.compact_on_demand(&tx, Arc::new(AtomicBool::new(false)))
            .await,
        crate::harness::core::ManualCompactionOutcome::Compacted
    );
}

#[tokio::test]
async fn normal_summary_preserves_the_connectors_output_setting() {
    for window in [None, Some(1_000_000)] {
        for output in [None, Some(32_000)] {
            let (mut connector, mut requests, server) = output_limited_summarizer(32_000).await;
            if let Some(output) = output {
                connector = connector.with_max_tokens(output);
            }
            let mut h = Harness::new_test();
            h.connector = connector;
            h.discovered_window = window;
            // A small trigger budget is not an output generation limit.
            h.context_manager = ContextManager::new(2_000);
            let result = h
                .stream_summarize_for_compaction("summarize", "source", |_| {})
                .await;
            server.abort();
            assert_eq!(requests.try_recv().unwrap(), output.map(u64::from));
            assert!(
                result.is_ok(),
                "window={window:?}, output={output:?}: {result:?}"
            );
        }
    }
}

#[tokio::test]
async fn automatic_compaction_selects_summarizer_before_testing_its_window() {
    let mut h = work_harness(205_000)
        .with_discovered_window(4_000)
        .with_summarization_models(vec![("lmstudio".into(), "large-summarizer".into())])
        .with_mock_chat(Ok("## Objective\n- complete handoff"));
    h.mock_summarization_windows
        .insert("large-summarizer".into(), 1_000_000);
    let events = run_small_work(&mut h).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Progress { .. }
            }
        )),
        "the summarizer can handle the complete request; the agent's smaller window must not force MapReduce"
    );
    assert_eq!(h.mock_compaction_models.len(), 1);
}

#[tokio::test]
async fn truncated_summary_is_attempted_once_and_never_committed() {
    let (connector, mut requests, server) = output_limited_summarizer(u64::MAX).await;
    let mut h = work_harness(205_000);
    h.connector = connector;
    h.discovered_window = Some(1_000_000);
    let events = run_small_work(&mut h).await;
    server.abort();
    let mut calls = 0;
    while requests.try_recv().is_ok() {
        calls += 1;
    }
    assert_eq!(calls, 1);

    let mut visible = String::new();
    let mut resets = 0;
    let mut failures = 0;
    for event in &events {
        match event {
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::OutputStarted,
            } => {
                visible.clear();
                resets += 1;
            }
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed,
            } => failures += 1,
            HarnessEvent::LlmCompactionToken { text } => visible.push_str(text),
            _ => {}
        }
    }
    assert_eq!(resets, 0);
    assert_eq!(failures, 1);
    assert_eq!(
        visible.matches("## Objective").count(),
        1,
        "the TUI must show only the latest response, not concatenated retries"
    );
    assert!(
        !h.context_manager
            .items_snapshot()
            .iter()
            .any(|item| matches!(
                item,
                crate::harness::context::ContextItem::Compaction { .. }
            ))
    );
    assert!(events.iter().any(|event| matches!(event,
        HarnessEvent::Toast { message, .. } if message.contains("finish reason: length"))));
}
