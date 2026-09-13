use super::*;
use crate::harness::events::HarnessEvent;

/// A real SSE provider requires more than a map's 2k output for the handoff.
/// Its first audit asks for a correction, so both complete-output stages run.
async fn checkpoint_provider() -> (
    Connector,
    tokio::sync::mpsc::UnboundedReceiver<(String, Option<u64>)>,
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
            let prompt = request["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap();
            let limit = request["max_tokens"].as_u64();
            let (stage, mut text) = if prompt.contains("[Map summaries to audit]") {
                (
                    "audit",
                    if prompt.contains("CORRECTED") {
                        "PASS"
                    } else {
                        "Preserve the missing constraint."
                    }
                    .to_string(),
                )
            } else if prompt.contains("[Validation audits]") {
                (
                    "correction",
                    format!("CORRECTED\n{}", "preserve verified evidence\n".repeat(800)),
                )
            } else {
                (
                    "final",
                    format!(
                        "## Objective\n{}",
                        "preserve verified evidence\n".repeat(800)
                    ),
                )
            };
            tx.send((stage.into(), limit)).unwrap();
            let tokens = crate::util::TokenEncoding::Cl100k.estimate(&text);
            let truncated = limit.is_some_and(|limit| tokens > limit as usize);
            if truncated {
                text = "partial handoff".into();
            }
            let chunk = serde_json::json!({"choices":[{
                "delta":{"content":text}, "finish_reason":if truncated {"length"} else {"stop"}
            }]});
            let body = format!(
                "data: {chunk}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":100,\"completion_tokens\":50,\"total_tokens\":150}}}}\n\ndata: [DONE]\n\n"
            );
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    (
        Connector::new("openrouter")
            .unwrap()
            .with_model("glm-5.3-flash")
            .with_api_key("test-only")
            .with_base_url(format!("http://{address}/v1"))
            .with_retry(false),
        rx,
        server,
    )
}

#[tokio::test]
async fn final_reduction_and_correction_preserve_output_settings_while_audits_stay_bounded() {
    for configured_output in [None, Some(8_192)] {
        let (mut connector, mut requests, server) = checkpoint_provider().await;
        if let Some(output) = configured_output {
            connector = connector.with_max_tokens(output);
        }
        let mut h = Harness::new_test();
        h.connector = connector;
        h.context_manager = ContextManager::new(80_000);
        h.context_manager
            .add_user("Preserve the API constraint and verified evidence.");
        h.context_manager.begin_manual_compaction();
        assert!(h.context_manager.begin_map_reduce(32_000));
        // These maps have already completed. Drive the actual final reducer,
        // audit, correction, re-audit and atomic commit over their evidence.
        for map in h.context_manager.pending_map_requests() {
            assert!(
                h.context_manager
                    .accept_map_summary(map.ordinal, "Independent verified evidence")
            );
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tokio::time::timeout(Duration::from_secs(15), h.drive_map_reduce(&tx))
            .await
            .unwrap();
        server.abort();
        let sent: Vec<_> = std::iter::from_fn(|| requests.try_recv().ok()).collect();
        assert!(
            result.is_ok_and(|committed| committed),
            "requests: {sent:?}"
        );
        assert_eq!(
            sent,
            vec![
                ("final".into(), configured_output.map(u64::from)),
                ("audit".into(), Some(2_000)),
                ("correction".into(), configured_output.map(u64::from)),
                ("audit".into(), Some(2_000)),
            ]
        );
        let view = h
            .context_manager
            .build_messages("")
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(view.contains("CORRECTED"));
        assert!(crate::util::TokenEncoding::Cl100k.estimate(&view) > 2_000);
        let usage_count = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| matches!(event, HarnessEvent::Usage { .. }))
            .count();
        assert_eq!(usage_count, 4);
    }
}
