//! Transport behavior tests driven over in-memory duplex pipes.
//!
//! Each test reproduces a real-world failure mode observed in Zed, Helix,
//! crush or opencode (documented inline) so regressions stay impossible.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::DuplexStream;

use crate::lsp::test::spawn_fake_server;
use crate::lsp::{ExitReason, LspError, RpcError, Transport, jsonrpc};

fn start_transport(name: &str, stream: DuplexStream) -> Transport {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Safety-ish: test-only global logger init.
        let _ = env_logger::builder().is_test(true).try_init();
    });
    let (read_half, write_half) = tokio::io::split(stream);
    // A duplex whose peer is dropped on purpose: EOF immediately, so the
    // stderr drainer exits without ever seeing a line.
    let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
    Transport::start(name, read_half, write_half, Some(dead_stderr))
}

/// Asserts that no wire frame reaches the fake server within the window.
/// Silence OR clean channel-closure count as "nothing was sent"; an actual
/// message fails the test.
async fn assert_no_wire_frame(server: &mut crate::lsp::test::FakeServer, window: Duration) {
    match tokio::time::timeout(window, server.next_client_message()).await {
        Err(_elapsed) => (), // silent window ✓
        Ok(None) => (),      // peer pipe closed without writing ✓
        Ok(Some(msg)) => panic!("unexpected frame on wire: {msg}"),
    }
}

/// Basic request → response round-trip with numeric ids.
#[tokio::test]
async fn request_response_roundtrip() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    let fut = transport.request(
        "initialize",
        Some(json!({"processId": 1})),
        Duration::from_secs(5),
    );

    let request = server.next_client_message().await.expect("client request");
    let parsed: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(parsed["method"], "initialize");
    let id = parsed["id"].clone();

    // Answer out-of-band with the same id.
    server
        .send_body(&format!(
            r#"{{"jsonrpc":"2.0","id":{},"result":{{"capabilities":{{}}}}}}"#,
            id
        ))
        .await;

    let response = fut.await.expect("response");
    assert_eq!(response["capabilities"], json!({}));
}

