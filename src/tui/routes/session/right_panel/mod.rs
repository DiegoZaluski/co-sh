use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};
use crate::util::text_region::{TextRegion, text_from_cell_row};

pub mod todo;
pub mod types;

use todo::{render_todo_section, todo_section_height};
use types::{
    PtySession, RightPanelState, SubagentActivityLine, SubagentBodyCache, activity_lines,
    sanitize_subagent_text, split_subagent_output, subagent_visible_body, wrap_chars,
};

use std::collections::HashMap;

use cosh_tools::subagent::events::{PlanEntryStatus, ToolCallStatus};

use crate::component::spinner_highlight::HighlightSpinner;

/// Black-or-white text color with readable contrast on `bg`.
fn contrast_fg(bg: RGBA) -> RGBA {
    let (r, g, b, _) = bg.to_ints();
    let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if lum > 128.0 {
        RGBA::from_ints(0, 0, 0, 255)
    } else {
        RGBA::from_ints(255, 255, 255, 255)
    }
}

/// Linear blend of two colors: `t = 0` returns `base`, `t = 1` returns
/// `accent`. Used to tint a subagent window's background with its agent
/// color while staying close enough to the theme to keep text readable.
fn blend(base: RGBA, accent: RGBA, t: f32) -> RGBA {
    let mix = |a: u8, b: u8| -> u8 { (f32::from(a) * (1.0 - t) + f32::from(b) * t).round() as u8 };
    let (ar, ag, ab, _) = base.to_ints();
    let (br, bg_, bb, _) = accent.to_ints();
    RGBA::from_ints(mix(ar, br), mix(ag, bg_), mix(ab, bb), 255)
}

/// Box-tint accent for a review severity (Phase 3b.1): the theme's
/// success/warning/error colors stand in for green/yellow/red.
fn severity_rgba(sev: cosh_tools::subagent::severity::Severity, theme: &Theme) -> RGBA {
    match sev {
        cosh_tools::subagent::severity::Severity::Green => theme.success,
        cosh_tools::subagent::severity::Severity::Yellow => theme.warning,
        cosh_tools::subagent::severity::Severity::Red => theme.error,
    }
}

/// Draw a highlighted label ("highlighter pen" chip): a colored background
/// pill with padded text, used for mode badges (bash live/history).
fn draw_chip(buf: &mut Buffer, x: u16, y: u16, label: &str, bg: RGBA) {
    let style = Style::default()
        .fg(rgba_color(contrast_fg(bg)))
        .bg(rgba_color(bg));
    let padded = format!(" {label} ");
    // Width must cover exactly the pill: an oversized bound would overflow
    // draw_text's internal `checked_add` and silently skip drawing.
    let w = padded.chars().count() as u16;
    draw_text(buf, &padded, x, y, w, style);
}

/// Panel width in columns.
pub const RIGHT_PANEL_WIDTH: u16 = 42;
/// Extra rows between consecutive section BANDS. Each section already
/// renders its own TOP_GAP above its box, so `0` here makes the visible gap
/// between two boxes exactly that 1 TOP_GAP row — the same rhythm as the
/// boxes' internal padding (TOP_PAD/BOTTOM_PAD), keeping the layout compact.
const SECTION_GAP: i32 = 0;
/// Margin kept BELOW the last section. Each section renders its own TOP_GAP
/// above its box, but nothing reserved the mirrored gap at the bottom: when
/// content grew, boxes ran flush against the panel's bottom edge. This
/// reserves that row (unpainted panel background) so the bottom margin is
/// respected exactly like the top one, regardless of content size. Not tied
/// to SECTION_GAP (which is 0): the bottom edge always keeps its 1-row gap.
const BOTTOM_MARGIN: i32 = 1;

/// Total byte budget for the cached rendered subagent bodies. Real sessions
/// hold 1-3 entries (a few MB); when the accumulated output of many sessions
/// exceeds this, the whole cache is dropped and simply re-rendered (rare).
const SUBAGENT_BODY_CACHE_BUDGET: usize = 16 * 1024 * 1024;

/// Stable key for the body cache across theme switches: the markdown palette
/// derives from the text fg and the box bg, so a live theme change must
/// invalidate the cached cells (which carry concrete colors).
fn subagent_theme_key(theme: &Theme) -> u64 {
    let (tr, tg, tb, _) = theme.text.to_ints();
    let (br, bg, bb, _) = theme.background_element.to_ints();
    u64::from_le_bytes([tr, tg, tb, br, bg, bb, 0, 0])
}

/// Determine whether the right panel should be visible based on terminal width and content.
pub fn should_show_right_panel(terminal_width: u16, state: &RightPanelState) -> bool {
    // Manual override first (Ctrl+P): the user's explicit hide wins over
    // content and terminal size.
    if state.user_hidden {
        return false;
    }
    if terminal_width < 100 {
        return false;
    }
    if state.todos.is_empty() && state.pty_sessions.is_empty() {
        return false;
    }
    true
}

/// Count visible sections and return a bitmask: todo, bash, subagent.
fn count_sections(state: &RightPanelState) -> (bool, bool, bool) {
    let has_todos = !state.todos.is_empty();
    let has_bash = state
        .pty_sessions
        .iter()
        .any(|p| !p.command.starts_with("subagent:"));
    let has_subagent = state
        .pty_sessions
        .iter()
        .any(|p| p.command.starts_with("subagent:"));
    (has_todos, has_bash, has_subagent)
}

/// Compute the natural height (in rows) that a section would occupy if unlimited.
/// Box framing overhead included in each section's natural height.
/// Mirrors the TOP_GAP / TOP_PAD / BOTTOM_PAD constants in each section.
const BOX_OVERHEAD: i32 = 1 + 1 + 1;

fn natural_section_height(
    state: &mut RightPanelState,
    inner_w: u16,
    kind: types::SectionKind,
) -> i32 {
    match kind {
        types::SectionKind::Todo => {
            if state.todos.is_empty() {
                0
            } else {
                todo_section_height(&state.todos, inner_w) as i32
            }
        }
        types::SectionKind::Bash => {
            // Wrap width must match the section renderer (its inner width =
            // max_w − LEFT_PAD) so heights agree with the drawn rows.
            let bash_wrap_w = inner_w.saturating_sub(1).max(1);
            state.bash_wrap_w = bash_wrap_w;
            let lines = state.bash_buffer(bash_wrap_w).len() as i32;
            if lines == 0 { 0 } else { BOX_OVERHEAD + lines }
        }
        types::SectionKind::Subagent => {
            // The wrap width must match the renderer's (section width minus
            // the left+right padding), otherwise the natural height would
            // disagree with the drawn body and the box would clip or overhang.
            let wrap_w = inner_w.saturating_sub(2);
            state.subagent_wrap_w = wrap_w;
            subagent_natural_height(state, wrap_w)
        }
    }
}

/// Natural height of the SUBAGENT section for the resolved window set:
/// sum of each visible session's content rows, plus the between-window
/// separator margins ([`RightPanelState::subagent_window_margins`] — none
/// after the last window), plus box overhead. Must be called AFTER
/// [`RightPanelState::resolve_visible_subagents`].
fn subagent_natural_height(state: &mut RightPanelState, wrap_w: u16) -> i32 {
    let rows = state.subagent_section_rows_for_display(wrap_w);
    let total: i32 = state
        .visible_subagents
        .iter()
        .map(|&i| i32::from(rows.get(i).copied().unwrap_or(1)))
        .sum();
    if total == 0 {
        0
    } else {
        BOX_OVERHEAD
            + total
            + types::RightPanelState::subagent_window_margins(state.visible_subagents.len())
    }
}

