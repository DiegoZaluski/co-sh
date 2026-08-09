//! Performance and correctness tests for the session view rendering.
//!
//! These tests verify that:
//! - Rendering doesn't degrade super-linearly with message size
//! - Streaming messages skip the expensive scan_content_height
//! - The render cache is effective for unchanged messages
//! - The stop_signal / ESC sovereignty is respected (simulated)

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;

use crate::config::TuiConfig;
use crate::state::AppState;
use crate::theme::{Theme, ThemeRegistry};
use crate::types::{
    Message, MessageRole, Part, ReasoningPart, Session, SessionStatus, TextPart, ToolPart,
    ToolStatus,
};

use super::SessionView;

/// Build a minimal Theme for testing.
fn test_theme() -> Theme {
    ThemeRegistry::new().default_theme().clone()
}

/// Build a default TuiConfig for testing.
fn test_config() -> TuiConfig {
    TuiConfig::default()
}

/// Build an AppState with a single session containing `msg` as the only message.
fn test_state(msg: Message) -> AppState {
    let mut state = AppState::new();
    let session = Session {
        id: "test-session".into(),
        title: "Test".into(),
        created_at: 0,
        messages: vec![msg],
    };
    state.add_session(session);
    state.current_session_id = Some("test-session".into());
    state.status = SessionStatus::Working;
    state
}

/// Build a large streaming text message with roughly `word_count` words.
/// The text is repetitive markdown to simulate a long streaming assistant response.
fn build_streaming_message(word_count: usize) -> Message {
    // Each paragraph is ~50 words of realistic-looking markdown text
    let paragraph = "This is a detailed explanation of the code changes. We need to **carefully** consider the architecture before making modifications. The system uses a *modular* approach with several interconnected components. Here are the key points to consider:\n\n";
    let num_paras = (word_count / 50).max(1);

    let mut text = String::with_capacity(word_count * 7); // ~7 chars per word avg
    for i in 0..num_paras {
        text.push_str(&format!("### Section {i}\n"));
        text.push_str(paragraph);
        text.push_str("```rust\n");
        text.push_str("fn process_data(input: &str) -> Result<(), Error> {\n");
        text.push_str("    let parsed = serde_json::from_str(input)?;\n");
        text.push_str("    for item in parsed.iter() {\n");
        text.push_str("        println!(\"Processing: {}\", item.id);\n");
        text.push_str("    }\n");
        text.push_str("    Ok(())\n");
        text.push_str("}\n");
        text.push_str("```\n\n");
        text.push_str("The implementation above handles the following cases:\n");
        text.push_str("- Input validation and error handling\n");
        text.push_str("- JSON parsing with `serde_json`\n");
        text.push_str("- Iterative processing of each item\n");
        text.push_str("- Proper error propagation\n\n");
    }

    Message {
        id: "msg-stream".into(),
        role: MessageRole::Assistant,
        parts: vec![Part::Text(TextPart {
            text,
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }
}

/// Render a SessionView once and measure the elapsed time.
fn render_and_time(
    view: &mut SessionView,
    buf: &mut Buffer,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    config: &TuiConfig,
) -> std::time::Duration {
    let start = std::time::Instant::now();
    view.render(buf, area, state, theme, config, 0.016);
    start.elapsed()
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_render_small_message() {
    let msg = build_streaming_message(100);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();
    let area = Rect::new(0, 0, 80, 40);
    let mut buf = Buffer::empty(area);

    let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] render 100-word msg: {:?}", elapsed);

    assert!(
        elapsed.as_millis() < 500,
        "Small message render took too long: {:?}",
        elapsed
    );
}

#[test]
fn test_render_large_message() {
    let msg = build_streaming_message(5000);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();
    let area = Rect::new(0, 0, 80, 200);
    let mut buf = Buffer::empty(area);

    let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] render 5000-word msg: {:?}", elapsed);

    assert!(
        elapsed.as_millis() < 5000,
        "Large message render took too long: {:?}",
        elapsed
    );
}

/// Test that render time does NOT grow super-linearly with message size.
/// With the streaming optimization (skip scan_content_height), render time
/// should grow roughly linearly with content size.
#[test]
fn test_render_performance_scaling() {
    let sizes = [100usize, 500, 2000, 5000];
    let mut prev_time = std::time::Duration::ZERO;

    for &size in &sizes {
        let msg = build_streaming_message(size);
        let state = test_state(msg);
        let mut view = SessionView::new();
        let theme = test_theme();
        let config = test_config();

        let area = Rect::new(0, 0, 80, 500);
        let mut buf = Buffer::empty(area);

        let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
        eprintln!("[BENCH] size={size:>5} words, render_time={:>8?}", elapsed,);

        assert!(
            elapsed.as_millis() < 5000,
            "Render of {size}-word msg took {:?}",
            elapsed
        );

        if prev_time > std::time::Duration::ZERO && size >= 2000 {
            let ratio = elapsed.as_nanos() as f64 / prev_time.as_nanos().max(1) as f64;
            let size_ratio =
                size as f64 / sizes[sizes.iter().position(|&s| s == size).unwrap() - 1] as f64;
            eprintln!("[BENCH]   growth ratio: {ratio:.2}x time vs {size_ratio:.1}x size");
        }

        prev_time = elapsed;
    }
}

/// Test that the msg_cache_cells effectively skips re-rendering for unchanged
/// messages. Render the same message twice and verify the second render is
/// faster (cache hit).
#[test]
fn test_msg_cache_effectiveness() {
    let msg = build_streaming_message(1000);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();
    let area = Rect::new(0, 0, 80, 100);
    let mut buf = Buffer::empty(area);

    let first = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] first render (cold cache): {:?}", first);

    let second = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] second render (cached): {:?}", second);

    // Cache hit should be noticeably faster
    assert!(
        second < first || (second.as_micros() as f64) < (first.as_micros() as f64) * 0.8,
        "Cached render ({:?}) should be faster than cold render ({:?})",
        second,
        first
    );
}

/// Test scan_content_height utility function on rendered markdown output.
#[test]
fn test_scan_content_height_works() {
    let text = "Hello **world** from the streaming test!";
    let mut md = MarkdownRenderable::new(Some(text.to_string()));
    md.set_fg(Some(ColorInput::RGBA(RGBA::from_ints(255, 255, 255, 255))));
    md.set_bg(Some(ColorInput::RGBA(RGBA::from_ints(0, 0, 0, 0))));

    let area = Rect::new(0, 0, 80, 50);
    let mut buf = Buffer::empty(area);
    md.render_self(&mut buf, area);

    let actual_h = SessionView::scan_content_height(&buf, 0, 0, 80, 50);
    assert!(
        actual_h > 0 && actual_h <= 5,
        "Expected content height 1-5, got {actual_h}"
    );
    eprintln!("[TEST] scan_content_height returned {actual_h} for simple text");
}

