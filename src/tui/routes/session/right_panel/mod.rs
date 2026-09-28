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
    PtySession, RightPanelState, SubagentActivityLine, SubagentBlockRenderer, SubagentBodyCache,
    activity_lines, sanitize_subagent_text, split_subagent_output, subagent_visible_body,
    wrap_chars,
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
/// `accent`. Used to tint a subagent window's background with its review
/// verdict color while staying close enough to the theme to keep text
/// readable.
fn blend(base: RGBA, accent: RGBA, t: f32) -> RGBA {
    let mix = |a: u8, b: u8| -> u8 { (f32::from(a) * (1.0 - t) + f32::from(b) * t).round() as u8 };
    let (ar, ag, ab, _) = base.to_ints();
    let (br, bg_, bb, _) = accent.to_ints();
    RGBA::from_ints(mix(ar, br), mix(ag, bg_), mix(ab, bb), 255)
}

/// Opacity of the severity background tint: how much of the verdict color
/// mixes into the window's background. Raised from the original 0.18: the
/// near-black box background dragged every tint's luminance down so far
/// that the theme's amber warning read as BROWN (dark yellow is
/// perceptually brown), and even green/red barely registered. The value is
/// set by the middle severity: the color must survive the blend still
/// recognizable as its own family — orange lands on #A8642D (copper, hue
/// ~27°, squarely between red and green), green on #1B8543 and red on
/// #A03132, all three unmistakable. At that opacity the light body text
/// keeps workable contrast on DARK backgrounds (≈3.8:1 on green, ≈5.7:1
/// on red, ≈3.7:1 on the copper orange — the least readable of the three,
/// accepted so the verdict color survives the blend; on light-background
/// themes the same opacity would need its own audit, out of scope here).
const SEVERITY_TINT_ALPHA: f32 = 0.65;

