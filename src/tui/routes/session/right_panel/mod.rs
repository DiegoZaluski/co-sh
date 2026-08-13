use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;

use crate::theme::Theme;

pub mod pty;
pub mod todo;
pub mod types;

use todo::{render_todo_section, todo_section_height};
use types::{
    PtySession, RightPanelState, SubagentBodyCache, sanitize_subagent_text, split_subagent_output,
};

pub fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
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
            let lines = state.bash_buffer().len() as i32;
            if lines == 0 { 0 } else { BOX_OVERHEAD + lines }
        }
        types::SectionKind::Subagent => {
            // The wrap width must match the renderer's (section width minus
            // the left+right padding), otherwise the natural height would
            // disagree with the drawn body and the box would clip or overhang.
            let wrap_w = inner_w.saturating_sub(3);
            let rows: i32 = state
                .subagent_section_rows(wrap_w)
                .iter()
                .map(|&r| i32::from(r))
                .sum();
            if rows == 0 { 0 } else { BOX_OVERHEAD + rows }
        }
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
        // Leftover goes to the first needy section
        if pool > 0
            && let Some(&i) = needy.first()
        {
            allocations[i] += pool;
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
    let mut cur_y = viewport_top;
    for (pos, &idx) in render_order.iter().enumerate() {
        let (kind, natural_h) = visible[idx];
        let allocated = allocations[idx].max(1) as u16;

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
                render_bash_section(buf, inner_x, cur_y as u16, inner_w, allocated, state, theme);
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
                );
            }
        }

        cur_y += allocated as i32;
        if pos < render_order.len() - 1 {
            cur_y += SECTION_GAP;
        }
    }
}

/// Render all bash PTYs as a single continuous text buffer with line-based scroll.
fn render_bash_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    const LEFT_PAD: u16 = 1;
    const TOP_GAP: u16 = 1;
    const TOP_PAD: u16 = 1;
    const BOTTOM_PAD: u16 = 1;

    let total_lines = state.bash_buffer().len() as i32;
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
    let scroll_y = if has_scroll {
        state.bash_scroll_y = state.bash_scroll_y.min(total_lines - inner_h as i32);
        state.bash_scroll_y
    } else {
        0
    };

    let start_line = scroll_y as usize;
    let visible = inner_h as usize;
    let buffer = state.bash_buffer();

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

    if has_scroll {
        draw_section_scrollbar(buf, x + max_w, y, max_h, total_lines, scroll_y, theme);
    }
}