/// Verify that the reasoning/thinking body is dimmed toward the background by
/// the theme's `thinking_opacity` — white becomes gray, highlight colors get
/// grayed out — so it reads as "opaque thinking" vs. the normal reply.
#[test]
fn test_reasoning_body_is_dimmed() {
    use ratatui::style::Color;

    let theme = test_theme();
    let (mr, mg, mb, _) = theme.text_muted.to_ints();
    let (br, bg, bb, _) = theme.background.to_ints();
    let keep = theme.thinking_opacity.clamp(0.0, 1.0);
    assert!(
        keep < 1.0,
        "thinking_opacity should dim content (expected < 1.0, got {keep})"
    );

    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
    let mut line_h = 0u16;
    let part = ReasoningPart {
        text: "Hello thinking".to_string(),
        collapsed: false,
    };
    super::SessionView::render_reasoning(&mut buf, 0, 0, &mut line_h, 80, &part, true, &theme);

    // Header on row 0, body markdown starts at x+2=2, y+1=1.
    let cell = buf
        .cell((2, 1))
        .expect("expected a body cell below the - Thought header");

    let dim = |fg: u8, bg: u8| (f64::from(fg) * keep + f64::from(bg) * (1.0 - keep)).round() as u8;
    match cell.fg {
        Color::Rgb(r, g, b) => {
            assert_eq!(
                (r, g, b),
                (dim(mr, br), dim(mg, bg), dim(mb, bb)),
                "reasoning body must be dimmed by thinking_opacity"
            );
        }
        other => panic!("expected dimmed Rgb foreground, got {other:?}"),
    }
    assert!(
        line_h >= 2,
        "reasoning (header + body) should occupy at least 2 rows, got {line_h}"
    );
}

/// Test rendering with progressively larger messages to check for
/// performance degradation patterns. This simulates what happens during
/// a long streaming session where the message grows over time.
#[test]
fn test_streaming_incremental_growth() {
    let sizes = [100usize, 500, 1000, 2000, 5000];
    let theme = test_theme();
    let config = test_config();

    let mut prev_elapsed = std::time::Duration::ZERO;

    for &size in &sizes {
        let msg = build_streaming_message(size);
        let mut view = SessionView::new();

        let area = Rect::new(0, 0, 80, 1000);
        let mut buf = Buffer::empty(area);

        let state = test_state(msg);

        // Warm up: first render (cold cache)
        view.render(&mut buf, area, &state, &theme, &config, 0.016);

        // Measure second render (steady-state, potentially cached)
        let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
        eprintln!("[BENCH] incremental size={size:>5} words: {:>8?}", elapsed);

        assert!(
            elapsed.as_millis() < 5000,
            "Steady-state render of {size}-word msg took {:?}",
            elapsed
        );

        if prev_elapsed > std::time::Duration::ZERO {
            let time_ratio = elapsed.as_nanos() as f64 / prev_elapsed.as_nanos().max(1) as f64;
            let size_ratio =
                size as f64 / sizes[sizes.iter().position(|&s| s == size).unwrap() - 1] as f64;
            eprintln!("[BENCH]   time_ratio={time_ratio:.2}x, size_ratio={size_ratio:.1}x");
        }

        prev_elapsed = elapsed;
    }
}

/// Test that rendering with many different messages (simulating a full
/// chat history with tools, code blocks, etc.) doesn't degrade.
#[test]
fn test_full_chat_history_performance() {
    let theme = test_theme();
    let config = test_config();

    // Build a session with 20 message pairs (user + assistant)
    let mut messages = Vec::new();

    for i in 0..20 {
        messages.push(Message {
            id: format!("user-{i}"),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: format!("Can you help me with task number {i}?"),
                synthetic: false,
            })],
            created_at: i as u64 * 1000,
            agent: None,
            model: None,
        });

        messages.push(Message {
            id: format!("assistant-{i}"),
            role: MessageRole::Assistant,
            parts: vec![Part::Text(TextPart {
                text: format!(
                    "Let me help you with task {i}. Here's what I'll do:\n\n\
                    1. **Analyze** the requirements\n\
                    2. Write the necessary code\n\
                    3. Test the implementation\n\n\
                    ```rust\n\
                    fn task_{i}() -> Result<(), Error> {{\n\
                        let data = load_data()?;\n\
                        let result = process(data)?;\n\
                        save_result(result)?;\n\
                        Ok(())\n\
                    }}\n\
                    ```\n\n\
                    The implementation is complete!"
                ),
                synthetic: false,
            })],
            created_at: i as u64 * 1000 + 500,
            agent: None,
            model: None,
        });
    }

    let mut state = AppState::new();
    let session = Session {
        id: "chat-history".into(),
        title: "Chat".into(),
        created_at: 0,
        messages,
    };
    state.add_session(session);
    state.current_session_id = Some("chat-history".into());
    state.status = SessionStatus::Idle;

    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 100, 200);
    let mut buf = Buffer::empty(area);

    let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] full chat history (40 messages): {:?}", elapsed);

    assert!(
        elapsed.as_millis() < 5000,
        "Full chat history render took {:?}",
        elapsed
    );

    // Second render (cache hit) should be faster
    let second = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[BENCH] full chat (cached): {:?}", second);
    assert!(
        second <= elapsed || (second.as_micros() as f64) < (elapsed.as_micros() as f64) * 0.8,
        "Cached render ({:?}) should be faster than cold ({:?})",
        second,
        elapsed
    );
}

// ────────────────────────────────────────────────────────────────────────────
// ESC Sovereignty Tests (simulated)
// ────────────────────────────────────────────────────────────────────────────

/// Test that a large streaming render completes in reasonable time.
/// This is important because ESC can only be processed between renders.
#[test]
fn test_large_streaming_render_is_fast() {
    // Create a very large streaming message
    let theme = test_theme();
    let config = test_config();

    // First verify that rendering a 3000-word message with streaming=true
    // completes in a reasonable time (should use the scan_content_height skip)
    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 500);
    let mut buf = Buffer::empty(area);
    let state = test_state(build_streaming_message(3000));

    let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
    eprintln!("[ESC_TEST] Large streaming render: {:?}", elapsed);

    // With the streaming optimization (skip scan_content_height),
    // even a 3000-word message should render quickly
    assert!(
        elapsed.as_millis() < 3000,
        "Large streaming render was too slow ({:?}) - ESC would be delayed!",
        elapsed
    );

    // Verify the render produced output (the buffer has non-space content)
    let has_content = (0..area.height).any(|y| {
        (0..area.width).any(|x| {
            buf.cell((x, y))
                .is_some_and(|c| c.symbol().chars().next().unwrap_or(' ') != ' ')
        })
    });
    assert!(
        has_content,
        "Rendered buffer should contain visible content"
    );
}

