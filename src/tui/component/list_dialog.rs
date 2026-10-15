//! The shared SEARCHABLE LIST DIALOG component — the cosh-standard picker
//! box (`/models`, the theme list) extracted from `ui::dialogs` so every
//! list dialog renders, scrolls and filters ONE way.
//!
//! The component owns the three behaviors every cosh list box shares:
//!
//! 1. **Filter** ([`FilterState`]) — the "Search" line: chars append,
//!    Backspace pops, every change resets the selection to the first row
//!    and lights the shared [`Cursor`]. Matching is the cosh convention
//!    (case-insensitive `contains`, checked per row by the OWNER — the
//!    component never sees the row source).
//! 2. **Cursor-follow scroll** ([`ListBoxLayout::scroll_offset_for`]) —
//!    the selected row always stays inside the visible window; the window
//!    moves only once the selection falls past the last visible row. The
//!    SAME arithmetic backs the renderer, the keyboard nav and the mouse
//!    hit-test, so the painted highlight, the wheel and a click always
//!    agree (and the selection can never slide off-screen — the box leaks
//!    past the terminal only if the terminal itself does).
//! 3. **Mouse row mapping** ([`FilterState::mouse_hit`]) — click Y → the
//!    visible row → the visual row index, through the window the
//!    renderer painted.
//!
//! The component is RENDER-BY-GEOMETRY, not render-by-content: the caller
//! maps its own data model onto [`ListRow`]s (grouped models with provider
//! headers, flat config values …) and keeps ownership of what confirming
//! one does; the component paints the cosh-standard box chrome (borderless
//! `background_element` surface, bold title + right-aligned `esc`, Search
//! line, rows from `y+3`) and all the geometry. No list dialog may
//! re-implement this arithmetic a second time.

use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line_compact;
use cosh_tui::core::lib::rgba::RGBA;

/// One painted row of the shared list box. The caller maps its own data
/// model onto rows; `header` rows are scope titles (a model list grouped
/// by provider): rendered muted+bold full width and NEVER selectable —
/// they occupy visual rows but no selection index.
#[derive(Debug, Clone)]
pub struct ListRow {
    /// Scope header (muted+bold, not selectable).
    pub header: bool,
    /// Primary label (model id, value name, theme name …).
    pub label: String,
    /// Secondary text after the label (the value id for config boxes) —
    /// it shares the row's foreground. Empty draws nothing extra.
    pub detail: String,
    /// The row currently active at the harness (the ● indicator).
    pub current: bool,
    /// Draw the label in the theme's accent color while the row is NOT
    /// selected (the model picker's ★ auto fallback-row treatment).
    pub accent: bool,
    /// Color chips right-anchored at the row's edge (the theme picker's
    /// swatch strip); the label truncates to leave them room. Empty draws
    /// nothing extra.
    pub swatches: Vec<RGBA>,
}

impl ListRow {
    /// A plain selectable row.
    #[must_use]
    pub fn item(label: impl Into<String>, detail: impl Into<String>, current: bool) -> Self {
        Self {
            header: false,
            label: label.into(),
            detail: detail.into(),
            current,
            accent: false,
            swatches: Vec::new(),
        }
    }

    /// A scope header row.
    #[must_use]
    pub fn scope(label: impl Into<String>) -> Self {
        Self {
            header: true,
            label: label.into(),
            detail: String::new(),
            current: false,
            accent: false,
            swatches: Vec::new(),
        }
    }

    /// A selectable row whose label carries the accent color while
    /// unselected (the model picker's ★ auto row).
    #[must_use]
    pub fn accent_item(label: impl Into<String>, current: bool) -> Self {
        Self {
            accent: true,
            ..Self::item(label, "", current)
        }
    }

    /// A selectable row with a right-anchored swatch strip (the theme
    /// picker's rows).
    #[must_use]
    pub fn swatch_item(label: impl Into<String>, current: bool, swatches: Vec<RGBA>) -> Self {
        Self {
            swatches,
            ..Self::item(label, "", current)
        }
    }
}