/// Render all subagent PTYs as one scrollable markdown document.
///
/// Different agent CLIs (e.g. opencode + kilo) stack; the same agent called
/// again replaces its previous entry (handled in `app.rs` via `retain`). Each
/// session renders as: the command header line (normal text), the optional
/// "→ cosh:" main-agent input line (normal text), then the subagent body as    /// **markdown** — the same renderer the chat uses for its content, so the
/// report's headings/code/lists survive inside the box. Scrolling is in
/// markdown layout rows (matching [`RightPanelState::subagent_section_rows`])
/// and the visible slice of each body is blitted from a reusable scratch
/// buffer, since the markdown renderer always lays out from content row 0.
///
/// The content reserves padding on BOTH sides (1 column each) so wrapped
/// markdown lines and the scrollbar never touch the box's right edge
/// (flush-right content read as a leak past the box).
fn render_subagent_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    // The right side gets EXTRA clearance (2 cols vs 1 on the left):
    // table borders and the scrollbar sit on that side, and a single
    // column still read as flush against the box edge.
    const LEFT_PAD: u16 = 1;
    const RIGHT_PAD: u16 = 2;
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
    if inner_h == 0 || wrap_w == 0 {
        return;
    }

    // Heights first (needs `&mut self`), then the sessions borrow: holding
    // the session references across the cache call would conflict.
    let rows: Vec<u16> = state.subagent_section_rows(wrap_w).to_vec();
    let total_rows: i32 = rows.iter().map(|&r| i32::from(r)).sum();
    if total_rows == 0 {
        return;
    }
    let sessions: Vec<&PtySession> = state
        .pty_sessions
        .iter()
        .filter(|p| p.command.starts_with("subagent:"))
        .collect();

    let has_scroll = total_rows > i32::from(inner_h);
    // Disjoint field borrows: the loop needs the reusable scratch (mutable)
    // and the scroll offset (mutable) at once.
    let subagent_scratch = &mut state.subagent_scratch;
    let subagent_scroll_y = &mut state.subagent_scroll_y;
    let scroll_y = if has_scroll {
        let max_scroll = total_rows - i32::from(inner_h);
        *subagent_scroll_y = (*subagent_scroll_y).min(max_scroll);
        *subagent_scroll_y
    } else {
        0
    };

    let text_style = Style::default().fg(rgba_color(theme.text));
    let theme_key = subagent_theme_key(theme);
    let content_bottom = scroll_y + i32::from(inner_h);
    let mut content_row: i32 = 0;

    for (session_idx, (session, &sess_rows)) in sessions.iter().zip(rows.iter()).enumerate() {
        let sess_rows = i32::from(sess_rows);
        let (input_line, body) = split_subagent_output(&session.output);
        let body_start = content_row + 1 + i32::from(input_line.is_some());
        let sess_end = content_row + sess_rows;

        // Command header row.
        if content_row >= scroll_y && content_row < content_bottom {
            let line_y = inner_y + (content_row - scroll_y) as u16;
            draw_text(
                buf,
                &session.command,
                x + LEFT_PAD,
                line_y,
                wrap_w,
                text_style,
            );
        }
        // Main-agent input line (plain text, normal style).
        if let Some(input) = input_line {
            let row = content_row + 1;
            if row >= scroll_y && row < content_bottom {
                let line_y = inner_y + (row - scroll_y) as u16;
                draw_text(buf, input, x + LEFT_PAD, line_y, wrap_w, text_style);
            }
        }
        // Markdown body: blit the visible slice of the previously rendered
        // cells (cache hit), or re-render the full body into the scratch and
        // store the cells when the content, wrap width or theme changed. The
        // renderer lays out from content row 0, so the full body must be
        // rendered on a miss — but that happens once per streamed chunk, not
        // every frame (re-rendering the whole body per frame was the
        // right-panel frame bottleneck).
        let body_h = sess_rows - 1 - i32::from(input_line.is_some());
        if body_h > 0 && body_start < content_bottom && sess_end > scroll_y {
            let clean = sanitize_subagent_text(body);
            if !clean.trim().is_empty() {
                let body_h_u = body_h as u16;
                let hit = state
                    .subagent_body_cache
                    .get(session_idx)
                    .and_then(|e| e.as_ref())
                    .is_some_and(|e| {
                        e.id == session.id
                            && e.layout_gen == state.subagent_layout_gen
                            && e.wrap_w == wrap_w
                            && e.h == body_h_u
                            && e.theme_key == theme_key
                    });
                if !hit {
                    let area = Rect::new(0, 0, wrap_w, body_h_u);
                    let scratch = subagent_scratch.get_or_insert_with(|| Buffer::empty(area));
                    if *scratch.area() != area {
                        scratch.resize(area);
                    }
                    let mut md = MarkdownRenderable::new(Some(clean));
                    md.set_fg(Some(ColorInput::RGBA(theme.text)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background_element)));
                    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(
                        255, 200, 0, 255,
                    ))));
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
                    while state.subagent_body_cache.len() <= session_idx {
                        state.subagent_body_cache.push(None);
                    }
                    state.subagent_body_cache[session_idx] = Some(SubagentBodyCache {
                        id: session.id.clone(),
                        layout_gen: state.subagent_layout_gen,
                        wrap_w,
                        h: body_h_u,
                        theme_key,
                        cells,
                    });
                }

                // Blit the visible slice from the cached cells.
                let cached = state.subagent_body_cache[session_idx].as_ref().unwrap();
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
            }
        }
        content_row = sess_end;
    }

    if has_scroll {
        // Scrollbar on the box's right-padding column (clear of the edge).
        draw_section_scrollbar(
            buf,
            x.saturating_add(max_w).saturating_sub(RIGHT_PAD),
            y,
            max_h,
            total_rows,
            scroll_y,
            theme,
        );
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

/// Draw a scrollbar within a section (not the full panel).
fn draw_section_scrollbar(
    buf: &mut Buffer,
    sb_x: u16,
    sb_y: u16,
    sb_h: u16,
    content_h: i32,
    scroll_y: i32,
    theme: &Theme,
) {
    let sb_h = sb_h as i32;
    if sb_h <= 0 || content_h <= 0 {
        return;
    }
    let thumb_size = (sb_h as f64 * sb_h as f64 / content_h as f64)
        .max(1.0)
        .min(sb_h as f64) as i32;

    let max_thumb_pos = sb_h - thumb_size;
    let scroll_ratio = if content_h > sb_h {
        scroll_y as f64 / (content_h - sb_h) as f64
    } else {
        0.0
    };
    let thumb_pos = (scroll_ratio * max_thumb_pos as f64) as i32;

    let sb_style = Style::default().fg(rgba_color(theme.text_muted));

    for row in 0..sb_h {
        let Some(y) = sb_y.checked_add(row as u16) else {
            break;
        };
        if let Some(cell) = buf.cell_mut((sb_x, y))
            && row >= thumb_pos
            && row < thumb_pos + thumb_size
        {
            cell.set_char('█');
            cell.set_style(sb_style);
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

        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme);

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
    /// The content keeps clear of the box's right edge: wrapped markdown
    /// lines (e.g. a long code word) end 2 columns before the edge, and the
    /// rightmost box column stays blank even with a line long enough to wrap
    /// to the full width. The box is [0, 50), so columns 48 and 49 (the
    /// right padding and the box edge) must both stay blank.
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

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme);

        for y in 0..12 {
            for col in [48u16, 49u16] {
                if let Some(c) = buf.cell((col, y)) {
                    assert_eq!(
                        c.symbol().chars().next().unwrap_or(' '),
                        ' ',
                        "column {col} (box right padding/edge) must stay blank on row {y}"
                    );
                }
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

        render_subagent_section(&mut buf, 0, 0, 50, 20, &mut state, &theme);

        let entry = state
            .subagent_body_cache
            .first()
            .and_then(|e| e.as_ref())
            .expect("body cache entry after first render");
        let (id, layout_gen, wrap_w, h) =
            (entry.id.clone(), entry.layout_gen, entry.wrap_w, entry.h);
        assert_eq!(id, "pty-1");
        assert_eq!(layout_gen, state.subagent_layout_gen);
        assert_eq!(wrap_w, 50 - 3, "box width minus left+right padding");
        assert_eq!(h, entry.cells.len() as u16 / wrap_w);

        let mut buf2 = Buffer::empty(Rect::new(0, 0, 50, 20));
        render_subagent_section(&mut buf2, 0, 0, 50, 20, &mut state, &theme);

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
    /// columns 48/49 of a [0, 50) box.
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

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme);

        for y in 0..12 {
            for col in [48u16, 49u16] {
                if let Some(c) = buf.cell((col, y)) {
                    assert_eq!(
                        c.symbol().chars().next().unwrap_or(' '),
                        ' ',
                        "table border must not reach column {col} on row {y}"
                    );
                }
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

        render_subagent_section(&mut buf, 0, 0, 50, 12, &mut state, &theme);

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
}