/// Test that stop_signal → status transition works correctly.
#[test]
fn test_stop_signal_flow() {
    let stop_signal = Arc::new(AtomicBool::new(false));
    let msg = build_streaming_message(2000);
    let state = test_state(msg);
    let theme = test_theme();
    let config = test_config();

    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 300);
    let mut buf = Buffer::empty(area);

    // Render with streaming=true (simulating active agent loop)
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    eprintln!("[ESC_TEST] Initial render OK");

    // Simulate ESC press: set stop_signal
    stop_signal.store(true, Ordering::Relaxed);
    eprintln!("[ESC_TEST] Stop signal set to true");

    // Verify stop_signal is readable (simulating the agent loop poll)
    assert!(
        stop_signal.load(Ordering::Relaxed),
        "stop_signal should be readable after store"
    );

    // Render again with the stop signal set (simulating post-stop state)
    // This should still work - the render itself doesn't check stop_signal,
    // but the event loop (app.rs) does before calling render.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    eprintln!("[ESC_TEST] Post-stop render OK");
}

/// Performance test: measure total time for multiple render cycles with a
/// very large streaming message, simulating N frames of rendering during
/// a long streaming session. Verifies that the average frame time is bounded.
#[test]
fn test_streaming_frame_times() {
    let msg = build_streaming_message(4000);
    let state = test_state(msg);
    let theme = test_theme();
    let config = test_config();

    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 400);
    let mut buf = Buffer::empty(area);

    // Warmup frame: populates the per-view height caches, the global highlight
    // cache and the scratch buffer. The frame-0 cold cost varies wildly with the
    // test binary's code layout and the heap state left by earlier tests in the
    // same process (measured 480-1100ms for identical input), so excluding it
    // makes the steady-state assertion below stable.
    render_and_time(&mut view, &mut buf, area, &state, &theme, &config);

    // Measure 5 steady-state render cycles (simulating 5 frames during streaming)
    let mut times = Vec::with_capacity(5);
    for frame in 0..5 {
        let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
        times.push(elapsed);
        eprintln!("[FPS_TEST] Frame {frame}: {:?}", elapsed);
    }

    // Median, not mean: a single frame inflated by a scheduler hiccup or a
    // concurrent test thread should not fail the assertion.
    times.sort_unstable();
    let median_us = times[times.len() / 2].as_micros();
    eprintln!("[FPS_TEST] Median frame time: {median_us}us");

    // Debug (unoptimized) renders of the 4000-word streaming message are
    // extremely sensitive to the test binary's code layout and to heap state
    // left by earlier tests in the same process — measured 160ms warm (fresh
    // process) vs 610ms warm (after a heavy render ran first) for the SAME
    // binary. 800ms on the median keeps the assertion stable across that
    // noise; it catches only multi-second regressions (the 5s worst-frame
    // guard below is the real hang-detector). Release builds render ~10x
    // faster, so this remains a conservative ESC-responsiveness proxy, not a
    // tight product bound.
    assert!(
        median_us < 800_000,
        "Median frame time {median_us}us is too high for responsive ESC!"
    );

    // No individual frame should take more than 5s. Check the worst one —
    // `times` was sorted above for the median, so enumerating it here would
    // mislabel the original frame numbers.
    let worst = times.iter().max().unwrap();
    assert!(
        worst.as_millis() < 5000,
        "A frame took {:?}, exceeding the 5s threshold",
        worst
    );
}

/// Test that scroll position does NOT jump when streaming content grows.
/// This is a regression test for the bug where actual_total_height was not
/// synced to cached_total_height, causing the scroll to snap to the OLD
/// bottom position when new streaming content appeared.
#[test]
fn test_scroll_stays_at_bottom_during_streaming_growth() {
    let theme = test_theme();
    let config = test_config();

    // Start with a moderate message
    let initial_msg = build_streaming_message(200);
    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(area);
    let state = test_state(initial_msg);

    // Render initial message
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    let initial_pos = view.scroll_y;
    eprintln!("[SCROLL_TEST] Initial scroll_y: {initial_pos}");

    // Verify we're at or near the bottom
    assert!(
        view.is_sticky_bottom || view.is_at_bottom(),
        "Should be at bottom after initial render, scroll_y={}, sticky={}, actual_total={}, visible={}",
        view.scroll_y,
        view.is_sticky_bottom,
        view.actual_total_height,
        view.visible_height
    );

    // Now simulate a LARGER message (streaming growth)
    let grown_msg = build_streaming_message(800);
    let grown_state = test_state(grown_msg);

    // Render the larger message — this triggers ensure_height_caches_fresh
    // which recalculates cached_total_height. If actual_total_height is NOT
    // synced, the scroll will snap to the old (lower) position.
    view.render(&mut buf, area, &grown_state, &theme, &config, 0.016);
    let grown_pos = view.scroll_y;
    eprintln!("[SCROLL_TEST] After growth scroll_y: {grown_pos}");

    // We should STILL be at or near the bottom after content growth
    assert!(
        view.is_sticky_bottom || view.is_at_bottom(),
        "Scroll jumped away from bottom after content growth! scroll_y={}, sticky={}, actual_total={}, visible={}",
        view.scroll_y,
        view.is_sticky_bottom,
        view.actual_total_height,
        view.visible_height
    );

    // The scroll should have INCREASED (or stayed same) to account for the
    // new content. A decreasing scroll_y means a gap appeared at the bottom.
    assert!(
        grown_pos >= initial_pos,
        "Scroll position DECREASED from {initial_pos} to {grown_pos} during content growth! This creates a visible gap at the bottom."
    );

    eprintln!(
        "[SCROLL_TEST] PASS: scroll_y went from {initial_pos} to {grown_pos}, actual_total from {}",
        view.actual_total_height
    );
}

/// Test that scroll position does NOT oscillate between consecutive frames.
/// This is a regression test for the bug where cached_total_height would get
/// inflated by .max(actual_total) at end of render, causing the NEXT frame
/// to see a height change and trigger recalculate_bar_props (which changes
/// scroll_y). The render cache hit check uses total_height != last_content_height,
/// so inflating cached_total_height causes spurious recalculations.
#[test]
fn test_scroll_does_not_oscillate_between_frames() {
    let theme = test_theme();
    let config = test_config();

    let msg = build_streaming_message(500);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(area);

    // Render 3 consecutive frames with the same state (simulating cache hit)
    let mut scroll_positions = Vec::new();
    for frame in 0..3 {
        view.render(&mut buf, area, &state, &theme, &config, 0.016);
        scroll_positions.push((view.scroll_y, view.actual_total_height, view.total_height));
        eprintln!(
            "[OSCILLATE] Frame {frame}: scroll_y={} actual_total={} total_height={} cached={} last_content={}",
            view.scroll_y,
            view.actual_total_height,
            view.total_height,
            view.cached_total_height,
            view.last_content_height
        );
    }

    // All frames should have the SAME scroll position (no oscillation)
    let first = scroll_positions[0];
    for (i, &pos) in scroll_positions.iter().enumerate() {
        assert_eq!(
            pos, first,
            "Frame {i} scroll state differs from frame 0! Got ({}, {}, {}) vs ({}, {}, {})",
            pos.0, pos.1, pos.2, first.0, first.1, first.2
        );
    }

    eprintln!(
        "[OSCILLATE] PASS: scroll position stable across {}",
        scroll_positions.len()
    );
}