/// Painted geometry of one list box: ONE computation for the renderer, the
/// mouse hit-test and the keyboard scroll bounds — the pattern the text
/// input dialogs already follow (`text_input_dialog_box`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListBoxLayout {
    /// Top-left of the dialog box.
    pub x: u16,
    pub y: u16,
    pub width: u16,
    /// Full box height: header rows (title + search + gap, +1 with a
    /// subtitle), the visible rows, and one row of bottom padding.
    pub height: u16,
    /// Visible list rows (never 0).
    pub max_visible: usize,
    /// The box carries a subtitle line above the search line.
    pub subtitle: bool,
    /// The cursor-follow scroll window this geometry was computed with
    /// (the offset the renderer MUST paint — and the mouse MUST hit-test —
    /// so the three can never drift).
    pub scroll_offset: usize,
}

impl ListBoxLayout {
    /// The screen Y where the list rows start (below the header chrome).
    #[must_use]
    pub fn list_top(&self) -> u16 {
        self.y + 3 + u16::from(self.subtitle)
    }

    /// The screen Y of visual row `index` (scroll offset applied).
    #[must_use]
    pub fn row_y(&self, index: usize) -> u16 {
        self.list_top() + (index - self.scroll_offset) as u16
    }

    /// The screen rect of the whole box (outside → dismiss for the mouse).
    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }

    /// The header interior: (x, y, width) with the 4-column padding the
    /// title/search/esc rows use.
    #[must_use]
    pub fn header(&self) -> (u16, u16, u16) {
        (
            self.x + HEADER_PAD,
            self.y,
            self.width.saturating_sub(HEADER_PAD * 2),
        )
    }

    /// The cursor-follow scroll offset for a `rows`-row visual list whose
    /// selected row sits at `visual_selected`: scroll only once the
    /// selection falls past the last visible row, then clamp to the tail.
    /// The arithmetic the ModelList renderer and its mouse handler have
    /// always run — extracted so every box shares it.
    #[must_use]
    pub fn scroll_offset_for(visual_selected: usize, rows: usize, max_visible: usize) -> usize {
        if rows == 0 || max_visible == 0 {
            return 0;
        }
        let offset = if visual_selected >= max_visible && max_visible < rows {
            visual_selected - (max_visible - 1)
        } else {
            0
        };
        offset.min(rows.saturating_sub(max_visible))
    }
}

/// Search-line inset: 4 columns of padding each side (the original
/// ModelList chrome).
const HEADER_PAD: u16 = 4;

/// Filter + selection state of one searchable list box. Lives in the
/// dialog variant (the models/theme boxes keep their `filter` field shape)
/// or beside the dialog instance — the component only requires this state,
/// not a place for it.
#[derive(Debug, Clone, Default)]
pub struct FilterState {
    /// The search text (empty → placeholder "Search").
    pub filter: String,
    /// Selected index into the CALLER'S filtered row space.
    pub selected: usize,
    /// The shared blinking cursor (the same component every text surface
    /// uses).
    pub cursor: Cursor,
}

impl FilterState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one character and reset the selection to the first row —
    /// a fresh filter must keep the cosh behavior.
    pub fn push_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.selected = 0;
        self.cursor.note_activity();
    }

    /// Pop one character (Backspace) with the same selection reset.
    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.selected = 0;
        self.cursor.note_activity();
    }

    /// Case-insensitive `contains` match of `haystack` against the filter —
    /// the cosh filter convention, shared by the model and theme pickers.
    #[must_use]
    pub fn matches(&self, haystack: &str) -> bool {
        matches_filter(&self.filter, haystack)
    }

    /// Clamp a selection into the filtered row count (the safety net when
    /// the row list shrank under the cursor).
    #[must_use]
    pub fn clamped(&self, len: usize) -> usize {
        len.saturating_sub(1).min(self.selected)
    }

    /// Move the selection one row UP, wrapping to the last row.
    pub fn select_prev(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 || self.selected >= len {
            len - 1
        } else {
            self.selected - 1
        };
    }

    /// Move the selection one row DOWN, wrapping to the first row.
    pub fn select_next(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.selected = if self.selected + 1 >= len {
            0
        } else {
            self.selected + 1
        };
    }

    /// Full box geometry for a visual row list (`rows`) whose selected row
    /// sits at `visual_selected`. `max_visible_cap` caps the window (the
    /// compact pickers cap at 10; an uncapped box passes `usize::MAX`);
    /// `max_w` is the box's preferred width (ModelList 50, config box 60)
    /// and `min_w` its responsive floor (ModelList 30, config box 28).
    /// Everything else is the shared chrome arithmetic.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn layout(
        &self,
        visual_selected: usize,
        rows: usize,
        max_visible_cap: usize,
        max_w: u16,
        min_w: u16,
        area: Rect,
        subtitle: bool,
    ) -> ListBoxLayout {
        compute_layout(
            visual_selected,
            rows,
            max_visible_cap,
            max_w,
            min_w,
            area,
            subtitle,
        )
    }

    /// Mouse hit-test against a painted layout: outside the box →
    /// [`MouseHit::Outside`], on the header's "esc" → [`MouseHit::Dismiss`],
    /// on a list row → [`MouseHit::Row`] with the VISUAL row index (scroll
    /// offset applied — the caller maps it onto its own model, skipping
    /// header rows). Everything else inside is [`MouseHit::Inside`].
    #[must_use]
    pub fn mouse_hit(&self, layout: &ListBoxLayout, x: u16, y: u16) -> MouseHit {
        hit_test(layout, x, y)
    }
}

