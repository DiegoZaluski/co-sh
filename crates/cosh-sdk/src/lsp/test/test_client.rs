//! Client behavior tests: handshake, negotiation, document sync and
//! server→client dispatch, driven against the scripted fake server.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use crate::lsp::LspError;
use crate::lsp::client::{Event, LanguageServer, LanguageServerConfig, ServerState, TouchOutcome};
use crate::lsp::test::{FakeServer, serve_initialize, spawn_fake_server};

/// Build a client over a fake server pair plus its event collector.
fn start_client(
    settings: serde_json::Value,
) -> (LanguageServer, FakeServer, mpsc::UnboundedReceiver<Event>) {
    let (server, client_stream) = spawn_fake_server(64 * 1024);
    let (read_half, write_half) = tokio::io::split(client_stream);
    let (events_tx, events_rx) = mpsc::unbounded_channel();

    let mut config = LanguageServerConfig::new(
        "fake",
        std::path::PathBuf::from("/unused/fake-lsp"),
        std::path::PathBuf::from("/tmp"),
    );
    config.settings = settings;
    config.events = Some(events_tx);

    let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
    let client = LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
    (client, server, events_rx)
}

/// Run the handshake to completion and return (client, fake, events).
async fn running_client(
    capabilities_json: &str,
    settings: serde_json::Value,
) -> (LanguageServer, FakeServer, mpsc::UnboundedReceiver<Event>) {
    let (client, mut server, events) = start_client(settings);
    let serve = serve_initialize(&mut server, capabilities_json);
    let init = client.initialize(Duration::from_secs(5));
    let (_, result) = tokio::join!(serve, init);
    result.expect("initialize handshake succeeds");
    (client, server, events)
}

#[tokio::test]
async fn handshake_reaches_running_with_capabilities() {
    let caps = r#"{"positionEncoding":"utf-8","definitionProvider":true}"#;
    let (client, _server, _events) = running_client(caps, serde_json::Value::Null).await;

    assert_eq!(client.state(), ServerState::Running);
    let capabilities = client.capabilities().expect("capabilities stored");
    assert!(capabilities.position_encoding.is_some());
}

/// Strict assertion on initialize params + position encoding negotiation.
#[tokio::test]
async fn initialize_negotiates_utf8_and_announces_encodings() {
    let (client, mut server, _events) = start_client(serde_json::Value::Null);

    let serve = async {
        let request = server.next_client_message().await.expect("initialize");
        let parsed: serde_json::Value = serde_json::from_str(&request).unwrap();
        let params = &parsed["params"];

        let encodings = params["capabilities"]["general"]["positionEncodings"]
            .as_array()
            .expect("positionEncodings announced")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(encodings, ["utf-8", "utf-16"]);
        assert_eq!(
            params["capabilities"]["textDocument"]["synchronization"]["didSave"],
            false
        );
        assert_eq!(
            params["capabilities"]["textDocument"]["publishDiagnostics"]["versionSupport"],
            true
        );
        assert_eq!(params["rootUri"].as_str().unwrap(), "file:///tmp");

        let id = parsed["id"].clone();
        server
            .send_body(&format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{"positionEncoding":"utf-8"}}}}}}"#
            ))
            .await;

        let initialized = server.next_client_message().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&initialized).unwrap()["method"],
            "initialized"
        );
    };

    let init = client.initialize(Duration::from_secs(5));
    let ((), result) = tokio::join!(serve, init);
    result.unwrap();
    assert_eq!(
        client.position_encoding(),
        crate::lsp::PositionEncoding::Utf8
    );
}

/// Servers that stay silent about position encoding get UTF-16 (spec default).
#[tokio::test]
async fn position_encoding_defaults_to_utf16() {
    let (client, _server, _events) = running_client("{}", serde_json::Value::Null).await;
    assert_eq!(
        client.position_encoding(),
        crate::lsp::PositionEncoding::Utf16
    );
}