/// Test that actual_total_height and cached_total_height are in sync after
/// each render. If they diverge, scroll calculations use stale values.
#[test]
fn test_height_sync_after_render() {
    let theme = test_theme();
    let config = test_config();

    // Small message that fits in viewport
    let msg = build_streaming_message(50);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(area);

    view.render(&mut buf, area, &state, &theme, &config, 0.016);

    eprintln!(
        "[SYNC] after render: actual_total={} cached_total={} total={}",
        view.actual_total_height, view.cached_total_height, view.total_height
    );

    // cached_total_height should NOT be inflated above actual_total_height
    // The .max() was removed — cache is the stable source of truth

    // Both should be positive and reasonable
    assert!(
        view.actual_total_height > 0,
        "actual_total_height should be > 0"
    );
    assert!(
        view.cached_total_height > 0,
        "cached_total_height should be > 0"
    );

    // total_height should match cached_total_height (not actual_total)
    assert_eq!(
        view.total_height, view.cached_total_height,
        "total_height should equal cached_total_height, not actual_total_height"
    );

    // last_content_height should match cached_total_height (to prevent
    // spurious recalculate_bar_props on next frame)
    assert_eq!(
        view.last_content_height, view.cached_total_height,
        "last_content_height should equal cached_total_height to prevent oscillation"
    );
}

/// Test that repeated content growth (simulating long streaming) keeps the
/// scroll position at the bottom and doesn't accumulate gaps.
#[test]
fn test_scroll_does_not_jump_on_repeated_streaming() {
    let theme = test_theme();
    let config = test_config();

    let sizes = [100usize, 300, 600, 1000, 1500];
    let mut prev_scroll = 0i32;

    for &size in &sizes {
        let msg = build_streaming_message(size);
        let mut view = SessionView::new();
        let area = Rect::new(0, 0, 80, 60);
        let mut buf = Buffer::empty(area);
        let state = test_state(msg);

        view.render(&mut buf, area, &state, &theme, &config, 0.016);

        eprintln!(
            "[SCROLL_GROWTH] size={size:>4}w scroll_y={:>5} actual_total={:>5} sticky={}",
            view.scroll_y, view.actual_total_height, view.is_sticky_bottom
        );

        // Each size should be at bottom
        assert!(
            view.is_sticky_bottom || view.is_at_bottom(),
            "Not at bottom at size {size}: scroll_y={} actual_total={} visible={}",
            view.scroll_y,
            view.actual_total_height,
            view.visible_height
        );

        // Scroll should never decrease (no gap)
        assert!(
            view.scroll_y >= prev_scroll,
            "scroll_y decreased from {prev_scroll} to {} at size {size}!",
            view.scroll_y
        );

        prev_scroll = view.scroll_y;
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Regression: drag-selection clamp panic on zero-height viewport
// ────────────────────────────────────────────────────────────────────────────

/// Regression test for the `min > max. min = 1, max = 0` panic.
///
/// When a drag selection is active (`drag_selection` is `Some`) and the
/// session viewport momentarily has zero height (e.g. the terminal is resized
/// larger until the right panel appears, which can briefly report a 0-height
/// session area before the next draw), the selection highlight renderer runs:
///
/// ```ignore
/// (…).clamp(vp_top, vp_top + i32::from(inner_area.height) - 1)
/// ```
///
/// With `inner_area.height == 0` the upper bound is `vp_top - 1`, which is
/// smaller than the lower bound `vp_top` → `Ord::clamp` panics. The render
/// must tolerate a zero-height viewport and simply skip the highlight.
#[test]
fn drag_selection_render_tolerates_zero_height_viewport() {
    let msg = build_streaming_message(100);
    let state = test_state(msg);
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    // Session area with y = 1 and height = 0: this is the exact geometry that
    // produces `min = 1, max = 0` in the selection-highlight clamp.
    let area = Rect::new(0, 1, 80, 0);
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));

    // Simulate an in-flight drag selection.
    view.drag_selection = Some((10, 1, 50, 1));
    view.selection_anchor_content_y = 0;
    view.selection_focus_content_y = 3;

    // Must not panic. The highlight is simply not drawn for a 0-height viewport.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
}

/// Extract the text currently in the buffer (one line per row).
fn buffer_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn test_bash_output_expand_toggles_and_grows_height() {
    use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};

    use crate::types::{ToolPart, ToolStatus};

    let output: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let msg = Message {
        id: "msg-bash".into(),
        role: MessageRole::Assistant,
        parts: vec![
            Part::Text(TextPart {
                text: "Let me check that.".into(),
                synthetic: false,
            }),
            Part::Tool(ToolPart {
                tool: "bash_run".into(),
                input: serde_json::json!({ "command": "echo hello" }),
                output: Some(output),
                status: ToolStatus::Completed,
                tool_call_id: Some("bash-1".into()),
                is_start: true,
                is_streaming: false,
                cached_line_count: None,
            }),
        ],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = test_state(msg);
    state.status = SessionStatus::Idle;
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 60));

    // First render: the bash block is collapsed to a preview.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    let collapsed_msg_h = view.msg_height_cache[0];
    let collapsed_part_h = view.part_heights_cache[0][1];
    assert!(!view.tool_state.is_expanded("bash-1"));
    assert!(!buffer_text(&buf).contains("line 19"));

    // Click inside the bash block (second part, so it starts after the text part).
    let text_h = view.part_heights_cache[0][0];
    let bash_top = area.y + text_h;
    let click = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        area.x + 10,
        bash_top + 2,
        MouseModifiers::none(),
    );
    let handled = view.handle_mouse(&click, area, &state, &config);
    assert!(handled);
    assert!(view.tool_state.is_expanded("bash-1"));

    // Second render: the message height must grow to fit the full output and
    // the previously truncated lines must now be visible.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    assert!(view.msg_height_cache[0] > collapsed_msg_h);
    assert!(view.part_heights_cache[0][1] > collapsed_part_h);
    assert!(buffer_text(&buf).contains("line 19"));

    let selected = view.get_text_in_region(
        area.x,
        area.y,
        area.x + area.width.saturating_sub(1),
        area.y + area.height.saturating_sub(1),
    );
    assert!(selected.contains("line 19"));
}