/// The shared box geometry, free-standing so arms that keep their filter
/// OUTSIDE a [`FilterState`] (the `ModelList`/`ThemeList` dialog variants
/// hold a plain `String` + the instance cursor) share the ONE computation
/// too. Same parameters as [`FilterState::layout`].
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn compute_layout(
    visual_selected: usize,
    rows: usize,
    max_visible_cap: usize,
    max_w: u16,
    min_w: u16,
    area: Rect,
    subtitle: bool,
) -> ListBoxLayout {
    let max_w = max_w.min(area.width.saturating_sub(4));
    let width = max_w.max(min_w).min(area.width.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(width) / 2;

    // Fit the list to the available height: the header chrome (3 rows,
    // 4 with a subtitle) plus one bottom padding row leave the rest.
    let chrome = 4 + u16::from(subtitle);
    let max_visible_height = area.height.saturating_sub(chrome) as usize;
    let max_visible = max_visible_height.min(rows.max(1)).min(max_visible_cap).max(1);
    let height = max_visible as u16 + chrome;
    let y = area.y + area.height.saturating_sub(height) / 2;

    ListBoxLayout {
        x,
        y,
        width,
        height,
        subtitle,
        max_visible,
        scroll_offset: ListBoxLayout::scroll_offset_for(visual_selected, rows, max_visible),
    }
}

/// The shared mouse hit-test, free-standing for the same reason as
/// [`compute_layout`].
#[must_use]
pub fn hit_test(layout: &ListBoxLayout, x: u16, y: u16) -> MouseHit {
    let rect = layout.rect();
    if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
        return MouseHit::Outside;
    }
    let (header_x, header_y, header_w) = layout.header();
    let esc_label = "esc";
    let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
    if y == header_y && x >= esc_x && x < esc_x + esc_label.len() as u16 {
        return MouseHit::Dismiss;
    }
    let list_top = layout.list_top();
    if y >= list_top {
        let row = (y - list_top) as usize;
        if row < layout.max_visible {
            return MouseHit::Row(layout.scroll_offset + row);
        }
    }
    MouseHit::Inside
}

/// Filter edit on the DialogInstance shape (a plain filter `String` plus
/// the instance's shared cursor — the `ModelList`/`ThemeList` variants'
/// field layout): append one char, reset the selection to the first row,
/// blink the cursor. The ONE definition [`FilterState::push_char`] shares.
pub fn push_filter(filter: &mut String, selected: &mut usize, cursor: &mut Cursor, ch: char) {
    filter.push(ch);
    *selected = 0;
    cursor.note_activity();
}

/// Backspace on the DialogInstance shape — the ONE definition
/// [`FilterState::pop_char`] shares.
pub fn pop_filter(filter: &mut String, selected: &mut usize, cursor: &mut Cursor) {
    filter.pop();
    *selected = 0;
    cursor.note_activity();
}

/// The cosh filter convention, free-standing: case-insensitive `contains`
/// (an empty filter matches everything). [`FilterState::matches`] delegates
/// here; dialog arms that match several fields (model name + provider,
/// config value id + display name) call it per field and OR the results.
#[must_use]
pub fn matches_filter(filter: &str, haystack: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let needle = filter.to_lowercase();
    haystack.to_lowercase().contains(&needle)
}