/// Render the right panel with section-based layout and dynamic space borrowing.
pub fn render_right_panel(
    buf: &mut Buffer,
    area: Rect,
    state: &mut RightPanelState,
    theme: &Theme,
    terminal_width: u16,
) {
    if !should_show_right_panel(terminal_width, state) {
        // The panel is hidden: drop the last frame's section bounds so mouse
        // events never target sections that are not on screen.
        state.clear_section_layouts();
        return;
    }

    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_panel.into()));
    bg_box.render_self(buf, area);

    let inner_x = area.x + 2;
    let inner_w = area.width.saturating_sub(4);
    let viewport_top = area.y as i32;
    let viewport_h = area.bottom() as i32 - viewport_top;

    state.visible_height = viewport_h;

    // ── Phase 0: resolve the visible subagent windows ────────────────
    // The subagent section owns its area and may BORROW leftover space
    // from the todo/bash areas (loans have no time guarantee — owners
    // reclaim whenever they need it). The budget below IS that space;
    // window selection ranks focused > pinned > running > finished so
    // the queue being navigated is never hidden by stale content.
    let wrap_w = inner_w.saturating_sub(2);
    let (has_todos, has_bash, has_subagent) = count_sections(state);
    {
        let fixed_nat = natural_section_height(state, inner_w, types::SectionKind::Todo)
            + natural_section_height(state, inner_w, types::SectionKind::Bash);
        let present = i32::from(has_todos) + i32::from(has_bash) + i32::from(has_subagent);
        let gaps = (present - 1).max(0) * SECTION_GAP;
        // The window fit counts CONTENT rows; the section box itself adds
        // BOX_OVERHEAD framing rows that must also come out of the budget,
        // otherwise the natural height can overflow the viewport by up to
        // the frame size and squeeze a neighbouring section.
        let frame = i32::from(has_subagent) * BOX_OVERHEAD;
        // BOTTOM_MARGIN keeps the panel's bottom gap out of the subagent
        // budget: without it a tall window list would grow back over the
        // reserved row in Phase 2.
        let budget =
            (viewport_h - gaps - fixed_nat - frame - BOTTOM_MARGIN).max(types::MIN_WINDOW_ROWS);
        state.subagent_wrap_w = wrap_w;
        if has_subagent {
            state.resolve_visible_subagents(wrap_w, budget);
        } else {
            state.visible_subagents.clear();
        }
    }

    // ── Phase 1: collect visible sections and their natural heights ──
    let sections_info = [
        (types::SectionKind::Todo, has_todos),
        (types::SectionKind::Bash, has_bash),
        (types::SectionKind::Subagent, has_subagent),
    ];

    let visible: Vec<(types::SectionKind, i32)> = sections_info
        .iter()
        .filter(|(_, visible)| *visible)
        .map(|(kind, _)| {
            let h = natural_section_height(state, inner_w, *kind);
            (*kind, h)
        })
        .collect();

    if visible.is_empty() {
        return;
    }

    // ── Phase 2: allocate space fairly (independent of render order) ──
    let count = visible.len() as i32;
    let gap_total = (count - 1) * SECTION_GAP;
    // The bottom margin is carved out of the layout space up front: every
    // section is capped to what fits ABOVE the reserved row, so the gap
    // survives even when sections are squeezed to their scrollable bands.
    let available = viewport_h - gap_total - BOTTOM_MARGIN;
    let base_h = available / count;

    let mut allocations = vec![0i32; visible.len()];
    let mut pool = 0i32;

    // First pass: each section gets min(natural, base_h); surplus goes to pool
    for (i, &(_, natural_h)) in visible.iter().enumerate() {
        if natural_h <= base_h {
            allocations[i] = natural_h;
            pool += base_h - natural_h;
        } else {
            allocations[i] = base_h;
        }
    }

    // Second pass: distribute pool to sections that still need more
    if pool > 0 {
        let needy: Vec<usize> = (0..visible.len())
            .filter(|&i| visible[i].1 > allocations[i])
            .collect();

        let per_needy = pool / needy.len().max(1) as i32;
        for &i in &needy {
            let deficit = visible[i].1 - allocations[i];
            let extra = per_needy.min(deficit);
            allocations[i] += extra;
            pool -= extra;
        }
        // Any remaining pool is redistributed deficit-capped across ALL
        // still-needy sections (a section must never be allocated beyond
        // its natural height — that reserved unpainted void inside the box,
        // the reported "subagent expands but occupies much less" bug).
        // Whatever does not fit any deficit stays as the panel's bottom
        // margin.
        if pool > 0 {
            let mut progressed = true;
            while pool > 0 && progressed {
                progressed = false;
                for &i in &needy {
                    if pool == 0 {
                        break;
                    }
                    let deficit = visible[i].1 - allocations[i];
                    if deficit > 0 {
                        let extra = pool.min(deficit);
                        allocations[i] += extra;
                        pool -= extra;
                        progressed = true;
                    }
                }
            }
        }
    }

    // ── Phase 3: render order by activity (most recent first) ──
    let mut render_order: Vec<usize> = (0..visible.len()).collect();
    render_order.sort_by(|&a, &b| {
        let a_act = state.section_activity(visible[a].0);
        let b_act = state.section_activity(visible[b].0);
        b_act.cmp(&a_act)
    });

    // ── Phase 4: render sections in activity order ──
    // Refresh the screen bounds used by mouse events (wheel/keyboard scroll
    // and drag selection) and decide whether the text regions (selection
    // extraction) must be rebuilt from the latest content.
    state.clear_section_layouts();
    let rebuild_regions =
        state.pty_gen != state.text_regions_gen || inner_w != state.text_regions_w;
    let mut cur_y = viewport_top;
    for (pos, &idx) in render_order.iter().enumerate() {
        let (kind, natural_h) = visible[idx];
        let allocated = allocations[idx].max(1) as u16;

        state.push_section_layout(kind, cur_y, cur_y + i32::from(allocated));

        match kind {
            types::SectionKind::Todo => {
                // Snapshot the strike frames before the scroll borrow: the
                // counters live on `state`, which is already borrowed mutably
                // for `todo_scroll_y` below.
                let strike_frames: Vec<Option<u64>> = (0..state.todos.len())
                    .map(|i| state.todo_strike_frame(i))
                    .collect();
                let scroll = if natural_h > allocated as i32 {
                    state.todo_scroll_y = state.todo_scroll_y.min(natural_h - allocated as i32);
                    Some(&mut state.todo_scroll_y)
                } else {
                    None
                };
                render_todo_section(
                    buf,
                    inner_x,
                    cur_y as u16,
                    inner_w,
                    allocated,
                    &state.todos,
                    &strike_frames,
                    theme,
                    scroll,
                );
            }
            types::SectionKind::Bash => {
                render_bash_section(
                    buf,
                    inner_x,
                    cur_y as u16,
                    inner_w,
                    allocated,
                    state,
                    theme,
                    rebuild_regions,
                );
            }
            types::SectionKind::Subagent => {
                render_subagent_section(
                    buf,
                    inner_x,
                    cur_y as u16,
                    inner_w,
                    allocated,
                    state,
                    theme,
                    rebuild_regions,
                );
            }
        }

        cur_y += allocated as i32;
        if pos < render_order.len() - 1 {
            cur_y += SECTION_GAP;
        }
    }
    if rebuild_regions {
        state.text_regions_gen = state.pty_gen;
        state.text_regions_w = inner_w;
    }
}

/// Render all bash PTYs as a single continuous text buffer with line-based scroll.
#[allow(clippy::too_many_arguments)]
fn render_bash_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
    rebuild_regions: bool,
) {
    const LEFT_PAD: u16 = 1;
    const TOP_GAP: u16 = 1;
    const TOP_PAD: u16 = 1;
    const BOTTOM_PAD: u16 = 1;

    // Wrap width = the section's inner width; the buffer breaks long lines
    // into visual rows so nothing disappears past the box edge.
    let wrap_w = max_w.saturating_sub(LEFT_PAD).max(1);
    let total_lines = state.bash_buffer(wrap_w).len() as i32;
    if total_lines == 0 {
        return;
    }

    let box_y = y + TOP_GAP;
    let box_h = max_h.saturating_sub(TOP_GAP);

    // Fill section background
    let mut bg = BoxRenderable::new();
    bg.set_background_color(Some(theme.background_element.into()));
    bg.render_self(buf, Rect::new(x, box_y, max_w, box_h));

    let inner_y = box_y + TOP_PAD;
    let inner_h = box_h.saturating_sub(TOP_PAD + BOTTOM_PAD);
    let inner_w = max_w.saturating_sub(LEFT_PAD);

    let has_scroll = total_lines > inner_h as i32;
    // Normalize the stored offset every render: `scroll_to_bottom()` leaves an
    // `i32::MAX` sentinel which is only clamped when content overflows. When
    // the content fits, the sentinel must be reset to 0, otherwise it leaks
    // into the selection content mapping and overflows the highlight math.
    let scroll_y = if has_scroll {
        state.bash_scroll_y = state.bash_scroll_y.min(total_lines - inner_h as i32);
        state.bash_scroll_y
    } else {
        state.bash_scroll_y = 0;
        0
    };

    // Rebuild the selection text regions when content or width changed.
    // `bash_buffer` borrows `&mut state`, so collect the lines first.
    if rebuild_regions {
        state.bash_text_regions.clear();
        let lines: Vec<String> = state.bash_buffer(wrap_w).to_vec();
        let text_max_w = max_w.saturating_sub(LEFT_PAD);
        for (i, line) in lines.iter().enumerate() {
            let truncated: String = line.chars().take(text_max_w as usize).collect();
            state.bash_text_regions.push(TextRegion::one_row(
                i as i32,
                x + LEFT_PAD,
                x + LEFT_PAD + text_max_w,
                truncated,
            ));
        }
    }

    let start_line = scroll_y as usize;
    let visible = inner_h as usize;
    let buffer = state.bash_buffer(wrap_w);

    for (i, line) in buffer.iter().skip(start_line).take(visible).enumerate() {
        let line_y = inner_y + i as u16;
        // Command lines ($ cmd) in normal text, output in muted
        let line_style = if line.starts_with("$ ") {
            Style::default().fg(rgba_color(theme.text))
        } else {
            Style::default().fg(rgba_color(theme.text_muted))
        };
        let truncated: String = line.chars().take(inner_w as usize).collect();
        draw_text(buf, &truncated, x + LEFT_PAD, line_y, inner_w, line_style);
    }

    // History-mode badge (top-left, on the box's padding row so it never
    // collides with content): which view the section is in — the newest
    // command only ("live") or ALL commands stacked ("history").
    {
        let (label, bg) = if state.bash_history_mode {
            ("history", theme.accent)
        } else {
            ("live", theme.success)
        };
        draw_chip(buf, x + LEFT_PAD, box_y, label, bg);
    }

    highlight_section_selection(
        buf,
        state,
        crate::routes::session::right_panel::types::SectionKind::Bash,
        SelectionBand {
            min_x: x + LEFT_PAD,
            max_x: x + LEFT_PAD + inner_w,
            inner_y,
            inner_h,
            scroll_y,
        },
    );
}