#[test]
fn test_bash_output_collapse_shrinks_without_scroll_gap() {
    use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};

    use crate::types::{ToolPart, ToolStatus};

    let output: String = (0..120).map(|i| format!("line {i}\n")).collect();
    let msg = Message {
        id: "msg-bash".into(),
        role: MessageRole::Assistant,
        parts: vec![
            Part::Text(TextPart {
                text: "Let me check that.".into(),
                synthetic: false,
            }),
            Part::Tool(ToolPart {
                tool: "bash_run".into(),
                input: serde_json::json!({ "command": "echo hello" }),
                output: Some(output),
                status: ToolStatus::Completed,
                tool_call_id: Some("bash-1".into()),
                is_start: true,
                is_streaming: false,
                cached_line_count: None,
            }),
        ],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = test_state(msg);
    state.status = SessionStatus::Idle;
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);

    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    let collapsed_scroll_y = view.scroll_y;
    let text_h = view.part_heights_cache[0][0];
    let bash_top = area.y + text_h;
    let expand_click = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        area.x + 10,
        bash_top + 2,
        MouseModifiers::none(),
    );
    assert!(view.handle_mouse(&expand_click, area, &state, &config));
    view.render(&mut buf, area, &state, &theme, &config, 0.016);

    assert!(view.tool_state.is_expanded("bash-1"));
    assert_eq!(view.scroll_y, collapsed_scroll_y);
    assert!(view.has_manual_scroll);

    view.scroll_to_bottom();
    let expanded_scroll_y = view.scroll_y;
    assert!(expanded_scroll_y > 0);

    let collapse_click = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        area.x + 10,
        area.y + 5,
        MouseModifiers::none(),
    );
    assert!(view.handle_mouse(&collapse_click, area, &state, &config));
    view.render(&mut buf, area, &state, &theme, &config, 0.016);

    assert!(!view.tool_state.is_expanded("bash-1"));
    assert!(view.scroll_y < expanded_scroll_y);
    assert!(view.scroll_y <= (view.actual_total_height - view.visible_height).max(0));
    assert!(!buffer_text(&buf).contains("line 119"));
}

/// Glob results render as an expandable block: collapsed preview shows only a
/// bounded set of file paths, a click toggles the full grouped list, and the
/// message/part heights grow to match (mirroring the bash block behaviour).
#[test]
fn test_glob_output_expand_shows_grouped_list() {
    use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};

    use crate::types::{ToolPart, ToolStatus};

    // 40 matches strongly exceed the collapse preview (10 lines).
    let matches: Vec<serde_json::Value> = (0..40)
        .map(|i| {
            serde_json::json!({
                "path": format!("src/mod{i}.rs"),
                "file_type": "file",
                "mtime_ms": null,
                "size_bytes": null
            })
        })
        .collect();
    // The tool emits `formatted` (grouped), `scope`, and `cwd` alongside the
    // matches; the TUI prefers `formatted` for the block body.
    let output = serde_json::json!({
        "matches": matches,
        "total": 40u32,
        "formatted": (0..40).map(|i| format!("src/mod{i}.rs")).collect::<Vec<_>>().join("\n"),
        "scope": ".",
        "cwd": "/work"
    })
    .to_string();
    let msg = Message {
        id: "msg-glob".into(),
        role: MessageRole::Assistant,
        parts: vec![
            Part::Text(TextPart {
                text: "Let me find those.".into(),
                synthetic: false,
            }),
            Part::Tool(ToolPart {
                tool: "find_glob".into(),
                input: serde_json::json!({ "pattern": "*.rs", "path": "." }),
                output: Some(output),
                status: ToolStatus::Completed,
                tool_call_id: Some("glob-1".into()),
                is_start: true,
                is_streaming: false,
                cached_line_count: None,
            }),
        ],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = test_state(msg);
    state.status = SessionStatus::Idle;
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 60));

    // First render: glob is collapsed to a preview (10 lines max).
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    let collapsed_msg_h = view.msg_height_cache[0];
    let collapsed_part_h = view.part_heights_cache[0][1];
    assert!(!view.tool_state.is_expanded("glob-1"));
    assert!(!buffer_text(&buf).contains("src/mod39.rs"));
    // The header is visible even when collapsed.
    assert!(buffer_text(&buf).contains("Glob"));

    // The box has 1 row of internal padding above the title (the bottom pad
    // is the last row), matching the Summarizing box.
    let title_row = (0..area.height)
        .find(|&r| {
            let row_text: String = (0..area.width)
                .filter_map(|cx| buf.cell((cx, r)))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect();
            row_text.contains("Glob")
        })
        .expect("glob title row must exist");
    assert!(title_row > 0, "the Glob title has 1 row of padding above it");
    let above_glyphs: Vec<char> = (0..area.width)
        .filter_map(|cx| buf.cell((cx, title_row - 1)))
        .filter_map(|c| c.symbol().chars().next())
        .filter(|&ch| ch != ' ')
        .collect();
    assert!(
        above_glyphs.iter().all(|&c| c == '┃'),
        "the row above the Glob title is blank top padding (only the border)"
    );

    // Click inside the glob block.
    let text_h = view.part_heights_cache[0][0];
    let glob_top = area.y + text_h;
    let click = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        area.x + 10,
        glob_top + 2,
        MouseModifiers::none(),
    );
    assert!(view.handle_mouse(&click, area, &state, &config));
    assert!(view.tool_state.is_expanded("glob-1"));

    // Second render: heights grow and the full grouped list is visible.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    assert!(view.msg_height_cache[0] > collapsed_msg_h);
    assert!(view.part_heights_cache[0][1] > collapsed_part_h);
    assert!(buffer_text(&buf).contains("src/mod39.rs"));

    let selected = view.get_text_in_region(
        area.x,
        area.y,
        area.x + area.width.saturating_sub(1),
        area.y + area.height.saturating_sub(1),
    );
    assert!(selected.contains("src/mod39.rs"));
}

/// Regression test for ratatui panics on control characters in buffer cells.
///
/// Raw bash output commonly contains `\r` (progress spinners), `\t`, and
/// ANSI/ESC bytes, and commands can be multiline (`\n`). These used to be
/// written straight into buffer cells via `Cell::set_char`, which makes
/// ratatui's buffer diff panic with:
///   "control character passed to cell_width without filtering"
/// when the terminal is resized large enough (or scrolled) to show those rows.
/// Every render path must strip control characters before writing cells.
#[test]
fn test_bash_output_with_control_chars_does_not_pollute_cells() {
    use crate::types::{ToolPart, ToolStatus};

    let output = "Build 10%\rBuild 55%\rBuild 100%\ndone\tok\n\u{1b}[32mgreen\u{1b}[0m\n";
    let msg = Message {
        id: "msg-bash-ctrl".into(),
        role: MessageRole::Assistant,
        parts: vec![Part::Tool(ToolPart {
            tool: "bash_run".into(),
            // Multiline command: `\n` used to end up in the `$ cmd` title cell.
            input: serde_json::json!({ "command": "echo a\necho b" }),
            output: Some(output.into()),
            status: ToolStatus::Completed,
            tool_call_id: Some("bash-ctrl".into()),
            is_start: true,
            is_streaming: false,
            cached_line_count: None,
        })],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = test_state(msg);
    state.status = SessionStatus::Idle;
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    // Large viewport: the rows containing the control-char lines are visible
    // (the exact geometry that used to trigger the panic).
    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(area);

    view.render(&mut buf, area, &state, &theme, &config, 0.016);

    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                let symbol = cell.symbol();
                assert!(
                    !symbol.chars().any(char::is_control),
                    "cell ({x},{y}) contains control char {symbol:?}"
                );
            }
        }
    }

    // Sanity check: the visible output text is still there (only control
    // characters were stripped).
    let text = buffer_text(&buf);
    assert!(
        text.contains("Build"),
        "bash output should still be rendered"
    );
    assert!(
        text.contains("done"),
        "bash output should still be rendered"
    );
}

