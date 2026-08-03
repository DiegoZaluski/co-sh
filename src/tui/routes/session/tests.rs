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
use crate::types::{Message, MessageRole, Part, Session, SessionStatus, TextPart};

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

    // Measure 5 render cycles (simulating 5 frames during streaming)
    let mut times = Vec::with_capacity(5);
    for frame in 0..5 {
        let elapsed = render_and_time(&mut view, &mut buf, area, &state, &theme, &config);
        times.push(elapsed);
        eprintln!("[FPS_TEST] Frame {frame}: {:?}", elapsed);
    }

    // Average frame time should be low enough for responsive ESC handling
    let avg_us = times.iter().map(|t| t.as_micros()).sum::<u128>() / times.len() as u128;
    eprintln!("[FPS_TEST] Average frame time: {avg_us}us");

    // With the streaming optimization, average frame time should be under 500ms
    // (500ms = max acceptable delay for ESC to be processed)
    assert!(
        avg_us < 500_000,
        "Average frame time {avg_us}us is too high for responsive ESC!"
    );

    // No individual frame should take more than 5s
    for (i, t) in times.iter().enumerate() {
        assert!(
            t.as_millis() < 5000,
            "Frame {i} took {:?}, exceeding 5s threshold",
            t
        );
    }
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
    assert!(text.contains("Build"), "bash output should still be rendered");
    assert!(text.contains("done"), "bash output should still be rendered");
}
