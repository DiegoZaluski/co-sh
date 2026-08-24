use std::time::Instant;

use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;

use crate::theme::Theme;
use crate::util::text_region::TextRegion;

pub mod todo;
pub mod types;

use todo::{render_todo_section, todo_section_height};
use types::{
    PtySession, RightPanelState, SubagentBodyCache, sanitize_subagent_text, split_subagent_output,
    wrap_chars,
};

pub fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

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

/// Draw a highlighted label ("highlighter pen" chip): a colored background
/// pill with padded text, used for mode badges and queue counters.
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
/// Gap (in rows) between consecutive sections.
const SECTION_GAP: i32 = 1;

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
/// sum of each visible session's content rows plus box overhead. Must be
/// called AFTER [`RightPanelState::resolve_visible_subagents`].
fn subagent_natural_height(state: &mut RightPanelState, wrap_w: u16) -> i32 {
    let rows = state.subagent_section_rows_for_display(wrap_w);
    let total: i32 = state
        .visible_subagents
        .iter()
        .map(|&i| i32::from(rows.get(i).copied().unwrap_or(1)))
        .sum();
    if total == 0 { 0 } else { BOX_OVERHEAD + total }
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
    // window selection ranks pinned > running > finished so active work
    // is never hidden by stale content.
    let wrap_w = inner_w.saturating_sub(2);
    {
        let (has_todos, has_bash, has_subagent) = count_sections(state);
        let fixed_nat = natural_section_height(state, inner_w, types::SectionKind::Todo)
            + natural_section_height(state, inner_w, types::SectionKind::Bash);
        let present = i32::from(has_todos) + i32::from(has_bash) + i32::from(has_subagent);
        let gaps = (present - 1).max(0) * SECTION_GAP;
        // The window fit counts CONTENT rows; the section box itself adds
        // BOX_OVERHEAD framing rows that must also come out of the budget,
        // otherwise the natural height can overflow the viewport by up to
        // the frame size and squeeze a neighbouring section.
        let frame = i32::from(has_subagent) * BOX_OVERHEAD;
        let budget = (viewport_h - gaps - fixed_nat - frame).max(types::MIN_WINDOW_ROWS);
        state.subagent_wrap_w = wrap_w;
        if has_subagent {
            state.resolve_visible_subagents(wrap_w, budget);
        } else {
            state.visible_subagents.clear();
        }
    }

    // ── Phase 1: collect visible sections and their natural heights ──
    let sections_info = [
        (types::SectionKind::Todo, count_sections(state).0),
        (types::SectionKind::Bash, count_sections(state).1),
        (types::SectionKind::Subagent, count_sections(state).2),
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
    let available = viewport_h - gap_total;
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
            state.bash_text_regions.push(TextRegion {
                y1: i as i32,
                y2: i as i32 + 1,
                x1: x + LEFT_PAD,
                x2: x + LEFT_PAD + text_max_w,
                text: truncated,
            });
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
        x + LEFT_PAD,
        x + LEFT_PAD + inner_w,
        inner_y,
        inner_h,
        scroll_y,
    );
}