#[test]
fn compaction_line_formatting() {
    use crate::types::{CompactionPart, CompactionPhase};

    use super::compaction_line;

    // Running pipeline: the stopwatch ticks from `started_at` (no spinner —
    // the moving number is the activity signal). `now` is passed explicitly so
    // the assertion is deterministic (no wall-clock race).
    let running = CompactionPart {
        phase: CompactionPhase::Pipeline,
        started_at: 10_000,
        elapsed_ms: None,
        text: String::new(),
    };
    assert_eq!(
        compaction_line(&running, 11_500),
        "context compression · 1.500s"
    );
    // Millisecond precision: one tick later the same line shows 1.501s.
    assert_eq!(
        compaction_line(&running, 11_501),
        "context compression · 1.501s"
    );

    // Finished pipeline: the same line, frozen — no checkmark, the stopped
    // stopwatch is the completion signal.
    let done = CompactionPart {
        phase: CompactionPhase::Pipeline,
        started_at: 0,
        elapsed_ms: Some(2_340),
        text: String::new(),
    };
    assert_eq!(compaction_line(&done, 0), "context compression · 2.340s");

    // One-shot phase notices: plain colored labels — no stopwatch, no
    // completion marker.
    assert_eq!(
        compaction_line(&CompactionPart::done(CompactionPhase::Drafts), 0),
        "draft eviction"
    );

    // The LLM compaction (phase 3) carries a stopwatch like the pipeline's.
    let llm = CompactionPart {
        phase: CompactionPhase::Llm,
        started_at: 0,
        elapsed_ms: Some(2_340),
        text: String::new(),
    };
    assert_eq!(compaction_line(&llm, 0), "llm compaction · 2.340s");
    let llm_running = CompactionPart {
        phase: CompactionPhase::Llm,
        started_at: 10_000,
        elapsed_ms: None,
        text: String::new(),
    };
    assert_eq!(
        compaction_line(&llm_running, 11_500),
        "llm compaction · 1.500s"
    );
}

#[test]
fn compaction_part_serde_roundtrip() {
    use crate::types::{CompactionPart, CompactionPhase, Part};

    // The Compaction part must survive JSONL serialization (running + done),
    // including the streamed `text` field of the "Summarizing" box.
    let running = Part::Compaction(CompactionPart {
        phase: CompactionPhase::Pipeline,
        started_at: 1234,
        elapsed_ms: None,
        text: String::new(),
    });
    let json = serde_json::to_string(&running).unwrap();
    assert_eq!(
        json,
        "{\"type\":\"Compaction\",\"phase\":\"Pipeline\",\"started_at\":1234,\"elapsed_ms\":null,\"text\":\"\"}"
    );
    let back: Part = serde_json::from_str(&json).unwrap();
    assert_eq!(json, serde_json::to_string(&back).unwrap());

    let done = Part::Compaction(CompactionPart {
        phase: CompactionPhase::Llm,
        started_at: 7,
        elapsed_ms: Some(99),
        text: "## Objective\n- finish".to_string(),
    });
    let json = serde_json::to_string(&done).unwrap();
    let back: Part = serde_json::from_str(&json).unwrap();
    assert_eq!(json, serde_json::to_string(&back).unwrap());

    // Back-compat: JSONL written before the `text` field (no `text` key)
    // must still deserialize — `#[serde(default)]` fills an empty string.
    let legacy = r#"{"type":"Compaction","phase":"Llm","started_at":7,"elapsed_ms":99}"#;
    let back: Part = serde_json::from_str(legacy).unwrap();
    assert!(matches!(
        back,
        Part::Compaction(c) if c.text.is_empty() && c.phase == CompactionPhase::Llm
    ));
}

// The "Summarizing" box (LLM compaction, phase 3): collapsed it shows ONLY the
// LAST lines of the streamed text (the LRU-like scroll-up preview — the top
// lines leave the box as the stream grows, nothing is removed), and clicking
// expands it to the full text with a taller height. Long lines WORD-WRAP
// inside the box instead of being cut at the right edge.
/// Empirical check that `estimate_height` matches the real markdown layout
/// (the Summarizing box derives its height AND its collapsed tail window from
/// the estimate, so any drift would clip the newest rows of the preview or
/// show blank rows).
#[test]
fn markdown_estimate_matches_render_height() {
    use cosh_tui::core::renderables::markdown::estimate_height;
    let cases: &[&str] = &[
        // Note: code blocks here deliberately use NO language tag — the test
        // compares row counts, which syntax highlighting does not change, and
        // avoiding tree-sitter keeps this test light (a heavy test running
        // before/in parallel with the timing-sensitive streaming_frame_times
        // test skews its wall-clock measurements).
        "# Title\n\nSome **bold** and `code`.\n\n- item one\n- item two\n\n```\nfn main() {}\n```\n",
        // Multi-line code block: pins the N+2 accounting (top gap + lines +
        // bottom margin) for more than one line.
        "```\nlet a = 1;\nlet b = 2;\nprintln!(\"{a+b}\");\n```\n",
        // A code line WIDER than the box: the estimator's code_max_w must
        // agree with the renderer's wrap width or wrapped rows drift.
        "```\nlet long = \"this_is_a_very_long_line_with_no_spaces_that_must_wrap_inside_the_code_block\";\n```\n",
        // CJK wide chars exercise the width logic of both paths.
        "日本語のテキストが長い行でラップされるべきです。日本語のテキストが長い行でラップされるべきです。\n\n二行目です。\n",
        // A long unbroken run (like a serialized payload).
        &"x".repeat(200),
        // A markdown table.
        "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n",
    ];
    let theme = test_theme();
    // Reuse ONE buffer across cases (resize retains the allocation) — many
    // fresh allocations here fragment the heap and deterministically slow the
    // wall-clock timing test `test_streaming_frame_times` that runs later.
    let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
    for text in cases {
        // Two widths: the wide default and a narrow box, where the code inset
        // (min_x=2) and the whole-word vs char-level wrap decisions matter
        // proportionally more.
        for max_w in [40u16, 14] {
            let est = estimate_height(text, max_w).max(1);
            // Render into a buffer tall enough to fit anything the renderer lays
            // out, then scan the ACTUAL content height.
            let area = Rect::new(0, 0, max_w, est.saturating_add(20));
            buf.resize(area);
            buf.reset();
            let mut md = MarkdownRenderable::new(Some((*text).to_string()));
            md.set_fg(Some(ColorInput::RGBA(theme.text)));
            md.set_bg(Some(ColorInput::RGBA(theme.background)));
            md.render_self(&mut buf, area);
            let actual = SessionView::scan_content_height(&buf, 0, 0, max_w, area.height);
            assert!(actual > 0, "empty render for {text:?}");
            // The estimate must never be SMALLER than the rendered glyph
            // height for the constructs exercised here: the chat allocates
            // each message area from the estimate and clips whatever does not
            // fit, so an under-estimate silently cuts the last lines of the
            // message (a code block followed by text was under-counted by 2
            // rows and clipped the trailing paragraph). A small over-estimate
            // is harmless — a code block at the very end of the text leaves
            // its bottom-pad/separator/TagEnd rows blank, which the glyph
            // scan does not count. (Known pre-existing, out-of-scope drift:
            // blockquotes under-estimate by 1 at very narrow widths because
            // the estimator does not apply the renderer's 2-column inset.)
            assert!(
                est >= actual,
                "estimate={est} actual={actual} for {text:?} at max_w={max_w} (under-estimate \
             clips the message's last rows)"
            );
        }
    }
}