/// Settings travel through `workspace/didChangeConfiguration` after the
/// handshake when non-null.
#[tokio::test]
async fn settings_are_mirrored_after_initialize() {
    let (client, mut server, _events) = start_client(json!({"gopls": {"buildFlags": []}}));

    let serve = async {
        serve_initialize(&mut server, "{}").await;
        let mirrored = server.next_client_message().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&mirrored).unwrap();
        assert_eq!(parsed["method"], "workspace/didChangeConfiguration");
        assert_eq!(
            parsed["params"]["settings"]["gopls"]["buildFlags"],
            json!([])
        );
    };

    let init = client.initialize(Duration::from_secs(5));
    let (_, result) = tokio::join!(serve, init);
    result.unwrap();
}

/// `workspace/configuration` is answered from our settings, per section.
#[tokio::test]
async fn workspace_configuration_is_answered_from_settings() {
    let (client, mut server, _events) =
        running_client("{}", json!({"gopls": {"hoverKind": "FullDocumentation"}})).await;

    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"cfg","method":"workspace/configuration","params":{"items":[{"section":"gopls"},{"section":"missing.path"},{"section":"rust-analyzer"}]}}"#,
        )
        .await;

    // First wire message after the handshake is the settings mirror (the
    // client mirrors non-null settings through didChangeConfiguration).
    let mirrored = server.next_client_message().await.unwrap();
    assert!(
        mirrored.contains("didChangeConfiguration"),
        "expected settings mirror first, got {mirrored}"
    );

    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    let results = parsed["result"].as_array().unwrap();
    assert_eq!(results[0]["hoverKind"], "FullDocumentation");
    assert!(results[1].is_null());
    assert!(results[2].is_null());
    assert_eq!(parsed["id"], "cfg");
    drop(client);
}

/// `registerCapability` is acknowledged with null (an error here makes gopls
/// abort — Helix's lesson).
#[tokio::test]
async fn register_capability_acknowledged_and_tracked() {
    let (client, mut server, mut events) = running_client("{}", serde_json::Value::Null).await;

    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"reg","method":"client/registerCapability","params":{"registrations":[{"id":"pull-1","method":"textDocument/diagnostic"}]}}"#,
        )
        .await;

    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(parsed["id"], "reg");
    assert!(parsed["result"].is_null());
    assert!(parsed.get("error").is_none());

    match tokio::time::timeout(
        Duration::from_secs(2),
        next_event_skipping_state(&mut events),
    )
    .await
    {
        Ok(Event::CapabilityRegistered(method)) => {
            assert_eq!(method.as_ref(), "textDocument/diagnostic");
        }
        other => panic!("expected CapabilityRegistered event, got {other:?}"),
    }

    assert!(
        client
            .registered_methods()
            .contains("textDocument/diagnostic")
    );
}