/// Render the resolved subagent windows as one scrollable markdown document.
///
/// The visible set comes from [`RightPanelState::visible_subagents`]
/// (dynamic windows: pinned queues first, then running, then finished —
/// superseded entries return only via ← navigation). Each session renders
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
        .sum();
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
            if pad_y < box_y + box_h
                && let Some(agent) = state.pty_sessions[sess_idx].subagent_agent()
            {
                let tint = blend(
                    theme.background_element,
                    state.agent_area_color(agent),
                    0.18,
                );
                let mut fill = BoxRenderable::new();
                fill.set_background_color(Some(tint.into()));
                fill.render_self(buf, Rect::new(x, pad_y, max_w, 1));
            }
        }
    }
    // ONE read-only pass builds every per-window decoration input (queue
    // counter, nav recency, area tint) — method calls borrow all of
    // `state`, so nothing of this shape may run inside the render loop.
    struct WindowDecor {
        counter: Option<(usize, usize)>,
        last_nav: Option<Instant>,
        tint: RGBA,
    }
    let mut decor: Vec<WindowDecor> = Vec::with_capacity(sessions.len());
    for (_, s) in &sessions {
        let agent = s.subagent_agent().unwrap_or("");
        let counter = state.queue_nav_index(agent).and_then(|idx| {
            state
                .queue_is_pinned(agent)
                .then(|| (idx + 1, state.queue_len(agent)))
        });
        decor.push(WindowDecor {
            counter,
            last_nav: state.queue_last_nav(agent),
            tint: blend(
                theme.background_element,
                state.agent_area_color(agent),
                0.18,
            ),
        });
    }
    // The chips share ONE top-left slot (the section box's padding row), so
    // only the MOST RECENTLY navigated pinned window may draw its counter —
    // otherwise two pinned queues would overwrite each other's chips.
    let counter_window: Option<usize> = decor
        .iter()
        .enumerate()
        .filter(|(_, d)| d.counter.is_some())
        .filter_map(|(w, d)| d.last_nav.map(|t| (t, w)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, w)| w);

    // Screen bands of each rendered window (for click-to-focus mapping).
    state.subagent_window_layouts.clear();

    let has_scroll = total_rows > i32::from(inner_h);
    // Disjoint field borrows: the loop needs the reusable scratch (mutable)
    // and the scroll offset (mutable) at once.
    let subagent_scratch = &mut state.subagent_scratch;
    let subagent_scroll_y = &mut state.subagent_scroll_y;
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
        let body_start = content_row + head_h + input_h;
        let sess_end = content_row + sess_rows;

        // Area tint of THIS window: fill its visible slice (clipped to the
        // section's inner band) with the blended color before any text or
        // body cells land on it.
        let tint = decor[window].tint;
        {
            let vis_top = content_row.max(scroll_y);
            let vis_bottom = sess_end.min(content_bottom);
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

        // Queue counter as two glued chips on the box's padding row
        // (top-left of the section): [index][total], drawn only by the
        // most recently navigated pinned window (they share one slot).
        // Hidden at the newest entry — that is live, nothing to show.
        if Some(window) == counter_window
            && let Some((k, n)) = decor[window].counter
            && k < n
        {
            draw_chip(buf, x + LEFT_PAD, box_y, &k.to_string(), theme.primary);
            let idx_w = k.to_string().chars().count() as u16 + 2; // padded
            let total_x = (x + LEFT_PAD + idx_w).min(x + LEFT_PAD + wrap_w);
            draw_chip(buf, total_x, box_y, &n.to_string(), theme.text_muted);
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
        // Markdown body: blit the visible slice of the previously rendered
        // cells (cache hit), or re-render the full body into the scratch and
        // store the cells when the content, wrap width or theme changed. The
        // renderer lays out from content row 0, so the full body must be
        // rendered on a miss — but that happens once per streamed chunk, not
        // every frame (re-rendering the whole body per frame was the
        // right-panel frame bottleneck).
        let body_h = sess_rows - head_h - input_h;
        if body_h > 0 && body_start < content_bottom && sess_end > scroll_y {
            let clean = sanitize_subagent_text(body);
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
                        let mut line_text = String::with_capacity(wrap_w as usize);
                        for dx in 0..wrap_w {
                            line_text.push(
                                cached.cells[base + dx as usize]
                                    .symbol()
                                    .chars()
                                    .next()
                                    .unwrap_or(' '),
                            );
                        }
                        let trimmed = line_text.trim_end().to_string();
                        state.subagent_text_regions.push(TextRegion {
                            y1: body_start + i32::from(body_row),
                            y2: body_start + i32::from(body_row) + 1,
                            x1: x + LEFT_PAD,
                            x2: x + LEFT_PAD + wrap_w,
                            text: trimmed,
                        });
                    }
                }
            }
        }
        content_row = sess_end;
    }

    highlight_section_selection(
        buf,
        state,
        x + LEFT_PAD,
        x + LEFT_PAD + wrap_w,
        inner_y,
        inner_h,
        scroll_y,
    );
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
        regions.push(TextRegion {
            y1: y,
            y2: y + 1,
            x1: x,
            x2: x + wrap_w,
            text: seg.clone(),
        });
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