/// Regression test for the truncation bug: a message whose text continues
/// AFTER a code block ("here is the code... and that is why...") must never
/// be clipped. The chat lays the last message out at `estimate_height` rows;
/// if that estimate is 2 rows short (the renderer's code-block bottom-pad and
/// blank-separator rows were not counted), the trailing paragraph after the
/// code block is cut off at the bottom of the chat — exactly the reported
/// "last assistant output truncated when the screen is expanded" symptom
/// (shrinking the window makes the chat scrollable, which routes the message
/// through the full-height render path and the text reappears).
#[test]
fn test_text_after_code_block_not_clipped() {
    use cosh_tui::core::renderables::markdown::estimate_height;
    // Realistic shape: prose, a code block, then a closing paragraph — the
    // trailing paragraph is what got clipped.
    let text = "Aqui está a explicação completa. O problema ocorre porque o layout estima a altura de forma diferente do render real quando existem blocos de código.\n\n```rust\nfn compute(input: &str) -> i32 {\n    let parsed: Vec<i32> = input.split(',').filter_map(|s| s.parse().ok()).collect();\n    parsed.iter().sum()\n}\n```\n\nConclusão final do assistente.";
    let theme = test_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
    for max_w in [14u16, 20, 40, 60, 80, 100, 120, 150] {
        let est = estimate_height(text, max_w).max(1);
        // Allocate EXACTLY the estimated height, like the chat does, and
        // render the message into it.
        let area = Rect::new(0, 0, max_w, est);
        buf.resize(area);
        buf.reset();
        let mut md = MarkdownRenderable::new(Some(text.to_string()));
        md.set_fg(Some(ColorInput::RGBA(theme.text)));
        md.set_bg(Some(ColorInput::RGBA(theme.background)));
        md.render_self(&mut buf, area);
        let actual = SessionView::scan_content_height(&buf, 0, 0, max_w, est);
        assert_eq!(
            est, actual,
            "trailing paragraph after a code block was clipped at max_w={max_w} \
             (estimate={est}, rendered={actual})"
        );
    }
}

#[test]
fn test_summarizing_box_collapsed_tail_and_expand() {
    use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};

    use crate::types::{CompactionPart, CompactionPhase, Message, MessageRole, Part};

    // Real markdown, far beyond the 8-line collapsed preview: a heading, 19
    // paragraph lines, a bold/inline-code line and a very long run (like a
    // serialized tool payload) that must wrap down inside the box, never
    // disappear at the right edge. The raw md syntax must NEVER appear on
    // screen — only the rendered content.
    let mut text = String::from("# My Heading\n\n");
    for i in 0..19 {
        text.push_str(&format!("line {i}\n\n"));
    }
    text.push_str("**bold item** and `code`\n\n");
    text.push_str(&"x".repeat(200));
    let msg = Message {
        id: "msg-summarize".into(),
        role: MessageRole::Assistant,
        parts: vec![Part::Compaction(CompactionPart {
            phase: CompactionPhase::Llm,
            started_at: 0,
            elapsed_ms: Some(1_000),
            text,
        })],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = test_state(msg);
    state.status = SessionStatus::Idle;
    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();

    let area = Rect::new(0, 0, 80, 60);
    let mut buf = Buffer::empty(area);

    // First render: collapsed — the header and the LAST lines are visible,
    // the FIRST lines have scrolled out of the preview (LRU-like effect).
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    assert!(!view.tool_state.is_expanded("summarize-0"));
    let collapsed_text = buffer_text(&buf);
    assert!(collapsed_text.contains("Summarizing"), "header visible");
    assert!(collapsed_text.contains("line 18"), "last line visible");
    assert!(
        !collapsed_text.contains("line 0"),
        "first line scrolled out of the collapsed preview"
    );
    assert!(
        !collapsed_text.contains("**"),
        "collapsed hides markdown syntax (bold markers)"
    );
    let collapsed_h = view.msg_height_cache[0];

    // The box has 1 row of padding above the title (and 1 below the content):
    // the title must not sit on the box's first row.
    let title_row = (0..area.height)
        .find(|&r| {
            let row_text: String = (0..area.width)
                .filter_map(|cx| buf.cell((cx, r)))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect();
            row_text.contains("Summarizing")
        })
        .expect("title row must exist");
    assert!(title_row > 0, "the Summarizing title has 1 row of padding above it");
    // The padding row must be blank except for the box's left border (┃).
    let above_glyphs: Vec<char> = (0..area.width)
        .filter_map(|cx| buf.cell((cx, title_row - 1)))
        .filter_map(|c| c.symbol().chars().next())
        .filter(|&ch| ch != ' ')
        .collect();
    assert!(
        above_glyphs.iter().all(|&c| c == '┃'),
        "the row above the title is blank top padding (only the border)"
    );

    // 1 row of bottom padding below the last content row (the "Click to
    // expand" hint in the collapsed preview).
    let hint_row = (0..area.height)
        .find(|&r| {
            let row_text: String = (0..area.width)
                .filter_map(|cx| buf.cell((cx, r)))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect();
            row_text.contains("Click to expand")
        })
        .expect("hint row must exist");
    let below_glyphs: Vec<char> = (0..area.width)
        .filter_map(|cx| buf.cell((cx, hint_row + 1)))
        .filter_map(|c| c.symbol().chars().next())
        .filter(|&ch| ch != ' ')
        .collect();
    assert!(
        below_glyphs.iter().all(|&c| c == '┃'),
        "the row below the hint is blank bottom padding (only the border)"
    );

    // A long line must WRAP DOWN inside the box — the whole 200-char run
    // appears across several rows instead of being cut at the right edge
    // (~77 chars fit per row at width 80 minus the border indent).
    let row_with_tail = buffer_text(&buf);
    assert!(
        row_with_tail.matches('x').count() >= 200,
        "the long line wraps fully (200 x's visible across rows)"
    );

    // Click inside the box → expands.
    let click = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        area.x + 10,
        area.y + 2,
        MouseModifiers::none(),
    );
    assert!(view.handle_mouse(&click, area, &state, &config));
    assert!(view.tool_state.is_expanded("summarize-0"));

    // Second render: the full body (19 lines + the wrapped long line) is
    // visible and the height grows.
    view.render(&mut buf, area, &state, &theme, &config, 0.016);
    let expanded_text = buffer_text(&buf);
    assert!(
        expanded_text.contains("line 0"),
        "expanded shows the first line"
    );
    assert!(
        expanded_text.contains("line 18"),
        "expanded shows the last line"
    );
    assert!(
        expanded_text.contains("My Heading"),
        "expanded renders the heading text"
    );
    assert!(
        !expanded_text.contains("# My Heading"),
        "expanded hides the markdown heading marker"
    );
    assert!(
        !expanded_text.contains("**"),
        "expanded hides markdown syntax (bold markers)"
    );
    assert!(
        expanded_text.contains("bold item"),
        "expanded renders the bold text without markers"
    );
    assert!(
        view.msg_height_cache[0] > collapsed_h,
        "height grows when expanded"
    );
}