/// Unknown server requests get `MethodNotFound` instead of silence, and
/// progress creation gets an eager `null`.
#[tokio::test]
async fn unknown_requests_get_method_not_found_progress_gets_null() {
    let (client, mut server, _events) = running_client("{}", serde_json::Value::Null).await;

    server
        .send_body(r#"{"jsonrpc":"2.0","id":"wdp","method":"window/workDoneProgress/create","params":{"token":"t"}}"#)
        .await;
    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(parsed["id"], "wdp");
    assert!(parsed["result"].is_null());

    server
        .send_body(r#"{"jsonrpc":"2.0","id":"mystery","method":"workspace/x/custom","params":{}}"#)
        .await;
    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(parsed["error"]["code"], -32601);
    drop(client);
}

/// publishDiagnostics flows through as an Event.
#[tokio::test]
async fn diagnostics_surface_as_events() {
    let (client, mut server, mut events) = running_client("{}", serde_json::Value::Null).await;

    server
        .send_body(
            r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/a.rs","version":3,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}},"message":"boom","severity":1}]}}"#,
        )
        .await;

    match tokio::time::timeout(
        Duration::from_secs(2),
        next_event_skipping_state(&mut events),
    )
    .await
    {
        Ok(Event::PublishDiagnostics(params)) => {
            assert!(params.uri.as_str().ends_with("/tmp/a.rs"));
            assert_eq!(params.version, Some(3));
            assert_eq!(params.diagnostics.len(), 1);
        }
        other => panic!("expected diagnostics event, got {other:?}"),
    }
    drop(client);
}

/// touch_file lifecycle: open → unchanged → change → explicit close.
#[tokio::test]
async fn touch_file_open_change_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.py");
    std::fs::write(&path, "x = 1\n").unwrap();

    let (client, mut server, _events) = running_client("{}", serde_json::Value::Null).await;

    // Open: watched-files nudge then didOpen with languageId from the SDK.
    assert_eq!(
        client.touch_file(&path).await.unwrap(),
        TouchOutcome::Opened
    );
    let first = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(parsed["method"], "workspace/didChangeWatchedFiles");

    let open = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&open).unwrap();
    assert_eq!(parsed["method"], "textDocument/didOpen");
    assert_eq!(parsed["params"]["textDocument"]["languageId"], "python");
    assert_eq!(parsed["params"]["textDocument"]["version"], 0);
    assert_eq!(parsed["params"]["textDocument"]["text"], "x = 1\n");

    // Same content: nothing goes out.
    assert_eq!(
        client.touch_file(&path).await.unwrap(),
        TouchOutcome::Unchanged
    );

    // New content: full didChange, version bumped.
    std::fs::write(&path, "x = 2\n").unwrap();
    assert_eq!(
        client.touch_file(&path).await.unwrap(),
        TouchOutcome::Changed
    );
    let change = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&change).unwrap();
    assert_eq!(parsed["method"], "textDocument/didChange");
    assert_eq!(parsed["params"]["textDocument"]["version"], 1);
    assert_eq!(parsed["params"]["textDocument"]["uri"], uri_of(&path));
    assert_eq!(parsed["params"]["contentChanges"][0]["text"], "x = 2\n");

    // Explicit close.
    assert!(client.close_file(&path).unwrap());
    let close = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&close).unwrap();
    assert_eq!(parsed["method"], "textDocument/didClose");
    assert!(client.open_documents().is_empty());
}

/// LRU eviction closes the oldest document once capacity is exceeded.
#[tokio::test]
async fn lru_eviction_sends_did_close() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..70usize)
        .map(|i| {
            let p = dir.path().join(format!("f{i}.txt"));
            std::fs::write(&p, format!("{i}")).unwrap();
            p
        })
        .collect();

    let (client, mut server, _events) = running_client("{}", serde_json::Value::Null).await;

    // Collector task: touches produce ~140 messages and the fake server's
    // queue backpressures, so drain concurrently instead of strict pairing.
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let collector_handle = {
        let collected = std::sync::Arc::clone(&collected);
        tokio::spawn(async move {
            while let Some(msg) = server.next_client_message().await {
                collected.lock().unwrap().push(msg);
            }
        })
    };

    for path in &paths {
        client.touch_file(path).await.unwrap();
    }

    assert!(client.open_documents().len() <= 64);

    // Wait until every notification drained through the pipe.
    for _ in 0..50 {
        if collected.lock().unwrap().len() >= 70 * 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(client); // ends the session; collector finishes on EOF
    let _ = collector_handle.await;

    let wire = collected.lock().unwrap().join("\n");
    assert!(
        wire.contains("\"textDocument/didClose\"") && wire.contains("f0.txt"),
        "eviction must send didClose for f0.txt"
    );
    assert_eq!(
        wire.matches("\"textDocument/didClose\"").count(),
        6,
        "70 opens → 6 evictions"
    );
}