/// Verdict colors for a review-severity box tint: a dedicated palette of
/// pure, vivid, mid-tone green/orange/red, deliberately NOT the theme's
/// success/warning/error stand-ins. Two findings forced the change: theme
/// semantic colors are not hue-stable across themes (the `orng` theme's
/// `success` is blue), and dark or earthy tones collapse through the
/// background blend into perceptually different colors — the cosh theme's
/// amber `warning` (#D4A742) at the old 18% blend over the near-black
/// background read as brown, not yellow. The middle severity therefore
/// carries an ORANGE origin color: yellow, even light, darkened through
/// the blend into a burnt goldenrod that still read as dirty yellow-brown,
/// while orange darkens into copper — a family that survives. These
/// constants are theme-independent, so a verdict always reads as the same
/// green/orange/red in every theme; the unblended color also drives the
/// running tool-call spinner's beam, where the orange shows at full
/// vividness. The enum variant keeps its protocol name (`Severity::Yellow`
/// is the wire format's middle severity); only the rendered color moved.
fn severity_rgba(sev: cosh_tools::subagent::severity::Severity) -> RGBA {
    match sev {
        cosh_tools::subagent::severity::Severity::Green => RGBA::from_hex("#22C55E"),
        cosh_tools::subagent::severity::Severity::Yellow => RGBA::from_hex("#FB923C"),
        cosh_tools::subagent::severity::Severity::Red => RGBA::from_hex("#EF4444"),
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

/// Compute the clickable rects of the header row: one button per EXISTING
/// section plus the hint-only mixed button right-aligned at the edge.
/// The buttons exist whenever two or more boxes EXIST — the rule counts the
/// sections present in the panel, not the ones currently displayed, so the
/// buttons survive maximization and switching the owner costs one click.
/// Consecutive section buttons keep three spare columns between them (the
/// middle one carries the centered delimiter); one clearance column stays
/// between the last section button and the mixed button.
fn build_header_buttons(
    x: u16,
    y: u16,
    max_w: u16,
    present: &[bool; 3],
) -> Vec<types::HeaderButton> {
    let mut buttons = Vec::new();
    // The mixed button owns the header's right edge: the bare "F4" hint
    // plus one clearance column on each side.
    let hint_w = types::HEADER_MIXED_HINT.chars().count() as u16;
    let mixed_x1 = x.saturating_add(max_w);
    let mixed_x0 = mixed_x1.saturating_sub(hint_w + 2);
    let section_limit = mixed_x0.saturating_sub(1);
    let mut cx = x;
    for (kind, label) in types::HEADER_SECTION_LABELS {
        if !present[types::section_kind_index(kind)] {
            continue;
        }
        let w = label.chars().count() as u16;
        if cx.saturating_add(w) > section_limit {
            break;
        }
        buttons.push(types::HeaderButton {
            target: Some(kind),
            x0: cx,
            x1: cx + w,
            top: y,
        });
        // Three spare columns between consecutive buttons: the delimiter
        // rides in the MIDDLE one (`next.x0 - 2`), one empty column away
        // from the previous label AND one away from the next — centered.
        cx += w + 3;
    }
    buttons.push(types::HeaderButton {
        target: None,
        x0: mixed_x0,
        x1: mixed_x1,
        top: y,
    });
    buttons
}

/// Paint the header buttons computed by [`build_header_buttons`]: the section
/// buttons are bare text on the panel background — no background of their
/// own, like the mixed hint beside them — and the section that currently owns
/// the panel (`selected`) has its LABEL painted white so the user can see at
/// a glance which button is active; the resting labels share the dim band
/// color with the delimiter between them. Between two consecutive section
/// buttons the layout's spare column carries the faint
/// [`types::HEADER_SECTION_SEPARATOR`] delimiter — a pure divider, never a
/// button.
///
/// The resting color is NOT the raw box background: as a 1-cell glyph on the
/// panel background it would be invisible (#0E0E11 on #000000 in the default
/// theme), so [`header_band_color`] lifts it toward the text color while
/// keeping the recessed feel.
fn header_band_color(theme: &Theme) -> RGBA {
    blend(theme.background_element, theme.text, 0.25)
}

fn draw_header_row(
    buf: &mut Buffer,
    buttons: &[types::HeaderButton],
    selected: Option<types::SectionKind>,
    theme: &Theme,
) {
    for (i, button) in buttons.iter().enumerate() {
        match button.target {
            Some(kind) => {
                // The delimiter between two consecutive section buttons
                // rides in the MIDDLE of the three spare columns the layout
                // leaves between them (`cx += w + 3`): one empty column away
                // from each neighbor label. It is OUTSIDE both hit rects —
                // the previous button ends at `x0 - 3` exclusive — so a
                // click on it falls through to nothing. Painted in the dim
                // band color derived from the boxes' background, a pure
                // divider that never takes the selection color.
                if i > 0
                    && buttons[i - 1].target.is_some()
                    && let Some(sep_x) = button.x0.checked_sub(2)
                {
                    let sep_style = Style::default().fg(rgba_color(header_band_color(theme)));
                    draw_text(
                        buf,
                        types::HEADER_SECTION_SEPARATOR,
                        sep_x,
                        button.top,
                        types::HEADER_SECTION_SEPARATOR.chars().count() as u16,
                        sep_style,
                    );
                }
                let label = types::HEADER_SECTION_LABELS
                    .iter()
                    .find(|(k, _)| *k == kind)
                    .map(|(_, label)| *label)
                    .unwrap_or("");
                // The selected section's label is painted WHITE — the FONT
                // changes, the background stays the panel's own; the
                // highlight follows the clicks and vanishes when the mixed
                // view returns. Resting labels share the dim band color of
                // the delimiter between them.
                let fg = if selected == Some(kind) {
                    theme.text
                } else {
                    header_band_color(theme)
                };
                let style = Style::default().fg(rgba_color(fg));
                // The drawn text must match the hit rect exactly: the bare
                // label — no leading space (the chip padding is gone with
                // the background, and the spare columns now belong to the
                // centered delimiter).
                let w = label.chars().count() as u16;
                draw_text(buf, label, button.x0, button.top, w, style);
            }
            // The mixed button NEVER takes the selection color: it is a
            // layout-level control, not a member of the group. Its bare
            // "F4" hint keeps the dim band color — the resting look the
            // section buttons share — with no glyph and no background.
            None => {
                let style = Style::default().fg(rgba_color(header_band_color(theme)));
                draw_text(
                    buf,
                    types::HEADER_MIXED_HINT,
                    button.x0 + 1,
                    button.top,
                    types::HEADER_MIXED_HINT.chars().count() as u16,
                    style,
                );
            }
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

    let (has_todos, has_bash, has_subagent) = count_sections(state);
    let present_mask = [has_todos, has_bash, has_subagent];

    // ── Phase M: the maximized section ───────────────────────────────
    // A section that lost its content while maximized (todo list cleared,
    // last PTY finished) falls back to the mixed view: a maximized panel
    // must never render empty.
    if let Some(kind) = state.maximized_section
        && !present_mask[types::section_kind_index(kind)]
    {
        state.maximized_section = None;
    }
    // The header exists whenever two or more boxes EXIST (the rule counts
    // the sections present in the panel, not the ones currently displayed,
    // so the buttons survive maximization). It owns row 0 only: the 1-row
    // margin below it IS each section's own TOP_GAP row, so the boxes begin
    // exactly one row down — the same single-row rhythm as before.
    let header_rows =
        if state.maximized_section.is_some() || present_mask.iter().filter(|p| **p).count() >= 2 {
            1u16
        } else {
            0
        };

    // ── Phase 0: resolve the visible subagent windows ────────────────
    // The subagent section owns its area and may BORROW leftover space
    // from the todo/bash areas (loans have no time guarantee — owners
    // reclaim whenever they need it). The budget below IS that space;
    // window selection ranks focused > pinned > running > finished so
    // the queue being navigated is never hidden by stale content.
    let wrap_w = inner_w.saturating_sub(2);
    state.subagent_wrap_w = wrap_w;
    if state.maximized_section.is_some() {
        // Maximized: the section owns the whole viewport (minus the header
        // rows and the bottom margin), so the subagent window budget is the
        // full space instead of the mixed view's leftover.
        if has_subagent {
            let budget = (viewport_h
                - i32::from(header_rows)
                - i32::from(has_subagent) * BOX_OVERHEAD
                - BOTTOM_MARGIN)
                .max(types::MIN_WINDOW_ROWS);
            state.resolve_visible_subagents(wrap_w, budget);
        } else {
            state.visible_subagents.clear();
        }
    } else {
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
            (viewport_h - gaps - fixed_nat - frame - BOTTOM_MARGIN - i32::from(header_rows))
                .max(types::MIN_WINDOW_ROWS);
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

    // Maximized: the chosen section alone owns the panel; the other sections
    // are not laid out (their bands would still map mouse events to hidden
    // content).
    let sections_info: Vec<(types::SectionKind, bool)> = match state.maximized_section {
        Some(kind) => sections_info.iter().map(|&(k, _)| (k, k == kind)).collect(),
        None => sections_info.to_vec(),
    };

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
    // The header rows (button row + its 1-row margin) are reserved the same
    // way: sections never grow back over the header.
    let available = viewport_h - gap_total - BOTTOM_MARGIN - i32::from(header_rows);
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

    // ── Phase H: the header row ──────────────────────────────────────
    // The header owns row 0; the margin below it IS each section's own
    // TOP_GAP row, so the boxes begin exactly one row down (the same
    // single-row rhythm the mixed view already had). The buttons are
    // recomputed each frame so a section appearing or disappearing updates
    // the row immediately.
    let mut cur_y = viewport_top + i32::from(header_rows);
    if header_rows > 0 {
        let buttons = build_header_buttons(inner_x, viewport_top as u16, inner_w, &present_mask);
        draw_header_row(buf, &buttons, state.maximized_section, theme);
        state.set_header_buttons(buttons);
    }

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
    // Selection regions are rebuilt from scratch on every content/width
    // change — the bash section's contract. Without this clear the regions
    // ACCUMULATED one generation per streamed chunk, and extraction read
    // stale rows from old layouts: a selection pasted text entirely
    // different from what was on screen.
    if rebuild_regions {
        state.subagent_text_regions.clear();
    }
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
                // Only a review verdict still colors the frame: the severity
                // tint extends to the padding rows so the box edge never
                // reads as belonging to a different verdict. Without a
                // verdict the padding keeps the box's own default color.
                if let Some(sev) = state.pty_sessions[sess_idx].severity {
                    let tint = blend(
                        theme.background_element,
                        severity_rgba(sev),
                        SEVERITY_TINT_ALPHA,
                    );
                    let mut fill = BoxRenderable::new();
                    fill.set_background_color(Some(tint.into()));
                    fill.render_self(buf, Rect::new(x, pad_y, max_w, 1));
                }
            }
        }
    }
    // ONE read-only pass builds each window's area tint — method calls
    // borrow all of `state`, so nothing of this shape may run inside the
    // render loop. Per-agent colors are GONE: every window keeps the
    // section's default box color (the same background bash and the TODO
    // panel use), because color is now reserved for MEANING — only a
    // code-review verdict tints its window green/orange/red (Phase 3b.1).
    // The accent (the unblended color) doubles as the running tool-call
    // spinner's highlight color, so a red verdict window also sweeps red;
    // without a verdict the sweep runs in the plain text color.
    let (tints, accents): (Vec<RGBA>, Vec<RGBA>) = sessions
        .iter()
        .map(|(_, s)| match s.severity {
            Some(sev) => {
                let accent = severity_rgba(sev);
                (
                    blend(theme.background_element, accent, SEVERITY_TINT_ALPHA),
                    accent,
                )
            }
            None => (theme.background_element, theme.text),
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
    let mut live_block_keys: Vec<String> = Vec::new();

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
        // The single height source is the shared `visual_rows` helper:
        // multi-row wrapped tool lines and markdown Message/Thought blocks
        // cost what they will actually draw, so the body starts below the
        // LAST drawn row and the renderer can never disagree.
        let activity_h = i32::from(
            activity
                .iter()
                .map(|l| l.visual_rows(wrap_w))
                .fold(0u16, u16::saturating_add),
        );
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
            &mut live_block_keys,
            rebuild_regions,
            &mut state.subagent_activity_cache,
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
                    // blitted cells keep the window's color underneath
                    // (default box background, or the verdict's severity).
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
    // animating state alive across turns. The block renderers follow the
    // same pattern keyed by timeline index: entries whose index vanished
    // (timeline bounded at 200; sessions finish) are dropped so their
    // markdown state and scratch buffers do not accumulate.
    let live: std::collections::HashSet<&str> =
        live_spinner_keys.iter().map(String::as_str).collect();
    subagent_spinners.retain(|key, _| live.contains(key.as_str()));
    let live_blocks: std::collections::HashSet<&str> =
        live_block_keys.iter().map(String::as_str).collect();
    state
        .subagent_activity_cache
        .retain(|key, _| live_blocks.contains(key.as_str()));

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
///   …) followed by the extracted detail (path/query/URL), wrapped to the
///   box's text width (the wrap happens in [`activity_lines`]; every row
///   draws indented at `x + 2`, so the wrap width is `wrap_w − 2`).
///   InProgress calls render their first row through the luminous-sweep
///   [`HighlightSpinner`] (highlight = window accent: a verdict window
///   sweeps in its severity color, a verdict-less one in the plain text
///   color), clamped to the text width so the animation can
///   never bleed across the box edge; the other statuses draw a plain
///   ✓/✗/· marker colored success/error/text.
/// - Diff: dimmed `path +N −M` rows under the call.
/// - Plan: compact checkbox glyphs (☐ pending/in-progress, ☑ completed —
///   the outer TODO panel's row style is deliberately NOT reused: it takes
///   too much width inside the box).
/// - Message/Thought: whole markdown blocks rendered through
///   [`MarkdownRenderable`] with their cells cached per
///   `{session_id}:{index}` (same philosophy as the finished-window body
///   cache, per-block granularity). Message draws in the normal text
///   color (the agent speaking); Thought draws dimmed (text_muted) — the
///   same "opaque thinking" treatment as the main agent's reasoning but
///   always visible (no hidden state).
///
/// Selection text regions are appended for drawn rows when
/// `rebuild_regions`. Spinner map entries are created/updated keyed by
/// `{session_id}:{call_id}`; `live_keys` collects the InProgress keys so
/// the caller can prune finished entries after the window loop.
/// `live_block_keys` does the same for the block renderer map: every
/// Message/Thought timeline index seen this frame is collected so entries
/// whose index vanished (timeline bounded, sessions finished) are pruned.
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
    tint: RGBA,
    accent: RGBA,
    theme: &Theme,
    text_style: Style,
    session_id: &str,
    spinners: &mut HashMap<String, HighlightSpinner>,
    live_keys: &mut Vec<String>,
    live_block_keys: &mut Vec<String>,
    rebuild_regions: bool,
    block_cache: &mut HashMap<String, SubagentBlockRenderer>,
    regions: &mut Vec<TextRegion>,
) {
    let marker_style_ok = Style::default().fg(rgba_color(theme.success));
    let marker_style_err = Style::default().fg(rgba_color(theme.error));
    let dimmed_style = Style::default().fg(rgba_color(theme.text_muted));
    let theme_key = subagent_theme_key(theme);

    let mut row = start_row;
    for line in lines {
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
        match line {
            SubagentActivityLine::Tool { id, rows, status } => {
                for (ri, seg) in rows.iter().enumerate() {
                    let r = row + ri as i32;
                    if r < scroll_y || r >= content_bottom {
                        continue;
                    }
                    let line_y = inner_y + (r - scroll_y) as u16;
                    match status {
                        ToolCallStatus::InProgress => {
                            // ONE spinner per running call, keyed by
                            // session+call id, holding the FULL concatenated
                            // label. The beam is normalised over that whole
                            // string, so the sweep flows ACROSS the wrapped
                            // rows — a broken path like `src/tui/routes/…`
                            // is swept to its end instead of dying at the
                            // first wrap. render_row paints this row's slice
                            // at the row's cumulative character offset; the
                            // spinner's base colour IS the dimmed style, so
                            // rows far from the beam still read as muted
                            // continuation text.
                            let key = format!("{session_id}:{id}");
                            // The spinner filters control characters out of
                            // its text (`HighlightSpinner::new`), so the
                            // concatenated label and the per-row offsets must
                            // be built from the SAME filtered rows — raw
                            // offsets would desync the row slices (and, with
                            // a mismatched text, `set_text` would reset the
                            // beam every frame).
                            let clean: Vec<String> = rows
                                .iter()
                                .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
                                .collect();
                            let joined: String = clean.concat();
                            let start_char: usize =
                                clean.iter().take(ri).map(|s| s.chars().count()).sum();
                            let spinner = spinners.entry(key).or_insert_with(|| {
                                HighlightSpinner::new(&joined, accent, theme.text_muted)
                            });
                            if spinner.text() != joined {
                                spinner.set_text(&joined);
                            }
                            spinner.set_colors(accent, theme.text_muted);
                            // The row's own width is the paint limit: a shared
                            // `wrap_w` would let a SHORT row (wrap_chars
                            // produces variable-length rows) also paint the
                            // first chars of the NEXT row's slice — the
                            // duplicated-text corruption found in review.
                            let row_chars = seg.chars().count() as u16;
                            spinner.render_row(
                                buf,
                                x + 2,
                                line_y,
                                row_chars.min(wrap_w.saturating_sub(2)),
                                start_char,
                            );
                            if rebuild_regions {
                                // The spinner paints the text at the shared
                                // x + 2 indent — the region must start there
                                // or extraction is shifted by two columns.
                                regions.push(TextRegion::one_row(
                                    r,
                                    x + 2,
                                    x + wrap_w,
                                    seg.clone(),
                                ));
                            }
                        }
                        ToolCallStatus::Completed => {
                            if ri == 0 {
                                draw_text(buf, "✓", x, line_y, wrap_w, marker_style_ok);
                            }
                            draw_text(
                                buf,
                                seg,
                                x + 2,
                                line_y,
                                wrap_w.saturating_sub(2),
                                text_style,
                            );
                            if rebuild_regions {
                                let text = if ri == 0 {
                                    format!("✓ {seg}")
                                } else {
                                    seg.clone()
                                };
                                regions.push(TextRegion::one_row(r, x, x + wrap_w, text));
                            }
                        }
                        ToolCallStatus::Failed => {
                            if ri == 0 {
                                draw_text(buf, "✗", x, line_y, wrap_w, marker_style_err);
                            }
                            draw_text(
                                buf,
                                seg,
                                x + 2,
                                line_y,
                                wrap_w.saturating_sub(2),
                                text_style,
                            );
                            if rebuild_regions {
                                let text = if ri == 0 {
                                    format!("✗ {seg}")
                                } else {
                                    seg.clone()
                                };
                                regions.push(TextRegion::one_row(r, x, x + wrap_w, text));
                            }
                        }
                        // Pending / Unknown: dimmed dot, not yet running.
                        _ => {
                            if ri == 0 {
                                draw_text(buf, "·", x, line_y, wrap_w, dimmed_style);
                            }
                            draw_text(
                                buf,
                                seg,
                                x + 2,
                                line_y,
                                wrap_w.saturating_sub(2),
                                dimmed_style,
                            );
                            if rebuild_regions {
                                let text = if ri == 0 {
                                    format!("· {seg}")
                                } else {
                                    seg.clone()
                                };
                                regions.push(TextRegion::one_row(r, x, x + wrap_w, text));
                            }
                        }
                    }
                }
            }
            SubagentActivityLine::Diff(rows) => {
                // Indented under its tool line; counts stay dimmed so the
                // path dominates.
                for (ri, seg) in rows.iter().enumerate() {
                    let r = row + ri as i32;
                    if r < scroll_y || r >= content_bottom {
                        continue;
                    }
                    let line_y = inner_y + (r - scroll_y) as u16;
                    draw_text(
                        buf,
                        seg,
                        x + 2,
                        line_y,
                        wrap_w.saturating_sub(2),
                        dimmed_style,
                    );
                    if rebuild_regions {
                        regions.push(TextRegion::one_row(r, x + 2, x + wrap_w, seg.clone()));
                    }
                }
            }
            SubagentActivityLine::Plan { status, rows } => {
                let (glyph, style) = match status {
                    PlanEntryStatus::Completed => ("☑", marker_style_ok),
                    PlanEntryStatus::InProgress => ("☐", text_style),
                    PlanEntryStatus::Pending => ("☐", dimmed_style),
                    _ => ("☐", dimmed_style),
                };
                for (ri, seg) in rows.iter().enumerate() {
                    let r = row + ri as i32;
                    if r < scroll_y || r >= content_bottom {
                        continue;
                    }
                    let line_y = inner_y + (r - scroll_y) as u16;
                    if ri == 0 {
                        draw_text(buf, glyph, x, line_y, wrap_w, style);
                    }
                    draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), style);
                    if rebuild_regions {
                        let text = if ri == 0 {
                            format!("{glyph} {seg}")
                        } else {
                            seg.clone()
                        };
                        regions.push(TextRegion::one_row(r, x, x + wrap_w, text));
                    }
                }
            }
            SubagentActivityLine::Message { index, text }
            | SubagentActivityLine::Thought { index, text } => {
                // Whole markdown blocks: a PERSISTENT per-entry renderer
                // (chat-style, like the streaming body's `tail_md`) feeds a
                // reusable scratch that is blitted row by row. The renderer
                // instance survives across frames, so `set_content`
                // re-parses only the streamed tail and closed sub-blocks
                // come back from its internal cache — a fresh cold-cache
                // renderer per chunk was the previous design (and the
                // right-panel bottleneck pattern at body scale).
                let dimmed = matches!(line, SubagentActivityLine::Thought { .. });
                let fg = if dimmed { theme.text_muted } else { theme.text };
                let h = usize::from(line.visual_rows(wrap_w));
                let key = format!("{session_id}:{index}");
                live_block_keys.push(key.clone());
                let stale = block_cache.get(&key).is_none_or(|e| {
                    e.wrap_w != wrap_w
                        || e.theme_key != theme_key
                        || e.bg != tint
                        || e.dimmed != dimmed
                });
                if stale {
                    let mut md = MarkdownRenderable::new(Some(text.clone()));
                    md.set_fg(Some(ColorInput::RGBA(fg)));
                    // Rendered ON the window's area tint so the blitted
                    // cells keep the window's color underneath (default box
                    // background, or the verdict's severity).
                    md.set_bg(Some(ColorInput::RGBA(tint)));
                    crate::util::markdown::apply_theme(&mut md, theme);
                    let area = Rect::new(0, 0, wrap_w, h as u16);
                    block_cache.insert(
                        key.clone(),
                        SubagentBlockRenderer {
                            wrap_w,
                            theme_key,
                            bg: tint,
                            dimmed,
                            text: text.clone(),
                            md,
                            scratch: Buffer::empty(area),
                        },
                    );
                }
                let entry = block_cache.get_mut(&key).unwrap_or_else(|| {
                    panic!("live block entry {key} must exist (just inserted or valid)")
                });
                if entry.text != *text {
                    // Incremental chat-style update: only the changed tail
                    // re-parses; closed blocks reuse their cached rows.
                    entry.md.set_content(text.clone());
                    entry.text = text.clone();
                }
                let area = Rect::new(0, 0, wrap_w, h as u16);
                if entry.scratch.area() != &area {
                    entry.scratch.resize(area);
                }
                // `render_self` fills the whole area with the background
                // first, so no separate clearing is needed.
                entry.md.render_self(&mut entry.scratch, area);
                for ri in 0..h {
                    let r = row + ri as i32;
                    if r < scroll_y || r >= content_bottom {
                        continue;
                    }
                    let dst_y = inner_y + (r - scroll_y) as u16;
                    for dx in 0..wrap_w {
                        let cell = entry.scratch.cell((dx, ri as u16)).cloned();
                        if let Some(dst) = buf.cell_mut((x + dx, dst_y)) {
                            *dst = cell.unwrap_or_default();
                        }
                    }
                    // Selection text regions from the same painted cells,
                    // so extraction matches the display.
                    if rebuild_regions {
                        let row_cells: Vec<ratatui::buffer::Cell> = (0..wrap_w)
                            .map(|dx| {
                                entry
                                    .scratch
                                    .cell((dx, ri as u16))
                                    .cloned()
                                    .unwrap_or_default()
                            })
                            .collect();
                        let trimmed = text_from_cell_row(&row_cells, wrap_w as usize);
                        regions.push(TextRegion::one_row(r, x, x + wrap_w, trimmed));
                    }
                }
            }
        }
        row += i32::from(line.visual_rows(wrap_w));
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

    /// REGRESSION (user-visible "reasoning in columns"): the timeline's
    /// stream coalescing used to require the matching entry to be the
    /// timeline's LAST one, so a model that interleaves reasoning and text
    /// deltas opened a new tiny sub-block per alternation and the box
    /// painted rows that used a fraction of the width. With nearest-entry
    /// coalescing (the documented mini-chat contract), interleaved chunks
    /// merge into one entry per kind and every painted row fills the box.
    #[test]
    fn interleaved_reasoning_and_text_coalesce_into_full_width_blocks() {
        use cosh_tools::subagent::events::SubagentEvent;

        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.start_pty("subagent: ".to_string(), None);
        // The internal bridge's stream shape: reasoning and message deltas
        // alternate within one response.
        for i in 0..10 {
            state.update_subagent_activity(&SubagentEvent::Thought {
                text: format!("pensando no passo {i} "),
            });
            state.update_subagent_activity(&SubagentEvent::Message {
                text: format!("ponto {i}; "),
            });
        }
        let activity = state.pty_sessions[0].subagent_activity.clone();
        // Two entries total: one Thought run, one Message run.
        assert_eq!(activity.timeline.len(), 2);

        let wrap_w = 40u16;
        let rows = state.subagent_section_rows_for_display(wrap_w);
        let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
        // max_w = wrap_w + LEFT_PAD + RIGHT_PAD: the renderer derives its
        // own wrap width from max_w, so the height math and the paint must
        // agree on the same 40 columns.
        let mut buf = Buffer::empty(Rect::new(0, 0, wrap_w + 2, natural));
        render_subagent_section(
            &mut buf,
            0,
            0,
            wrap_w + 2,
            natural,
            &mut state,
            &theme,
            false,
        );
        // The old bug's signature is the RATIO of short rows: one tiny
        // sub-block per alternation painted nearly every row narrow. The
        // coalesced layout paints full rows with at most ONE short final
        // line per block (two blocks: one Thought run, one Message run).
        let content_rows = (0..natural)
            .map(|y| row_text(&buf, y).trim_end().to_string())
            .filter(|t| !t.trim().is_empty() && !t.contains("subagent:"))
            .collect::<Vec<_>>();
        assert!(
            !content_rows.is_empty(),
            "the coalesced blocks must be visible"
        );
        let short_rows = content_rows
            .iter()
            .filter(|t| t.trim_start().chars().count() as u16 <= wrap_w / 2)
            .count();
        assert!(
            short_rows <= 2,
            "stream blocks must coalesce into wide rows; {short_rows} narrow rows betray \
             per-alternation fragments: {content_rows:?}"
        );
    }

    /// REGRESSION (user-visible misleading copy): the subagent box's
    /// selection regions were only ever PUSHED, never cleared, so every
    /// rebuild (one per streamed chunk — `pty_gen` bumps constantly) added
    /// another overlapping generation of regions and `extract_selected_text`
    /// returned stale text from old layouts: pasting produced text entirely
    /// different from the selection. The bash section's clear-on-rebuild
    /// contract applies here too: consecutive rebuilds must leave the SAME
    /// region set, and its text must match what is on screen.
    #[test]
    fn subagent_copy_regions_rebuild_from_scratch_not_accumulate() {
        use cosh_tools::subagent::events::SubagentEvent;

        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.start_pty("subagent: ".to_string(), None);
        state.update_subagent_activity(&SubagentEvent::Message {
            text: "linha unica".to_string(),
        });

        let wrap_w = 40u16;
        let rows = state.subagent_section_rows_for_display(wrap_w);
        let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;

        // Two consecutive rebuilds (e.g. a streamed chunk bumped pty_gen).
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, natural));
        render_subagent_section(&mut buf, 0, 0, 60, natural, &mut state, &theme, true);
        let first = state.subagent_text_regions.clone();
        render_subagent_section(&mut buf, 0, 0, 60, natural, &mut state, &theme, true);
        let second = state.subagent_text_regions.clone();

        assert_eq!(
            first, second,
            "rebuilds must replace the regions, not append another generation"
        );
        assert!(
            !first.is_empty(),
            "the visible message must produce copy regions"
        );
        // The copied text must be exactly what the box displays.
        let on_screen: Vec<String> = first.iter().map(|r| r.text.clone()).collect();
        assert!(
            on_screen.iter().any(|t| t.contains("linha unica")),
            "a selection over the message must copy its own text, got {on_screen:?}"
        );
    }

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

    /// Whether ANY cell of the `x0..x1` span at row `y` carries `fg`.
    fn column_fg_has(buf: &Buffer, x0: u16, x1: u16, y: u16, fg: ratatui::style::Color) -> bool {
        (x0..x1).any(|x| buf.cell((x, y)).map(|c| c.fg) == Some(fg))
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
            // The margin shares the nearest window's tint — with per-agent
            // colors gone, a margin between two verdict-less windows is
            // plain box background (and still blank).
            let want = rgba_color(theme.background_element);
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

    /// Per-agent area colors are GONE: every subagent window (external CLI
    /// or internal) renders on the section's DEFAULT box color — the same
    /// background bash and the TODO panel use. Color is reserved for
    /// meaning: only a review verdict tints a window.
    #[test]
    fn subagent_windows_render_default_box_background() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        for cmd in ["subagent: kilo", "subagent: opencode", "subagent: "] {
            state.start_pty(cmd.to_string(), None);
            state.complete_last_pty("report line\n".to_string());
        }

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        // One content row per session (header + 1 body row), stacked
        // in chronological order from inner_y = TOP_GAP + TOP_PAD = 2,
        // separated by the 1-row margin between consecutive windows.
        let plain = rgba_color(theme.background_element);
        for row in [3usize, 6, 9] {
            let got = buf.cell((1u16, row as u16)).map(|c| c.bg);
            assert_eq!(
                got,
                Some(plain),
                "row {row} bg must be the default box background"
            );
        }
    }

    /// Color carries MEANING: a review verdict (the consumed
    /// `<!-- severity: ... -->` header) tints its window green/orange/red,
    /// while a verdict-less window keeps the default box color.
    #[test]
    fn subagent_windows_tint_only_by_review_verdict() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        // First window: red verdict (short body → header + 1 body row, so
        // both windows sit at the same known rows). Second: no header.
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("<!-- severity: red -->\nBug found.\n".to_string());
        state.start_pty("subagent: opencode".to_string(), None);
        state.complete_last_pty("report line\n".to_string());

        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        render_subagent_section(&mut buf, 0, 0, 50, 30, &mut state, &theme, true);

        let red_tint = rgba_color(blend(
            theme.background_element,
            severity_rgba(cosh_tools::subagent::severity::Severity::Red),
            SEVERITY_TINT_ALPHA,
        ));
        let plain = rgba_color(theme.background_element);
        let got_red = buf.cell((1u16, 3u16)).map(|c| c.bg);
        assert_eq!(got_red, Some(red_tint), "verdict window must tint red");
        let got_plain = buf.cell((1u16, 6u16)).map(|c| c.bg);
        assert_eq!(
            got_plain,
            Some(plain),
            "verdict-less window keeps the default box background"
        );
    }

    /// The middle severity renders ORANGE, not yellow: over the near-black
    /// element background even a light yellow blends down into a burnt
    /// goldenrod that reads as dirty yellow-brown, while an orange blends
    /// into copper — still clearly between red and green. The test pins the
    /// HUE BAND of the blended tint (orange ≈ 15..45°, yellow begins past
    /// 45°) so the constant cannot drift back into the yellow family.
    #[test]
    fn yellow_verdict_tints_orange_not_yellow() {
        let theme = test_theme();
        let tint = blend(
            theme.background_element,
            severity_rgba(cosh_tools::subagent::severity::Severity::Yellow),
            SEVERITY_TINT_ALPHA,
        );
        let (r, g, b, _) = tint.to_ints();
        // Standard RGB hue in degrees (max-channel method; sRGB and linear
        // hue agree closely for this saturated a color, so the plain
        // formula is enough to pin the family).
        let (min, max) = (r.min(g).min(b) as f32, r.max(g).max(b) as f32);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max as u8 == r {
            60.0 * ((g as f32 - b as f32) / delta % 6.0)
        } else if max as u8 == g {
            60.0 * ((b as f32 - r as f32) / delta + 2.0)
        } else {
            60.0 * ((r as f32 - g as f32) / delta + 4.0)
        };
        let hue = if hue < 0.0 { hue + 360.0 } else { hue };
        assert!(
            (15.0..45.0).contains(&hue),
            "the middle-severity tint must be ORANGE (hue 15..45°), got {hue:.1}° for #{tint:X?}"
        );
        // The tint must also stay distinct from its neighbors on BOTH ends:
        // red's hue is near 0, green's near 140 — the orange band separates
        // the three verdicts even after the dark blend.
        let red = blend(
            theme.background_element,
            severity_rgba(cosh_tools::subagent::severity::Severity::Red),
            SEVERITY_TINT_ALPHA,
        );
        let green = blend(
            theme.background_element,
            severity_rgba(cosh_tools::subagent::severity::Severity::Green),
            SEVERITY_TINT_ALPHA,
        );
        assert_ne!(tint, red, "middle tint must differ from red");
        assert_ne!(tint, green, "middle tint must differ from green");
    }

    /// PROPERTY: no streaming-typical markdown shape paints MORE rows than
    /// `estimate_height` reserves when rendered through the REAL activity
    /// path (themed fg/bg + apply_theme). If one did, the block's tail
    /// would be clipped at the reserved height and the NEXT activity item
    /// would paint where the tail should be (the reported overlap).
    #[test]
    fn activity_block_estimate_matches_real_paint_height() {
        use cosh_tui::core::renderable::Renderable;
        use cosh_tui::core::renderables::markdown::estimate_height;

        let theme = test_theme();
        let shapes: Vec<(&str, String)> = vec![
            ("long single paragraph", "word ".repeat(120)),
            ("thought fragments no spaces", "x".repeat(400)),
            ("unclosed table", "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 ".to_string()),
            ("closed table", "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n".to_string()),
            (
                "table long cells",
                "| aaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbb |\n|---|---|\n| cccccccccccccccc | dddddddddddddddd |\n".to_string(),
            ),
            ("crlf lines", "line one\r\nline two\r\nline three\r\n".to_string()),
            (
                "multi paragraph + heading",
                "# Head\n\npara one\n\npara two\n\n- li one\n- li two\n".to_string(),
            ),
            ("fence unclosed", "```rust\nfn main() {}\n".to_string()),
            (
                "blockquote stream",
                "> quoted chunk one\n> quoted chunk two\n> three".to_string(),
            ),
            (
                "list unclosed item",
                "- item one\n- item two with a very long tail that must wrap around the column width for sure".to_string(),
            ),
            ("nested list", "- a\n  - b\n    - c\n- d\n".to_string()),
            ("cjk paragraph", "汉字测试 ".repeat(40)),
            ("cjk long word", "漢".repeat(100)),
            ("emoji line", "🚀🔥 ".repeat(30)),
            (
                "accented paragraph",
                "ação coração pinga é útil ".repeat(20),
            ),
            (
                "long inline code",
                format!("`{}`", "x".repeat(100)),
            ),
            ("long heading", format!("# {}", "word ".repeat(40))),
            ("setext heading", format!("Setext\n{}", "=".repeat(60))),
            (
                "autolink long",
                "https://example.com/very/long/path/segment/that/wraps/around".to_string(),
            ),
            ("hr", "---".to_string()),
            ("nested quote", "> > deep\n> > quote".to_string()),
            (
                "mixed doc",
                "# Título\n\nparágrafo com acentuação\n\n- item um\n- item dois\n\n```rust\nfn x() {}\n```\n".to_string(),
            ),
        ];
        for w in [30u16, 40, 48, 60, 76, 96, 120] {
            for (name, body) in &shapes {
                let est = usize::from(estimate_height(body, w));
                let mut buf = Buffer::empty(Rect::new(0, 0, w, est as u16 + 12));
                // EXACT real-path setup: themed fg, tint bg, apply_theme.
                let mut md = MarkdownRenderable::new(Some(body.clone()));
                md.set_fg(Some(ColorInput::RGBA(theme.text)));
                md.set_bg(Some(ColorInput::RGBA(theme.background_element)));
                crate::util::markdown::apply_theme(&mut md, &theme);
                md.render_self(&mut buf, Rect::new(0, 0, w, (est + 12) as u16));
                let last_paint = (0..(est + 12))
                    .filter(|&y| {
                        (0..w).any(|x| buf.cell((x, y as u16)).is_some_and(|c| c.symbol() != " "))
                    })
                    .map(|y| y + 1)
                    .max()
                    .unwrap_or(0);
                assert!(
                    last_paint <= est,
                    "OVERLAP: {name} w={w} paints {last_paint} rows but estimate reserves {est}"
                );
            }
        }
    }

    /// REGRESSION: a REAL interleaved timeline (thought chunks → tool
    /// call → message chunks) rendered frame by frame must paint its items
    /// in chronological order WITHOUT overlap: the tool-call row must sit
    /// strictly below the last painted thought row, and the message rows
    /// strictly below the tool row. Uses the REAL 100 ms throttle and the
    /// app's auto-follow so the streaming conditions match production.
    #[test]
    fn interleaved_timeline_paints_in_order_without_overlap() {
        use cosh_tools::subagent::events::{SubagentEvent, ToolCallStatus, ToolKind};

        let theme = test_theme();
        let mut state = RightPanelState::new();
        // REAL throttle: frames arrive faster than 100 ms (no sleeps), so
        // the static rows cache serves stale heights while the activity
        // grows — the exact streaming condition of a fast CLI.
        state.subagent_rebuild_interval = std::time::Duration::from_millis(100);
        state.start_pty("subagent: opencode".to_string(), None);

        // A realistic opencode-like turn: per step, a thought burst, a tool
        // call and a message sentence; then a final conclusion message.
        let mut events: Vec<SubagentEvent> = Vec::new();
        for i in 0..6 {
            events.push(SubagentEvent::Thought {
                text: format!("pensando no passo {i} com bastante detalhe; "),
            });
            events.push(SubagentEvent::ToolCall {
                id: format!("c{i}"),
                title: "Reading files".to_string(),
                kind: ToolKind::Read,
                status: ToolCallStatus::InProgress,
                raw_input: Some(serde_json::json!({ "path": format!("src/f{i}.rs") })),
            });
            events.push(SubagentEvent::Message {
                text: format!("passo {i} concluído. "),
            });
        }
        events.push(SubagentEvent::Message {
            text: "Conclusão: tudo verificado e certo.".to_string(),
        });

        let wrap_w = 48u16;
        for (frame, event) in events.iter().enumerate() {
            state.update_subagent_activity(event);
            // The app auto-follows on every visible change.
            if !state.is_scrolled_up() {
                state.scroll_to_bottom();
            }
            let rows = state.subagent_section_rows_for_display(wrap_w);
            let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
            let mut buf = Buffer::empty(Rect::new(0, 0, 50, natural));
            render_subagent_section(&mut buf, 0, 0, 50, natural, &mut state, &theme, false);
            let rows_painted: Vec<String> = (0..natural).map(|y| row_text(&buf, y)).collect();

            // NO OVERLAP: a row carrying a tool detail never carries
            // thought/message words, and vice versa.
            for (y, row) in rows_painted.iter().enumerate() {
                let has_tool = row.contains("src/f");
                let has_thought = row.contains("pensando");
                let has_msg = row.contains("passo") || row.contains("Conclusão");
                assert!(
                    !(has_tool && (has_thought || has_msg)),
                    "frame {frame}: OVERLAP on screen row {y}: {row:?}"
                );
            }

            // CHRONOLOGY: tool c{i} paints below its preceding thought.
            if frame >= 2 {
                let tool_row = rows_painted
                    .iter()
                    .position(|r| r.contains("src/f0"))
                    .unwrap();
                let thought_row = rows_painted
                    .iter()
                    .position(|r| r.contains("pensando no passo 0"))
                    .unwrap();
                assert!(
                    thought_row < tool_row,
                    "frame {frame}: tool c0 (row {tool_row}) must paint BELOW thought 0 (row {thought_row})"
                );
            }
        }

        // The final frame must show the newest content (auto-follow).
        let rows = state.subagent_section_rows_for_display(wrap_w);
        let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, natural));
        render_subagent_section(&mut buf, 0, 0, 50, natural, &mut state, &theme, false);
        let final_rows: Vec<String> = (0..natural).map(|y| row_text(&buf, y)).collect();
        assert!(
            final_rows.iter().any(|r| r.contains("Conclusão")),
            "final message must be visible with auto-follow; rows: {final_rows:?}"
        );
    }

    /// REGRESSION: same interleaved stream as above but rendered in a
    /// CONSTRAINED viewport (inner_h smaller than the content — scroll
    /// active, clip bands live) with hostile event shapes mixed in: tool
    /// updates carrying diffs, plan replacements, multi-line titles (tool
    /// rows are NOT sanitized), long markdown with fences, and a message
    /// long enough to trip `bounded_tail`.
    #[test]
    fn constrained_viewport_hostile_events_paint_without_overlap() {
        use cosh_tools::subagent::events::{SubagentEvent, ToolCallStatus, ToolKind};

        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::from_millis(100);
        state.start_pty("subagent: opencode".to_string(), None);

        let mut events: Vec<SubagentEvent> = Vec::new();
        for i in 0..6 {
            events.push(SubagentEvent::Thought {
                text: format!("pensando no passo {i}: analisando módulos e dependências\n\n- ponto um\n- ponto dois\n"),
            });
            events.push(SubagentEvent::ToolCall {
                id: format!("c{i}"),
                title: format!("step {i}\nsecond line of title"),
                kind: ToolKind::Read,
                status: ToolCallStatus::InProgress,
                raw_input: Some(serde_json::json!({ "path": format!("src/f{i}.rs") })),
            });
            events.push(SubagentEvent::ToolCallUpdate {
                id: format!("c{i}"),
                status: Some(ToolCallStatus::Completed),
                title: None,
                raw_output: None,
                content: cosh_tools::subagent::events::ToolOutputBlock::default(),
            });
            events.push(SubagentEvent::Plan {
                entries: (0..=i)
                    .map(|j| cosh_tools::subagent::events::PlanEntry {
                        content: format!("tarefa {j} com descrição"),
                        status: if j < i {
                            cosh_tools::subagent::events::PlanEntryStatus::Completed
                        } else {
                            cosh_tools::subagent::events::PlanEntryStatus::InProgress
                        },
                        priority: cosh_tools::subagent::events::PlanEntryPriority::Medium,
                    })
                    .collect(),
            });
            events.push(SubagentEvent::Message {
                text: format!("passo {i} concluído.\n\n```rust\nfn exemplo_{i}() {{}}\n```\n"),
            });
        }
        // Long message to trip bounded_tail (8000 bytes).
        events.push(SubagentEvent::Message {
            text: "x".repeat(9000),
        });
        events.push(SubagentEvent::Message {
            text: "Conclusão final após tudo.".to_string(),
        });

        let wrap_w = 48u16;
        for (frame, event) in events.iter().enumerate() {
            state.update_subagent_activity(event);
            if !state.is_scrolled_up() {
                state.scroll_to_bottom();
            }
            let rows = state.subagent_section_rows_for_display(wrap_w);
            let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
            // CONSTRAINED viewport: half the natural height (scroll active).
            let inner_h = (natural / 2).max(4);
            let mut buf = Buffer::empty(Rect::new(0, 0, 50, inner_h));
            render_subagent_section(&mut buf, 0, 0, 50, inner_h, &mut state, &theme, false);
            let painted: Vec<String> = (0..inner_h).map(|y| row_text(&buf, y)).collect();

            for (y, row) in painted.iter().enumerate() {
                let has_tool = row.contains("src/f");
                let has_thought = row.contains("pensando") || row.contains("ponto um");
                let has_msg = row.contains("passo") || row.contains("Conclusão");
                assert!(
                    !(has_tool && (has_thought || has_msg)),
                    "frame {frame}: OVERLAP on screen row {y}: {row:?}"
                );
            }
            // Newest content must be reachable (auto-follow shows the tail).
            if frame == events.len() - 1 {
                assert!(
                    painted.iter().any(|r| r.contains("Conclusão")),
                    "final message must be visible with auto-follow in constrained viewport; rows: {painted:?}"
                );
            }
        }
    }

    /// REGRESSION (live run): the subagent box's tool spinner held only the
    /// FIRST wrapped row, so its beam swept a few characters and died at the
    /// wrap — a label like "edit src/tui/routes/…" never lit "/routes/…".
    /// The spinner must hold the FULL concatenated label (beam normalised
    /// over the whole string) and every visible row must be painted through
    /// `render_row` at its cumulative character offset, so the sweep flows
    /// across the wrap.
    #[test]
    fn wrapped_tool_label_is_swept_across_row_boundaries() {
        use cosh_tools::subagent::events::{SubagentEvent, ToolCallStatus, ToolKind};
        use ratatui::style::Color;

        let fg_rgb = |buf: &Buffer, x: u16, y: u16| match buf.cell((x, y))?.style().fg {
            Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        };
        let dist = |a: (u8, u8, u8), b: (u8, u8, u8)| {
            i32::from(a.0.abs_diff(b.0))
                + i32::from(a.1.abs_diff(b.1))
                + i32::from(a.2.abs_diff(b.2))
        };

        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        // The detail is long enough that wrap_chars MUST break it onto
        // several rows at the panel's wrap width.
        state.update_subagent_activity(&SubagentEvent::ToolCall {
            id: "c1".to_string(),
            title: "src/tui/routes/session/tool_render.rs".to_string(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::InProgress,
            raw_input: None,
        });

        let wrap_w = 30u16;
        let session = state.pty_sessions.last().expect("pty session");
        let activity = activity_lines(&session.subagent_activity, wrap_w);
        let Some(SubagentActivityLine::Tool { id, rows, status }) = activity
            .iter()
            .find(|l| matches!(l, SubagentActivityLine::Tool { .. }))
        else {
            panic!("tool line missing");
        };
        assert_eq!(status, &ToolCallStatus::InProgress);
        assert!(
            rows.len() >= 2,
            "the test needs a wrapped label (got {rows:?})"
        );
        let session_id = session.id.clone();
        let id = id.clone();
        let rows: Vec<String> = rows.clone();

        let mut spinners = HashMap::new();
        let mut live_keys = Vec::new();
        let mut live_block_keys = Vec::new();
        let mut block_cache = HashMap::new();
        let mut regions = Vec::new();
        let mut paint = |spinners: &mut HashMap<String, HighlightSpinner>| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
            draw_activity_lines(
                &mut buf,
                0,
                0,
                &activity,
                0,
                0,
                6,
                wrap_w,
                theme.background_element,
                RGBA::from_ints(255, 107, 48, 255),
                &theme,
                Style::default().fg(rgba_color(theme.text)),
                &session_id,
                spinners,
                &mut live_keys,
                &mut live_block_keys,
                false,
                &mut block_cache,
                &mut regions,
            );
            buf
        };

        // First paint creates the spinner for the WHOLE label.
        let _ = paint(&mut spinners);
        let key = format!("{session_id}:{id}");
        let spinner = spinners.get(&key).expect("spinner created");
        assert_eq!(
            spinner.text(),
            rows.concat(),
            "the spinner must hold the full concatenated label, not just row 0"
        );

        // Each row paints EXACTLY its own slice — a short row must not also
        // paint the head of the next row's slice (the duplicate-text
        // corruption the review probe found when every row shared one
        // max_w). Symbol-level, so a colour-only pass cannot mask it.
        let buf = paint(&mut spinners);
        for (i, row) in rows.iter().enumerate() {
            let painted: String = (0..row.chars().count() as u16)
                .map(|cx| {
                    buf.cell((2 + cx, i as u16))
                        .and_then(|c| c.symbol().chars().next())
                        .unwrap_or(' ')
                })
                .collect();
            assert_eq!(
                painted, *row,
                "row {i} must paint exactly its own slice of the label"
            );
            // Nothing may be painted past the row's own text either.
            let tail = buf.cell((2 + row.chars().count() as u16, i as u16));
            if let Some(c) = tail {
                assert_eq!(
                    c.symbol().chars().next().unwrap(),
                    ' ',
                    "row {i} must not paint past its own text"
                );
            }
        }

        // Beam centred on the LAST row: that row must light up MORE than
        // row 0 (the sweep crossed the wrap instead of dying at it).
        let total: usize = rows.iter().map(|r| r.chars().count()).sum();
        let offsets: Vec<usize> = rows
            .iter()
            .scan(0usize, |acc, r| {
                let start = *acc;
                *acc += r.chars().count();
                Some(start)
            })
            .collect();
        let last = rows.len() - 1;
        let centre = offsets[last] + rows[last].chars().count() / 2;
        let spinner = spinners.get_mut(&key).unwrap();
        spinner.set_beam_pos(centre as f32 / (total - 1) as f32);
        let buf = paint(&mut spinners);

        let base = rgba_color(theme.text_muted);
        let base_rgb = match base {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => unreachable!(),
        };
        let max_row_dist = |y: u16| {
            (0..rows[last].chars().count() as u16)
                .filter_map(|cx| fg_rgb(&buf, 2 + cx, y))
                .map(|rgb| dist(rgb, base_rgb))
                .max()
                .unwrap()
        };
        let row0 = max_row_dist(0);
        let row_last = max_row_dist(last as u16);
        assert!(
            row_last > row0 + 30,
            "the beam must light the continuation row when it reaches it (last={row_last}, row0={row0})"
        );
        assert!(row_last > 60, "the continuation row must actually be lit");
    }

    /// PROPERTY (deterministic fuzz): random interleavings of
    /// message/thought chunks, tool calls/updates and plans, rendered
    /// frame by frame at several widths with the REAL throttle active.
    /// Every timeline entry carries unique `X<i>start`/`X<i>end` markers;
    /// any painted row that mixes markers of two DIFFERENT entries — same
    /// family or not — means the height math and the painter disagree (the
    /// reported overlap).
    #[test]
    fn fuzz_activity_paint_order_and_no_overlap() {
        use cosh_tools::subagent::events::{
            PlanEntry, PlanEntryPriority, PlanEntryStatus, SubagentEvent, ToolCallStatus, ToolKind,
        };

        let theme = test_theme();
        let shapes: &[&str] = &[
            "texto simples {m}\n",
            "título {m}\n\nparágrafo longo: {f}\n",
            "- bullet um {m}\n- bullet dois\n",
            "```rust\nfn f_{m}() {{}}\n```\n",
            "| a | b |\n|---|---|\n| {m} | x |\n",
            "> citação {m}\n> segunda linha\n",
            "{f} sem espaços {m}\n",
            "# h {m}\n\ntail {f}\n",
        ];
        let words = ["word", "supercalifragilístico", "ação", "漢字", "🚀"];
        let mut s: u64 = 0xCAFE_BABE;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        // Parse all "Xn{start,end}" markers in a row → unique (family, idx)
        // pairs. A row carrying markers of two different entries fails.
        let markers_on = |row: &str| -> Vec<(char, usize)> {
            let bytes = row.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i].is_ascii_uppercase() {
                    let fam = row[i..].chars().next().unwrap();
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > i + 1
                        && let Ok(idx) = row[i + 1..j].parse::<usize>()
                    {
                        for kind in ["start", "end"] {
                            if row[j..].starts_with(kind) {
                                out.push((fam, idx));
                                break;
                            }
                        }
                    }
                }
                i += 1;
            }
            out.sort_unstable();
            out.dedup();
            out
        };

        for case in 0..120u32 {
            let wrap_w = 36u16 + (next() % 28) as u16; // 36..=63
            let mut state = RightPanelState::new();
            state.subagent_rebuild_interval = std::time::Duration::from_millis(100);
            state.start_pty("subagent: fuzz".to_string(), None);

            let mut events: Vec<SubagentEvent> = Vec::new();
            let n_steps = 3 + (next() % 5) as usize;
            for i in 0..n_steps {
                let shape = shapes[(next() as usize) % shapes.len()];
                let filler =
                    words[(next() as usize) % words.len()].repeat((2 + next() % 12) as usize);
                events.push(SubagentEvent::Message {
                    text: shape
                        .replace("{m}", &format!("M{i}start M{i}end"))
                        .replace("{f}", &filler),
                });
                // A thought burst (coalesces into one growing block).
                for c in 0..1 + next() % 3 {
                    events.push(SubagentEvent::Thought {
                        text: format!("K{i}start pensamento {c} sobre o passo {i} K{i}end "),
                    });
                }
                events.push(SubagentEvent::ToolCall {
                    id: format!("t{i}"),
                    title: format!("edit T{i}start src/m{i}.rs T{i}end"),
                    kind: ToolKind::Edit,
                    status: ToolCallStatus::InProgress,
                    raw_input: Some(serde_json::json!({
                        "path": format!("T{i}start src/f{i}.rs T{i}end")
                    })),
                });
                events.push(SubagentEvent::ToolCallUpdate {
                    id: format!("t{i}"),
                    status: Some(ToolCallStatus::Completed),
                    title: None,
                    raw_output: None,
                    content: cosh_tools::subagent::events::ToolOutputBlock::default(),
                });
                if next() % 2 == 0 {
                    events.push(SubagentEvent::Plan {
                        entries: (0..=i)
                            .map(|j| PlanEntry {
                                content: format!("P{j}start tarefa {j} P{j}end"),
                                status: PlanEntryStatus::Completed,
                                priority: PlanEntryPriority::Medium,
                            })
                            .collect(),
                    });
                }
            }

            for (frame, event) in events.iter().enumerate() {
                state.update_subagent_activity(event);
                if !state.is_scrolled_up() {
                    state.scroll_to_bottom();
                }
                let rows = state.subagent_section_rows_for_display(wrap_w);
                let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
                let mut buf = Buffer::empty(Rect::new(0, 0, wrap_w + 2, natural));
                render_subagent_section(
                    &mut buf,
                    0,
                    0,
                    wrap_w + 2,
                    natural,
                    &mut state,
                    &theme,
                    false,
                );
                let painted: Vec<String> = (0..natural).map(|y| row_text(&buf, y)).collect();

                for (y, row) in painted.iter().enumerate() {
                    let ms = markers_on(row);
                    if ms.len() > 1 {
                        panic!(
                            "case {case} w={wrap_w} frame {frame}: OVERLAP on row {y} \
                             (markers {ms:?}): {row:?}"
                        );
                    }
                }
                if frame == events.len() - 1 {
                    let last = format!("M{}end", n_steps - 1);
                    assert!(
                        painted.iter().any(|r| r.contains(&last)),
                        "case {case} w={wrap_w}: newest message marker {last} not visible; \
                         rows: {painted:?}"
                    );
                }
            }
        }
    }

    /// REGRESSION: replay the REAL captured opencode streams
    /// (`subagent_stream_capture.json`, 20 events; and
    /// `subagent_stream_capture2.json`, multi-tool with narration, 86
    /// events) frame by frame, embedded via `include_str!` from the
    /// `testdata` directory. Every captured Message/Thought chunk is
    /// replayed with a unique `Xn{start,end}` marker appended when its
    /// timeline entry COALESCES no more (detected by simulating the same
    /// coalescing rule the timeline uses), so a marker must paint EXACTLY
    /// once, inside its entry's band — any overlap moves it onto another
    /// entry's rows or duplicates it.
    #[test]
    fn subagent_captured_opencode_stream_replay_has_no_overlap() {
        use cosh_tools::subagent::events::SubagentEvent;

        let theme = test_theme();
        const CAPTURE1: &str = include_str!("testdata/subagent_stream_capture.json");
        const CAPTURE2: &str = include_str!("testdata/subagent_stream_capture2.json");
        for (name, raw) in [("single-tool", CAPTURE1), ("multi-tool", CAPTURE2)] {
            let parsed: serde_json::Value = serde_json::from_str(raw).expect("parse capture");
            let events = parsed["events"].as_array().expect("events array");
            let report = parsed["report"].as_str().expect("report");

            let mut state = RightPanelState::new();
            state.subagent_rebuild_interval = std::time::Duration::from_millis(100);
            state.start_pty("subagent: opencode".to_string(), None);
            state.update_last_pty(
                crate::routes::session::right_panel::types::subagent_input_line(
                    "multi-step tool narration prompt …",
                )
                .expect("echo"),
            );

            // Faithful mirror of `SubagentActivity::apply` coalescing: a
            // Message/Thought chunk joins the timeline's LAST entry when it
            // is of the same kind; ToolCall pushes an entry (only NEW ids —
            // re-announcements patch); ToolCallUpdate patches (no entry);
            // Plan pushes once, then replaces in place; Usage/Mode/Info
            // touch nothing. `entry_of[j]` = timeline entry index that
            // event j belongs to (or touched).
            let mut entry_kinds: Vec<&str> = Vec::new();
            let mut seen_tool_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut entry_of: Vec<i64> = Vec::new();
            for ev in events.iter() {
                let ev = &ev["event"];
                let kind = ev["kind"].as_str().expect("kind");
                match kind {
                    "Message" | "Thought" => {
                        let coalesces = entry_kinds.last().is_some_and(|k| *k == kind);
                        if !coalesces {
                            entry_kinds.push(kind);
                        }
                        entry_of.push(entry_kinds.len() as i64 - 1);
                    }
                    "ToolCall" => {
                        let id = ev["id"].as_str().unwrap_or_default().to_string();
                        if seen_tool_ids.insert(id) {
                            entry_kinds.push("Tool");
                        }
                        entry_of.push(entry_kinds.len() as i64 - 1);
                    }
                    // Patching/ignorable events: they touch the LAST entry
                    // at most — attribute them to it (never a new entry).
                    _ => entry_of.push(entry_kinds.len() as i64 - 1),
                }
            }
            // The marker of a Message/Thought entry lands on its LAST
            // chunk event: once that frame renders, the entry's text is
            // final and the marker must paint exactly once from then on.
            let mut marker_text: std::collections::HashMap<usize, String> =
                std::collections::HashMap::new();
            for (j, ev) in events.iter().enumerate() {
                let kind = ev["event"]["kind"].as_str().unwrap_or("");
                if kind != "Message" && kind != "Thought" {
                    continue;
                }
                let entry = entry_of[j];
                let is_last_chunk_of_entry = !events[j + 1..].iter().any(|later| {
                    let k2 = later["event"]["kind"].as_str().unwrap_or("");
                    (k2 == "Message" || k2 == "Thought")
                        && entry_of[events.iter().position(|e| std::ptr::eq(e, later)).unwrap()]
                            == entry
                });
                if is_last_chunk_of_entry {
                    let fam = if kind == "Message" { 'M' } else { 'K' };
                    marker_text.insert(j, format!("{fam}{entry}end "));
                }
            }

            let wrap_w = 48u16;
            for (frame, ev) in events.iter().enumerate() {
                let ev = &ev["event"];
                let event = match ev["kind"].as_str().expect("kind") {
                    "Message" => SubagentEvent::Message {
                        text: format!(
                            "{}{}",
                            ev["text"].as_str().expect("text"),
                            marker_text.get(&frame).cloned().unwrap_or_default()
                        ),
                    },
                    "Thought" => SubagentEvent::Thought {
                        text: format!(
                            "{}{}",
                            ev["text"].as_str().expect("text"),
                            marker_text.get(&frame).cloned().unwrap_or_default()
                        ),
                    },
                    "ToolCall" => SubagentEvent::ToolCall {
                        id: ev["id"].as_str().expect("id").to_string(),
                        title: ev["title"].as_str().unwrap_or_default().to_string(),
                        kind: serde_json::from_value(ev["tool_kind"].clone()).unwrap(),
                        status: serde_json::from_value(ev["status"].clone()).unwrap(),
                        raw_input: ev["raw_input"]
                            .as_object()
                            .map(|o| serde_json::Value::Object(o.clone())),
                    },
                    "ToolCallUpdate" => SubagentEvent::ToolCallUpdate {
                        id: ev["id"].as_str().expect("id").to_string(),
                        status: ev["status"].as_str().map(|s| {
                            serde_json::from_value(serde_json::Value::String(s.into())).unwrap()
                        }),
                        title: ev["title"].as_str().map(str::to_string),
                        raw_output: ev["raw_output"]
                            .as_object()
                            .map(|o| serde_json::Value::Object(o.clone())),
                        content: serde_json::from_value(ev["content"].clone()).unwrap(),
                    },
                    "Usage" => SubagentEvent::Usage {
                        context_window: ev["context_window"].as_u64().unwrap_or(0),
                        tokens_in_context: ev["tokens_in_context"].as_u64().unwrap_or(0),
                    },
                    other => panic!("unexpected captured kind {other}"),
                };
                state.update_subagent_activity(&event);
                if !state.is_scrolled_up() {
                    state.scroll_to_bottom();
                }
                let rows = state.subagent_section_rows_for_display(wrap_w);
                let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
                let mut buf = Buffer::empty(Rect::new(0, 0, wrap_w + 2, natural));
                render_subagent_section(
                    &mut buf,
                    0,
                    0,
                    wrap_w + 2,
                    natural,
                    &mut state,
                    &theme,
                    false,
                );
                let painted: Vec<String> = (0..natural).map(|y| row_text(&buf, y)).collect();

                // Every completed-entry marker paints EXACTLY ONCE in this
                // frame (it belongs to a stable, coalesced timeline entry).
                // NOTE: an entry that will GROW later has no marker yet.
                for (lc, marker) in &marker_text {
                    if *lc > frame {
                        continue; // its entry is still streaming
                    }
                    let count = painted.iter().filter(|r| r.contains(marker)).count();
                    assert_eq!(
                        count, 1,
                        "{name} frame {frame}: marker {marker} painted {count}× (overlap or loss); rows: {painted:?}"
                    );
                }
            }

            // Final settled frame: the report replaces the activity.
            state.complete_last_pty(format!("→ placeholder\n{report}"));
            state.subagent_rebuild_interval = std::time::Duration::ZERO;
            let rows = state.subagent_section_rows_for_display(wrap_w);
            let natural = (BOX_OVERHEAD + i32::from(rows.iter().sum::<u16>())).max(4) as u16;
            let mut buf = Buffer::empty(Rect::new(0, 0, wrap_w + 2, natural));
            render_subagent_section(
                &mut buf,
                0,
                0,
                wrap_w + 2,
                natural,
                &mut state,
                &theme,
                false,
            );
            let painted: Vec<String> = (0..natural).map(|y| row_text(&buf, y)).collect();
            // A distinctive tail of the report must be visible.
            let tail = report.lines().next_back().unwrap_or(report);
            let probe = tail.split_whitespace().next_back().unwrap_or(tail);
            let probe = &probe[..probe.len().min(12)];
            assert!(
                painted.iter().any(|r| r.contains(probe)),
                "{name}: final report fragment {probe:?} must be visible in the body; rows: {painted:?}"
            );
        }
    }

    /// REGRESSION: the FULL panel path — `render_right_panel` with the
    /// real budget/fit/scroll machinery, a bash section competing for
    /// space, and TWO subagent windows (both captured opencode streams
    /// replayed concurrently via `include_str!` testdata) — must keep the
    /// two streams' tool rows separated: no row may mix one window's tool
    /// title with the other's file paths.
    #[test]
    fn subagent_two_windows_full_panel_no_cross_overlap() {
        use cosh_tools::subagent::events::SubagentEvent;

        let theme = test_theme();
        let raw1 = include_str!("testdata/subagent_stream_capture.json");
        let raw2 = include_str!("testdata/subagent_stream_capture2.json");
        let parsed1: serde_json::Value = serde_json::from_str(raw1).expect("parse");
        let parsed2: serde_json::Value = serde_json::from_str(raw2).expect("parse");

        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::from_millis(100);
        // A bash PTY competes for the same budget (production has both).
        state.start_pty("echo static bash output".to_string(), None);
        state.update_last_pty("static bash line\n".repeat(30));
        state.complete_last_pty("static bash line\n".repeat(30));
        // Window 1 (idx 1): the single-tool capture; Window 2 (idx 2):
        // multi-tool. Each gets its own input echo like production.
        state.start_pty("subagent: opencode".to_string(), None);
        state.start_pty("subagent: kilo".to_string(), None);
        for idx in [0usize, 1] {
            let echo = crate::routes::session::right_panel::types::subagent_input_line(
                format!("multi-step prompt {idx} …").as_str(),
            )
            .expect("echo");
            state.pty_sessions[idx + 1].output = echo;
        }

        let events1 = parsed1["events"].as_array().expect("events");
        let events2 = parsed2["events"].as_array().expect("events");
        let n_frames = events1.len().max(events2.len());

        // Interleave: window 1 replays capture1 events, window 2 capture2,
        // on the same frames — two streams running concurrently, the
        // hardest production condition.
        for frame in 0..n_frames {
            for (stream, events) in [(1usize, events1), (2usize, events2)] {
                if frame >= events.len() {
                    continue;
                }
                let ev = &events[frame]["event"];
                let event = match ev["kind"].as_str().expect("kind") {
                    "Message" => SubagentEvent::Message {
                        text: ev["text"].as_str().expect("text").to_string(),
                    },
                    "Thought" => SubagentEvent::Thought {
                        text: ev["text"].as_str().expect("text").to_string(),
                    },
                    "ToolCall" => SubagentEvent::ToolCall {
                        id: ev["id"].as_str().expect("id").to_string(),
                        title: ev["title"].as_str().unwrap_or_default().to_string(),
                        kind: serde_json::from_value(ev["tool_kind"].clone()).unwrap(),
                        status: serde_json::from_value(ev["status"].clone()).unwrap(),
                        raw_input: ev["raw_input"]
                            .as_object()
                            .map(|o| serde_json::Value::Object(o.clone())),
                    },
                    "ToolCallUpdate" => SubagentEvent::ToolCallUpdate {
                        id: ev["id"].as_str().expect("id").to_string(),
                        status: ev["status"].as_str().map(|s| {
                            serde_json::from_value(serde_json::Value::String(s.into())).unwrap()
                        }),
                        title: ev["title"].as_str().map(str::to_string),
                        raw_output: ev["raw_output"]
                            .as_object()
                            .map(|o| serde_json::Value::Object(o.clone())),
                        content: serde_json::from_value(ev["content"].clone()).unwrap(),
                    },
                    "Usage" => SubagentEvent::Usage {
                        context_window: ev["context_window"].as_u64().unwrap_or(0),
                        tokens_in_context: ev["tokens_in_context"].as_u64().unwrap_or(0),
                    },
                    other => panic!("unexpected captured kind {other}"),
                };
                // Route to the right window: `update_subagent_activity`
                // targets the last RUNNING subagent session. With bash at
                // index 0 and the two windows at 1/2: stream 1 events must
                // reach window 1, so swap (1,2) around the call to make it
                // last; stream 2 events reach window 2 directly (it is the
                // last one by default).
                if stream == 1 {
                    state.pty_sessions.swap(1, 2);
                }
                state.update_subagent_activity(&event);
                if stream == 1 {
                    state.pty_sessions.swap(1, 2);
                }
            }
            // NO auto-follow here: production auto-follows only when the
            // user has not scrolled; a scrolled-away panel is a valid
            // steady state, and the clamp path is what must not overlap.
            let mut buf = Buffer::empty(Rect::new(0, 0, 110, 44));
            render_right_panel(&mut buf, Rect::new(0, 0, 110, 44), &mut state, &theme, 110);
            let painted: Vec<String> = (0..44).map(|y| row_text(&buf, y)).collect();
            // The tool ids of the two captures never collide, so a row
            // carrying details of BOTH streams (`src/f` paths + grep) is
            // impossible when layout is correct. Check no row mixes the
            // two captures' tool titles with the other's paths.
            for (y, row) in painted.iter().enumerate() {
                let has_read1 = row.contains("closure.rs");
                let has_grep2 = row.contains("TurnClosure");
                assert!(
                    !(has_read1 && has_grep2),
                    "frame {frame}: cross-window overlap on row {y}: {row:?}"
                );
            }
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

    /// REGRESSION: the subagent box's TOP/BOTTOM padding rows follow the
    /// NEAREST window's color — a verdict tints them with its severity, a
    /// verdict-less window leaves the default box background (no hole and
    /// no stray color).
    #[test]
    fn subagent_box_padding_rows_follow_nearest_window_color() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = std::time::Duration::ZERO;
        state.text_regions_w = 38;
        // First window carries a red verdict; last one carries none.
        state.start_pty("subagent: kilo".to_string(), None);
        state
            .complete_last_pty("<!-- severity: red -->\n\n## Critical\n\nBug found.\n".to_string());
        state.start_pty("subagent: opencode".to_string(), None);
        state.complete_last_pty("report line\n".to_string());
        // Direct renderer call with a known geometry.
        let max_h = 12u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, max_h));
        render_subagent_section(&mut buf, 0, 0, 50, max_h, &mut state, &theme, false);

        let box_y = 1u16; // TOP_GAP
        let top_bg = buf.cell((1u16, box_y)).map(|c| c.bg);
        let bottom_bg = buf.cell((1u16, max_h - 1)).map(|c| c.bg);
        let red_tint = blend(
            theme.background_element,
            severity_rgba(cosh_tools::subagent::severity::Severity::Red),
            SEVERITY_TINT_ALPHA,
        );
        let plain = theme.background_element;
        assert_eq!(
            top_bg,
            Some(rgba_color(red_tint)),
            "top pad = first window's verdict tint"
        );
        assert_eq!(
            bottom_bg,
            Some(rgba_color(plain)),
            "bottom pad = default box background (last window has no verdict)"
        );
    }

    /// Panel state with one section of each kind (todo + bash + subagent).
    fn full_panel_state() -> RightPanelState {
        let mut state = RightPanelState::new();
        state.set_todos(vec![types::TodoItem {
            status: "pending".to_string(),
            content: "task one".to_string(),
        }]);
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("out\n".to_string());
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("report\n".to_string());
        state
    }

    /// The header buttons only appear when TWO OR MORE sections are present:
    /// with a single section there is nothing to prioritize, so the layout
    /// must stay exactly as before (content at the very top, no header row).
    #[test]
    fn header_buttons_appear_only_with_two_or_more_sections() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("out\n".to_string());
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        assert!(
            state.header_button_rect(types::SectionKind::Bash).is_none(),
            "a single section must not show header buttons"
        );
        assert!(
            !row_text(&buf, 0).contains("Bash"),
            "no header row with one section"
        );

        // A second section appears: the header row now carries one button per
        // present section, and the content begins BELOW header + its gap row.
        state.set_todos(vec![types::TodoItem {
            status: "pending".to_string(),
            content: "task one".to_string(),
        }]);
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let header = row_text(&buf, 0);
        assert!(
            header.contains("TODO") && header.contains("Bash"),
            "header must carry one button per present section, got {header:?}"
        );
        assert!(
            state.section_layouts.iter().all(|l| l.top >= 1),
            "sections must start right below the header (their own TOP_GAP row is the margin)"
        );
    }

    /// Between two consecutive section buttons the header paints the faint
    /// `·` delimiter in the dim band color (derived from the boxes' own
    /// background) — a pure divider, CENTERED between the labels: one empty
    /// column on each side. It rides in the middle spare column — a click on
    /// it must fall through — and NEVER appears before the first section
    /// button or before the mixed button.
    #[test]
    fn header_separator_sits_between_section_buttons_only() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let buttons = state.header_buttons().to_vec();
        let section_buttons: Vec<_> = buttons
            .iter()
            .filter(|b| b.target.is_some())
            .copied()
            .collect();
        assert!(section_buttons.len() >= 2, "three sections present");
        let top = section_buttons[0].top;

        // Every consecutive pair of section buttons carries the delimiter in
        // the MIDDLE of the three spare columns between them, painted in the
        // dim band color.
        let sep_fg = rgba_color(header_band_color(&theme));
        for pair in section_buttons.windows(2) {
            let (prev, next) = (pair[0], pair[1]);
            assert_eq!(
                next.x0,
                prev.x1 + 3,
                "the layout leaves exactly three spare columns between buttons"
            );
            // The delimiter cell itself, one empty column on EACH side.
            // (`next.x0` itself belongs to the NEXT button's hit rect and is
            // legitimately clickable, so it is not probed here.)
            for x in [next.x0 - 2, next.x0 - 1] {
                let cell = buf.cell((x, top)).expect("separator cell");
                if x == next.x0 - 2 {
                    assert_eq!(cell.symbol(), types::HEADER_SECTION_SEPARATOR);
                    assert_eq!(
                        cell.fg, sep_fg,
                        "the delimiter must be font-painted in the dim band color"
                    );
                    // The divider is NOT a button: a click on its column is
                    // consumed by nothing.
                    assert!(
                        !state.header_click(x, top),
                        "the separator column must not be clickable"
                    );
                } else {
                    assert_ne!(
                        cell.symbol(),
                        types::HEADER_SECTION_SEPARATOR,
                        "the delimiter must be centered: col {x} must stay empty"
                    );
                    assert!(
                        !state.header_click(x, top),
                        "the spare columns must not be clickable"
                    );
                }
            }
        }

        // No delimiter before the FIRST section button or before the mixed
        // button — it only separates consecutive section buttons.
        let mixed = buttons
            .iter()
            .find(|b| b.target.is_none())
            .expect("mixed button present");
        for x in [section_buttons[0].x0 - 1, mixed.x0 - 1] {
            let cell = buf.cell((x, top)).expect("edge cell");
            assert_ne!(
                cell.symbol(),
                types::HEADER_SECTION_SEPARATOR,
                "no delimiter before the first section button or the mixed button"
            );
        }
    }

    /// Clicking a section's header button maximizes it: that section becomes
    /// the SOLE owner of the panel area and the other sections vanish.
    #[test]
    fn bash_button_maximizes_bash_over_the_whole_panel() {
        let theme = test_theme();
        let mut state = full_panel_state();
        // Long bash output so the maximized box actually fills the area
        // (a fresh bash session: the last PTY above is the subagent's).
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty(
            (0..40)
                .map(|i| format!("bash line {i}\n"))
                .collect::<String>(),
        );
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let (x0, x1, top, _bottom) = state
            .header_button_rect(types::SectionKind::Bash)
            .expect("bash button present with 3 sections");
        assert!(
            !state.header_click(x0.saturating_sub(1), top),
            "a click left of the buttons must not be consumed"
        );
        assert!(state.header_click((x0 + x1) / 2, top));
        assert_eq!(
            state.maximized_section,
            Some(types::SectionKind::Bash),
            "the clicked button maximizes its section"
        );

        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let text: String = (0..30).map(|y| row_text(&buf, y)).collect();
        assert!(text.contains("bash line"), "bash owns the panel");
        assert!(!text.contains("task one"), "todo is hidden while maximized");
        assert!(
            !text.contains("subagent:"),
            "subagent is hidden while maximized"
        );
        let band = state
            .section_layouts
            .iter()
            .find(|l| l.kind == types::SectionKind::Bash)
            .expect("bash rendered");
        assert_eq!(
            band.top, 1,
            "maximized section starts right below the header row"
        );
        assert!(
            band.bottom >= 29,
            "maximized section must fill the panel down to the bottom margin, got {band:?}"
        );
    }

    /// The hint-only mixed button is ALWAYS present while two or more boxes
    /// exist — including while a section owns the panel — so switching the
    /// owner costs ONE click. It sits at the RIGHT edge of the header, apart
    /// from the section buttons, and paints no background: a bare dim
    /// band-colored "F4" on the panel background.
    #[test]
    fn mixed_button_stays_visible_and_right_aligned_while_maximized() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let (x0, x1, top, _bottom) = state
            .header_button_rect(types::SectionKind::Bash)
            .expect("bash button present");
        assert!(state.header_click((x0 + x1) / 2, top));
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        // While maximized the section buttons REMAIN (one click to switch
        // the owner) and the mixed hint remains too.
        let header = row_text(&buf, 0);
        assert!(
            header.contains("Bash") && header.contains(types::HEADER_MIXED_HINT),
            "maximized header must keep the section buttons and the mixed hint, got {header:?}"
        );
        assert!(state.header_button_rect(types::SectionKind::Bash).is_some());

        // The mixed button is the RIGHTMOST button, separated from the rest.
        let mixed = state
            .header_buttons()
            .iter()
            .find(|b| b.target.is_none())
            .copied()
            .expect("mixed button present while maximized");
        let others_x1 = state
            .header_buttons()
            .iter()
            .filter(|b| b.target.is_some())
            .map(|b| b.x1)
            .max()
            .expect("section buttons present while maximized");
        assert!(
            mixed.x0 > others_x1,
            "mixed button must sit right of the section buttons, got mixed {}..{} vs others ..{others_x1}",
            mixed.x0,
            mixed.x1
        );
        assert_eq!(
            mixed.x1,
            state
                .header_buttons()
                .iter()
                .map(|b| b.x1)
                .max()
                .unwrap_or(0),
            "the mixed button must touch the header's right edge"
        );

        // No background pill: the hint keeps the panel's own background and
        // the dim band color — the resting look the section buttons share.
        let hint_x = (mixed.x0 + mixed.x1) / 2;
        let cell = buf.cell((hint_x, top)).expect("hint cell painted");
        assert_eq!(
            cell.bg,
            rgba_color(theme.background_panel),
            "the mixed button must paint NO background of its own"
        );
        assert_eq!(
            cell.fg,
            rgba_color(header_band_color(&theme)),
            "the mixed hint must be dim band-colored (never white)"
        );

        // One click on the mixed hint brings the mixed view back.
        assert!(state.header_click((mixed.x0 + mixed.x1) / 2, top));
        assert_eq!(state.maximized_section, None, "mixed brings mixed back");

        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let text: String = (0..30).map(|y| row_text(&buf, y)).collect();
        assert!(text.contains("task one"), "todo is back");
        assert!(text.contains("subagent:"), "subagent is back");
        assert!(text.contains("out"), "bash is back");
    }

    /// The owner section's button carries a FIXED highlight color after the
    /// click, so the user can see at a glance which section is selected; the
    /// highlight moves when another section button is clicked and disappears
    /// when the mixed button is clicked. The mixed button itself NEVER takes
    /// the highlight.
    #[test]
    fn selected_section_button_is_highlighted_and_follows_the_clicks() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let click = |state: &mut RightPanelState, kind: types::SectionKind| {
            let (x0, x1, top, _) = state
                .header_button_rect(kind)
                .unwrap_or_else(|| panic!("{kind:?} button present"));
            assert!(state.header_click((x0 + x1) / 2, top));
        };

        // Nothing is selected in the mixed view: no button carries the
        // white focus color yet — every resting label is in the dim band
        // color (the same one the delimiter between them uses).
        let band = rgba_color(header_band_color(&theme));
        for kind in [
            types::SectionKind::Todo,
            types::SectionKind::Bash,
            types::SectionKind::Subagent,
        ] {
            let (x0, x1, top, _) = state.header_button_rect(kind).expect("button present");
            assert!(
                !column_fg_has(&buf, x0, x1, top, rgba_color(theme.text)),
                "no button may carry the white focus color before any click"
            );
            assert!(
                column_fg_has(&buf, x0, x1, top, band),
                "every resting button carries the dim band color"
            );
        }

        // Clicking Bash selects it: its label turns WHITE while the other
        // section buttons keep the dim band color.
        click(&mut state, types::SectionKind::Bash);
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let (bx0, bx1, top, _) = state
            .header_button_rect(types::SectionKind::Bash)
            .expect("bash button");
        let (tx0, tx1, _, _) = state
            .header_button_rect(types::SectionKind::Todo)
            .expect("todo button");
        assert!(
            column_fg_has(&buf, bx0, bx1, top, rgba_color(theme.text)),
            "the clicked section's label must turn white"
        );
        assert!(
            !column_fg_has(&buf, tx0, tx1, top, rgba_color(theme.text))
                && column_fg_has(&buf, tx0, tx1, top, band),
            "the unselected buttons keep the dim band color"
        );

        // Clicking Subagent moves the white label; Bash returns to the band.
        click(&mut state, types::SectionKind::Subagent);
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let (sx0, sx1, stop, _) = state
            .header_button_rect(types::SectionKind::Subagent)
            .expect("subagent button");
        assert!(
            column_fg_has(&buf, sx0, sx1, stop, rgba_color(theme.text)),
            "the white label must follow the newest click"
        );
        assert!(
            !column_fg_has(&buf, bx0, bx1, top, rgba_color(theme.text))
                && column_fg_has(&buf, bx0, bx1, top, band),
            "the dethroned button returns to the band color"
        );

        // Clicking the mixed hint clears the highlight — and the mixed
        // button itself never takes it.
        let mixed = state
            .header_buttons()
            .iter()
            .find(|b| b.target.is_none())
            .copied()
            .expect("mixed button present");
        assert!(state.header_click((mixed.x0 + mixed.x1) / 2, stop));
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        for kind in [
            types::SectionKind::Todo,
            types::SectionKind::Bash,
            types::SectionKind::Subagent,
        ] {
            let (x0, x1, top, _) = state.header_button_rect(kind).expect("button present");
            assert!(
                !column_fg_has(&buf, x0, x1, top, rgba_color(theme.text))
                    && column_fg_has(&buf, x0, x1, top, band),
                "no section button may stay white after the mixed click"
            );
        }
        // The mixed button's own color is INDEPENDENT of the selection: it
        // keeps the dim band color whether or not a section owns the panel.
        // (The focus color would be white, so "never highlighted" is
        // structural — the mixed arm of draw_header_row never reads
        // `selected` — and verified by the unchanged color here.)
        assert!(
            column_fg_has(&buf, mixed.x0, mixed.x1, stop, band)
                && !column_fg_has(&buf, mixed.x0, mixed.x1, stop, rgba_color(theme.text)),
            "the mixed button keeps its own dim band color"
        );

        // Hardening: when the maximized section's content vanishes, the
        // fallback to the mixed view must clear the highlight in the SAME
        // frame — the highlight is derived from the maximized state the
        // layout itself just cleared. Every bash session ends while Bash
        // owns the panel; only the subagent PTY survives, so bash is no
        // longer present and the layout must fall back.
        click(&mut state, types::SectionKind::Bash);
        state.pty_sessions.retain(|p| p.is_subagent());
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        assert_eq!(
            state.maximized_section, None,
            "the vanished section must fall back to the mixed view"
        );
        // The bash button is gone with the section; the surviving buttons
        // carry no highlight after the fallback.
        let (rx0, rx1, rtop, _) = state
            .header_button_rect(types::SectionKind::Subagent)
            .expect("subagent button still present (2 boxes remain)");
        assert!(
            !column_fg_has(&buf, rx0, rx1, rtop, rgba_color(theme.text)),
            "the highlight must clear in the same frame as the fallback"
        );
    }

    /// The section buttons are BARE TEXT on the panel background: no button
    /// carries a background of its own, the gaps between buttons included —
    /// the header row is one uninterrupted stretch of panel background from
    /// the first section button through the mixed hint. Only the FONT color
    /// distinguishes the buttons: an unselected label is ordinary text color,
    /// and the selected one takes the theme's primary.
    #[test]
    fn section_buttons_are_bare_text_on_the_panel_background() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let buttons = state.header_buttons();
        let first = buttons
            .iter()
            .find(|b| b.target == Some(types::SectionKind::Todo))
            .copied()
            .expect("todo button present");
        let mixed = buttons
            .iter()
            .find(|b| b.target.is_none())
            .copied()
            .expect("mixed button present");
        let top = first.top;

        // The whole header stretch is bare panel background: no element
        // fill anywhere, the between-button gaps included.
        let panel = rgba_color(theme.background_panel);
        let element = rgba_color(theme.background_element);
        for x in first.x0..mixed.x1 {
            assert_eq!(
                buf.cell((x, top)).map(|c| c.bg),
                Some(panel),
                "the header row is bare panel background, col {x}"
            );
            assert_ne!(
                buf.cell((x, top)).map(|c| c.bg),
                Some(element),
                "no element fill may remain in the header row, col {x}"
            );
        }
        // An unselected label is in the dim band color on that bare ground.
        let band = rgba_color(header_band_color(&theme));
        assert_eq!(
            buf.cell(((first.x0 + first.x1) / 2, top)).map(|c| c.fg),
            Some(band),
            "the unselected label is the dim band color"
        );
        // The icon cell itself stays bare panel background.
        assert_eq!(
            buf.cell(((mixed.x0 + mixed.x1) / 2, top)).map(|c| c.bg),
            Some(panel),
            "the mixed hint keeps its bare background"
        );
    }

    /// The header buttons read Subagent, Bash, TODO in that order — Subagent
    /// FIRST — and every control carries its function-key hint: F1 AFTER the
    /// Subagent label, F2 after Bash, F3 after TODO, and F4 BEFORE the mixed
    /// glyph, the only hint that sits ahead of its control.
    #[test]
    fn header_buttons_list_subagent_first_with_function_key_hints() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let buttons = state.header_buttons();
        let sub = buttons
            .iter()
            .find(|b| b.target == Some(types::SectionKind::Subagent))
            .copied()
            .expect("subagent button present");
        let bash = buttons
            .iter()
            .find(|b| b.target == Some(types::SectionKind::Bash))
            .copied()
            .expect("bash button present");
        let todo = buttons
            .iter()
            .find(|b| b.target == Some(types::SectionKind::Todo))
            .copied()
            .expect("todo button present");
        let mixed = buttons
            .iter()
            .find(|b| b.target.is_none())
            .copied()
            .expect("mixed button present");

        assert!(
            sub.x0 < bash.x0 && bash.x0 < todo.x0,
            "buttons must read Subagent, Bash, TODO left to right, got sub {} bash {} todo {}",
            sub.x0,
            bash.x0,
            todo.x0
        );
        assert!(
            todo.x1 < mixed.x0,
            "the mixed button stays right of the section buttons"
        );

        let top = sub.top;
        let header = row_text(&buf, top);
        assert!(
            header.contains("Subagent F1"),
            "subagent hint AFTER its label, got {header:?}"
        );
        assert!(
            header.contains("Bash F2"),
            "bash hint AFTER its label, got {header:?}"
        );
        assert!(
            header.contains("TODO F3"),
            "todo hint AFTER its label, got {header:?}"
        );
        let f4_pos = header
            .find("F4")
            .unwrap_or_else(|| panic!("mixed hint F4 must be on the row, got {header:?}"));
        // The bare "F4" is the mixed control itself now — no glyph beside
        // it, so the hint is simply the rightmost label on the row.
        let todo_pos = header
            .find("TODO F3")
            .unwrap_or_else(|| panic!("todo hint must be on the row, got {header:?}"));
        assert!(
            todo_pos < f4_pos,
            "the mixed hint must sit right of the section labels, got {header:?}"
        );
    }

    /// The REAL panel width (42 columns, inner 38) must still fit every
    /// control: the hinted labels made the row longer, and a button that no
    /// longer fits is silently dropped by the build loop — the TODO button
    /// vanished from the real panel while the 50-column test area kept
    /// hiding the regression.
    #[test]
    fn all_section_buttons_fit_the_real_panel_width() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, RIGHT_PANEL_WIDTH, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        for kind in [
            types::SectionKind::Subagent,
            types::SectionKind::Bash,
            types::SectionKind::Todo,
        ] {
            assert!(
                state.header_button_rect(kind).is_some(),
                "{kind:?} button must exist at the real panel width"
            );
        }
        let top = state
            .header_button_rect(types::SectionKind::Subagent)
            .map(|(_, _, top, _)| top)
            .expect("subagent button present");
        let header = row_text(&buf, top);
        assert!(
            header.contains("Subagent F1")
                && header.contains("Bash F2")
                && header.contains("TODO F3"),
            "all three hinted labels must render at the real width, got {header:?}"
        );
        assert!(
            header.contains(types::HEADER_MIXED_HINT),
            "the mixed hint must render at the real width, got {header:?}"
        );
    }

    /// Switching the owner while a section owns the panel costs exactly ONE
    /// click: the section buttons stay registered while maximized, so the
    /// user never has to detour through the mixed view.
    #[test]
    fn switching_owner_while_maximized_takes_a_single_click() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let (bx0, bx1, top, _) = state
            .header_button_rect(types::SectionKind::Bash)
            .expect("bash button present");
        assert!(state.header_click((bx0 + bx1) / 2, top));
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        assert_eq!(state.maximized_section, Some(types::SectionKind::Bash));

        // One click on the Subagent button swaps the owner directly.
        let (sx0, sx1, stop, _) = state
            .header_button_rect(types::SectionKind::Subagent)
            .expect("subagent button stays registered while maximized");
        assert_eq!(stop, top, "all buttons share the header row");
        assert!(state.header_click((sx0 + sx1) / 2, stop));
        assert_eq!(
            state.maximized_section,
            Some(types::SectionKind::Subagent),
            "the owner must switch in a single click, no mixed-view detour"
        );

        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        let text: String = (0..30).map(|y| row_text(&buf, y)).collect();
        assert!(text.contains("subagent:"), "subagent now owns the panel");
        assert!(!text.contains("bash line"), "bash was dethroned");
    }

    /// A maximized section that loses its content must fall back to the mixed
    /// view instead of rendering an empty maximized panel.
    #[test]
    fn maximized_section_that_disappears_falls_back_to_mixed() {
        let theme = test_theme();
        let mut state = RightPanelState::new();
        state.set_todos(vec![types::TodoItem {
            status: "pending".to_string(),
            content: "task one".to_string(),
        }]);
        state.start_pty("ls".to_string(), None);
        state.complete_last_pty("out\n".to_string());
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let (x0, x1, top, _bottom) = state
            .header_button_rect(types::SectionKind::Todo)
            .expect("todo button present");
        assert!(state.header_click((x0 + x1) / 2, top));
        assert_eq!(state.maximized_section, Some(types::SectionKind::Todo));

        // The todo list is cleared while maximized on it.
        state.set_todos(vec![]);
        buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);
        assert_eq!(
            state.maximized_section, None,
            "a vanished section must fall back to the mixed view"
        );
        let text: String = (0..30).map(|y| row_text(&buf, y)).collect();
        assert!(text.contains("out"), "bash is visible again");
    }

    /// The header keeps the EXISTING one-row margin below it: row 1 must stay
    /// bare panel background. The margin IS the section's own TOP_GAP row, so
    /// section bands legitimately BEGIN there — what may never occupy the row
    /// is a section BOX (its content or its element background).
    #[test]
    fn header_row_keeps_one_blank_row_below_it() {
        let theme = test_theme();
        let mut state = full_panel_state();
        let area = Rect::new(0, 0, 50, 30);
        let mut buf = Buffer::empty(area);
        render_right_panel(&mut buf, area, &mut state, &theme, 120);

        let panel_bg = rgba_color(theme.background_panel);
        for x in 0u16..50 {
            assert_eq!(
                buf.cell((x, 1)).map(|c| c.bg),
                Some(panel_bg),
                "row 1 must stay bare panel background (the margin below the header), col {x}"
            );
        }
        assert!(
            state.section_layouts.iter().all(|l| l.top >= 1),
            "bands begin at the margin row, whose blank look comes from their TOP_GAP"
        );
        // No section may paint its ELEMENT background on the margin row: the
        // boxes themselves (content + framing) start at row 2.
        let element_bg = rgba_color(theme.background_element);
        for x in 0u16..50 {
            assert_ne!(
                buf.cell((x, 1)).map(|c| c.bg),
                Some(element_bg),
                "no section box may fill the margin row with its own background, col {x}"
            );
        }
    }
}