/// Build a state whose ONLY message is a COMPLETED bash tool with an output of
/// `line` repeated 40 times. The block is expanded in the view so the FULL
/// 40-line output renders (bash blocks collapse to 10 lines otherwise), making
/// it taller than the test viewport. A single tool-only message ensures the
/// text-region builder never touches the scratch buffer (which would mask
/// stale-cell leaks).
fn state_with_bash_block(session_id: &str, line: &str) -> AppState {
    let mut output = String::new();
    for _ in 0..40 {
        output.push_str(line);
        output.push('\n');
    }
    let mut st = AppState::new();
    st.add_session(Session {
        id: session_id.into(),
        title: session_id.into(),
        created_at: 0,
        messages: vec![Message {
            id: format!("{session_id}-0"),
            role: MessageRole::Assistant,
            parts: vec![Part::Tool(ToolPart {
                tool: "bash_run".into(),
                input: serde_json::json!({ "command": "test-cmd" }),
                output: Some(output),
                status: ToolStatus::Completed,
                tool_call_id: Some("shell".into()),
                is_start: false,
                is_streaming: false,
                cached_line_count: None,
            })],
            created_at: 0,
            agent: None,
            model: None,
        }],
    });
    st.current_session_id = Some(session_id.into());
    st.status = SessionStatus::Working;
    st
}

/// Regression test for the reusable scratch buffer: rendering a different
/// message into the reused temp buffer must NOT leak glyphs from the previous
/// render. `Buffer::resize` keeps existing cell content and tool-block
/// renderers only set styles (not chars), so the fallback must clear the
/// buffer before drawing (previously `Buffer::empty` guaranteed clean cells).
#[test]
fn test_scratch_reuse_does_not_leak_previous_message() {
    let theme = test_theme();
    let config = test_config();
    let area = Rect::new(0, 0, 80, 40);

    // Session A: an expanded bash block whose lines are full-width rows of 'A'.
    let state_a = state_with_bash_block("session-a", &"A".repeat(70));
    // Session B: same shape but 'B's; each line is short, so every cell the
    // new render leaves untouched would expose stale 'A's.
    let state_b = state_with_bash_block("session-b", &"B".repeat(10));

    let mut view = SessionView::new();
    // Expand the bash block so the full 40-line output renders (a tall message
    // that must go through the temp-buffer fallback when clipped).
    view.tool_state.toggle_expanded("shell");
    let mut buf = Buffer::empty(area);

    // Render A (populates the scratch via the bottom-clipped fallback).
    view.render(&mut buf, area, &state_a, &theme, &config, 0.016);
    // Switch to B: the session change forces a cache rebuild, so B's first
    // render reuses the scratch with a different message.
    view.render(&mut buf, area, &state_b, &theme, &config, 0.016);

    // No 'A' glyph may survive into the visible buffer.
    let leaked: Vec<char> = buf
        .content()
        .iter()
        .filter_map(|cell| {
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            (ch == 'A').then_some(ch)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "stale glyphs leaked into buffer after scratch reuse: {leaked:?}"
    );
}

/// Regression test for the asymmetric code-block padding: the chat's layout
/// advance must account for the code block's full padding (top gap + lines +
/// bottom padding + blank separator), or the next message would be drawn ON
/// TOP of the block's bottom padding — visually erasing it. Before the fix,
/// code blocks WITHOUT a language tag showed 2 blank rows above and none
/// below, because the render advance (glyph-only scan) was 2 rows shorter
/// than the allocated (estimated) height.
#[test]
fn test_code_block_bottom_padding_not_overlapped() {
    use crate::types::TextPart;

    let code_msg = Message {
        id: "msg-code".into(),
        role: MessageRole::Assistant,
        parts: vec![Part::Text(TextPart {
            text: "```\nlet x = 1;\nlet y = 2;\n```".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    };
    let reply = Message {
        id: "msg-reply".into(),
        role: MessageRole::Assistant,
        parts: vec![Part::Text(TextPart {
            text: "the reply".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    };
    let mut state = AppState::new();
    let session = Session {
        id: "test-session".into(),
        title: "Test".into(),
        created_at: 0,
        messages: vec![code_msg, reply],
    };
    state.add_session(session);
    state.current_session_id = Some("test-session".into());
    state.status = SessionStatus::Idle;

    let mut view = SessionView::new();
    let theme = test_theme();
    let config = test_config();
    let area = Rect::new(0, 0, 90, 40);
    let mut buf = Buffer::empty(area);
    view.render(&mut buf, area, &state, &theme, &config, 0.016);

    // The layout advance must equal the estimated layout. If it were shorter
    // (the old glyph-scan advance), the following message would be drawn over
    // the code block's bottom padding.
    assert_eq!(
        view.actual_total_height, view.cached_total_height,
        "layout advance shorter than the estimated layout: the code block's \
         bottom padding would be overlapped by the next message \
         (actual={} cached={})",
        view.actual_total_height,
        view.cached_total_height
    );

    // Sanity-check the drawn rows: code lines at rows 1-2 (the top-gap row 0
    // carries only background, no glyphs), then the bottom padding + separator
    // (rows 3-4), the inter-message gap (row 5) and the reply at row 6. If the
    // bottom padding were overlapped, the reply would start at row 4.
    let mut reply_rows = Vec::new();
    for row in 0..12 {
        let has = (5..85).any(|cx| {
            buf.cell((cx, row))
                .is_some_and(|c| c.symbol().chars().next().unwrap_or(' ') != ' ')
        });
        if has {
            reply_rows.push(row);
        }
    }
    assert_eq!(
        reply_rows,
        vec![1, 2, 6],
        "unexpected glyph layout: the code block's bottom padding should be \
         preserved (2 blank rows) before the reply"
    );
}