/// Render the resolved subagent windows as one scrollable markdown document.
///
/// The visible set comes from [`RightPanelState::visible_subagents`]
/// (dynamic windows: the focused queue's window first, then pinned
/// queues, then running, then finished — superseded entries return only
/// via ← navigation). Each session renders
/// as: the command header line (normal text), the optional "→ cosh:"
/// main-agent input line (normal text), then the subagent body as
/// **markdown** — the same renderer the chat uses for its content, so the
/// report's headings/code/lists survive inside the box. Scrolling is in
/// markdown layout rows (matching [`RightPanelState::subagent_section_rows`])
/// and the visible slice of each body is blitted from a reusable scratch
/// buffer, since the markdown renderer always lays out from content row 0.
///
/// The content reserves padding on BOTH sides (1 column each) so wrapped
/// markdown lines never touch the box's right edge (flush-right content read
/// as a leak past the box).
#[allow(clippy::too_many_arguments)]
fn render_subagent_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
    rebuild_regions: bool,
) {
    // Table borders and the old scrollbar used to sit on this side; with the
    // scrollbar gone, 1 column of clearance on each side keeps content off
    // the box edge.
    const LEFT_PAD: u16 = 1;
    const RIGHT_PAD: u16 = 1;
    const TOP_GAP: u16 = 1;
    const TOP_PAD: u16 = 1;
    const BOTTOM_PAD: u16 = 1;

    let box_y = y + TOP_GAP;
    let box_h = max_h.saturating_sub(TOP_GAP);

    // Fill section background
    let mut bg = BoxRenderable::new();
    bg.set_background_color(Some(theme.background_element.into()));
    bg.render_self(buf, Rect::new(x, box_y, max_w, box_h));

    let inner_y = box_y + TOP_PAD;
    let inner_h = box_h.saturating_sub(TOP_PAD + BOTTOM_PAD);
    // The wrap width must match `natural_section_height` exactly: table
    // borders and the scrollbar stay clear of the box's right edge.
    let wrap_w = max_w.saturating_sub(LEFT_PAD + RIGHT_PAD);
    state.subagent_wrap_w = wrap_w;
    if inner_h == 0 || wrap_w == 0 {
        return;
    }

    // Heights first (needs `&mut self`), then the sessions borrow: holding
    // the session references across the cache call would conflict.
    // `subagent_section_rows_for_display` also reloads any output spilled
    // to disk so heights and bodies see real content.
    let rows_all: Vec<u16> = state.subagent_section_rows_for_display(wrap_w).to_vec();
    // Window set resolved by the panel layout phase. Direct callers of this
    // renderer (unit tests) may skip resolution — fall back to every
    // subagent session so nothing silently disappears.
    let visible: Vec<usize> = if state.visible_subagents.is_empty() {
        state
            .pty_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_subagent())
            .map(|(i, _)| i)
            .collect()
    } else {
        state.visible_subagents.clone()
    };
    let total_rows: i32 = visible
        .iter()
        .map(|&i| i32::from(rows_all.get(i).copied().unwrap_or(1)))
        .sum::<i32>()
        + types::RightPanelState::subagent_window_margins(visible.len());
    if total_rows == 0 {
        return;
    }
    let sessions: Vec<(usize, &PtySession)> = visible
        .iter()
        .map(|&i| (i, &state.pty_sessions[i]))
        .collect();

    // Paint the box's TOP/BOTTOM padding rows with the NEAREST window's
    // area tint: the background hole around the first/last agent would
    // otherwise read as wasted space. Padding itself is kept (it frames
    // the internal subagent content).
    if let (Some(first), Some(last)) = (sessions.first(), sessions.last()) {
        for (pad_y, sess_idx) in [(box_y, first.0), (box_y + box_h.saturating_sub(1), last.0)] {
            if pad_y < box_y + box_h {
                // A review window's severity color wins over the agent
                // color (same rule as the window tints below); a padding
                // row must never read as belonging to a different verdict.
                let tint = if let Some(sev) = state.pty_sessions[sess_idx].severity {
                    blend(theme.background_element, severity_rgba(sev, theme), 0.18)
                } else if let Some(agent) = state.pty_sessions[sess_idx].subagent_agent() {
                    blend(
                        theme.background_element,
                        state.agent_area_color(agent),
                        0.18,
                    )
                } else {
                    continue;
                };
                let mut fill = BoxRenderable::new();
                fill.set_background_color(Some(tint.into()));
                fill.render_self(buf, Rect::new(x, pad_y, max_w, 1));
            }
        }
    }
    // ONE read-only pass builds each window's area tint — method calls
    // borrow all of `state`, so nothing of this shape may run inside the
    // render loop. A review window with a consumed severity header tints
    // green/yellow/red (Phase 3b.1) instead of its agent color. The accent
    // (the unblended color) doubles as the running tool-call spinner's
    // highlight color, so a red verdict window also sweeps red.
    let (tints, accents): (Vec<RGBA>, Vec<RGBA>) = sessions
        .iter()
        .map(|(_, s)| match s.severity {
            Some(sev) => {
                let accent = severity_rgba(sev, theme);
                (blend(theme.background_element, accent, 0.18), accent)
            }
            None => {
                let agent = s.subagent_agent().unwrap_or("");
                let accent = state.agent_area_color(agent);
                (blend(theme.background_element, accent, 0.18), accent)
            }
        })
        .unzip();

    // Screen bands of each rendered window (for click-to-focus mapping).
    state.subagent_window_layouts.clear();

    let has_scroll = total_rows > i32::from(inner_h);
    // Disjoint field borrows: the loop needs the reusable scratch (mutable)
    // and the scroll offset (mutable) at once.
    let subagent_scratch = &mut state.subagent_scratch;
    let subagent_scroll_y = &mut state.subagent_scroll_y;
    let subagent_spinners = &mut state.subagent_tool_spinners;
    // Normalize the stored offset every render (same rationale as the bash
    // section): the `i32::MAX` scroll-to-bottom sentinel must not survive a
    // render that fits the content, or it leaks into selection math.
    let scroll_y = if has_scroll {
        let max_scroll = total_rows - i32::from(inner_h);
        *subagent_scroll_y = (*subagent_scroll_y).min(max_scroll);
        *subagent_scroll_y
    } else {
        *subagent_scroll_y = 0;
        0
    };

    let text_style = Style::default().fg(rgba_color(theme.text));
    let theme_key = subagent_theme_key(theme);
    let content_bottom = scroll_y + i32::from(inner_h);
    let mut content_row: i32 = 0;
    // Spinner keys that are InProgress THIS frame — the map is pruned to
    // this set after the loop so finished calls stop animating.
    let mut live_spinner_keys: Vec<String> = Vec::new();

    for (window, (sess_idx, session)) in sessions.iter().enumerate() {
        let sess_rows = i32::from(rows_all.get(*sess_idx).copied().unwrap_or(1));
        let (input_line, body) = split_subagent_output(&session.output);
        // Header and input wrap like the rows math: split each into its
        // visual rows so the body starts right below the last one.
        let header_rows = wrap_chars(&session.command, wrap_w);
        let input_rows = input_line
            .map(|input| wrap_chars(input, wrap_w))
            .unwrap_or_default();
        let head_h = header_rows.len() as i32;
        let input_h = input_rows.len() as i32;
        let sess_end = content_row + sess_rows;
        // The 1-row separator margin exists only BELOW non-last windows —
        // the last window leans on the box's own bottom padding instead.
        let has_next_window = window + 1 < sessions.len();

        // Area tint of THIS window: fill its visible slice (clipped to the
        // section's inner band) with the blended color before any text or
        // body cells land on it. A following margin row shares the tint so
        // consecutive windows read as separated colored blocks.
        let tint = tints[window];
        {
            let vis_top = content_row.max(scroll_y);
            let tint_end = if has_next_window {
                sess_end + 1
            } else {
                sess_end
            };
            let vis_bottom = tint_end.min(content_bottom);
            if vis_bottom > vis_top && wrap_w > 0 {
                let rect = Rect::new(
                    x,
                    inner_y + (vis_top - scroll_y) as u16,
                    max_w,
                    (vis_bottom - vis_top) as u16,
                );
                let mut tint_bg = BoxRenderable::new();
                tint_bg.set_background_color(Some(tint.into()));
                tint_bg.render_self(buf, rect);
            }
        }

        // Screen band of this window (clipped to the section's inner area)
        // — recorded so a click can resolve WHICH agent queue to focus.
        {
            let band_top = inner_y as i32 + (content_row - scroll_y).max(0);
            let band_bottom = inner_y as i32 + i32::from(inner_h).min(sess_end - scroll_y);
            if band_bottom > band_top && band_top >= inner_y as i32 {
                state
                    .subagent_window_layouts
                    .push((*sess_idx, band_top, band_bottom));
            }
        }

        // Command header + main-agent input, wrapped; the same visual rows
        // feed the selection text regions (rebuilt only when content or
        // width changed).
        for (rows, row_offset) in [(&header_rows, 0), (&input_rows, head_h)] {
            draw_wrapped_rows(
                buf,
                x + LEFT_PAD,
                inner_y,
                rows,
                content_row + row_offset,
                scroll_y,
                content_bottom,
                wrap_w,
                text_style,
            );
            if rebuild_regions {
                push_region_rows(
                    &mut state.subagent_text_regions,
                    rows,
                    content_row + row_offset,
                    x + LEFT_PAD,
                    wrap_w,
                );
            }
        }
        // Live activity (Phase 3b.2): tool calls, plan, thought — the same
        // lines the height math counted for RUNNING sessions. Finished
        // windows have none (activity is cleared on completion), so this
        // contributes nothing for history entries.
        let activity = activity_lines(&session.subagent_activity, wrap_w);
        let activity_h = activity.len() as i32;
        let body_start = content_row + head_h + input_h + activity_h;
        draw_activity_lines(
            buf,
            x + LEFT_PAD,
            inner_y,
            &activity,
            content_row + head_h + input_h,
            scroll_y,
            content_bottom,
            wrap_w,
            tint,
            accents[window],
            theme,
            text_style,
            session.id.as_str(),
            subagent_spinners,
            &mut live_spinner_keys,
            rebuild_regions,
            &mut state.subagent_text_regions,
        );
        // Markdown body: blit the visible slice of the previously rendered
        // cells (cache hit), or re-render the full body into the scratch and
        // store the cells when the content, wrap width or theme changed. The
        // renderer lays out from content row 0, so the full body must be
        // rendered on a miss — but that happens once per streamed chunk, not
        // every frame (re-rendering the whole body per frame was the
        // right-panel frame bottleneck).
        // body_h derives from the (throttled, ~100 ms coalesced) row cache
        // minus the LIVE activity count: a new tool row arriving between
        // rebuilds can push body_h ≤ 0 for a frame — the guard skips the
        // body blit until the cache catches up. Self-healing by design;
        // do NOT clamp the body into the window here, the row math owns it.
        let body_h = sess_rows - head_h - input_h - activity_h;
        if body_h > 0 && body_start < content_bottom && sess_end > scroll_y {
            // The severity header is consumed (it tinted the window above)
            // and is never shown as report text. Failed windows keep the
            // neutral color (no severity was extracted) but still lose a
            // leading header line here — accepted: a header on a FAILED
            // run is a malformed report.
            let clean = sanitize_subagent_text(subagent_visible_body(body));
            if !clean.trim().is_empty() {
                let body_h_u = body_h as u16;
                let hit = state
                    .subagent_body_cache
                    .get(*sess_idx)
                    .and_then(|e| e.as_ref())
                    .is_some_and(|e| {
                        e.id == session.id
                            && e.layout_gen == state.subagent_layout_gen
                            && e.wrap_w == wrap_w
                            && e.h == body_h_u
                            && e.theme_key == theme_key
                            && e.bg == tint
                    });
                if !hit {
                    let area = Rect::new(0, 0, wrap_w, body_h_u);
                    let scratch = subagent_scratch.get_or_insert_with(|| Buffer::empty(area));
                    if *scratch.area() != area {
                        scratch.resize(area);
                    }
                    let mut md = MarkdownRenderable::new(Some(clean));
                    md.set_fg(Some(ColorInput::RGBA(theme.text)));
                    // The body is rendered ON the window's area tint so the
                    // blitted cells keep the agent's color underneath.
                    md.set_bg(Some(ColorInput::RGBA(tint)));
                    crate::util::markdown::apply_theme(&mut md, theme);
                    md.render_self(scratch, area);

                    // Keep the rendered cells for the next frames (row-major).
                    let mut cells = Vec::with_capacity(wrap_w as usize * body_h_u as usize);
                    for dy in 0..body_h_u {
                        for dx in 0..wrap_w {
                            cells.push(scratch.cell((dx, dy)).cloned().unwrap_or_default());
                        }
                    }
                    // Bound the total cache: on overflow, drop everything
                    // (re-render is correct, just rare).
                    let bytes = cells.len() * std::mem::size_of::<ratatui::buffer::Cell>();
                    let total: usize = state
                        .subagent_body_cache
                        .iter()
                        .flatten()
                        .map(|e| e.cells.len() * std::mem::size_of::<ratatui::buffer::Cell>())
                        .sum::<usize>()
                        .saturating_add(bytes);
                    if total > SUBAGENT_BODY_CACHE_BUDGET {
                        state.subagent_body_cache.clear();
                    }
                    while state.subagent_body_cache.len() <= *sess_idx {
                        state.subagent_body_cache.push(None);
                    }
                    state.subagent_body_cache[*sess_idx] = Some(SubagentBodyCache {
                        id: session.id.clone(),
                        layout_gen: state.subagent_layout_gen,
                        wrap_w,
                        h: body_h_u,
                        theme_key,
                        bg: tint,
                        cells,
                    });
                }

                // Blit the visible slice from the cached cells.
                let cached = state.subagent_body_cache[*sess_idx].as_ref().unwrap();
                let first_vis = scroll_y.max(body_start);
                let last_vis = content_bottom.min(sess_end);
                for cr in first_vis..last_vis {
                    let body_row = (cr - body_start) as u16;
                    let dst_y = inner_y + (cr - scroll_y) as u16;
                    let base = body_row as usize * wrap_w as usize;
                    for dx in 0..wrap_w {
                        if let Some(dst) = buf.cell_mut((x + LEFT_PAD + dx, dst_y)) {
                            *dst = cached.cells[base + dx as usize].clone();
                        }
                    }
                }

                // Selection text regions: body rows from the same cached cells
                // that were blitted, so extraction matches the display.
                if rebuild_regions {
                    for body_row in 0..body_h_u {
                        let base = body_row as usize * wrap_w as usize;
                        let trimmed = text_from_cell_row(&cached.cells[base..], wrap_w as usize);
                        state.subagent_text_regions.push(TextRegion::one_row(
                            body_start + i32::from(body_row),
                            x + LEFT_PAD,
                            x + LEFT_PAD + wrap_w,
                            trimmed,
                        ));
                    }
                }
            }
        }
        // Next window starts below THIS window's 1-row separator margin.
        content_row = sess_end + i32::from(has_next_window);
    }

    // Prune spinner entries that left the InProgress state this frame (the
    // call finished or its window completed): finished calls must not keep
    // animating state alive across turns.
    let live: std::collections::HashSet<&str> =
        live_spinner_keys.iter().map(String::as_str).collect();
    subagent_spinners.retain(|key, _| live.contains(key.as_str()));

    highlight_section_selection(
        buf,
        state,
        crate::routes::session::right_panel::types::SectionKind::Subagent,
        SelectionBand {
            min_x: x + LEFT_PAD,
            max_x: x + LEFT_PAD + wrap_w,
            inner_y,
            inner_h,
            scroll_y,
        },
    );
}

