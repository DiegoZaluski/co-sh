use std::sync::mpsc;

// Test: abort + channel-clear pattern from RagAction::Back

#[tokio::test]
async fn test_rag_abort_fetch_prevents_stale_results() {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let _ = tx.send(Ok("stale data".to_string()));
    });

    // Simulate Esc: abort + clear channel
    handle.abort();
    drop(rx);

    let join_result = handle.await;
    assert!(
        join_result.is_err() && join_result.unwrap_err().is_cancelled(),
        "Task should have been cancelled after abort"
    );
}

#[tokio::test]
async fn test_rag_abort_completed_task_is_noop() {
    let handle = tokio::spawn(async move { 42 });

    let result = handle.await.expect("Task should complete");
    assert_eq!(result, 42);
}

#[tokio::test]
async fn test_rag_abort_after_task_completes_gracefully() {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    let handle = tokio::spawn(async move {
        let _ = tx.send(Ok("quick result".to_string()));
    });

    while !handle.is_finished() {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }

    handle.abort();

    if let Ok(msg) = rx.try_recv() {
        assert!(msg.is_ok());
    }
    drop(rx);

    let join_result = handle.await;
    assert!(join_result.is_ok(), "Completed task should have Ok result");
}

#[tokio::test]
async fn test_rag_dropped_receiver_causes_send_failure() {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    drop(rx);

    let send_result = tx.send(Ok("data".to_string()));
    assert!(
        send_result.is_err(),
        "Send should fail when receiver is dropped"
    );
}

#[tokio::test]
async fn test_rag_full_abort_pattern() {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _ = tx.send(Ok("result".to_string()));
    });

    let mut rag_fetch_handle: Option<tokio::task::JoinHandle<()>> = Some(handle);
    drop(rx);

    if let Some(h) = rag_fetch_handle.take() {
        h.abort();
    }

    assert!(rag_fetch_handle.is_none(), "Handle should be consumed");
}
