//! Stress probe for full-rebuild paths that a single user action triggers:
//!
//! - `expand_collapse`: one click on "expand tool output" sets
//!   `tool_state.version += 1` → `ensure_height_caches_fresh` does a FULL
//!   rebuild: re-estimates every part height AND re-hashes the entire
//!   transcript (`msg_change_tokens`). Should stay imperceptible (<50 ms).
//! - `resize`: width change → same full rebuild.
//!
//! (Session/theme switches take the same code path as resize via
//! `cache_max_w`/`config_token`, so they are not exercised separately.)
//!
//! Run: cargo test --release stress_full_rebuild -- --ignored --nocapture

use ratatui::{Terminal, backend::TestBackend};

use super::App;
use crate::types::*;

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

fn markdown_answer(r: usize) -> String {
    format!(
        "### Answer {r}\n\nDetailed analysis with `code` spans and lists.\n\n\
         - point one with **bold** text\n- point two with [links](https://example.com)\n\n\
         ```rust\nlet x = {r};\nprintln!(\"{{x}}\");\n```\n\n\
         Final paragraph wrapping across several columns so lines fold.\n"
    )
}

/// N rounds of user+assistant with reasoning, markdown and a big read output.
fn big_session(n_rounds: usize, file_lines: usize) -> Vec<Message> {
    let mut messages = Vec::new();
    for r in 0..n_rounds {
        messages.push(Message {
            id: format!("u-{r}"),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: format!("question number {r}: please analyze module {r} under load"),
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
                    text: "reasoning about the fix... ".repeat(30),
                    collapsed: false,
                }),
                Part::Text(TextPart {
                    text: markdown_answer(r),
                    synthetic: false,
                }),
                Part::Tool(ToolPart {
                    tool: "read".into(),
                    input: serde_json::json!({"file": format!("src/m{r}.rs")}),
                    output: Some(big_rust_file(file_lines)),
                    status: ToolStatus::Completed,
                    tool_call_id: Some(format!("read-{r}")),
                    is_start: false,
                    is_streaming: false,
                    cached_line_count: None,
                    lsp_notes: None,
                }),
            ],
            created_at: r as u64 * 1000 + 1,
            agent: None,
            model: Some("m".into()),
        });
    }
    messages
}

fn setup(n_msgs: usize) -> (App, Terminal<TestBackend>) {
    let mut app = App::new("/tmp".to_string());
    app.title_generated = true;
    let id = "stress".to_string();
    let messages = if n_msgs == 1000 {
        big_session(500, 600)
    } else {
        big_session(n_msgs / 2, 600)
    };
    app.state.add_session(crate::types::Session {
        id: id.clone(),
        title: "STRESS".into(),
        created_at: 0,
        title_generated: true,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages,
    });
    app.state.current_session_id = Some(id);
    let terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    (app, terminal)
}

#[tokio::test]
#[ignore]
async fn stress_full_rebuild() {
    const W: u16 = 160;
    const H: u16 = 48;

    // ── 1. Cold open: first frame of a 1000-message session ──
    let (mut app, mut terminal) = setup(1000);
    let t = std::time::Instant::now();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    println!(
        "[STRESS] cold_open 1000 msgs (height cache build): {:.1}ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    let t = std::time::Instant::now();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    println!(
        "[STRESS] warm frame after cold open:               {:.1}ms",
        t.elapsed().as_secs_f64() * 1000.0
    );

    // ── 2. Expand/collapse toggle (tool_state.version bump → full rebuild) ──
    println!("[STRESS] --- expand/collapse toggles (each = full rebuild) ---");
    for i in 0..5 {
        let t = std::time::Instant::now();
        app.session_view.tool_state.version += 1;
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
        println!(
            "[STRESS] toggle {}: {:.1}ms",
            i + 1,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    // ── 3. Terminal resize (width change → full rebuild) ──
    // NOTE: `Terminal::resize` gets undone by `autoresize` on the next draw
    // (Fullscreen viewport queries the backend). A real terminal resize is
    // the BACKEND changing size, so resize the backend directly.
    println!("[STRESS] --- resize (width change = full rebuild) ---");
    for (w, h) in [(120u16, 40u16), (200, 60), (160, 48)] {
        terminal.backend_mut().resize(w, h);
        let t = std::time::Instant::now();
        terminal
            .draw(|f| {
                app.render(f, 0.016);
            })
            .unwrap();
        println!(
            "[STRESS] resize to {w}x{h}: {:.1}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}