/// Draw the live activity lines of one running sub-agent window (Phase
/// 3b.2), starting at content row `start_row`, clipped to the visible band.
///
/// Rendering per variant (design decisions from the user):
/// - Tool: NO icons — the lowercase tool name (`read`, `edit`, `search`,
///   …) followed by the extracted detail (path/query/URL). InProgress calls
///   render through the luminous-sweep [`HighlightSpinner`] (highlight =
///   window accent, so a red-verdict window sweeps red); the other
///   statuses draw a plain ✓/✗/· marker colored success/error/text.
/// - Diff: one dimmed `path +N −M` line under the call.
/// - Plan: compact checkbox glyphs (☐ pending/in-progress, ☑ completed —
///   the outer TODO panel's row style is deliberately NOT reused: it takes
///   too much width inside the box).
/// - Thought: dimmed (text_muted) wrapped rows of the latest thought, the
///   same "opaque thinking" treatment as the main agent's reasoning but
///   always visible (no hidden state).
///
/// Selection text regions are appended for drawn rows when
/// `rebuild_regions`. Spinner map entries are created/updated keyed by
/// `{session_id}:{call_id}`; `live_keys` collects the InProgress keys so
/// the caller can prune finished entries after the window loop.
#[allow(clippy::too_many_arguments)]
fn draw_activity_lines(
    buf: &mut Buffer,
    x: u16,
    inner_y: u16,
    lines: &[SubagentActivityLine],
    start_row: i32,
    scroll_y: i32,
    content_bottom: i32,
    wrap_w: u16,
    _tint: RGBA,
    accent: RGBA,
    theme: &Theme,
    text_style: Style,
    session_id: &str,
    spinners: &mut HashMap<String, HighlightSpinner>,
    live_keys: &mut Vec<String>,
    rebuild_regions: bool,
    regions: &mut Vec<TextRegion>,
) {
    let marker_style_ok = Style::default().fg(rgba_color(theme.success));
    let marker_style_err = Style::default().fg(rgba_color(theme.error));
    let dimmed_style = Style::default().fg(rgba_color(theme.text_muted));

    for (offset, line) in lines.iter().enumerate() {
        let row = start_row + offset as i32;
        // Spinner liveness is tracked BEFORE the clip check: an off-screen
        // running call keeps its animation entry, so scrolling it back in
        // does not restart the sweep from phase 0.
        if let SubagentActivityLine::Tool {
            id,
            status: ToolCallStatus::InProgress,
            ..
        } = line
        {
            live_keys.push(format!("{session_id}:{id}"));
        }
        if row < scroll_y || row >= content_bottom {
            continue;
        }
        let line_y = inner_y + (row - scroll_y) as u16;
        match line {
            SubagentActivityLine::Tool { id, text, status } => {
                let key = format!("{session_id}:{id}");
                match status {
                    ToolCallStatus::InProgress => {
                        // Create the spinner on first sight of this call;
                        // afterwards only keep its text in sync (titles do
                        // not change while running, but stay defensive).
                        let spinner = spinners.entry(key).or_insert_with(|| {
                            HighlightSpinner::new(text, accent, theme.text_muted)
                        });
                        if spinner.text() != text {
                            spinner.set_text(text);
                        }
                        spinner.set_colors(accent, theme.text_muted);
                        spinner.render(buf, x, line_y);
                        let shown = text.chars().count().min(wrap_w as usize);
                        if rebuild_regions {
                            regions.push(TextRegion::one_row(
                                row,
                                x,
                                x + shown as u16,
                                text.clone(),
                            ));
                        }
                    }
                    ToolCallStatus::Completed => {
                        draw_text(buf, "✓", x, line_y, wrap_w, marker_style_ok);
                        draw_text(
                            buf,
                            text,
                            x + 2,
                            line_y,
                            wrap_w.saturating_sub(2),
                            text_style,
                        );
                        if rebuild_regions {
                            regions.push(TextRegion::one_row(
                                row,
                                x,
                                x + wrap_w,
                                format!("✓ {text}"),
                            ));
                        }
                    }
                    ToolCallStatus::Failed => {
                        draw_text(buf, "✗", x, line_y, wrap_w, marker_style_err);
                        draw_text(
                            buf,
                            text,
                            x + 2,
                            line_y,
                            wrap_w.saturating_sub(2),
                            text_style,
                        );
                        if rebuild_regions {
                            regions.push(TextRegion::one_row(
                                row,
                                x,
                                x + wrap_w,
                                format!("✗ {text}"),
                            ));
                        }
                    }
                    // Pending / Unknown: dimmed dot, not yet running.
                    _ => {
                        draw_text(buf, "·", x, line_y, wrap_w, dimmed_style);
                        draw_text(
                            buf,
                            text,
                            x + 2,
                            line_y,
                            wrap_w.saturating_sub(2),
                            dimmed_style,
                        );
                        if rebuild_regions {
                            regions.push(TextRegion::one_row(
                                row,
                                x,
                                x + wrap_w,
                                format!("· {text}"),
                            ));
                        }
                    }
                }
            }
            SubagentActivityLine::Diff(summary) => {
                // Indent under its tool line; counts stay dimmed so the
                // path dominates.
                draw_text(
                    buf,
                    summary,
                    x + 2,
                    line_y,
                    wrap_w.saturating_sub(2),
                    dimmed_style,
                );
                if rebuild_regions {
                    regions.push(TextRegion::one_row(row, x, x + wrap_w, summary.clone()));
                }
            }
            SubagentActivityLine::Plan { status, text } => {
                let (glyph, style) = match status {
                    PlanEntryStatus::Completed => ("☑", marker_style_ok),
                    PlanEntryStatus::InProgress => ("☐", text_style),
                    PlanEntryStatus::Pending => ("☐", dimmed_style),
                    _ => ("☐", dimmed_style),
                };
                draw_text(buf, glyph, x, line_y, wrap_w, style);
                draw_text(
                    buf,
                    text,
                    x + 2,
                    line_y,
                    wrap_w.saturating_sub(2),
                    text_style,
                );
                if rebuild_regions {
                    regions.push(TextRegion::one_row(
                        row,
                        x,
                        x + wrap_w,
                        format!("{glyph} {text}"),
                    ));
                }
            }
            SubagentActivityLine::Message(text) => {
                // Normal text style — the agent speaking, not thinking.
                draw_text(buf, text, x, line_y, wrap_w, text_style);
                if rebuild_regions {
                    regions.push(TextRegion::one_row(row, x, x + wrap_w, text.clone()));
                }
            }
            SubagentActivityLine::Thought(text) => {
                // Always visible, dimmed — the sub-agent's "opaque thinking"
                // treatment (no hidden toggle here).
                draw_text(buf, text, x, line_y, wrap_w, dimmed_style);
                if rebuild_regions {
                    regions.push(TextRegion::one_row(row, x, x + wrap_w, text.clone()));
                }
            }
        }
    }
}