/// Where a mouse event landed relative to a painted list box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseHit {
    /// Outside the box entirely (the caller dismisses).
    Outside,
    /// On the header's "esc" label (the caller dismisses).
    Dismiss,
    /// On visual row `usize` (scroll offset applied; may be a header).
    Row(usize),
    /// Inside the box but on no interactive row (consumed, no-op).
    Inside,
}

/// Paint the SHARED box chrome: the borderless `background_element`
/// surface, the bold title with the right-aligned "esc", the optional
/// subtitle line, and the Search line (placeholder or filter text with the
/// blinking cursor) — pixel-identical to what the ModelList renderer has
/// always drawn.
pub fn render_chrome(
    buf: &mut Buffer,
    layout: &ListBoxLayout,
    theme: &Theme,
    now: SystemTime,
    filter: &FilterState,
    title: &str,
    subtitle: Option<&str>,
) {
    render_chrome_raw(
        buf,
        layout,
        theme,
        now,
        &filter.filter,
        &filter.cursor,
        title,
        subtitle,
    );
}

/// The chrome behind [`render_chrome`], free-standing so the arms that keep
/// their filter OUTSIDE a [`FilterState`] (the `ModelList`/`ThemeList`
/// variants hold a plain `String` + the instance cursor) paint the SAME
/// header with their own state.
#[allow(clippy::too_many_arguments)]
pub fn render_chrome_raw(
    buf: &mut Buffer,
    layout: &ListBoxLayout,
    theme: &Theme,
    now: SystemTime,
    filter_text: &str,
    cursor: &Cursor,
    title: &str,
    subtitle: Option<&str>,
) {
    let bg = rgba_color(theme.background_element);
    let surface = layout.rect();
    for y in surface.y..surface.bottom() {
        for x in surface.x..surface.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(bg));
            }
        }
    }

    let (header_x, header_y, header_w) = layout.header();

    // Line 0: bold title + right-aligned "esc".
    let title_style = Style::default()
        .fg(rgba_color(theme.text))
        .add_modifier(Modifier::BOLD);
    draw_text_line_compact(buf, title, header_x, header_y, header_w, title_style);
    let esc_label = "esc";
    let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
    draw_text_line_compact(
        buf,
        esc_label,
        esc_x,
        header_y,
        header_w,
        Style::default().fg(rgba_color(theme.text_muted)),
    );

    // Optional subtitle, then the Search line right below it.
    let search_y = match subtitle {
        Some(subtitle) => {
            draw_text_line_compact(
                buf,
                subtitle,
                header_x,
                header_y + 1,
                header_w,
                Style::default().fg(rgba_color(theme.text_muted)),
            );
            header_y + 2
        }
        None => header_y + 1,
    };

    // The Search line: the ModelList's exact rendering (placeholder +
    // cursor over its first char, or filter text + trailing cursor).
    let cursor_state = cursor.current_state(now);
    if filter_text.is_empty() {
        draw_text_line_compact(
            buf,
            "Search",
            header_x,
            search_y,
            header_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
        draw_search_cursor(buf, header_x, search_y, cursor_state, theme, bg);
    } else {
        draw_text_line_compact(
            buf,
            filter_text,
            header_x,
            search_y,
            header_w,
            Style::default().fg(rgba_color(theme.text)).bg(bg),
        );
        draw_search_cursor(
            buf,
            header_x + filter_text.len() as u16,
            search_y,
            cursor_state,
            theme,
            bg,
        );
    }
}

/// One blinking cursor cell on the Search line: solid primary while the
/// cursor is On, the muted medium shade otherwise (the ModelList's
/// two-state cursor, extracted with the chrome).
fn draw_search_cursor(
    buf: &mut Buffer,
    cell_x: u16,
    y: u16,
    cursor_state: CursorState,
    theme: &Theme,
    bg: Color,
) {
    if let Some(cell) = buf.cell_mut((cell_x, y)) {
        match cursor_state {
            CursorState::On => {
                cell.set_char('\u{2588}');
                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg));
            }
            CursorState::Off | CursorState::Blur => {
                cell.set_char('\u{2592}');
                cell.set_style(Style::default().fg(rgba_color(theme.text_muted)).bg(bg));
            }
        }
    }
}