/// Two concurrent requests whose responses arrive swapped still resolve to
/// their own callers.
#[tokio::test]
async fn interleaved_requests_route_independently() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    let f_a = transport.request("a", None, Duration::from_secs(5));
    let f_b = transport.request("b", None, Duration::from_secs(5));
    let req_a = server.next_client_message().await.unwrap();
    let req_b = server.next_client_message().await.unwrap();
    assert_ne!(
        serde_json::from_str::<Value>(&req_a).unwrap()["id"],
        serde_json::from_str::<Value>(&req_b).unwrap()["id"]
    );

    // Deliberately reversed order.
    let id_a = serde_json::from_str::<Value>(&req_a).unwrap()["id"].clone();
    let id_b = serde_json::from_str::<Value>(&req_b).unwrap()["id"].clone();
    server
        .send_body(&format!(r#"{{"jsonrpc":"2.0","id":{id_b},"result":"B"}}"#))
        .await;
    server
        .send_body(&format!(r#"{{"jsonrpc":"2.0","id":{id_a},"result":"A"}}"#))
        .await;

    assert_eq!(f_a.await.unwrap(), json!("A"));
    assert_eq!(f_b.await.unwrap(), json!("B"));
}

/// A request that exceeds its deadline fails cleanly; the late response is
/// discarded without affecting later requests (Zed timeout semantics).
#[tokio::test]
async fn timeout_fails_request_and_discards_late_response() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    let fut = transport.request("slow", None, Duration::from_millis(50));
    let _request = server.next_client_message().await;
    let err = fut.await.unwrap_err();
    assert!(matches!(err, LspError::Timeout { ref method } if method.as_ref() == "slow"));

    // The late answer must not crash or mis-route.
    server
        .send_body(r#"{"jsonrpc":"2.0","id":0,"result":"too late"}"#)
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Session remains usable.
    let again = transport.request("fast", None, Duration::from_secs(5));
    let _request = server.next_client_message().await;
    server
        .send_body(r#"{"jsonrpc":"2.0","id":1,"result":42}"#)
        .await;
    assert_eq!(again.await.unwrap(), json!(42));
}

/// Dropping an unresolved request future emits `$/cancelRequest` for it
/// (Zed cancel-on-drop).
#[tokio::test]
async fn dropping_future_sends_cancel_request() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    {
        let fut = transport.request("workspace/symbol", None, Duration::from_secs(30));
        let _request = server.next_client_message().await.unwrap();
        drop(fut);
    }

    let wire = tokio::time::timeout(Duration::from_secs(2), server.next_client_message())
        .await
        .expect("cancel must arrive promptly")
        .expect("cancel frame body");
    let parsed: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(parsed["method"], "$/cancelRequest");
    assert_eq!(parsed["params"]["id"], 0);
}

/// A successful response disarms the cancel guard: no stray `$/cancelRequest`
/// is emitted afterwards.
#[tokio::test]
async fn successful_response_does_not_cancel() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    let fut = transport.request("m", None, Duration::from_secs(5));
    let _request = server.next_client_message().await.unwrap();
    server
        .send_body(r#"{"jsonrpc":"2.0","id":0,"result":1}"#)
        .await;
    assert_eq!(fut.await.unwrap(), json!(1));
    drop(transport);
    let mut server = server;

    // Give any (incorrect) spurious send a chance to surface.
    assert_no_wire_frame(&mut server, Duration::from_millis(50)).await;
}

/// A server-reported error is still a *delivered* response: the transaction is
/// closed and no `$/cancelRequest` may follow (regression for review finding
/// MAJOR-1).
#[tokio::test]
async fn rpc_error_response_does_not_cancel() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    let fut = transport.request("textDocument/references", None, Duration::from_secs(5));
    let _request = server.next_client_message().await.unwrap();
    server
        .send_body(r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32603,"message":"internal"}}"#)
        .await;

    let err = fut.await.unwrap_err();
    assert!(matches!(err, LspError::Rpc { code: -32603, .. }));
    drop(transport);
    assert_no_wire_frame(&mut server, Duration::from_millis(50)).await;
}

/// When stdout dies, every in-flight request resolves with `StreamClosed` and
/// the exit watch flips (Helix pending-drain semantics).
#[tokio::test]
async fn stream_close_fails_pending_and_signals_exit() {
    let (server, client_stream) = spawn_fake_server(64 * 1024);
    let mut exited = {
        let transport = start_transport("test", client_stream);
        let exited = transport.exited();
        let fut = transport.request("doomed", None, Duration::from_secs(5));
        drop(server); // abrupt death: both fake halves dropped

        let err = fut.await.unwrap_err();
        assert!(matches!(err, LspError::StreamClosed));
        exited
    };

    exited.changed().await.expect("exit signal");
    assert!(matches!(
        exited.borrow().as_ref(),
        Some(ExitReason::StreamClosed)
    ));
}

/// Junk lines and unknown headers before `Content-Length` must not break
/// framing (Helix: servers that log into stdout).
#[tokio::test]
async fn garbage_before_headers_is_tolerated() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    server
        .send_raw(b"[INFO] gopls started\nContent-Type: text/plain\n")
        .await;
    server
        .send_body(r#"{"jsonrpc":"2.0","method":"window/showMessage","params":{"message":"hi"}}"#)
        .await;

    match transport.recv().await.expect("notification") {
        jsonrpc::IncomingMessage::Notification { method, .. } => {
            assert_eq!(method.as_ref(), "window/showMessage");
        }
        other => panic!("expected notification, got {other:?}"),
    }
}

/// An oversized announced frame terminates the session defensively instead of
/// allocating the announced size.
#[tokio::test]
async fn oversized_frame_terminates_session() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);
    let mut exited = transport.exited();

    let oversized = jsonrpc::MAX_FRAME_SIZE + 1;
    server
        .send_raw(format!("Content-Length: {oversized}\r\n\r\n").as_bytes())
        .await;

    exited.changed().await.expect("exit signal");
    assert!(matches!(
        exited.borrow().as_ref(),
        Some(ExitReason::Protocol(_))
    ));
}

