use super::super::core::Harness;
use super::super::events::HarnessEvent;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn test_agent_loop_simple_conversation() {
    let mut h = Harness::new_test()
        .with_mock_stream(Ok(vec!["Hello", " world"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    // Run in a task so we can collect events
    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, stop_signal).await;
    });

    // Collect events with timeout
    let mut events = Vec::new();
    let timeout = tokio::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(event, HarnessEvent::Done | HarnessEvent::Stopped | HarnessEvent::Error(_));
                events.push(event);
                if is_done {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    handle.abort();
    
    // Should get tokens then Done
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::Token { .. })));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::Done)));
}

#[tokio::test]
async fn test_agent_loop_with_tool_call() {
    let mut h = Harness::new_test()
        .with_test_tool(
            "test_tool",
            serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}),
        )
        .with_mock_stream(Ok(vec![
            "Let me call a tool ",
            r#"{"name": "test_tool", "arguments": {"x": "test"}}"#,
            " done",
        ]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("use tool", tx, answer_rx, stop_signal).await;
    });

    let mut events = Vec::new();
    let timeout = tokio::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(event, HarnessEvent::Done | HarnessEvent::Stopped | HarnessEvent::Error(_));
                events.push(event);
                if is_done {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    handle.abort();
    
    // Should get tokens, tool call, tool error (no server), then Error (mock consumed)
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::Token { .. })));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::ToolCall { .. })));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::ToolError { .. })));
    let last = events.last().unwrap();
    assert!(
        matches!(last, HarnessEvent::Done | HarnessEvent::Stopped | HarnessEvent::Error(_)),
        "expected terminal event, got {last:?}"
    );
}