/// Process death flips state to Exited and emits StateChanged.
#[tokio::test]
async fn death_flips_state_to_exited() {
    let (client, server, mut events) = running_client("{}", serde_json::Value::Null).await;

    drop(server); // abrupt EOF

    let deadline = Duration::from_secs(2);
    loop {
        match tokio::time::timeout(deadline, events.recv()).await.unwrap() {
            Some(Event::StateChanged(ServerState::Exited { .. })) => break,
            Some(_) => continue,
            None => panic!("event stream closed before Exited"),
        }
    }
    assert!(matches!(client.state(), ServerState::Exited { .. }));
}

/// The transport may terminate BEFORE the exit watcher subscribes (instant
/// EOF at startup). The watcher must still observe the pre-existing terminal
/// reason instead of leaving the state stuck in `Starting` forever.
#[tokio::test]
async fn instant_eof_at_startup_still_reaches_exited() {
    // Client-side halves of a peer dropped before the client is assembled:
    // every read returns EOF immediately.
    let (_server_peer, client_stream) = spawn_fake_server(1024);
    drop(_server_peer);
    let (read_half, write_half) = tokio::io::split(client_stream);
    let (dead_stderr, _dead_peer) = tokio::io::duplex(1);

    let mut config = LanguageServerConfig::new(
        "instant-death",
        std::path::PathBuf::from("/unused/instant-death"),
        std::path::PathBuf::from("/tmp"),
    );
    let (events_tx, mut events) = mpsc::unbounded_channel();
    config.events = Some(events_tx);

    let client = LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));

    // Nudge the scheduler so the transport's reader task gets every chance
    // to observe the EOF before the watcher would (the old race window).
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }

    let deadline = Duration::from_secs(2);
    loop {
        match tokio::time::timeout(deadline, events.recv()).await.unwrap() {
            Some(Event::StateChanged(ServerState::Exited { .. })) => break,
            Some(_) => continue,
            None => panic!("event stream closed before Exited"),
        }
    }
    assert!(matches!(client.state(), ServerState::Exited { .. }));
}

/// Concurrent first touches of the same file must produce exactly one
/// didOpen (the docs lock serializes the claim and the wire sends). True
/// parallelism matters: the old pre-fix window only opened when two touches
/// ran on different worker threads simultaneously.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_touches_send_exactly_one_did_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raced.txt");
    std::fs::write(&path, "content").unwrap();

    let (client, mut server, _events) = running_client("{}", serde_json::Value::Null).await;

    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let collector_handle = {
        let collected = std::sync::Arc::clone(&collected);
        tokio::spawn(async move {
            while let Some(msg) = server.next_client_message().await {
                collected.lock().unwrap().push(msg);
            }
        })
    };

    let client = std::sync::Arc::new(client);
    let touches: Vec<_> = (0..8)
        .map(|_| {
            let client = std::sync::Arc::clone(&client);
            let path = path.clone();
            tokio::spawn(async move { client.touch_file(&path).await })
        })
        .collect();
    for touch in touches {
        touch.await.unwrap().unwrap();
    }

    // One watched-files nudge + exactly one didOpen.
    for _ in 0..50 {
        if collected.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(client);
    let _ = collector_handle.await;

    let wire = collected.lock().unwrap().join("\n");
    assert_eq!(
        wire.matches("\"textDocument/didOpen\"").count(),
        1,
        "8 concurrent touches → exactly one didOpen"
    );
}

/// Graceful shutdown sequence on the wire: shutdown request → exit
/// notification → (flush) → stopped state.
#[tokio::test]
async fn shutdown_sequence_order_on_wire() {
    let (client, server, _events) = running_client("{}", serde_json::Value::Null).await;

    let teardown = tokio::spawn({
        let mut server = server;
        async move {
            let shutdown_req = server.next_client_message().await.unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&shutdown_req).unwrap();
            assert_eq!(parsed["method"], "shutdown");
            let id = parsed["id"].clone();
            server
                .send_body(&format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#))
                .await;

            let exit_note = server.next_client_message().await.unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&exit_note).unwrap();
            assert_eq!(parsed["method"], "exit");
        }
    });

    client.shutdown().await.unwrap();
    teardown.await.unwrap();

    // Give the exit watcher a beat; it must NOT override Stopped.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(client.state(), ServerState::Stopped);
}