/// Paint the visible window of `rows` through `layout`: rows outside
/// `[scroll_offset, scroll_offset + max_visible)` are skipped, `selected`
/// is the VISUAL index carrying the primary fill, and header rows render
/// muted+bold full width (never selectable, never highlighted).
pub fn render_rows(
    buf: &mut Buffer,
    layout: &ListBoxLayout,
    theme: &Theme,
    rows: &[ListRow],
    selected: usize,
) {
    let bg_element = rgba_color(theme.background_element);
    let sel_bg = rgba_color(theme.primary);
    for (index, row) in rows
        .iter()
        .enumerate()
        .skip(layout.scroll_offset)
        .take(layout.max_visible)
    {
        let y = layout.row_y(index);
        let list_x = layout.x + 1;
        let list_w = layout.width.saturating_sub(2);

        if row.header {
            let header_style = Style::default()
                .fg(rgba_color(theme.text_muted))
                .add_modifier(Modifier::BOLD);
            draw_text_line_compact(buf, &row.label, list_x, y, list_w, header_style);
            continue;
        }

        // Full-row background first (the primary fill only under the
        // cursor).
        for cx in list_x..list_x + list_w {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(if index == selected {
                    sel_bg
                } else {
                    bg_element
                }));
            }
        }

        // ● indicator for the row active at the harness (accent unselected,
        // luminance-adaptive under the selection) — the ModelList treatment.
        let (indicator_fg, indicator_ch) = if row.current {
            if index == selected {
                (selected_foreground(theme.primary), '\u{25cf}')
            } else {
                (rgba_color(theme.accent), '\u{25cf}')
            }
        } else {
            (Color::Reset, ' ')
        };
        if let Some(cell) = buf.cell_mut((list_x, y)) {
            cell.set_char(indicator_ch);
            cell.set_style(Style::default().fg(indicator_fg).bg(if index == selected {
                sel_bg
            } else {
                bg_element
            }));
        }
        if let Some(cell) = buf.cell_mut((list_x + 1, y)) {
            cell.set_char(' ');
            cell.set_style(Style::default().bg(if index == selected {
                sel_bg
            } else {
                bg_element
            }));
        }

        // Label + optional detail, with the selection's luminance-adaptive
        // foreground across the whole row (the compact pickers' rule). An
        // `accent` row carries the accent color while unselected (the ★
        // auto fallback row of the model picker).
        let (fg, bg) = if index == selected {
            (selected_foreground(theme.primary), sel_bg)
        } else if row.accent {
            (rgba_color(theme.accent), bg_element)
        } else {
            (rgba_color(theme.text), bg_element)
        };
        let style = Style::default().fg(fg).bg(bg);

        // Swatch chips right-anchor at the row's edge (the theme picker's
        // strip lines up in a clean column across rows); the label
        // truncates to leave the chips plus one gap cell room.
        let strip_w = crate::ui::theme_swatches::swatches_width(row.swatches.len());
        let gap_after_name = u16::from(!row.swatches.is_empty());
        let label_w = list_w
            .saturating_sub(2)
            .saturating_sub(strip_w)
            .saturating_sub(gap_after_name);
        draw_text_line_compact(buf, &row.label, list_x + 2, y, label_w, style);
        if strip_w > 0 {
            crate::ui::theme_swatches::draw_theme_swatches(
                buf,
                list_x + list_w.saturating_sub(strip_w),
                y,
                &row.swatches,
                bg,
            );
        }
        if !row.detail.is_empty() {
            let detail_offset = 2 + row.label.chars().count() as u16;
            draw_text_line_compact(
                buf,
                &row.detail,
                list_x + detail_offset,
                y,
                list_w.saturating_sub(detail_offset),
                style,
            );
        }
    }
}

