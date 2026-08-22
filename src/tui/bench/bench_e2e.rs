//! End-to-end performance benchmark: drives the REAL App pipeline.
//!
//! Unlike a view-level microbenchmark, this exercises the full frame path the
//! human perceives: harness event application (`poll_events`) → full
//! `App::render` (all views) → ratatui buffer diff (TestBackend does the same
//! cell diffing as the crossterm backend) under an agent-loop workload:
//! token floods, tool calls with large outputs, subagent PTY floods,
//! context snapshots and Done.
//!
//! Human-perception targets (the benchmark FAILS if violated):
//! - steady-state streaming frame p99 ≤ 16 ms  (60 fps feel)
//! - worst single frame ≤ 50 ms               (input→photon bound)
//! - event-drain step p99 ≤ 8 ms              (UI thread stays responsive)
//!
//! Run: cargo test --release bench_e2e_agent_loop -- --ignored --nocapture

use ratatui::{Terminal, backend::TestBackend};

use super::App;
use cosh::harness::HarnessEvent;

const W: u16 = 160;
const H: u16 = 48;
/// ~30 fps pacing used by App::run while live.
const FRAME_MS_BUDGET: f64 = 33.333;

struct FrameStats {
    name: &'static str,
    draw_ms: Vec<f64>,
    drain_ms: Vec<f64>,
}

impl FrameStats {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            draw_ms: Vec::new(),
            drain_ms: Vec::new(),
        }
    }
    fn report(self) -> bool {
        let mut d = self.draw_ms.clone();
        let mut e = self.drain_ms.clone();
        if d.is_empty() {
            return true;
        }
        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
        e.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = d.len();
        let pct = |v: &[f64], p: f64| v[(((v.len() as f64 - 1.0) * p) as usize).min(v.len() - 1)];
        let worst = d[n - 1];
        let p99 = pct(&d, 0.99);
        let edrain = pct(&e, 0.99);
        println!(
            "[BENCH] {:<26} frames={:<5} draw_avg={:>7.2}ms draw_p99={:>7.2}ms draw_max={:>8.2}ms drain_p99={:>7.2}ms",
            self.name,
            n,
            d.iter().sum::<f64>() / n as f64,
            p99,
            worst,
            edrain,
        );
        // Human-perception gates.
        let ok = p99 <= FRAME_MS_BUDGET && worst <= 50.0 && edrain <= 8.0;
        if !ok {
            println!(
                "[BENCH]   ✗ TARGET MISS (p99<={FRAME_MS_BUDGET}ms? {} max<=50ms? {} drain_p99<=8ms? {})",
                p99 <= FRAME_MS_BUDGET,
                worst <= 50.0,
                edrain <= 8.0,
            );
        }
        ok
    }
}

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