/// Invert fg/bg of the cells inside a section's drag-selection rectangle.
///
/// Mirrors the chat's flow-based highlight: the anchor/focus are stored in
/// CONTENT rows (so the highlight follows content during drag auto-scroll),
/// converted back to screen rows with the current scroll offset and clamped
/// to the section's visible content band. Rows between the band edges are
/// selected full-width, the first/last row only from the drag x positions.
fn highlight_section_selection(
    buf: &mut Buffer,
    state: &RightPanelState,
    content_min_x: u16,
    content_max_x: u16,
    inner_y: u16,
    inner_h: u16,
    scroll_y: i32,
) {
    let Some((anchor_x, _anchor_sy, focus_x, _focus_sy)) = state.drag_selection else {
        return;
    };
    if state.selection_section.is_none() || inner_h == 0 {
        return;
    }

    let content_top = i32::from(inner_y);
    let band_bottom = content_top + i32::from(inner_h) - 1;
    // Saturating: content rows can transiently carry the `i32::MAX`
    // scroll-to-bottom sentinel (e.g. a click racing a render that clamps
    // it), which would overflow a plain `- scroll_y + content_top`.
    let anchor_screen_y = state
        .selection_anchor_content_y
        .saturating_sub(scroll_y)
        .saturating_add(content_top)
        .clamp(content_top, band_bottom) as u16;
    let focus_screen_y = state
        .selection_focus_content_y
        .saturating_sub(scroll_y)
        .saturating_add(content_top)
        .clamp(content_top, band_bottom) as u16;

    let start_y = anchor_screen_y.min(focus_screen_y);
    let end_y = anchor_screen_y.max(focus_screen_y);
    let (start_x, end_x) = if anchor_screen_y == start_y {
        (anchor_x, focus_x)
    } else {
        (focus_x, anchor_x)
    };

    for cy in start_y..=end_y {
        let (lx1, lx2) = if start_y == end_y {
            (start_x.min(end_x), start_x.max(end_x))
        } else if cy == start_y {
            (start_x, content_max_x)
        } else if cy == end_y {
            (content_min_x, end_x)
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

    /// The subagent window counter renders as glued [index][total] chips at
    /// the top-left while navigating, and disappears entirely when the
    /// selection is back at the newest entry (live view).
    #[test]
    fn subagent_counter_chips_show_only_when_navigating() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        for i in 0..3 {
            state.start_pty("subagent: kilo".to_string(), None);
            state.complete_last_pty(format!("report {i}\n"));
        }
        state.subagent_rebuild_interval = std::time::Duration::ZERO;

        // Live view: no chips.
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 20));
        render_subagent_section(&mut buf, 0, 0, 50, 20, &mut state, &theme, true);
        let all: String = (0..20).map(|y| row_text(&buf, y)).collect();
        assert!(!all.contains("(2/"), "legacy indicator removed");
        assert!(
            !row_text(&buf, 1).contains(" 2 "),
            "no counter at live view"
        );

        // Navigate to the first entry → [1][3] chips on the padding row.
        state.panel_focus = Some(types::PanelFocus::Agent("kilo".to_string()));
        state.panel_left();
        state.set_queue_index("kilo", 0);
        buf = Buffer::empty(Rect::new(0, 0, 50, 20));
        render_subagent_section(&mut buf, 0, 0, 50, 20, &mut state, &theme, true);
        let pad_row = row_text(&buf, 1);
        assert!(pad_row.contains('1'), "index chip missing: {pad_row:?}");
        assert!(pad_row.contains('3'), "total chip missing: {pad_row:?}");
        assert!(pad_row.find('1').unwrap() < pad_row.find('3').unwrap());
    }

    /// Two pinned queues visible at once share the single top-left chip
    /// slot: only the MOST RECENTLY navigated queue draws its counter —
    /// the chips never overwrite each other.
    #[test]
    fn counter_chips_do_not_collide_between_queues() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        for agent in ["kilo", "opencode"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} report\n"));
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} report v2\n"));
        }
        state.subagent_rebuild_interval = std::time::Duration::ZERO;

        // Pin BOTH queues away from live (kilo first, opencode last).
        state.panel_focus = Some(types::PanelFocus::Agent("kilo".to_string()));
        state.set_queue_index("kilo", 0);
        state.panel_focus = Some(types::PanelFocus::Agent("opencode".to_string()));
        state.set_queue_index("opencode", 0);

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        // Exactly ONE pair of chips: index [1] and total [2] appear once
        // each on the padding row — not two overlapping pairs.
        let pad_row = row_text(&buf, 1);
        assert_eq!(
            pad_row.matches(" 1 ").count(),
            1,
            "single index chip expected: {pad_row:?}"
        );
        assert_eq!(
            pad_row.matches(" 2 ").count(),
            1,
            "single total chip expected: {pad_row:?}"
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

        // One content row per session (header + 1 body row), stacked in
        // chronological order from inner_y = TOP_GAP + TOP_PAD = 2.
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
        for (row, want) in [3usize, 5, 7].iter().zip(expected.iter()) {
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
            let mut md = MarkdownRenderable::new(Some(body.clone()));
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
            "section height must equal its natural height (3 overhead + 10 content)"
        );

        // Painted check inside the band: only TOP_GAP+TOP_PAD above and the
        // single BOTTOM_PAD row may be blank.
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