/// Black-on-primary when the primary fill is bright, white otherwise — the
/// selection contrast rule every cosh highlight follows.
#[must_use]
pub fn selected_foreground(primary: RGBA) -> Color {
    let (r, g, b, _) = primary.to_ints();
    let lum = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
    if lum > 0.5 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_offset_follows_the_cursor_only_past_the_window() {
        // 30 rows, 10 visible: rows 0..=9 stay put while the cursor walks.
        for i in 0..10 {
            assert_eq!(ListBoxLayout::scroll_offset_for(i, 30, 10), 0, "row {i}");
        }
        // Row 10 pushes the window by one; the cursor stays at the bottom.
        assert_eq!(ListBoxLayout::scroll_offset_for(10, 30, 10), 1);
        assert_eq!(ListBoxLayout::scroll_offset_for(29, 30, 10), 20);
        // A short list inside one window never scrolls.
        assert_eq!(ListBoxLayout::scroll_offset_for(9, 10, 10), 0);
        // Degenerate shapes stay at zero.
        assert_eq!(ListBoxLayout::scroll_offset_for(5, 0, 10), 0);
        assert_eq!(ListBoxLayout::scroll_offset_for(5, 30, 0), 0);
    }

    #[test]
    fn layout_clamps_to_the_terminal_and_the_row_cap() {
        let state = FilterState::new();
        // A tiny terminal: the box shrinks (responsive, min 28 wide) and
        // the window shrinks with the height — but never past the row
        // count (3 rows → a 3-row window even in a tall-enough area).
        let area = Rect::new(0, 0, 30, 12);
        let layout = state.layout(0, 3, 10, 60, 28, area, false);
        assert_eq!(layout.width, 28);
        assert_eq!(layout.max_visible, 3);
        // 500 rows under a 10-row cap: the window is the cap, and the box
        // stays fully on screen instead of leaking off the bottom.
        let tall_area = Rect::new(0, 0, 100, 30);
        let layout = state.layout(499, 500, 10, 60, 28, tall_area, false);
        assert_eq!(layout.max_visible, 10);
        assert_eq!(layout.height, 14);
        assert!(layout.y + layout.height <= tall_area.height);
        // The cursor rides the LAST visible row (499 is the tail of 500).
        assert_eq!(layout.scroll_offset, 490);
        // A subtitle consumes one chrome row from the window and adds one
        // to the box.
        let layout = state.layout(499, 500, 10, 60, 28, tall_area, true);
        assert_eq!(layout.height, 15);
        assert_eq!(layout.list_top(), layout.y + 4);
    }

    #[test]
    fn mouse_hit_maps_rows_through_the_scroll_window() {
        let state = FilterState::new();
        let area = Rect::new(0, 0, 100, 40);
        let layout = state.layout(15, 40, 10, 60, 28, area, false);
        // Selection 15 in a 10-row window → offset 6; the first visible
        // row is visual index 6, at y = layout.y + 3.
        assert_eq!(layout.scroll_offset, 6);
        assert_eq!(
            state.mouse_hit(&layout, layout.x + 5, layout.y + 3),
            MouseHit::Row(6)
        );
        // The last visible row is index 15 — the cursor's own row.
        assert_eq!(
            state.mouse_hit(&layout, layout.x + 5, layout.y + 3 + 9),
            MouseHit::Row(15)
        );
        // One row past the window (the bottom padding): inside, inert.
        assert_eq!(
            state.mouse_hit(&layout, layout.x + 5, layout.y + 3 + 10),
            MouseHit::Inside
        );
        // The esc label dismisses; the title row otherwise does not.
        let (hx, _, hw) = layout.header();
        let esc_x = hx + hw - 3;
        assert_eq!(state.mouse_hit(&layout, esc_x, layout.y), MouseHit::Dismiss);
        assert_eq!(state.mouse_hit(&layout, hx, layout.y), MouseHit::Inside);
        // Far outside the box.
        assert_eq!(state.mouse_hit(&layout, 0, 0), MouseHit::Outside);
    }

    #[test]
    fn filter_chars_reset_the_selection_and_match_case_insensitively() {
        let mut state = FilterState::new();
        state.selected = 7;
        state.push_char('O');
        state.push_char('p');
        assert_eq!(state.selected, 0);
        assert!(state.matches("OpenAI/gpt-6"));
        assert!(!state.matches("z.ai/glm"));
        state.pop_char();
        assert!(state.matches("anthropic/claude"));
        // Empty filter matches everything.
        state.pop_char();
        assert!(state.matches(""));
    }

    #[test]
    fn navigation_wraps_like_the_model_picker() {
        let mut state = FilterState::new();
        state.select_prev(5);
        assert_eq!(state.selected, 4);
        state.select_next(5);
        assert_eq!(state.selected, 0);
        state.selected = 4;
        state.select_next(5);
        assert_eq!(state.selected, 0);
        // Zero rows never move (nor panic).
        state.select_next(0);
        assert_eq!(state.selected, 0);
        // A selection that ran past the tail clamps, it does not panic.
        state.selected = 99;
        assert_eq!(state.clamped(5), 4);
    }
}
