//! Reproduces the real agent-loop workload against a REAL session file:
//! - loads ~/.local/share/cosh/sessions/<hash>/session-*.jsonl
//! - simulates the exact sequence that happens when the agent works:
//!   ToolCall (spinner active → per-frame re-render), streaming tokens,
//!   subagent_call (right panel appears → main width shrinks),
//!   measuring EVERY frame's draw cost.
//!
//! Run: cargo test --release probe_real_session -- --ignored --nocapture

use ratatui::{Terminal, backend::TestBackend};

use cosh::harness::HarnessEvent;

use super::App;

fn load_latest_real_session() -> Option<(String, Vec<u8>)> {
    let dir = directories::ProjectDirs::from("", "", "cosh")?
        .data_dir()
        .join("sessions");
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&p).ok()?.flatten() {
            let fp = f.path();
            if fp.extension().is_some_and(|e| e == "jsonl") {
                let meta = std::fs::metadata(&fp).ok()?;
                let mtime = meta.modified().ok()?;
                // Skip files created by our own benchmarks.
                if fp.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.contains("bench") || n.contains("probe") || n.contains("e2e")
                }) {
                    continue;
                }
                if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
                    best = Some((fp, mtime));
                }
            }
        }
    }
    let (path, _) = best?;
    println!("[REAL] loading {}", path.display());
    let bytes = std::fs::read(&path).ok()?;
    Some((path.to_string_lossy().to_string(), bytes))
}

#[tokio::test]
#[ignore]
async fn probe_real_session() {
    use crate::types::{Message, MessageRole, Part};

    #[derive(serde::Deserialize)]
    struct StoredHeader {
        title: String,
        created_at: u64,
    }

    #[derive(serde::Deserialize)]
    struct StoredMsg {
        id: String,
        role: String,
        parts: Vec<Part>,
        created_at: u64,
        agent: Option<String>,
        model: Option<String>,
    }

    let Some((path, bytes)) = load_latest_real_session() else {
        panic!("no real session found");
    };
    let content = String::from_utf8(bytes).unwrap();
    let mut lines = content.lines();
    let header: StoredHeader = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut messages: Vec<Message> = Vec::new();
    for line in lines {
        if let Ok(sm) = serde_json::from_str::<StoredMsg>(line) {
            messages.push(Message {
                id: sm.id,
                role: if sm.role == "assistant" {
                    MessageRole::Assistant
                } else {
                    MessageRole::User
                },
                parts: sm.parts,
                created_at: sm.created_at,
                agent: sm.agent,
                model: sm.model,
            });
        }
    }
    println!("[REAL] session {}: {} messages", path, messages.len());
    for (i, m) in messages.iter().enumerate() {
        println!(
            "[REAL]   msg{i} role={:?} parts={} bytes~{}",
            m.role,
            m.parts.len(),
            serde_json::to_string(m).unwrap().len() / 1024
        );
    }

    let mut app = App::new("/tmp".to_string());
    app.title_generated = true;
    let id = "real-session".to_string();
    app.state.add_session(crate::types::Session {
        id: id.clone(),
        title: header.title.clone(),
        created_at: header.created_at,
        title_generated: true,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages,
    });
    app.state.current_session_id = Some(id);
    app.state.status = crate::types::SessionStatus::Working;

    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();

    // ── Baseline idle frame ──
    let t = std::time::Instant::now();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    println!(
        "[PROBE] first frame (cold caches): {:.2}ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    let t = std::time::Instant::now();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    println!(
        "[PROBE] second frame (warm):       {:.2}ms",
        t.elapsed().as_secs_f64() * 1000.0
    );

    // ── Agent starts working: ToolCall → active spinner → per-frame redraws ──
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "bash_run".into(),
            input: serde_json::json!({"command": "cargo build --release"}),
        })
        .unwrap();
    app.poll_events();
    println!("[PROBE] --- frames with ACTIVE TOOL SPINNER (agent working) ---");
    let mut worst = 0.0f64;
    let mut sum = 0.0f64;
    let n = 90; // ~3s at 30fps
    for i in 0..n {
        app.event_tx
            .send(HarnessEvent::Token {
                text: "streaming answer chunk ".repeat(3),
            })
            .unwrap();
        app.event_tx
            .send(HarnessEvent::ToolOutput {
                tool: "bash_run".into(),
                output: format!("compiling crate {i}/500\n"),
                finished: false,
                agent: None,
            })
            .unwrap();
        app.poll_events();
        let t = std::time::Instant::now();
        terminal.draw(|f| app.render(f, 0.033)).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        sum += ms;
        if i % 30 == 29 {
            println!("[PROBE] spinner frame {:>3}: {:.2}ms", i, ms);
        }
    }
    println!(
        "[PROBE] spinner frames: avg={:.2}ms max={:.2}ms over {n} frames",
        sum / n as f64,
        worst
    );

    // ── Subagent called: right panel APPEARS → main width shrinks ──
    println!("[PROBE] --- subagent_call (right panel appears) ---");
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "subagent_call".into(),
            input: serde_json::json!({"agent": "explore", "input": "find hot paths"}),
        })
        .unwrap();
    app.poll_events();
    for i in 0..10 {
        app.event_tx
            .send(HarnessEvent::ToolOutput {
                tool: "subagent_call".into(),
                output: "- reading src/**\n- found hot loop\n".repeat(5),
                finished: false,
                agent: None,
            })
            .unwrap();
        app.poll_events();
        let t = std::time::Instant::now();
        terminal.draw(|f| app.render(f, 0.033)).unwrap();
        println!(
            "[PROBE] panel-appear frame {i}: {:.2}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    // ── Sustained streaming with BOTH spinner + panel present ──
    println!("[PROBE] --- sustained: spinner + panel + token flood ---");
    let mut worst2 = 0.0f64;
    let mut sum2 = 0.0f64;
    let n2 = 120;
    for i in 0..n2 {
        app.event_tx
            .send(HarnessEvent::Token {
                text: "more streamed text ".repeat(4),
            })
            .unwrap();
        app.event_tx
            .send(HarnessEvent::ToolOutput {
                tool: "subagent_call".into(),
                output: "- exploring more\n".repeat(6),
                finished: false,
                agent: None,
            })
            .unwrap();
        app.poll_events();
        let t = std::time::Instant::now();
        terminal.draw(|f| app.render(f, 0.033)).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst2 = worst2.max(ms);
        sum2 += ms;
        if i % 40 == 39 {
            println!("[PROBE] sustained frame {:>3}: {:.2}ms", i, ms);
        }
    }
    println!(
        "[PROBE] sustained frames: avg={:.2}ms max={:.2}ms over {n2} frames",
        sum2 / n2 as f64,
        worst2
    );

    // ── Scroll while everything is active ──
    println!("[PROBE] --- scroll up during activity ---");
    for i in 0..12 {
        app.session_view.scroll_by_raw(-48.0);
        let t = std::time::Instant::now();
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
        println!(
            "[PROBE] scroll frame {i}: {:.2}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    // ── Done: transition out ──
    app.event_tx
        .send(HarnessEvent::Done {
            context: super::bench_e2e::realistic_context(100),
            checkup_verdict: None,
        })
        .unwrap();
    let t = std::time::Instant::now();
    app.poll_events();
    println!(
        "[PROBE] Done drain: {:.2}ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
}