/// One malformed frame is skipped; the session keeps flowing (Zed reader loop
/// logs and continues).
#[tokio::test]
async fn malformed_frame_is_skipped_session_survives() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    // Valid framing, invalid JSON payload.
    server.send_body(r#"{"jsonrpc":"2.0""#).await;
    // A response id that matches nothing must be discarded quietly too.
    server
        .send_body(r#"{"jsonrpc":"2.0","id":999,"result":"ghost"}"#)
        .await;
    server
        .send_body(r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics"}"#)
        .await;

    match transport.recv().await.expect("healthy message") {
        jsonrpc::IncomingMessage::Notification { method, .. } => {
            assert_eq!(method.as_ref(), "textDocument/publishDiagnostics");
        }
        other => panic!("expected notification, got {other:?}"),
    }
}

/// Notifications flow to the consumer in FIFO order.
#[tokio::test]
async fn notifications_arrive_in_order() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    for i in 0..10 {
        server
            .send_body(&format!(r#"{{"jsonrpc":"2.0","method":"n/{i}"}}"#))
            .await;
    }

    for i in 0..10 {
        match transport.recv().await.expect("notification") {
            jsonrpc::IncomingMessage::Notification { method, .. } => {
                assert_eq!(method.as_ref(), format!("n/{i}"));
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }
}

/// The incoming queue applies backpressure instead of buffering unboundedly
/// (Zed `INCOMING_MESSAGE_QUEUE_CAPACITY`): when the consumer stops reading,
/// at most a small slack beyond capacity is buffered while the rest waits in
/// the pipe — then everything drains once consumption resumes.
#[tokio::test]
async fn bounded_incoming_queue_applies_backpressure() {
    const TOTAL: usize = crate::lsp::transport::INCOMING_CAPACITY * 4;
    let (mut server, client_stream) = spawn_fake_server(256 * 1024);
    let transport = start_transport("test", client_stream);

    let writer = tokio::spawn(async move {
        for i in 0..TOTAL {
            let body = format!(
                r#"{{"jsonrpc":"2.0","method":"bulk/{i}","params":{{"pad":"{:0>512}"}}}}"#,
                ""
            );
            server.send_body(&body).await;
        }
    });

    // Do NOT consume yet: give the pipeline time to hit backpressure.
    tokio::time::sleep(Duration::from_millis(300)).await;

    fn expect_notification(message: jsonrpc::IncomingMessage) -> String {
        match message {
            jsonrpc::IncomingMessage::Notification { method, .. } => method.to_string(),
            other => panic!("expected notification, got {other:?}"),
        }
    }

    let mut consumed = Vec::new();
    while let Some(message) = transport.try_recv() {
        consumed.push(expect_notification(message));
    }
    assert!(
        consumed.len() <= crate::lsp::transport::INCOMING_CAPACITY + 8,
        "reader buffered {} messages while consumer was wedged",
        consumed.len()
    );

    // Resume consumption: everything must eventually arrive, FIFO overall.
    for (i, method) in consumed.iter().enumerate() {
        assert_eq!(method, &format!("bulk/{i}"), "prefix order");
    }
    for i in consumed.len()..TOTAL {
        let message = transport.recv().await.expect("notification during drain");
        assert_eq!(expect_notification(message), format!("bulk/{i}"));
    }
    writer.await.expect("fake server writer task");
}

/// Requests issued after the session ended fail fast with `NotRunning`.
#[tokio::test]
async fn requests_after_death_fail_fast() {
    let (server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);
    drop(server);

    // Let the reader observe EOF and tear the session down.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        transport.request("dead", None, Duration::from_millis(300)),
    )
    .await
    .expect("request resolves promptly after session death");
    assert!(
        outcome.is_err(),
        "request after death must fail, got {outcome:?}"
    );
    drop(transport);
}

/// Server-initiated requests are answered through [`Transport::respond`] with
/// exactly one of result/error on the wire.
#[tokio::test]
async fn respond_answers_server_requests() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    // Simulate a server asking for configuration (gopls/tsserver do this).
    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"cfg-1","method":"workspace/configuration","params":{"items":[{"section":"gopls"}]}}"#,
        )
        .await;

    match transport.recv().await.expect("server request") {
        jsonrpc::IncomingMessage::Request { id, method, params } => {
            assert_eq!(method.as_ref(), "workspace/configuration");
            assert_eq!(params.unwrap()["items"][0]["section"], "gopls");
            transport
                .respond(&id, Ok(&json!([{"hoverKind": "FullDocumentation"}])))
                .expect("respond ok");
        }
        other => panic!("expected request, got {other:?}"),
    }

    let reply = server.next_client_message().await.unwrap();
    let parsed: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(parsed["id"], "cfg-1");
    assert_eq!(parsed["result"][0]["hoverKind"], "FullDocumentation");
    assert!(parsed.get("error").is_none());

    // Error path.
    server
        .send_body(r#"{"jsonrpc":"2.0","id":"boom","method":"workspace/applyEdit","params":{}}"#)
        .await;
    if let Some(jsonrpc::IncomingMessage::Request { id, .. }) = transport.recv().await {
        transport
            .respond(&id, Err(&RpcError::new(-32601, "unsupported")))
            .unwrap();
    } else {
        panic!("expected second request");
    }
    let reply: Value = serde_json::from_str(&server.next_client_message().await.unwrap()).unwrap();
    assert_eq!(reply["error"]["code"], -32601);
    assert!(reply.get("result").is_none());
}

/// `flush` completes only after previously enqueued frames reached the wire.
#[tokio::test]
async fn flush_waits_for_outbound_frames() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    transport.notify("initialized", None).expect("notify");
    transport.flush().await;

    let first = server.next_client_message().await;
    assert_eq!(
        first.as_deref(),
        Some(r#"{"jsonrpc":"2.0","method":"initialized"}"#)
    );
}

/// Dropping the handle tears the session down: in-flight requests fail, the
/// exit watch flips (review finding MAJOR-2).
#[tokio::test]
async fn dropping_transport_fails_pending_and_flips_exit() {
    let (server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);
    let mut exited = transport.exited();

    let fut = transport.request("doomed", None, Duration::from_secs(30));
    drop(transport); // owner walked away

    // The pending request fails promptly — not after its deadline.
    let err = tokio::time::timeout(Duration::from_millis(500), fut)
        .await
        .expect("pending request must fail on drop, not hang until timeout")
        .unwrap_err();
    assert!(matches!(err, LspError::StreamClosed));

    exited.changed().await.expect("exit signal");
    assert_eq!(*exited.borrow(), Some(ExitReason::StreamClosed));
    drop(server);
}

/// `close()` ends the writer: later sends fail with `NotRunning` while `flush`
/// still resolves instead of hanging.
#[tokio::test]
async fn close_disables_outbound_but_flush_resolves() {
    let (_server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);

    transport.close();
    transport.flush().await; // must resolve even though writer already left

    let err = transport.notify("late/notification", None).unwrap_err();
    assert!(matches!(err, LspError::NotRunning | LspError::Backpressure));
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            transport.request("late", None, Duration::from_secs(5))
        )
        .await
        .expect("request future resolves fast")
        .is_err(),
        "requests after close must fail"
    );
}

