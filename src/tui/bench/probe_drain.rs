//! Per-event-type drain cost probe: measures `App::poll_events` for EACH
//! harness event kind against a large warm session, isolating which event
//! makes the UI thread stall while the agent works.
//!
//! Run: cargo test --release probe_drain_costs -- --ignored --nocapture

use ratatui::{Terminal, backend::TestBackend};

use cosh::harness::HarnessEvent;

use super::App;

fn big_rust_file(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            format!(
                "pub fn handler_{i}(req: Request) -> Response {{ let v = \"{i}\"; Ok(v.into()) }}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[ignore]
async fn probe_drain_costs() {
    let mut app = App::new("/tmp".to_string());
    app.title_generated = true;

    // Warm 200-message session (~10 MB with tool outputs).
    {
        use crate::types::*;
        let mut messages = Vec::new();
        for r in 0..100 {
            messages.push(Message {
                id: format!("u-{r}"),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: format!("question {r}"),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            });
            messages.push(Message {
                id: format!("a-{r}"),
                role: MessageRole::Assistant,
                parts: vec![
                    Part::Text(TextPart {
                        text: "### Answer\n\nbody text here\n".repeat(20),
                        synthetic: false,
                    }),
                    Part::Tool(ToolPart {
                        tool: "read".into(),
                        input: serde_json::json!({"file": format!("m{r}.rs")}),
                        output: Some(big_rust_file(500)),
                        status: ToolStatus::Completed,
                        tool_call_id: None,
                        is_start: false,
                        is_streaming: false,
                        cached_line_count: None,
                        lsp_notes: None,
                    }),
                ],
                created_at: 0,
                agent: None,
                model: None,
            });
        }
        let id = "probe".to_string();
        app.state.add_session(crate::types::Session {
            id,
            title: "P".into(),
            created_at: 0,
            title_generated: true,
            provider: None,
            model: None,
            reasoning: None,
            ctx_ids: Default::default(),
            messages,
        });
        app.state.current_session_id = Some("probe".into());
    }

    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    app.state.status = crate::types::SessionStatus::Working;

    // A running tool part exists so ToolOutput paths take the real branch.
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "bash_run".into(),
            input: serde_json::json!({"command": "cargo test"}),
        })
        .unwrap();
    app.poll_events();

    fn measure(app: &mut App, name: &str, ev: HarnessEvent) {
        app.event_tx.send(ev).unwrap();
        let t = std::time::Instant::now();
        app.poll_events();
        println!(
            "[PROBE] {:<34} drain={:>8.3}ms",
            name,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    let token = "streamed token chunk of text ";
    let bash_chunk = "[1234] INFO worker handled request bytes=4096 dur=12ms\n".to_string();
    let sub_chunk = "- reading src/**\n- found hot loop\n".repeat(10);

    // Measure each event type several times for stability.
    for i in 0..5 {
        measure(
            &mut app,
            &format!("Token #{i}"),
            HarnessEvent::Token {
                text: token.repeat(4),
            },
        );
        measure(
            &mut app,
            &format!("ToolOutput(bash) #{i}"),
            HarnessEvent::ToolOutput {
                tool: "bash_run".into(),
                output: bash_chunk.clone(),
                finished: false,
                agent: None,
            },
        );
        measure(
            &mut app,
            &format!("ToolOutput(subagent) #{i}"),
            HarnessEvent::ToolOutput {
                tool: "subagent_call".into(),
                output: sub_chunk.clone(),
                finished: false,
                agent: None,
            },
        );
        measure(
            &mut app,
            &format!("Reasoning #{i}"),
            HarnessEvent::Reasoning {
                text: "thinking ".repeat(50),
            },
        );

        // Frame between batches like the real loop.
        terminal.draw(|f| app.render(f, 0.033)).unwrap();
    }

    // ToolResult with a big output.
    measure(
        &mut app,
        "ToolResult(1200-line file)",
        HarnessEvent::ToolResult {
            output: big_rust_file(1200),
        },
    );
    terminal.draw(|f| app.render(f, 0.033)).unwrap();

    // ContextSnapshot → full session save on UI thread.
    measure(
        &mut app,
        "ContextSnapshot(full save)",
        HarnessEvent::ContextSnapshot {
            context: super::bench_e2e::realistic_context(100),
        },
    );
    terminal.draw(|f| app.render(f, 0.033)).unwrap();

    // Done → full save + transition.
    measure(
        &mut app,
        "Done(full save)",
        HarnessEvent::Done {
            context: super::bench_e2e::realistic_context(100),
            checkup_verdict: None,
        },
    );
    terminal.draw(|f| app.render(f, 0.033)).unwrap();
}