/// Next event, skipping lifecycle transitions (Running is emitted during the
/// handshake and races whatever the test actually cares about).
async fn next_event_skipping_state(events: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    loop {
        match events.recv().await {
            Some(Event::StateChanged(_)) => continue,
            other => return other.expect("event stream open"),
        }
    }
}

fn uri_of(path: &std::path::Path) -> String {
    crate::lsp::client::uri_from_path(path)
        .expect("test path converts to URI")
        .as_str()
        .to_owned()
}

/// `unregisterCapability` releases the tracked method and emits the event.
#[tokio::test]
async fn unregister_capability_releases_tracking() {
    let (client, mut server, mut events) = running_client("{}", serde_json::Value::Null).await;

    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"reg","method":"client/registerCapability","params":{"registrations":[{"id":"pull-1","method":"textDocument/diagnostic"}]}}"#,
        )
        .await;
    let _reply = server.next_client_message().await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(2),
        next_event_skipping_state(&mut events),
    )
    .await
    {
        Ok(Event::CapabilityRegistered(_)) => {}
        other => panic!("expected registration event, got {other:?}"),
    }

    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"unreg","method":"client/unregisterCapability","params":{"unregisterations":[{"id":"pull-1","method":"textDocument/diagnostic"}]}}"#,
        )
        .await;
    let _reply = server.next_client_message().await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(2),
        next_event_skipping_state(&mut events),
    )
    .await
    {
        Ok(Event::CapabilityUnregistered(method)) => {
            assert_eq!(method.as_ref(), "textDocument/diagnostic");
        }
        other => panic!("expected unregistration event, got {other:?}"),
    }
    assert!(
        !client
            .registered_methods()
            .contains("textDocument/diagnostic")
    );
}

/// Requests whose params fail to deserialize are answered with -32602.
#[tokio::test]
async fn invalid_params_get_invalid_params_error() {
    let (_client, mut server, _events) = running_client("{}", serde_json::Value::Null).await;

    // `items` must be an array; a string is invalid.
    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"bad","method":"workspace/configuration","params":{"items":"nope"}}"#,
        )
        .await;

    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(parsed["error"]["code"], -32602);
    assert!(parsed.get("result").is_none());
}

/// Nested dotted sections resolve against the settings object.
#[tokio::test]
async fn dotted_configuration_sections_resolve() {
    let (client, mut server, _events) = running_client(
        "{}",
        json!({"gopls": {"buildFlags": ["-tags=x"], "ui": {"hover": {"kind": "Full"}}}}),
    )
    .await;

    // Consume the settings mirror from the handshake.
    let _mirror = server.next_client_message().await.unwrap();

    server
        .send_body(
            r#"{"jsonrpc":"2.0","id":"cfg","method":"workspace/configuration","params":{"items":[{"section":"gopls.buildFlags"},{"section":"gopls.ui.hover.kind"}]}}"#,
        )
        .await;

    let reply = server.next_client_message().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
    let results = parsed["result"].as_array().unwrap();
    assert_eq!(results[0], json!(["-tags=x"]));
    assert_eq!(results[1], json!("Full"));
    drop(client);
}

/// `initialize` twice is rejected instead of silently renegotiating.
#[tokio::test]
async fn double_initialize_is_rejected() {
    let (client, mut server, _events) = start_client(serde_json::Value::Null);
    {
        let serve = serve_initialize(&mut server, "{}");
        let init = client.initialize(Duration::from_secs(5));
        let (_, result) = tokio::join!(serve, init);
        result.unwrap();
    }
    let second = client.initialize(Duration::from_secs(5)).await;
    assert!(matches!(second, Err(LspError::InvalidState(_))));
}