/// Stderr is drained into a bounded tail buffer for crash reporting; the
/// oldest lines fall off beyond [`STDERR_TAIL_LINES`].
#[tokio::test]
async fn stderr_tail_keeps_last_lines() {
    let (client_side, _keep_alive) = tokio::io::duplex(64 * 1024);
    let (stderr_write, stderr_read) = tokio::io::duplex(16 * 1024);
    let transport = {
        let (read_half, write_half) = tokio::io::split(client_side);
        Transport::start("test", read_half, write_half, Some(stderr_read))
    };

    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut stderr_write = stderr_write;
        for i in 0..150 {
            stderr_write
                .write_all(format!("server log line {i}\n").as_bytes())
                .await
                .unwrap();
        }
        stderr_write.flush().await.unwrap();
        // Half stays open until task end; EOF afterwards is harmless.
    });
    writer.await.unwrap();

    // Give the drainer time to consume every line.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let tail = transport.stderr_tail().expect("stderr tail exists");
    let lines: Vec<&str> = tail.lines().collect();
    assert!(
        lines.len() <= 100,
        "tail must stay bounded, got {}",
        lines.len()
    );
    assert_eq!(lines.last().copied(), Some("server log line 149"));
}

/// EOF in the middle of a frame body corrupts the stream: terminal condition
/// is a protocol error, not a clean close.
#[tokio::test]
async fn eof_mid_body_is_protocol_error() {
    let (mut server, client_stream) = spawn_fake_server(64 * 1024);
    let transport = start_transport("test", client_stream);
    let mut exited = transport.exited();

    server.send_raw(b"Content-Length: 100\r\n\r\nshort").await;
    drop(server);

    let _ = tokio::time::timeout(Duration::from_secs(3), exited.changed())
        .await
        .expect("exit signal must arrive");
    assert!(matches!(
        exited.borrow().as_ref(),
        Some(ExitReason::Protocol(_))
    ));
}
