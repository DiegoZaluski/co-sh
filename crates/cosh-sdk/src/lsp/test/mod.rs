//! Test scaffolding for the LSP engine: an in-memory fake language server
//! speaking the same framed protocol over duplex pipes (Zed's
//! `FakeLanguageServer` pattern, minus the process).
//!
//! ```ignore
//! let (mut server, client_stream) = spawn_fake_server(64 * 1024);
//! let (client_read, client_write) = tokio::io::split(client_stream);
//! let mut transport = Transport::start(
//!     "test",
//!     client_read,
//!     client_write,
//!     None::<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
//! );
//! ```
//!
//! Dropping [`FakeServer`] tears down its end of the pipe *deterministically*
//! (the internal reader task is signalled first, so both halves actually get
//! released and the transport observes EOF).
#![allow(dead_code)]

use tokio::{
    io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    sync::{mpsc, watch},
};

use crate::lsp::jsonrpc;

/// One end of a fake language server.
pub struct FakeServer {
    /// Frame bodies sent by the client (transport under test), in order.
    incoming: mpsc::Receiver<String>,
    write_half: WriteHalf<DuplexStream>,
    /// Signals the internal pump task to release its half of the pipe.
    shutdown: watch::Sender<bool>,
}

impl FakeServer {
    /// Send a raw wire payload (headers included) to the client. Useful to
    /// simulate non-conformant output.
    pub async fn send_raw(&mut self, wire: &[u8]) {
        self.write_half
            .write_all(wire)
            .await
            .expect("fake server write");
        self.write_half.flush().await.expect("fake server flush");
    }

    /// Send a properly framed message body to the client.
    pub async fn send_body(&mut self, body: &str) {
        jsonrpc::write_frame(&mut self.write_half, body)
            .await
            .expect("fake server frame write");
    }

    /// Await the next message body written by the client. `None` once the
    /// transport closed its outbound channel or was dropped.
    pub async fn next_client_message(&mut self) -> Option<String> {
        self.incoming.recv().await
    }
}

/// Drop closes the server side of the pipe: the pump task releases its half
/// and the transport observes EOF on its read side.
impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Build a connected `(FakeServer, client-side duplex)` pair and start the
/// server's reader task.
///
/// Split the returned stream into read/write halves to feed
/// [`Transport::start`](crate::lsp::Transport::start).
pub fn spawn_fake_server(buffer: usize) -> (FakeServer, DuplexStream) {
    let (client_side, server_side) = tokio::io::duplex(buffer);
    let (read_half, write_half) = tokio::io::split(server_side);

    // `tokio::io::split` halves hold the underlying stream via BiLock: the
    // peer only sees EOF once BOTH halves are gone. The shutdown watch makes
    // that ordering explicit — the pump leaves on signal, releasing its half.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (incoming_tx, incoming_rx) = mpsc::channel(64);
    tokio::spawn(pump_incoming(read_half, shutdown_rx, incoming_tx));

    (
        FakeServer {
            incoming: incoming_rx,
            write_half,
            shutdown: shutdown_tx,
        },
        // The duplex stream implements Read+Write in both directions: the
        // transport reads what we write and writes what we read.
        client_side,
    )
}

async fn pump_incoming(
    read_half: ReadHalf<DuplexStream>,
    mut shutdown: watch::Receiver<bool>,
    tx: mpsc::Sender<String>,
) {
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut header_buf = Vec::with_capacity(128);
    let mut body_buf = Vec::new();
    loop {
        // Fires on the shutdown flip OR on FakeServer being fully dropped.
        let frame = tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            frame = jsonrpc::read_frame(&mut reader, &mut header_buf, &mut body_buf) => frame,
        };
        match frame {
            Ok(body) => {
                if tx.send(body).await.is_err() {
                    break;
                }
            }
            Err(_) => {
                break;
            }
        }
    }
}

mod test_client;
mod test_jsonrpc;
mod test_manager;
mod test_transport;
/// Scripted initialize handshake: answers the client's `initialize` request
/// with `capabilities_json` (raw JSON for the capabilities object) and swallows
/// the follow-up `initialized` notification.
pub async fn serve_initialize(server: &mut FakeServer, capabilities_json: &str) {
    let request = server
        .next_client_message()
        .await
        .expect("initialize request");
    let parsed: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(
        parsed["method"], "initialize",
        "handshake starts with initialize"
    );

    let body = format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{{"capabilities":{capabilities_json}}}}}"#,
        parsed["id"]
    );
    server.send_body(&body).await;

    // The initialized notification follows; consume it so tests start clean.
    let notification = server
        .next_client_message()
        .await
        .expect("initialized notification");
    let parsed: serde_json::Value = serde_json::from_str(&notification).unwrap();
    assert_eq!(parsed["method"], "initialized");
}

/// Fully autonomous fake server: answers `initialize` with minimal
/// capabilities, replies `null` to every other request (including `shutdown`)
/// and keeps running until the client side closes the pipe.
pub async fn auto_serve(mut server: FakeServer) {
    use serde_json::json;

    while let Some(body) = server.next_client_message().await {
        let parsed: serde_json::Value = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let is_request = parsed.get("id").is_some_and(|id| !id.is_null());
        if !is_request {
            continue;
        }
        let id = parsed["id"].clone();
        let result = match parsed["method"].as_str() {
            Some("initialize") => json!({ "capabilities": {} }),
            _ => serde_json::Value::Null,
        };
        let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#);
        server.send_body(&reply).await;
    }
}