fn shell_out(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            format!(
                "[{i}] INFO worker handled request bytes={} dur={}ms",
                i * 37 % 9999,
                i % 90
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One agent round-trip pushed through the real event channel.
fn push_agent_round(app: &mut App, seed: usize) {
    use crate::types::{Message, MessageRole, Part, TextPart};
    app.event_tx
        .send(HarnessEvent::Token {
            text: format!("user question {seed}: fix the handler\n"),
        })
        .unwrap();
    app.poll_events();
    app.state
        .current_session_mut()
        .unwrap()
        .messages
        .push(Message {
            id: format!("u-{seed}"),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: format!("Fix handler {seed} under load"),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    app.poll_events();

    app.event_tx
        .send(HarnessEvent::Reasoning {
            text: "thinking about the fix... ".repeat(20),
        })
        .unwrap();
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "read".into(),
            input: serde_json::json!({"file": format!("src/mod_{seed}.rs")}),
        })
        .unwrap();
    app.event_tx
        .send(HarnessEvent::ToolResult {
            output: big_rust_file(1200),
        })
        .unwrap();
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "bash_run".into(),
            input: serde_json::json!({"command": "cargo test"}),
        })
        .unwrap();
    for _ in 0..10 {
        app.event_tx
            .send(HarnessEvent::ToolOutput {
                tool: "bash_run".into(),
                output: format!("{}\n", shell_out(40)),
                finished: false,
            })
            .unwrap();
    }
    app.event_tx
        .send(HarnessEvent::ToolResult {
            output: shell_out(300),
        })
        .unwrap();
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "subagent_call".into(),
            input: serde_json::json!({"agent": "explore", "input": "find hot paths"}),
        })
        .unwrap();
    for _ in 0..15 {
        app.event_tx
            .send(HarnessEvent::ToolOutput {
                tool: "subagent_call".into(),
                output: "- reading src/**\n- found hot loop in worker.rs\n- summarizing findings\n"
                    .repeat(8),
                finished: false,
            })
            .unwrap();
    }
    app.event_tx
        .send(HarnessEvent::ToolResult {
            output: "done exploring".into(),
        })
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn bench_e2e_agent_loop() {
    let mut app = App::new("/tmp".to_string());
    app.title_generated = true; // avoid spawning network title generation on Done

    // Seed a long session history so caches are warm and transcript is large.
    {
        use crate::types::*;
        let mut messages = Vec::new();
        for r in 0..100 {
            messages.push(Message {
                id: format!("u-{r}"),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: format!("question number {r} about module {r}"),
                    synthetic: false,
                })],
                created_at: r as u64 * 1000,
                agent: None,
                model: None,
            });
            messages.push(Message {
                id: format!("a-{r}"),
                role: MessageRole::Assistant,
                parts: vec![
                    Part::Reasoning(ReasoningPart {
                        text: "reasoning... ".repeat(30),
                        collapsed: false,
                    }),
                    Part::Text(TextPart {
                        text: format!(
                            "### Answer {r}\n\nDetailed analysis with `code` spans and lists.\n"
                        ),
                        synthetic: false,
                    }),
                    Part::Tool(ToolPart {
                        tool: "read".into(),
                        input: serde_json::json!({"file": format!("src/m{r}.rs")}),
                        output: Some(big_rust_file(600)),
                        status: ToolStatus::Completed,
                        tool_call_id: None,
                        is_start: false,
                        is_streaming: false,
                        cached_line_count: None,
                    }),
                ],
                created_at: r as u64 * 1000 + 1,
                agent: None,
                model: Some("m".into()),
            });
        }
        let id = "e2e-bench".to_string();
        app.state.add_session(crate::types::Session {
            id: id.clone(),
            title: "E2E".into(),
            created_at: 0,
            title_generated: true,
            messages,
        });
        app.state.current_session_id = Some(id);
    }

    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let _ = area;
    let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
    app.state.status = crate::types::SessionStatus::Working;

    // Warm-up: one full render pass.
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    app.poll_events();

    let mut all_ok = true;

    // ── Scenario A: idle steady state (no events, Working=false) ──
    {
        app.state.status = crate::types::SessionStatus::Idle;
        let mut ft = FrameStats::new("idle_steady");
        for _ in 0..200 {
            let t = std::time::Instant::now();
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
            let draw = t.elapsed();
            ft.draw_ms.push(draw.as_secs_f64() * 1000.0);
            ft.drain_ms.push(0.0);
        }
        all_ok &= ft.report();
        app.state.status = crate::types::SessionStatus::Working;
    }

    // ── Scenario B: agent burst — tokens + tools + subagent over many rounds.
    // Simulates "agent working": each round pushes a full agent round-trip and
    // renders frames between event batches, like the real loop coalescing.
    {
        let mut ft = FrameStats::new("agent_burst_e2e");
        let chunk = "streamed answer text that keeps growing the trailing block. ";
        for round in 0..12 {
            push_agent_round(&mut app, round);
            // Frames while events stream in: interleave drain+draw like run().
            for f in 0..25 {
                let t = std::time::Instant::now();
                app.event_tx
                    .send(HarnessEvent::Token {
                        text: chunk.repeat(3),
                    })
                    .unwrap();
                app.poll_events();
                let drained = t.elapsed().as_secs_f64() * 1000.0;
                let t2 = std::time::Instant::now();
                terminal.draw(|frame| app.render(frame, 0.033)).unwrap();
                let draw = t2.elapsed();
                ft.drain_ms.push(drained);
                ft.draw_ms.push(draw.as_secs_f64() * 1000.0);
                let _ = f;
            }
            // ContextSnapshot every 2 rounds (~10s throttle upstream).
            if round % 2 == 1 {
                let t = std::time::Instant::now();
                app.event_tx
                    .send(HarnessEvent::ContextSnapshot {
                        context_state: vec![0u8; 1024],
                    })
                    .unwrap();
                app.poll_events();
                ft.drain_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        all_ok &= ft.report();
    }

    // ── Scenario C: user scrolls to top while agent streams ──
    {
        let mut ft = FrameStats::new("scroll_during_burst");
        let page = f64::from(H);
        let mut y = app.session_view.scroll_y as f64;
        while y > 0.0 {
            y -= page;
            app.session_view.scroll_by_raw(-page);
            let t = std::time::Instant::now();
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
            ft.draw_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            ft.drain_ms.push(0.0);
        }
        while app.session_view.scroll_y
            < app.session_view.total_height - app.session_view.visible_height
        {
            app.session_view.scroll_by_raw(page);
            let t = std::time::Instant::now();
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
            ft.draw_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            ft.drain_ms.push(0.0);
        }
        all_ok &= ft.report();
    }

    // ── Scenario D: Done — final save + transition out of live mode ──
    {
        let mut ft = FrameStats::new("done_save_transition");
        let t = std::time::Instant::now();
        app.event_tx
            .send(HarnessEvent::Done {
                context_state: vec![0u8; 64 * 1024],
            })
            .unwrap();
        app.poll_events();
        ft.drain_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t2 = std::time::Instant::now();
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
        ft.draw_ms.push(t2.elapsed().as_secs_f64() * 1000.0);
        all_ok &= ft.report();
    }

    assert!(
        all_ok,
        "[BENCH] human-perception targets missed — see [BENCH] lines above"
    );
}