/// Draw a wrapped logical line as visual rows starting at content row
/// `start_row`, clipping to the section's visible band.
#[allow(clippy::too_many_arguments)]
fn draw_wrapped_rows(
    buf: &mut Buffer,
    x: u16,
    inner_y: u16,
    rows: &[String],
    start_row: i32,
    scroll_y: i32,
    content_bottom: i32,
    wrap_w: u16,
    style: Style,
) {
    for (offset, seg) in rows.iter().enumerate() {
        let row = start_row + offset as i32;
        if row >= scroll_y && row < content_bottom {
            let line_y = inner_y + (row - scroll_y) as u16;
            draw_text(buf, seg, x, line_y, wrap_w, style);
        }
    }
}

/// Append one selection text region per wrapped row of a logical line.
fn push_region_rows(
    regions: &mut Vec<TextRegion>,
    rows: &[String],
    start_row: i32,
    x: u16,
    wrap_w: u16,
) {
    for (offset, seg) in rows.iter().enumerate() {
        let y = start_row + offset as i32;
        regions.push(TextRegion::one_row(y, x, x + wrap_w, seg.clone()));
    }
}

/// Simple text drawing helper (filters ASCII control chars to prevent ratatui panics).
/// Uses `checked_add` to avoid u16 overflow when computing character positions.
fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().filter(|c| !c.is_ascii_control()).enumerate() {
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// The on-screen band a section's drag-selection may paint into: the
/// section's content x-range, its visible y-band, and the panel scroll
/// offset used to map content rows back to screen rows.
struct SelectionBand {
    min_x: u16,
    max_x: u16,
    inner_y: u16,
    inner_h: u16,
    scroll_y: i32,
}

/// Invert fg/bg of the cells inside a section's drag-selection rectangle.
///
/// Mirrors the chat's flow-based highlight: the anchor/focus are stored in
/// CONTENT rows (so the highlight follows content during drag auto-scroll),
/// converted back to screen rows with the current scroll offset and clamped
/// to the section's visible content band. Rows between the band edges are
/// selected full-width, the first/last row only from the drag x positions.
/// `kind` must be the section being rendered: content rows are per-section,
/// so only the section that OWNS the selection may draw it.
fn highlight_section_selection(
    buf: &mut Buffer,
    state: &RightPanelState,
    kind: crate::routes::session::right_panel::types::SectionKind,
    band: SelectionBand,
) {
    let Some((anchor_x, _anchor_sy, focus_x, _focus_sy)) = state.drag_selection else {
        return;
    };
    // Only the OWNING section may paint the highlight: content rows are
    // per-section coordinates, so re-interpreting them here would paint a
    // mirrored selection into the other section's box.
    if state.selection_section != Some(kind) || band.inner_h == 0 {
        return;
    }

    let content_min_x = band.min_x;
    let content_max_x = band.max_x;
    let content_top = i32::from(band.inner_y);
    let band_bottom = content_top + i32::from(band.inner_h) - 1;
    // Saturating: content rows can transiently carry the `i32::MAX`
    // scroll-to-bottom sentinel (e.g. a click racing a render that clamps
    // it), which would overflow a plain `- scroll_y + content_top`.
    let anchor_screen_y = state
        .selection_anchor_content_y
        .saturating_sub(band.scroll_y)
        .saturating_add(content_top)
        .clamp(content_top, band_bottom) as u16;
    let focus_screen_y = state
        .selection_focus_content_y
        .saturating_sub(band.scroll_y)
        .saturating_add(content_top)
        .clamp(content_top, band_bottom) as u16;

    let start_y = anchor_screen_y.min(focus_screen_y);
    let end_y = anchor_screen_y.max(focus_screen_y);
    // Direction and row bands are matched against CONTENT rows, not the
    // clamped screen rows (mirrors the chat highlight): an endpoint whose row
    // scrolled OUT of the section band is clamped to the edge for the
    // iteration bounds, but that edge row is then a MIDDLE row of the
    // selection and must be highlighted full-width. Matching clamped screen
    // rows instead froze the initial mouse-down x on the first/last visible
    // row for the whole drag.
    let anchor_content_y = state.selection_anchor_content_y;
    let focus_content_y = state.selection_focus_content_y;
    let (top_x, bottom_x) = if anchor_content_y <= focus_content_y {
        (anchor_x, focus_x)
    } else {
        (focus_x, anchor_x)
    };
    let top_content_y = anchor_content_y.min(focus_content_y);
    let bottom_content_y = anchor_content_y.max(focus_content_y);

    for cy in start_y..=end_y {
        // Content row under this screen row. Rows outside the true content
        // span (selection fully clamped off-screen) are not part of the
        // selection — skip them.
        let row_content_y = i32::from(cy)
            .saturating_sub(content_top)
            .saturating_add(band.scroll_y);
        if row_content_y < top_content_y || row_content_y > bottom_content_y {
            continue;
        }
        let (lx1, lx2) = if row_content_y == top_content_y && row_content_y == bottom_content_y {
            (top_x.min(bottom_x), top_x.max(bottom_x))
        } else if row_content_y == top_content_y {
            (top_x, content_max_x)
        } else if row_content_y == bottom_content_y {
            (content_min_x, bottom_x)
        } else {
            (content_min_x, content_max_x)
        };

        let lx1 = lx1.max(content_min_x);
        let lx2 = lx2.min(content_max_x);
        if lx1 > lx2 {
            continue;
        }

        for cx in lx1..=lx2 {
            if let Some(cell) = buf.cell_mut((cx, cy)) {
                let fg = cell.fg;
                let bg = cell.bg;
                cell.set_fg(bg);
                cell.set_bg(fg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn test_theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    /// Read a row of the buffer back as a string (for assertions).
    fn row_text(buf: &Buffer, y: u16) -> String {
        let w = buf.area().width;
        let mut s = String::new();
        for x in 0..w {
            if let Some(c) = buf.cell((x, y)) {
                s.push(c.symbol().chars().next().unwrap_or(' '));
            }
        }
        s
    }

    /// The subagent body is rendered as markdown (like the chat content):
    /// heading text appears without the `#` markers, `**` emphasis is
    /// consumed by the renderer, and the header + input lines survive as
    /// plain text.
    #[test]
    fn subagent_section_renders_markdown_body() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(
            "→ cosh: review this\n# Title\n\nSome **bold** text here.\n".to_string(),
        );
        let theme = test_theme();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));

        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        let all: String = (0..30)
            .map(|y| row_text(&buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("subagent: opencode"), "command header missing");
        assert!(all.contains("→ cosh: review this"), "input line missing");
        assert!(all.contains("Title"), "heading text missing: {all:?}");
        assert!(
            !all.contains("# Title"),
            "raw heading marker leaked: {all:?}"
        );
        assert!(!all.contains("**"), "raw emphasis markers leaked: {all:?}");
    }

    /// The bash section draws a mode chip on the box's padding row: "live"
    /// by default, "history" after toggling — English only, colored with
    /// theme colors (bg differs from the panel background).
    #[test]
    fn bash_section_draws_mode_chip() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.start_pty("echo one".to_string(), None);
        state.complete_last_pty("one".to_string());

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 12));
        render_bash_section(&mut buf, 0, 0, 50, 12, &mut state, &theme, false);

        let pad_row = row_text(&buf, 1); // box_y = TOP_GAP = 1
        assert!(pad_row.contains(" live "), "live chip missing: {pad_row:?}");
        assert!(!pad_row.contains("history"));

        state.panel_focus = Some(types::PanelFocus::Bash);
        state.panel_left(); // → history mode
        buf = Buffer::empty(Rect::new(0, 0, 50, 12));
        render_bash_section(&mut buf, 0, 0, 50, 12, &mut state, &theme, false);
        let pad_row = row_text(&buf, 1);
        assert!(
            pad_row.contains(" history "),
            "history chip missing: {pad_row:?}"
        );
    }

    /// A long subagent input line wraps onto multiple visual rows inside
    /// its window instead of being cut at the box edge.
    #[test]
    fn subagent_input_line_wraps() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.start_pty("subagent: kilo".to_string(), None);
        let tail = "word ".repeat(30); // 150 chars
        state.update_last_pty(format!("→ cosh: {tail}\n"));

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        // The input spans rows below the header; its LAST wrapped segment
        // must be present somewhere in the buffer.
        let all: String = (0..30)
            .map(|y| row_text(&buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("→ cosh:"), "input start missing");
        assert!(
            all.contains("word"),
            "wrapped input segments missing: {all:?}"
        );
        // Rows math agrees: header(1) + input(⌈151/48⌉=4) + 0 body ≥ 5.
        assert_eq!(state.subagent_section_rows(48), &[5]);
    }

    /// REGRESSION: every subagent window has its OWN 1-row bottom margin —
    /// a blank separator row between consecutive windows (and after the
    /// last one) instead of stacking them flush.
    #[test]
    fn subagent_windows_have_one_row_bottom_margin() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        for cmd in ["subagent: kilo", "subagent: opencode"] {
            state.start_pty(cmd.to_string(), None);
            state.complete_last_pty("report line\n".to_string());
        }

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        let mut bands = state.subagent_window_layouts.clone();
        bands.sort_by_key(|&(_, top, _)| top);
        for pair in bands.windows(2) {
            let gap = pair[1].1 - pair[0].2; // next.top - prev.bottom
            assert_eq!(
                gap, 1,
                "exactly one margin row between windows: bands {bands:?}"
            );
            // The margin shares the window's area tint (blank text only).
            let want = rgba_color(blend(
                theme.background_element,
                state.agent_area_color(state.pty_sessions[pair[0].0].subagent_agent().unwrap()),
                0.18,
            ));
            let margin_cell = buf.cell((1u16, pair[0].2 as u16)).map(|c| c.bg);
            assert_eq!(margin_cell, Some(want), "margin must carry the area tint");
            let margin = row_text(&buf, pair[0].2 as u16);
            assert!(
                margin.trim().is_empty(),
                "margin row must be blank: {margin:?}"
            );
        }

        // REGRESSION: a LONE (== last) window gets NO margin — the box's
        // own bottom padding frames it, so nothing is tinted right below
        // its content.
        let mut solo = RightPanelState::new();
        solo.subagent_rebuild_interval = std::time::Duration::ZERO;
        solo.start_pty("subagent: kilo".to_string(), None);
        solo.complete_last_pty("report line\n".to_string());
        let mut sbuf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut sbuf, 0, 0, 50, 30, &mut solo, &theme, true);
        assert_eq!(solo.subagent_window_layouts.len(), 1);
        let (_, _, solo_bottom) = solo.subagent_window_layouts[0];
        let plain = rgba_color(theme.background_element);
        assert_eq!(
            sbuf.cell((1u16, solo_bottom as u16)).map(|c| c.bg),
            Some(plain),
            "no margin row below the last displayed window"
        );
    }

    /// Each rendered subagent window carries its OWN area tint: external
    /// CLIs get distinct blended backgrounds and the INTERNAL subagent
    /// (empty agent name) shares the host color.
    #[test]
    fn subagent_windows_render_per_agent_area_tints() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        for cmd in ["subagent: kilo", "subagent: opencode", "subagent: "] {
            state.start_pty(cmd.to_string(), None);
            state.complete_last_pty("report line\n".to_string());
        }

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        // One content row pair per session (header + 1 body row), stacked
        // in chronological order from inner_y = TOP_GAP + TOP_PAD = 2,
        // separated by the 1-row margin between consecutive windows.
        let expected: Vec<ratatui::style::Color> = ["kilo", "opencode", ""]
            .iter()
            .map(|a| {
                rgba_color(blend(
                    theme.background_element,
                    state.agent_area_color(a),
                    0.18,
                ))
            })
            .collect();
        let mut seen: Vec<ratatui::style::Color> = Vec::new();
        for (row, want) in [3usize, 6, 9].iter().zip(expected.iter()) {
            let got = buf.cell((1u16, *row as u16)).map(|c| c.bg);
            assert_eq!(got, Some(*want), "row {row} bg must be the area tint");
            assert!(!seen.contains(&got.unwrap()), "tints must differ per CLI");
            seen.push(got.unwrap());
        }
    }

    /// REGRESSION (phantom fence row): documents ending in / containing
    /// code fences must not reserve the fence's trailing blank feed row —
    /// estimate and real painted layout must agree exactly.
    #[test]
    fn fence_body_has_no_phantom_trailing_row() {
        use cosh_tui::core::renderable::Renderable;
        use cosh_tui::core::renderables::markdown::estimate_height;

        let long = "a-b_c/d".repeat(16);
        let body = format!("```\n{long}\n```\n");
        for w in [30u16, 36, 50] {
            let est = usize::from(estimate_height(&body, w));
            let mut buf = Buffer::empty(Rect::new(0, 0, w, est as u16 + 2));
            let md = MarkdownRenderable::new(Some(body.clone()));
            md.render_self(&mut buf, Rect::new(0, 0, w, est as u16));
            let painted = (0..est)
                .filter(|&y| {
                    (0..w).any(|x| buf.cell((x, y as u16)).is_some_and(|c| c.symbol() != " "))
                })
                .count();
            assert_eq!(
                est, painted,
                "estimate must match painted layout for fenced body at w={w}"
            );
        }
    }

    /// PROPERTY (deterministic fuzz): no markdown document may make a
    /// subagent window reserve rows it never paints.
    #[test]
    fn fuzz_markdown_documents_do_not_reserve_unpainted_rows() {
        let theme = test_theme();
        let blocks: &[&str] = &[
            "# Heading\n",
            "## Sub heading\n",
            "Plain paragraph with words.\n",
            "Long paragraph: {}\n",
            "- bullet one\n- bullet two\n",
            "1. first\n2. second\n",
            "- [ ] task open\n- [x] task done\n",
            "```rust\nfn main() {{ println!(\"x\"); }}\n```\n",
            "```\nraw fenced line {}\n```\n",
            "| c1 | c2 |\n|---|---|\n| a | b |\n",
            "> quote line\n> quote two\n",
            "---\n",
            "***\n",
            "Setext\n======\n",
            "inline `code span` and *emph* and **strong**\n",
            "https://example.com/very/long/path/segment/that/wraps\n",
            "emoji \u{1F680}\u{389} and accents \u{E1}\u{E9}\u{ED}\u{F3}\u{FA} and CJK \u{6C49}\u{5B57}\u{6D4B}\u{8BD5}\n",
            "\n\n\n",
            "   indented three spaces\n",
            "\ttab indented\n",
            "> - list inside quote\n> - second\n",
        ];
        let words = [
            "word",
            "supercalifragilistic",
            "\u{6C49}\u{5B57}\u{6D4B}",
            "a-b_c/d",
        ];
        let mut s: u64 = 0x1234_5678;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for case in 0..300u32 {
            let nblocks = 1 + (next() % 5) as usize;
            let mut doc = String::from("\u{2192} cosh: task\n");
            for _ in 0..nblocks {
                let b = blocks[(next() as usize) % blocks.len()];
                let filler =
                    words[(next() as usize) % words.len()].repeat((3 + next() % 40) as usize);
                doc.push_str(&b.replace("{}", &filler));
                if next() % 3 == 0 {
                    doc.push('\n');
                }
            }
            let mut state = RightPanelState::new();
            state.subagent_rebuild_interval = std::time::Duration::ZERO;
            state.start_pty("subagent: fuzz".to_string(), None);
            state.update_last_pty(doc.clone());
            state.complete_last_pty(doc.clone());
            let wrap_w = 48u16; // = render max_w 50 − left/right pads
            let rows: Vec<u16> = state.subagent_section_rows(wrap_w).to_vec();
            let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
            let mut buf = Buffer::empty(Rect::new(0, 0, 50, natural));
            render_subagent_section(&mut buf, 0, 0, 50, natural, &mut state, &theme, false);
            // Painted bottom must reach the last content row: slack is only
            // the window's own BOTTOM_PAD (one row).
            let inner_bot = natural as usize - 1;
            let mut last_paint = 2usize;
            for y in 2..inner_bot {
                let t = row_text(&buf, y as u16);
                let cut = t
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= 48)
                    .last()
                    .unwrap_or(t.len());
                if !t[..cut].trim_end().is_empty() {
                    last_paint = y;
                }
            }
            let slack = inner_bot as i32 - 1 - last_paint as i32;
            assert!(
                slack <= 0,
                "case {case} reserved {slack} unpainted rows; doc:\n{doc}"
            );
        }
    }

    /// width. The box is [0, 50), so column 49 (the box edge) must stay blank
    /// — content may reach column 48 (the last inner column).
    #[test]
    fn subagent_section_content_does_not_reach_box_right_edge() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(
            "```\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n```\n"
                .to_string(),
        );
        let theme = test_theme();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 12));

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme, true);

        for y in 0..12 {
            if let Some(c) = buf.cell((49, y)) {
                assert_eq!(
                    c.symbol().chars().next().unwrap_or(' '),
                    ' ',
                    "column 49 (box edge) must stay blank on row {y}"
                );
            }
        }
    }

    /// The rendered body cells are cached per session: after the first
    /// render the entry matches the layout generation and wrap width, and a
    /// second render (no content change) serves the same cells — the blit
    /// must produce a byte-identical buffer to the fresh render.
    #[test]
    fn subagent_body_cache_is_populated_and_served() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty("# Title\n\nSome **bold** body.\n".to_string());
        let theme = test_theme();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 20));

        render_subagent_section(&mut buf, 0, 0, 50, 20, &mut state, &theme, true);

        let entry = state
            .subagent_body_cache
            .first()
            .and_then(|e| e.as_ref())
            .expect("body cache entry after first render");
        let (id, layout_gen, wrap_w, h) =
            (entry.id.clone(), entry.layout_gen, entry.wrap_w, entry.h);
        assert_eq!(id, "pty-1");
        assert_eq!(layout_gen, state.subagent_layout_gen);
        assert_eq!(wrap_w, 50 - 2, "box width minus left+right padding");
        assert_eq!(h, entry.cells.len() as u16 / wrap_w);

        let mut buf2 = Buffer::empty(Rect::new(0, 0, 50, 20));
        render_subagent_section(&mut buf2, 0, 0, 50, 20, &mut state, &theme, true);

        let served = state
            .subagent_body_cache
            .first()
            .and_then(|e| e.as_ref())
            .expect("body cache entry after second render");
        assert_eq!(served.layout_gen, layout_gen, "cache served, not rebuilt");
        assert_eq!(buf, buf2, "cached-cell blit must match the fresh render");
    }

    /// A table's border glyphs (which read as a hard "edge") stay clear of
    /// the box's right edge too — the border of a wide table never lands on
    /// column 49 of a [0, 50) box.
    #[test]
    fn subagent_section_table_stays_inside_box() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(
            "| col one | col two | col three | col four |\n|---|---|---|---|\n| a | b | c | d |\n"
                .to_string(),
        );
        let theme = test_theme();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 12));

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme, true);

        for y in 0..12 {
            if let Some(c) = buf.cell((49, y)) {
                assert_eq!(
                    c.symbol().chars().next().unwrap_or(' '),
                    ' ',
                    "table border must not reach column 49 on row {y}"
                );
            }
        }
    }

    /// Scrolling is in markdown layout rows: with the `scroll_to_bottom`
    /// sentinel (i32::MAX) the render clamps to the last rows, so the start
    /// of the report is clipped and the end is visible.
    #[test]
    fn subagent_section_scrolls_in_markdown_rows() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        // ~30 markdown rows at the box width.
        let body = (0..30)
            .map(|i| format!("Line {i} of a long report\n"))
            .collect::<String>();
        state.update_last_pty(body);
        state.subagent_scroll_y = i32::MAX; // sentinel from scroll_to_bottom()
        let theme = test_theme();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 12));

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme, true);

        let all: String = (0..12)
            .map(|y| row_text(&buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !all.contains("Line 0"),
            "start of the report clipped when scrolled to bottom"
        );
        assert!(
            all.contains("Line 29"),
            "end of the report visible at the bottom"
        );
        // The sentinel was clamped to a real row offset.
        assert!(state.subagent_scroll_y < i32::MAX);
    }

    /// Regression: a drag anchored in the SUBAGENT box must highlight ONLY
    /// the subagent rows — the bash renderer used to re-interpret the same
    /// per-section content rows with its own scroll offset, painting a
    /// mirrored selection inside the bash box.
    #[test]
    fn drag_in_subagent_never_highlights_bash_box() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;

        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("report line one\nreport line two\n".to_string());
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("f1\nf2\nf3\n".to_string());

        // Baseline render (no selection) — also populates section_layouts.
        let mut base = Buffer::empty(Rect::new(0, 0, 50, 40));
        render_right_panel(&mut base, Rect::new(0, 0, 50, 40), &mut state, &theme, 120);

        let bash_band = state
            .section_layouts
            .iter()
            .find(|l| l.kind == types::SectionKind::Bash)
            .copied()
            .expect("bash section on screen");
        let sub_band = state
            .section_layouts
            .iter()
            .find(|l| l.kind == types::SectionKind::Subagent)
            .copied()
            .expect("subagent section on screen");

        // Drag INSIDE the subagent box (anchor → focus both in its band).
        let y1 = u16::try_from(sub_band.top + 2).unwrap();
        let y2 = u16::try_from(sub_band.bottom - 2).unwrap();
        assert!(state.begin_selection(3, y1), "subagent drag must anchor");
        state.update_drag_selection(30, y2);

        let mut sel = Buffer::empty(Rect::new(0, 0, 50, 40));
        render_right_panel(&mut sel, Rect::new(0, 0, 50, 40), &mut state, &theme, 120);

        // The bash band must be pixel-identical to the baseline.
        for y in bash_band.top..bash_band.bottom {
            for x in 0u16..50 {
                assert_eq!(
                    base.cell((x, y as u16))
                        .map(|c| (c.symbol().to_string(), c.fg, c.bg)),
                    sel.cell((x, y as u16))
                        .map(|c| (c.symbol().to_string(), c.fg, c.bg)),
                    "bash row {y} changed under a subagent-only drag"
                );
            }
        }
        // Sanity: the subagent band DID get highlighted somewhere.
        let changed = (sub_band.top..sub_band.bottom).any(|y| {
            (0u16..50).any(|x| {
                base.cell((x, y as u16)).map(|c| (c.fg, c.bg))
                    != sel.cell((x, y as u16)).map(|c| (c.fg, c.bg))
            })
        });
        assert!(changed, "subagent drag must highlight the subagent box");
    }

    /// Regression: an upward drag that passes the section's top edge
    /// auto-scrolls, leaving the anchor content row BELOW the visible band
    /// and the focus row ABOVE it. Both endpoints clamp onto the band edge
    /// rows for the iteration bounds, but those edge rows are then MIDDLE
    /// rows of the selection and must be highlighted full-width. The old
    /// code matched bands against the clamped screen rows, so the last
    /// visible row kept the partial band from the initial mouse-down x.
    #[test]
    fn drag_past_band_edges_highlights_edge_rows_full_width() {
        let theme = test_theme();
        let mut state = RightPanelState::new();

        // Long, wide output lines so every visible content row carries text
        // out to the band's right edge (truncated at inner_w).
        let output: String = (0..80)
            .map(|i| {
                format!("data line {i:02} 0123456789012345678901234567890123456789012345678\n")
            })
            .collect();
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty(output);

        // Baseline render (no selection) — also populates section_layouts.
        let mut base = Buffer::empty(Rect::new(0, 0, 50, 60));
        render_right_panel(&mut base, Rect::new(0, 0, 50, 60), &mut state, &theme, 120);

        let band = state
            .section_layouts
            .iter()
            .find(|l| l.kind == types::SectionKind::Bash)
            .copied()
            .expect("bash section on screen");

        // Drag upward past the band's top edge: the anchor content row is far
        // below the visible band, the focus row far above it. Both clamp onto
        // the band edge rows.
        state.selection_section = Some(types::SectionKind::Bash);
        state.drag_selection = Some((
            6,
            u16::try_from(band.bottom - 1).unwrap(),
            30,
            u16::try_from(band.content_top).unwrap(),
        ));
        state.selection_anchor_content_y = 200;
        state.selection_focus_content_y = -50;

        let mut sel = Buffer::empty(Rect::new(0, 0, 50, 60));
        render_right_panel(&mut sel, Rect::new(0, 0, 50, 60), &mut state, &theme, 120);

        // Content columns: inner_x(2) + LEFT_PAD(1) .. + inner_w(46) - 1.
        // Text is truncated to exactly this width, so every content row
        // carries fg!=bg cells across the whole range.
        const TEXT_X0: u16 = 3;
        const TEXT_X1: u16 = 48;

        // Within the text columns, every cell must be either untouched
        // (outside the highlight band) or exactly fg/bg-swapped (inside it),
        // and each row must be CONSISTENT: a row with any swapped cell must
        // have ALL its fg!=bg cells swapped. The old code left the tail of
        // the clamped edge row in its initial formatting (mixed row).
        let mut swapped_rows = 0usize;
        let mut first_row_swapped = false;
        let mut last_row_swapped = false;
        for y in band.top..band.bottom {
            let mut swapped = 0usize;
            let mut unchanged = 0usize;
            for x in TEXT_X0..=TEXT_X1 {
                let (b, s) = match (base.cell((x, y as u16)), sel.cell((x, y as u16))) {
                    (Some(b), Some(s)) => (b, s),
                    _ => panic!("missing cell at ({x}, {y})"),
                };
                assert_eq!(
                    b.symbol(),
                    s.symbol(),
                    "row {y} col {x}: symbol changed by selection highlight"
                );
                if b.fg == b.bg {
                    // fg/bg swap is a no-op here; nothing to assert.
                    continue;
                }
                if s.fg == b.fg && s.bg == b.bg {
                    unchanged += 1;
                } else if s.fg == b.bg && s.bg == b.fg {
                    swapped += 1;
                } else {
                    panic!(
                        "row {y} col {x}: cell neither unchanged nor inverted \
                         (b.fg={:?} b.bg={:?}, s.fg={:?} s.bg={:?})",
                        b.fg, b.bg, s.fg, s.bg
                    );
                }
            }
            assert!(
                swapped == 0 || unchanged == 0,
                "row {y}: mixed row — {swapped} inverted and {unchanged} unchanged cells \
                 (edge rows must be full-width)"
            );
            if swapped > 0 {
                swapped_rows += 1;
                if y == band.content_top {
                    first_row_swapped = true;
                }
                if y == band.bottom - 2 {
                    last_row_swapped = true;
                }
            }
        }
        // The FIRST and LAST content rows are the clamped edge rows of this
        // selection — both are middle rows of the true content span and must
        // be highlighted (the old bug froze the initial mouse-down x there).
        assert!(
            first_row_swapped,
            "top content row (row {}) must be highlighted",
            band.content_top
        );
        assert!(
            last_row_swapped,
            "bottom content row (row {}) must be highlighted",
            band.bottom - 2
        );
        // Guard against a vacuous pass: the band must actually contain
        // highlighted content rows.
        assert!(
            swapped_rows >= 3,
            "expected several highlighted content rows, got {swapped_rows}"
        );
    }

    /// Regression: a bash PTY appearing while a subagent is present must not
    /// panic. Exercises the full panel render (layout + region rebuild +
    /// highlight) plus the cursor-targeted scroll and drag-selection paths
    /// against a section that just appeared.
    #[test]
    fn bash_appearing_alongside_subagent_does_not_panic() {
        let theme = test_theme();
        let mut state = RightPanelState::new();

        // Subagent first (streaming markdown body).
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty("# Report\n\nSome body text.\n".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, 30), &mut state, &theme, 120);

        // The bash `ls` runs: a new bash PTY appears alongside the subagent.
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty(
            "total 8\ndrwxr-xr-x 2 inky inky 4096 Aug 13 16:40 src\ndrwxr-xr-x 2 inky inky 4096 Aug 13 16:40 target\n"
                .to_string(),
        );
        buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, 30), &mut state, &theme, 120);

        // Cursor-targeted scroll over the bash section.
        state.scroll_down_at(25, 3);
        state.scroll_up_at(25, 3);

        // Drag a selection inside the bash section (anchor then spill past the
        // section edges), extract it, and let auto-scroll run a frame.
        state.begin_selection(2, 25);
        state.update_drag_selection(40, 5);
        let _text = state.extract_selected_text();
        state.cancel_selection();

        // Re-render after the interaction.
        buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, 30), &mut state, &theme, 120);
    }

    /// Regression: the `i32::MAX` scroll-to-bottom sentinel must not leak into
    /// the drag-selection content mapping and overflow when a bash section
    /// whose content fits (no scrollbar) is clicked right after a
    /// `scroll_to_bottom()`.
    #[test]
    fn selection_after_scroll_to_bottom_on_short_content_does_not_overflow() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("file_a\nfile_b\n".to_string());

        // scroll_to_bottom() leaves the i32::MAX sentinel; the render clamps it
        // only when the content overflows. With short content the render must
        // normalize it to 0 so it never leaks into the selection math.
        state.scroll_to_bottom();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, 30), &mut state, &theme, 120);
        assert_eq!(
            state.bash_scroll_y, 0,
            "sentinel normalized when content fits"
        );

        // Click inside the bash section → begin_selection reads the scroll.
        assert!(state.begin_selection(2, 4));
        // Rendering with an active selection exercises the highlight path.
        buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, 30), &mut state, &theme, 120);
    }

    /// REGRESSION: the subagent box must NEVER be taller than its content.
    /// Phase-2's leftover pool used to be dumped uncapped into the first
    /// needy section — when bash/todo fit their base share and their
    /// surplus exceeded the subagent's deficit, the subagent box grew past
    /// its natural height leaving a large unpainted void. Numbers below
    /// trigger exactly that path.
    #[test]
    fn subagent_section_never_allocated_beyond_content() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.text_regions_w = 38;

        // Bash: 3 buffer lines -> natural 6 (3 overhead + 3).
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("f1\nf2\n".to_string());
        // Subagent: header + input + 8 body lines -> natural 13.
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty(
            "\u{2192} cosh: task\n".to_string()
                + &(0..8)
                    .map(|i| format!("kilo line {i}\n"))
                    .collect::<String>(),
        );

        // H=26: available=25, base=12. Bash fits (pool += 6); sub deficit
        // is 1 -- the old dump pushed the leftover 5 rows into its box.
        let h = 26u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, h));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, h), &mut state, &theme, 120);

        let sub = state
            .section_layouts
            .iter()
            .find(|l| l.kind == types::SectionKind::Subagent)
            .expect("subagent section rendered");
        let band_h = sub.bottom - sub.top;
        assert_eq!(
            band_h, 13,
            "section height must equal its natural height (3 overhead + 10 content; a lone window carries no margin)"
        );

        // Painted check inside the band: only TOP_GAP+TOP_PAD above and the
        // single BOTTOM_PAD row may be blank (a lone window has no margin).
        let mut last_paint = sub.top;
        for y in sub.content_top..sub.bottom {
            let t = row_text(&buf, y as u16);
            if t[..38.min(t.len())].trim_end() != "" {
                last_paint = y;
            }
        }
        assert!(
            last_paint >= sub.bottom - 2,
            "unpainted void of {} rows inside the subagent box",
            sub.bottom - 1 - last_paint
        );
    }

    /// REGRESSION: the panel's BOTTOM margin must be respected even when
    /// the content overflows the viewport. Sections carry their own TOP_GAP
    /// above each box, but nothing reserved the mirrored gap below the last
    /// section: boxes grew flush against the panel's bottom edge. The layout
    /// now carves the margin row out of Phase 2's space and Phase 0's
    /// subagent budget, so the last section's band always ends at least one
    /// row above the viewport bottom.
    #[test]
    fn panel_respects_bottom_margin_when_content_overflows() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.text_regions_w = 38;

        // Todos + bash + subagent, all taller than the viewport.
        state.set_todos(
            (0..20)
                .map(|i| types::TodoItem {
                    status: "pending".to_string(),
                    content: format!("todo item {i} with a longish description to wrap"),
                })
                .collect(),
        );
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty(
            (0..40)
                .map(|i| format!("bash line {i}\n"))
                .collect::<String>(),
        );
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty(
            (0..40)
                .map(|i| format!("kilo line {i}\n"))
                .collect::<String>(),
        );

        // Viewport small enough that every section overflows and each gets
        // squeezed to its scrollable band.
        let h = 30u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, h));
        render_right_panel(&mut buf, Rect::new(0, 0, 50, h), &mut state, &theme, 120);

        // The lowest section band must stop at least BOTTOM_MARGIN (1) rows
        // above the viewport bottom — the reserved gap row.
        let lowest_bottom = state
            .section_layouts
            .iter()
            .map(|l| l.bottom)
            .max()
            .expect("sections rendered");
        assert!(
            lowest_bottom < i32::from(h),
            "last section must respect the bottom margin: band bottom {lowest_bottom} vs viewport {h}"
        );

        // And the reserved row must actually be unpainted panel background:
        // no section box fills it with its element color.
        let panel_bg = rgba_color(theme.background_panel);
        for x in 0u16..50 {
            assert_eq!(
                buf.cell((x, h - 1)).map(|c| c.bg),
                Some(panel_bg),
                "row {h}-1 must stay bare panel background (bottom margin), col {x}"
            );
        }

        // The VISIBLE gap between adjacent section boxes is exactly 1 row:
        // with SECTION_GAP = 0 the next band starts where the previous one
        // ends (band gap 0), and its own TOP_GAP row is the single blank row
        // between the two boxes — the same rhythm as the internal padding.
        let mut bands: Vec<(i32, i32)> = state
            .section_layouts
            .iter()
            .map(|l| (l.top, l.bottom))
            .collect();
        bands.sort_unstable();
        for pair in bands.windows(2) {
            let band_gap = pair[1].0 - pair[0].1; // next.top - prev.bottom
            assert_eq!(
                band_gap, SECTION_GAP,
                "bands must tile back-to-back (SECTION_GAP = 0): {bands:?}"
            );
            // The single separator row between the boxes is the next
            // section's TOP_GAP row: it must carry the bare panel background
            // (no box paints it), proving the visible gap is exactly 1 row.
            let sep_row = pair[0].1 as u16;
            for x in 0u16..50 {
                assert_eq!(
                    buf.cell((x, sep_row)).map(|c| c.bg),
                    Some(panel_bg),
                    "separator row {sep_row} between boxes must be bare panel background, col {x}"
                );
            }
        }
    }

    /// REGRESSION: the subagent box's TOP/BOTTOM padding rows are painted
    /// with the NEAREST window's area tint — no background-colored hole
    /// above the first agent or below the last one.
    #[test]
    fn subagent_box_padding_rows_use_nearest_agent_tint() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.text_regions_w = 38;
        for cmd in ["subagent: kilo", "subagent: opencode"] {
            state.start_pty(cmd.to_string(), None);
            state.complete_last_pty("report line\n".to_string());
        }
        // Direct renderer call with a known geometry.
        let max_h = 12u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, max_h));
        render_subagent_section(&mut buf, 0, 0, 50, max_h, &mut state, &theme, false);

        let box_y = 1u16; // TOP_GAP
        let box_h = max_h; // max_h already excludes TOP_GAP at caller level? renderer re-adds... use actual fills:
        let _ = box_h;
        let top_bg = buf.cell((1u16, box_y)).map(|c| c.bg);
        let bottom_bg = buf.cell((1u16, max_h - 1)).map(|c| c.bg);
        let first_tint = blend(
            theme.background_element,
            state.agent_area_color("kilo"),
            0.18,
        );
        let last_tint = blend(
            theme.background_element,
            state.agent_area_color("opencode"),
            0.18,
        );
        assert_eq!(
            top_bg,
            Some(rgba_color(first_tint)),
            "top pad = first window tint"
        );
        assert_eq!(
            bottom_bg,
            Some(rgba_color(last_tint)),
            "bottom pad = last window tint"
        );
        // And they differ from each other (two distinct agents).
        assert_ne!(top_bg, bottom_bg);
    }

    /// The palette is highlighter-cheerful: every entry is LIGHT (high
    /// per-channel brightness) so tints read as bright marca-texto hues.
    #[test]
    fn agent_palette_is_vivid_and_light() {
        for (r, g, b) in types::palette_entries() {
            let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
            assert!(
                lum > 170.0,
                "palette entry ({r},{g},{b}) too dark (lum {lum})"
            );
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            assert!(mx - mn > 40, "palette entry ({r},{g},{b}) looks gray/dead");
        }
    }
}
