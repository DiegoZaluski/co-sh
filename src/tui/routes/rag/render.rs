//! Rendering logic for the RAG Knowledge Base view.
//!
//! All drawing code extracted from the original monolithic view.rs.

use std::time::SystemTime;

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderables::markdown::markdown_to_visible_text;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::Theme;
use crate::util::markdown::render_markdown;

use super::models::{CreateDbFocus, RagMode};
use super::view::{RagLayout, RagView};

// Named constants

/// Max visible models in the form's expanded list (fits inside the box).
pub(crate) const MAX_VISIBLE_MODELS_IN_FORM: usize = 8;

/// Label text for the "show desc" button (also used in hit-testing).
pub(crate) const SHOW_DESC_BTN_TEXT: &str = " show desc ";

/// Number of padding cells subtracted from label width for model list items.
const MODEL_LABEL_PAD: u16 = 3;

/// Number of padding cells subtracted from label width for collapsed model line.
const MODEL_COLLAPSED_PAD: u16 = 20;

/// Width in cells of the "Name:  " / "Desc:  " field labels. The labels are
/// drawn outside the markdown content, so this is also how many columns the
/// value loses when it wraps.
pub(crate) const FIELD_LABEL_W: u16 = 7;

/// Visible items in the DB picker scroll list.
pub(crate) const DB_PICKER_VISIBLE: usize = 10;

/// Height of the DB picker / create-form popup (when collapsed).
pub(crate) const PICKER_BOX_HEIGHT: u16 = 5;

/// Maximum visible items in the DB picker list.
pub(crate) const DB_PICKER_LIST_VISIBLE: usize = 4;

/// Minimum area width below which rendering/sizing is skipped.
pub(crate) const MIN_CONTENT_WIDTH: u16 = 10;

/// Width of the 🗑 trash emoji in terminal cells.
pub(crate) const TRASH_EMOJI_WIDTH: u16 = 2;

/// Total horizontal padding per side for a DB row
/// (cx: 2 + checkbox: 1 + gap: 1 + right-pad: 2 = 6).
pub(crate) const DB_ROW_H_PADDING: u16 = 6;

// Overlay geometry constants

/// Preview overlay width as percentage of area width.
pub(crate) const PREVIEW_OVERLAY_WIDTH_PCT: u16 = 85;
/// Preview overlay height as percentage of area height.
pub(crate) const PREVIEW_OVERLAY_HEIGHT_PCT: u16 = 80;
/// Minimum width for the preview overlay.
pub(crate) const PREVIEW_OVERLAY_MIN_W: u16 = 40;
/// Minimum height for the preview overlay.
pub(crate) const PREVIEW_OVERLAY_MIN_H: u16 = 10;

/// Description popup width as percentage of area width.
pub(crate) const DESC_POPUP_WIDTH_PCT: u16 = 70;
/// Description popup height as percentage of area height.
pub(crate) const DESC_POPUP_HEIGHT_PCT: u16 = 50;
/// Minimum width for the description popup.
pub(crate) const DESC_POPUP_MIN_W: u16 = 30;
/// Minimum height for the description popup.
pub(crate) const DESC_POPUP_MIN_H: u16 = 5;

// Helpers
fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn fill_rect(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, style: Style) {
    for dy in 0..h {
        let row = y + dy;
        for dx in 0..w {
            if let Some(cell) = buf.cell_mut((x + dx, row)) {
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
    }
}

fn section_title(buf: &mut Buffer, x: u16, y: u16, title: &str, bg: Color, fg: Color) {
    for (i, ch) in title.chars().enumerate() {
        let cx = x + i as u16;
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(fg).bg(bg));
        }
    }
}

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
pub(crate) fn truncate_label(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// Draw a blinking cursor at an explicit cell position on a text input field.
///
/// `cursor` — the [`Cursor`] component with blink state.
/// `now` — the current time for blink timing.
/// `pad` — X position of the field start.
/// `col` — column of the cursor within the field (from the wrapped layout).
/// `y` — Y position of the cursor row.
/// `pad_w` — available width for the field (cursor hidden past the edge).
fn draw_input_cursor_at(
    buf: &mut Buffer,
    cursor: &Cursor,
    now: SystemTime,
    pad: u16,
    col: u16,
    y: u16,
    pad_w: u16,
    theme: &Theme,
    fg: Color,
    muted: Color,
    primary: Color,
) {
    if col > pad_w {
        return;
    }
    if let Some(cell) = buf.cell_mut((pad + col, y)) {
        match cursor.current_state(now) {
            CursorState::On => {
                cell.set_style(Style::default().fg(primary).bg(fg));
            }
            CursorState::Off | CursorState::Blur => {
                cell.set_char('\u{2592}');
                cell.set_style(
                    Style::default()
                        .fg(muted)
                        .bg(rgba_color(theme.background_element)),
                );
            }
        }
    }
}

/// Position (row, col) of the cursor within a wrapped markdown field.
///
/// Markdown markers are stripped first (matching what the renderer displays),
/// then the word-wrap logic of `estimate_height` / the markdown renderer
/// (layout_word_wrap / flush_layout_word) is mirrored so the cursor lands
/// exactly where the text is drawn, including multi-line wrapped fields.
///
/// `text` must be the render text produced by `markdown_render_text` (or any
/// plain text without blank lines).
fn wrapped_cursor_pos(text: &str, pad_w: u16) -> (u16, u16) {
    let mut visible = markdown_to_visible_text(text);
    // markdown_to_visible_text trims trailing whitespace; restore it (spaces,
    // newlines and invisible blank-line spacers) so the cursor sits on the
    // rows the renderer actually draws — e.g. right after a Shift+Enter the
    // caret is on the blank line below, not pulled back up.
    let trimmed = text.trim_end_matches(char::is_whitespace);
    visible.push_str(&text[trimmed.len()..]);
    walk_wrapped_cursor(&visible, pad_w)
}

/// Flush an accumulated word of `word_w` columns starting at `*x`, advancing
/// `*y` by the number of extra lines the word spans (mirrors
/// `flush_layout_word` in cosh-tui's markdown layout estimator).
fn flush_cursor_word(word_w: &mut u16, x: &mut u16, y: &mut u16, pad_w: u16) {
    if *word_w == 0 {
        return;
    }
    if *x + *word_w <= pad_w {
        *x += *word_w;
        *word_w = 0;
        return;
    }
    if *word_w <= pad_w {
        // Whole word fits on a fresh line: wrap it there intact.
        *y += 1;
        *x = *word_w;
        *word_w = 0;
        return;
    }
    // The word alone is wider than the whole line: break at character level.
    let end = u32::from(*x) + u32::from(*word_w);
    let lines = end.div_ceil(u32::from(pad_w)) as u16;
    *y = y.saturating_add(lines.saturating_sub(1));
    let rem = end % u32::from(pad_w);
    *x = if rem == 0 { pad_w } else { rem as u16 };
    *word_w = 0;
}

/// Invisible spacer used to keep blank lines alive inside markdown.
///
/// The CommonMark parser collapses blank lines, so a blank line (e.g. right
/// after pressing Shift+Enter) would disappear. Before handing text to the
/// renderer we insert one of these characters at the start of every line
/// that is otherwise blank. NBSP (`U+00A0`) is not whitespace to cmark, so
/// the line becomes a real paragraph; it renders as a plain space in every
/// terminal. For cursor/click geometry it is treated as width 0 so a blank
/// line's caret sits at column 0 — exactly where the next typed character
/// is drawn.
const MARKDOWN_BLANK_LINE_SPACER: char = '\u{00A0}';

/// Text actually handed to the markdown renderer, with blank lines kept
/// alive by invisible spacers.
///
/// Returns `(render_text, insertions)` where `insertions[i]` is the number
/// of spacer *bytes* inserted before original byte `i`. Callers translate a
/// logical byte offset into the render text with `render_byte_offset`.
pub(crate) fn markdown_render_text(text: &str) -> (String, Vec<usize>) {
    let mut insertions = vec![0usize; text.len() + 1];
    let mut out = String::with_capacity(text.len() + 16);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut at_line_start = true;
    let mut k = 0usize;
    while k < chars.len() {
        let (i, ch) = chars[k];
        if at_line_start && ch.is_whitespace() {
            // Blank line (rest of this line is all whitespace)? Anchor it so
            // cmark keeps it as a real row instead of collapsing it.
            let line_blank = chars[k..]
                .iter()
                .take_while(|(_, c)| *c != '\n')
                .all(|(_, c)| c.is_whitespace());
            if line_blank {
                out.push(MARKDOWN_BLANK_LINE_SPACER);
                insertions[i] += MARKDOWN_BLANK_LINE_SPACER.len_utf8();
            }
        }
        out.push(ch);
        at_line_start = ch == '\n';
        k += 1;
    }
    (out, insertions)
}

/// Map a logical byte offset in `text` to its byte offset in the render text
/// produced by `markdown_render_text(text)`, accounting for the invisible
/// spacers inserted before it.
pub(crate) fn render_byte_offset(insertions: &[usize], pos: usize) -> usize {
    let pos = pos.min(insertions.len().saturating_sub(1));
    pos + insertions[..=pos].iter().sum::<usize>()
}

/// Shared word-wrap walk over visible (marker-stripped) text, mirroring the
/// markdown renderer: `\n` starts a new row, spaces advance the column, and
/// the invisible blank-line spacer occupies no column.
fn walk_wrapped_cursor(visible: &str, pad_w: u16) -> (u16, u16) {
    let mut x = 0u16;
    let mut y = 0u16;
    let mut word_w = 0u16;

    for (grapheme, w) in cosh_tui::core::lib::unicode_util::graphemes_with_width(visible) {
        if grapheme == "\n" {
            flush_cursor_word(&mut word_w, &mut x, &mut y, pad_w);
            y += 1;
            x = 0;
            continue;
        }
        if grapheme == " " {
            flush_cursor_word(&mut word_w, &mut x, &mut y, pad_w);
            if x < pad_w {
                x += 1;
            }
            continue;
        }
        if grapheme == "\u{00A0}" {
            // Invisible spacer: keeps the blank line a real row but occupies
            // no column for cursor/click geometry.
            continue;
        }
        word_w += w;
    }
    flush_cursor_word(&mut word_w, &mut x, &mut y, pad_w);
    (y, x)
}

/// Same as `wrapped_cursor_pos`, but trailing plain spaces are not restored
/// (the renderer does not draw them) — only trailing newlines and blank-line
/// spacers, which are real rows.
fn wrapped_cursor_pos_display(text: &str, pad_w: u16) -> (u16, u16) {
    let mut visible = markdown_to_visible_text(text);
    let trimmed = text.trim_end_matches(char::is_whitespace);
    let tail: String = text[trimmed.len()..]
        .chars()
        .filter(|c| *c != ' ')
        .collect();
    visible.push_str(&tail);
    walk_wrapped_cursor(&visible, pad_w)
}

/// Map a click at (row, col) within a wrapped markdown field to a byte offset
/// into the raw `value`, mirroring where the renderer draws the text.
///
/// This inverts the word-wrap used for the cursor
/// (`wrapped_cursor_pos_display`): every possible caret position is sampled
/// and the one whose drawn cell is closest to the click wins. Ties prefer
/// the later position, so clicking on invisible markdown markers (`## `,
/// `**`) lands after them — typing there keeps the block/inline syntax
/// intact.
/// Visual row of the caret at `byte_pos` in a create-db field value wrapped
/// at `value_w` columns — the same walk that draws the cursor. The Left/Right
/// handlers use this so the caret never crosses a line boundary horizontally
/// (Up/Down move between lines instead).
pub(crate) fn caret_row(value: &str, byte_pos: usize, value_w: u16) -> u16 {
    let prefix = &value[..byte_pos.min(value.len())];
    let (render, insertions) = markdown_render_text(prefix);
    let rp = render_byte_offset(&insertions, prefix.len()).min(render.len());
    wrapped_cursor_pos(&render[..rp], value_w).0
}

pub(crate) fn byte_pos_at_click(value: &str, row: u16, col: u16, pad_w: u16) -> usize {
    let (render, insertions) = markdown_render_text(value);
    let mut best = 0usize;
    let mut best_dist = (u16::MAX, u16::MAX);
    let mut p = 0usize;
    loop {
        let rp = render_byte_offset(&insertions, p).min(render.len());
        let (r, c) = wrapped_cursor_pos_display(&render[..rp], pad_w);
        let dist = (row.abs_diff(r), col.abs_diff(c));
        // Iterating ascending, `<=` naturally prefers the later position on
        // ties (same cell: markers, trailing invisible columns).
        if dist <= best_dist {
            best_dist = dist;
            best = p;
        }
        if p >= value.len() {
            break;
        }
        p += value[p..].chars().next().map_or(1, |c| c.len_utf8());
    }
    best
}

// Render implementation

impl RagView {
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        // Remember the area of this render: handlers (Up/Down visual-line
        // navigation, newline-growth limit) need it to know the field width
        // and how much room the form has.
        self.last_area = Some(area);
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let warning = rgba_color(theme.warning);
        let success = rgba_color(theme.success);
        let primary = rgba_color(theme.primary);
        let panel_bg = rgba_color(theme.background_panel);
        let title_bg = primary;
        let title_fg = rgba_color(theme.background);

        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return;
        }

        // Layout (computed once, shared with mouse hit-testing)
        let layout = self.compute_layout(area);
        let mut y = area.y;

        // BOX 1: Embed Content
        self.render_box1(
            buf, inner_w, layout, &mut y, theme, fg, muted, warning, success, primary, panel_bg,
            title_bg, title_fg,
        );

        // BOX 2: Available Databases
        if y < area.bottom() {
            self.render_box2(
                buf, area, layout, &mut y, theme, fg, muted, warning, success, primary, panel_bg,
                title_bg, title_fg,
            );
        }

        // OVERLAY: Content Preview popup
        self.render_preview_overlay(buf, area, theme, fg, muted, success, primary);
    }

    // Box 1: Embed Content

    fn render_box1(
        &mut self,
        buf: &mut Buffer,
        inner_w: u16,
        layout: RagLayout,
        y: &mut u16,
        theme: &Theme,
        fg: Color,
        muted: Color,
        warning: Color,
        success: Color,
        primary: Color,
        panel_bg: Color,
        title_bg: Color,
        title_fg: Color,
    ) {
        let title = " Embed Content ";
        fill_rect(
            buf,
            layout.inner_x,
            *y,
            inner_w,
            layout.box1_h,
            Style::default().bg(panel_bg),
        );
        section_title(buf, layout.inner_x + 1, *y, title, title_bg, title_fg);

        let cx = layout.cx;
        let mut cy = *y + 2;

        // URL/path input
        let input_w = inner_w.saturating_sub(4);
        self.url_input
            .render(buf, cx, cy, input_w, layout.input_h, theme);

        // DB indicator on the first line of the input box (line 0), which
        // is always reserved (text starts at line 1). Shown regardless of
        // how much text is in the input.
        let input_bg = rgba_color(theme.background_element);
        let db_label = match &self.selected_db_for_embed {
            Some(name) => format!("DB {name}"),
            None => "No DB selected".to_string(),
        };
        let db_color = if self.selected_db_for_embed.is_some() {
            fg
        } else {
            warning
        };
        draw_text_line(
            buf,
            &db_label,
            cx + 2,
            cy,
            input_w.saturating_sub(4),
            Style::default().fg(db_color).bg(input_bg),
        );

        cy += layout.input_h;

        // Status line — content depends on mode.
        match self.mode {
            RagMode::Idle => {
                // Action buttons (only when form/picker not open)
                if !self.show_create_db && !self.show_db_picker {
                    let right_side_start = cx + input_w.saturating_sub(2);
                    let mut btn_x = right_side_start;

                    let create_btn = " +Create DB ";
                    btn_x = btn_x.saturating_sub(create_btn.len() as u16);
                    section_title(buf, btn_x, cy, create_btn, title_bg, title_fg);

                    if !self.registry.dbs.is_empty() {
                        let sel_btn = " Select DB ";
                        btn_x = btn_x.saturating_sub(sel_btn.len() as u16);
                        section_title(buf, btn_x, cy, sel_btn, success, title_fg);
                    }
                }
            }
            RagMode::Fetching | RagMode::Embedding => {
                let ch = self.spinner.current_char();
                if let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(success));
                }
                let label = match self.mode {
                    RagMode::Fetching => "Fetching content",
                    _ => "Embedding content",
                };
                draw_text_line(
                    buf,
                    label,
                    cx + 1,
                    cy,
                    input_w.saturating_sub(1),
                    Style::default().fg(muted),
                );
            }
            RagMode::Previewing => {
                draw_text_line(
                    buf,
                    "Preview available \u{2191}\u{2193} scroll  Esc to close",
                    cx,
                    cy,
                    input_w,
                    Style::default().fg(muted),
                );
            }
        }

        // DB picker (replaces button line when open)
        if self.show_db_picker && !self.show_create_db {
            self.render_db_picker(
                buf, cx, cy, input_w, theme, fg, muted, success, primary, title_bg, title_fg,
            );
        }

        // Create New Database form (replaces button line when open)
        if self.show_create_db {
            self.render_create_db_form(
                buf, cx, cy, input_w, theme, fg, muted, warning, primary, title_fg, panel_bg,
            );
        }

        *y = *y + layout.box1_h + 1;
    }

    // Create DB form

    fn render_create_db_form(
        &mut self,
        buf: &mut Buffer,
        cx: u16,
        cy: u16,
        input_w: u16,
        theme: &Theme,
        fg: Color,
        muted: Color,
        _warning: Color,
        primary: Color,
        title_fg: Color,
        _panel_bg: Color,
    ) {
        let mut form_y = cy;
        let pad = cx + 2;
        let pad_w = input_w.saturating_sub(4);
        let mini_box_h = self.create_db_mini_box_height(pad_w);
        let bg_term = rgba_color(theme.background);
        fill_rect(
            buf,
            cx,
            cy,
            input_w,
            mini_box_h,
            Style::default().bg(bg_term),
        );

        // Gap above the first field
        form_y += 1;

        // Model selector
        if self.models_expanded {
            draw_text_line(buf, "Model", pad, form_y, pad_w, Style::default().fg(muted));
            form_y += 1;

            let max_vis = MAX_VISIBLE_MODELS_IN_FORM;
            let total = self.available_models.len();

            // Clamp scroll offset
            if self.model_scroll_offset + max_vis > total && total > max_vis {
                self.model_scroll_offset = total.saturating_sub(max_vis);
            }

            let visible_end = (self.model_scroll_offset + max_vis).min(total);
            for i in self.model_scroll_offset..visible_end {
                if i >= self.available_models.len() {
                    break;
                }
                let is_sel = i == self.selected_model_index;
                let lbl = self.available_models[i].label();
                let max_label_w = pad_w.saturating_sub(MODEL_LABEL_PAD) as usize;
                let truncated = truncate_label(&lbl, max_label_w);
                let prefix = if is_sel { "\u{25cf} " } else { "  " };
                let line = format!("{prefix}{truncated}");

                if is_sel {
                    fill_rect(buf, cx, form_y, input_w, 1, Style::default().bg(primary));
                    draw_text_line(
                        buf,
                        &line,
                        pad,
                        form_y,
                        pad_w,
                        Style::default().fg(title_fg).bg(primary),
                    );
                } else {
                    draw_text_line(buf, &line, pad, form_y, pad_w, Style::default().fg(fg));
                }
                form_y += 1;
            }
        } else {
            let model_lbl = self
                .available_models
                .get(self.selected_model_index)
                .map(|m| m.label())
                .unwrap_or_else(|| "No models".to_string());
            let max_label_w = pad_w.saturating_sub(MODEL_COLLAPSED_PAD) as usize;
            let truncated = truncate_label(&model_lbl, max_label_w);
            let coll = format!("Model: {truncated}  (Click to expand)");
            draw_text_line(buf, &coll, pad, form_y, pad_w, Style::default().fg(primary));
            form_y += 1;
        }

        // Gap above the Name field
        form_y += 1;

        // Name & Description — the label is drawn as plain text and only the
        // value goes through the markdown renderer, so long input wraps to
        // extra lines with syntax highlighting and a leading label never
        // hides block syntax (e.g. "## titulo") from the cmark parser.
        let now = SystemTime::now();
        let name_focused = self.create_db_focus == CreateDbFocus::Name;
        let desc_focused = self.create_db_focus == CreateDbFocus::Description;
        let (name_lines, desc_lines) = self.create_db_field_lines(pad_w);

        // Name field
        let name_fg = theme.text;
        let name_bg = if name_focused {
            theme.background_element
        } else {
            theme.background
        };
        let label = "Name:  ";
        let label_w = label.chars().count() as u16;
        let value_w = pad_w.saturating_sub(label_w).max(1);
        draw_text_line(
            buf,
            label,
            pad,
            form_y,
            label_w,
            Style::default()
                .fg(rgba_color(name_fg))
                .bg(rgba_color(name_bg)),
        );
        let (name_render, _) = markdown_render_text(self.name_display_text());
        render_markdown(
            buf,
            Rect::new(pad + label_w, form_y, value_w, name_lines),
            &name_render,
            name_fg,
            name_bg,
        );
        if name_focused {
            let prefix =
                &self.db_name_input[..self.db_name_cursor_pos.min(self.db_name_input.len())];
            let (render, insertions) = markdown_render_text(prefix);
            let rp = render_byte_offset(&insertions, prefix.len()).min(render.len());
            let (row, col) = wrapped_cursor_pos(&render[..rp], value_w);
            draw_input_cursor_at(
                buf,
                &self.db_name_cursor,
                now,
                pad + label_w,
                col,
                form_y + row,
                value_w,
                theme,
                fg,
                muted,
                primary,
            );
        }
        form_y += name_lines;

        // Gap above the Description field
        form_y += 1;

        // Description field
        let desc_fg = if desc_focused {
            theme.text
        } else {
            theme.text_muted
        };
        let desc_bg = if desc_focused {
            theme.background_element
        } else {
            theme.background
        };
        let label = "Desc:  ";
        let label_w = label.chars().count() as u16;
        let value_w = pad_w.saturating_sub(label_w).max(1);
        draw_text_line(
            buf,
            label,
            pad,
            form_y,
            label_w,
            Style::default()
                .fg(rgba_color(desc_fg))
                .bg(rgba_color(desc_bg)),
        );
        let (desc_render, _) = markdown_render_text(self.desc_display_text());
        render_markdown(
            buf,
            Rect::new(pad + label_w, form_y, value_w, desc_lines),
            &desc_render,
            desc_fg,
            desc_bg,
        );
        if desc_focused {
            let prefix = &self.db_description_input[..self.db_description_cursor_pos
                .min(self.db_description_input.len())];
            let (render, insertions) = markdown_render_text(prefix);
            let rp = render_byte_offset(&insertions, prefix.len()).min(render.len());
            let (row, col) = wrapped_cursor_pos(&render[..rp], value_w);
            draw_input_cursor_at(
                buf,
                &self.db_description_cursor,
                now,
                pad + label_w,
                col,
                form_y + row,
                value_w,
                theme,
                fg,
                muted,
                primary,
            );
        }
    }

    /// Total height of the Create DB mini-form popup.
    ///
    /// Each field (model, name, description) is preceded by one gap line, and
    /// the Name/Description fields wrap to as many lines as their markdown
    /// content needs at width `pad_w`.
    /// When collapsed: gap + model + gap + name + gap + description + padding.
    /// When expanded:    gap + header + model_items* + gap + name + gap + description + padding.
    pub(crate) fn create_db_mini_box_height(&self, pad_w: u16) -> u16 {
        let (name_lines, desc_lines) = self.create_db_field_lines(pad_w);
        if self.models_expanded {
            // 6 = gap(1) + header(1) + gap(1) + gap(1) + padding(2)
            6 + MAX_VISIBLE_MODELS_IN_FORM as u16 + name_lines + desc_lines
        } else {
            // 6 = gap(1) + model(1) + gap(1) + gap(1) + padding(2)
            6 + name_lines + desc_lines
        }
    }

    // Box 2: Available Databases

    fn render_box2(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        layout: RagLayout,
        y: &mut u16,
        theme: &Theme,
        fg: Color,
        muted: Color,
        warning: Color,
        success: Color,
        primary: Color,
        panel_bg: Color,
        title_bg: Color,
        title_fg: Color,
    ) {
        let title = " Available Databases ";
        let inner_w = layout.inner_w;
        let inner_x = layout.inner_x;

        let content_h = 1
            + layout.header_gap
            + layout.warning_h
            + layout.filter_h
            + layout.list_h
            + layout.pad_bottom;
        fill_rect(
            buf,
            inner_x,
            *y,
            inner_w,
            content_h,
            Style::default().bg(panel_bg),
        );
        section_title(buf, inner_x + 1, *y, title, title_bg, title_fg);

        let cx = layout.cx;
        let mut cy = *y + 1; // title row

        cy += 1; // gap

        //  Warning
        draw_text_line(
            buf,
            "\u{26A0}  Many active DBs degrade LLM quality. Enable only relevant.",
            cx,
            cy,
            inner_w.saturating_sub(4),
            Style::default().fg(warning),
        );
        cy += 1;
        draw_text_line(
            buf,
            "   Each active DB adds its description to the LLM's context.",
            cx,
            cy,
            inner_w.saturating_sub(4),
            Style::default().fg(muted),
        );
        cy += 1;

        // SearchBar filter
        if layout.filter_h > 0 {
            self.db_filter
                .render(buf, cx, cy, inner_w.saturating_sub(4), theme);
            cy += 1;
        }

        // DB list
        let filtered = self.filtered_dbs();
        let max_visible = layout.list_h as usize;

        if filtered.is_empty() {
            draw_text_line(
                buf,
                "   No databases yet.",
                cx,
                cy,
                inner_w.saturating_sub(4),
                Style::default().fg(muted),
            );
        } else {
            let render_count = max_visible.min(filtered.len());
            for i in 0..render_count {
                let idx = self.dbs_scroll_offset + i;
                if idx >= filtered.len() {
                    break;
                }
                let db = filtered[idx];
                let db_y = cy + i as u16;
                if db_y >= area.bottom() {
                    break;
                }

                let is_sel = idx == self.selected_db_index;
                let is_act = self.active_dbs.contains(&db.name);
                let rc = if is_sel { primary } else { fg };

                if let Some(cell) = buf.cell_mut((cx, db_y)) {
                    cell.set_char(if is_act { '\u{2714}' } else { '\u{2718}' });
                    cell.set_style(Style::default().fg(if is_act { success } else { rc }));
                }

                let btn_w = SHOW_DESC_BTN_TEXT.len() as u16;
                let gap: u16 = 1;
                let total_btns_w = btn_w + gap + TRASH_EMOJI_WIDTH;
                let avail_name_w = (inner_w.saturating_sub(DB_ROW_H_PADDING) as usize)
                    .saturating_sub((total_btns_w + gap) as usize);
                let name_display = format!(" {}", db.name);
                let name_trunc = truncate_label(&name_display, avail_name_w);
                let name_len = name_trunc.chars().count() as u16;
                draw_text_line(
                    buf,
                    &name_trunc,
                    cx + 2,
                    db_y,
                    avail_name_w as u16,
                    Style::default().fg(rc),
                );

                // "show desc" button right after the name (no background, just accent color)
                let btn_x = cx + 2 + name_len + gap;
                draw_text_line(
                    buf,
                    SHOW_DESC_BTN_TEXT,
                    btn_x,
                    db_y,
                    btn_w,
                    Style::default().fg(success),
                );

                // 🗑 trash / delete button after "show desc"
                let trash_x = btn_x + btn_w + gap;
                if let Some(cell) = buf.cell_mut((trash_x, db_y)) {
                    cell.set_char('\u{1F5D1}');
                    cell.set_style(Style::default().fg(muted));
                }
                // Clear the cell after the wide emoji (handles both 1-cell and 2-cell rendering)
                if let Some(cell) = buf.cell_mut((trash_x + 1, db_y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default());
                }
            }
        }

        // Description popup (simple overlay with just the description text)
        self.render_desc_popup(buf, area, theme, fg);
    }

    fn render_desc_popup(&self, buf: &mut Buffer, area: Rect, theme: &Theme, fg: Color) {
        let Some(idx) = self.show_desc_for_db else {
            return;
        };
        let filtered = self.filtered_dbs();
        if idx >= filtered.len() {
            return;
        }
        let description = &filtered[idx].description;
        if description.is_empty() {
            return;
        }

        let Some(overlay) = self.desc_popup_rect(area) else {
            return;
        };
        let overlay_x = overlay.x;
        let overlay_y = overlay.y;
        let overlay_w = overlay.width;
        let overlay_h = overlay.height;

        // Fill background with main background color so it contrasts with panel_bg
        let bg_color = rgba_color(theme.background);
        fill_rect(
            buf,
            overlay_x,
            overlay_y,
            overlay_w,
            overlay_h,
            Style::default().bg(bg_color),
        );

        // Scrollable description content
        let content_x = overlay_x + 2;
        let content_w = overlay_w.saturating_sub(4) as usize;
        let content_h = (overlay_h.saturating_sub(2)) as usize;
        let total_lines = description.lines().count();
        let max_scroll = total_lines.saturating_sub(content_h).max(0);
        let scroll = self.desc_scroll.min(max_scroll);
        let lines: Vec<&str> = description.lines().collect();

        for i in 0..content_h {
            let idx = scroll + i;
            if idx >= lines.len() {
                break;
            }
            let line = lines[idx];
            let truncated: String = line.chars().take(content_w).collect();
            draw_text_line(
                buf,
                &truncated,
                content_x,
                overlay_y + 1 + i as u16,
                content_w as u16,
                Style::default().fg(fg),
            );
        }
    }

    // Preview overlay

    fn render_preview_overlay(
        &self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        fg: Color,
        _muted: Color,
        _success: Color,
        _primary: Color,
    ) {
        if !self.is_preview_visible() {
            return;
        }

        let Some(overlay) = self.preview_overlay_rect(area) else {
            return;
        };
        let overlay_x = overlay.x;
        let overlay_y = overlay.y;
        let overlay_w = overlay.width;
        let overlay_h = overlay.height;

        // Background fill
        let bg_color = rgba_color(theme.background_element);
        fill_rect(
            buf,
            overlay_x,
            overlay_y,
            overlay_w,
            overlay_h,
            Style::default().bg(bg_color),
        );

        // Border
        let content_x = overlay_x + 2;
        let content_w = overlay_w.saturating_sub(4);
        let mut content_y = overlay_y + 1;

        // Title bar with background (matching box titles)
        let title_bg = rgba_color(theme.primary);
        let title_fg = rgba_color(theme.background);
        section_title(
            buf,
            content_x,
            content_y,
            " Content Preview ",
            title_bg,
            title_fg,
        );
        content_y += 2;

        // Scrollable content (no border, no footer)
        let max_content_h = (overlay_y + overlay_h - 1).saturating_sub(content_y);
        let total_lines = self.content_preview.lines().count();
        let max_scroll = total_lines.saturating_sub(max_content_h as usize).max(0);
        let scroll = self.preview_scroll.min(max_scroll);
        let lines: Vec<&str> = self.content_preview.lines().collect();

        for i in 0..max_content_h {
            let idx = scroll + i as usize;
            if idx >= lines.len() {
                break;
            }
            let line = lines[idx];
            let truncated: String = line.chars().take(content_w as usize).collect();
            draw_text_line(
                buf,
                &truncated,
                content_x,
                content_y + i,
                content_w,
                Style::default().fg(fg),
            );
        }
    }

    // DB Picker (inline form, identical to create DB form)

    fn render_db_picker(
        &self,
        buf: &mut Buffer,
        cx: u16,
        cy: u16,
        input_w: u16,
        theme: &Theme,
        fg: Color,
        muted: Color,
        _success: Color,
        primary: Color,
        _title_bg: Color,
        _title_fg: Color,
    ) {
        let bg_term = rgba_color(theme.background);
        fill_rect(
            buf,
            cx,
            cy,
            input_w,
            PICKER_BOX_HEIGHT,
            Style::default().bg(bg_term),
        );

        let pad = cx + 2;
        let pad_w = input_w.saturating_sub(4);
        let mut row_y = cy;

        // Title line (matching "Model" label style in create form)
        draw_text_line(
            buf,
            "Select Database",
            pad,
            row_y,
            pad_w,
            Style::default().fg(primary),
        );
        row_y += 1;

        // DB list (up to 4 items fit in 5-line box: title + 4 list lines)
        let filtered = self.filtered_dbs();
        if filtered.is_empty() {
            draw_text_line(
                buf,
                "   No databases found.",
                pad,
                row_y,
                pad_w,
                Style::default().fg(muted),
            );
            return;
        }

        let max_vis = DB_PICKER_LIST_VISIBLE;
        let total = filtered.len();

        let mut scroll = self.db_picker_scroll_offset;
        if scroll + max_vis > total && total > max_vis {
            scroll = total.saturating_sub(max_vis);
        }

        let visible_end = (scroll + max_vis).min(total);
        for i in scroll..visible_end {
            let db = filtered[i];
            let is_sel = self.selected_db_for_embed.as_deref() == Some(&db.name);
            let prefix = if is_sel { "\u{25cf} " } else { "  " };
            let line = format!("{prefix}{} \u{2014} {}", db.name, db.description);
            let max_desc = pad_w as usize;
            let truncated = truncate_label(&line, max_desc);

            if is_sel {
                fill_rect(buf, cx, row_y, input_w, 1, Style::default().bg(primary));
                draw_text_line(
                    buf,
                    &truncated,
                    pad,
                    row_y,
                    pad_w,
                    Style::default()
                        .fg(rgba_color(theme.background))
                        .bg(primary),
                );
            } else {
                draw_text_line(buf, &truncated, pad, row_y, pad_w, Style::default().fg(fg));
            }
            row_y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wrapped_cursor_pos;
    use cosh_tui::core::renderables::markdown::estimate_height;

    #[test]
    fn pulldown_preserves_double_space() {
        // If pulldown_cmark collapsed "a  b" to a single space, both
        // estimates would be equal; the renderer's wrap then matches the
        // simulation used for cursor placement.
        let double = estimate_height("a  b", 3);
        let single = estimate_height("a b", 3);
        assert_eq!(double, 2, "double space should wrap b at width 3");
        assert_eq!(single, 1, "single space should fit at width 3");
    }

    #[test]
    fn heading_parses_without_label() {
        // The field labels are no longer part of the markdown content, so a
        // value like "## titulo" is recognized as a heading by cmark instead
        // of literal text inside a paragraph.
        assert_eq!(estimate_height("## titulo", 12), 1, "heading fits one line");
        assert_eq!(
            estimate_height("Name:  ## titulo", 12),
            2,
            "with a leading label the same text wraps as a paragraph"
        );
    }

    #[test]
    fn cursor_single_line() {
        assert_eq!(wrapped_cursor_pos("abc", 100), (0, 3));
        assert_eq!(wrapped_cursor_pos("a", 100), (0, 1));
        assert_eq!(wrapped_cursor_pos("", 100), (0, 0));
    }

    #[test]
    fn cursor_wraps_to_next_line() {
        // "ab cde" at width 5: "cde" doesn't fit after "ab ", wraps to row 1.
        assert_eq!(wrapped_cursor_pos("ab cde", 5), (1, 3));
        // A word wider than the line breaks at character level.
        assert_eq!(wrapped_cursor_pos("123456789", 5), (1, 4));
        // Cursor right after "ab": still on the first line, col 2.
        assert_eq!(wrapped_cursor_pos("ab", 5), (0, 2));
    }

    #[test]
    fn cursor_matches_estimate_rows() {
        // The cursor row for the full text equals (estimated lines - 1), even
        // when markdown markers are present (they occupy no display columns).
        for text in [
            "short",
            "a long description that wraps",
            "*em* and `code` mixed together",
            "**strong** with a [link](https://example.com) inside",
            "## a heading that also wraps around",
        ] {
            for w in [10u16, 20, 40] {
                let est = estimate_height(text, w).max(1);
                let (row, _col) = wrapped_cursor_pos(text, w);
                assert_eq!(
                    row,
                    est - 1,
                    "cursor row mismatch for {text:?} at width {w}"
                );
            }
        }
    }

    #[test]
    fn field_lines_grow_with_content() {
        use crate::routes::rag::view::RagView;
        let mut view = RagView::new();
        let (n1, d1) = view.create_db_field_lines(60);
        assert_eq!(n1, 1, "placeholder name fits one line at width 60");
        assert_eq!(d1, 1, "placeholder description fits one line at width 60");
        view.db_name_input =
            "a very long database name that definitely wraps at width 30".into();
        view.db_description_input =
            "a very long description that will wrap to multiple lines at this narrow width".into();
        let (n2, d2) = view.create_db_field_lines(30);
        assert!(n2 > 1, "long name should wrap to multiple lines");
        assert!(d2 > 1, "long description should wrap to multiple lines");
    }

    #[test]
    fn markdown_render_text_keeps_blank_lines() {
        // A blank line in the middle keeps its row (invisible spacer).
        let (render, ins) = super::markdown_render_text("abc\n\nX");
        assert_eq!(render, "abc\n\u{00A0}\nX");
        assert_eq!(ins[4], 2, "spacer bytes before the second newline");
        // A leading line break is anchored too (cmark would otherwise ignore
        // the blank first line and shift everything up one row).
        let (r2, i2) = super::markdown_render_text("\nX");
        assert_eq!(r2, "\u{00A0}\nX");
        assert_eq!(i2[0], 2);
        // Trailing blank lines: each one stays a row.
        let (r3, _) = super::markdown_render_text("abc\n\n");
        assert_eq!(r3, "abc\n\u{00A0}\n");
    }

    #[test]
    fn render_byte_offset_skips_spacers() {
        let (_, ins) = super::markdown_render_text("abc\n\nX");
        assert_eq!(super::render_byte_offset(&ins, 0), 0);
        assert_eq!(super::render_byte_offset(&ins, 3), 3);
        assert_eq!(super::render_byte_offset(&ins, 4), 6, "after first \\n");
        assert_eq!(super::render_byte_offset(&ins, 5), 7, "before X");
        assert_eq!(super::render_byte_offset(&ins, 6), 8, "end");
    }

    #[test]
    fn cursor_row_after_trailing_newline() {
        // Right after Shift+Enter the caret sits on the blank line below.
        let (render, _) = super::markdown_render_text("abc\n");
        assert_eq!(wrapped_cursor_pos(&render, 20), (1, 0));
        let (render2, _) = super::markdown_render_text("abc\n\n");
        assert_eq!(wrapped_cursor_pos(&render2, 20), (2, 0));
    }

    #[test]
    fn typing_after_leading_breaks_keeps_row() {
        // Regression: typing the first character after breaking lines used to
        // render one line ABOVE the caret (cmark collapses leading blank
        // lines, so the first real line started at row 0 while the caret was
        // on the phantom row below). The typed character must land on the
        // exact row the caret is on.
        let row_of = |t: &str| wrapped_cursor_pos(&super::markdown_render_text(t).0, 20).0;
        assert_eq!(row_of("\n"), row_of("\nX"), "one leading break");
        assert_eq!(row_of("\n\n"), row_of("\n\nX"), "two leading breaks");
        assert_eq!(row_of("\n\n\n"), row_of("\n\n\nX"), "three leading breaks");
        // The character lands exactly on the caret row, below the leading
        // blank rows.
        assert_eq!(row_of("\nX"), 1);
        assert_eq!(row_of("\n\nX"), 2);
    }

    #[test]
    fn byte_pos_at_click_plain_and_wrapped() {
        assert_eq!(super::byte_pos_at_click("hello", 0, 2, 100), 2);
        assert_eq!(super::byte_pos_at_click("hello", 0, 99, 100), 5, "clamp to end");
        // "ab cde" wraps at width 5: row 1 holds "cde", so a click on row 1
        // lands at the end of the wrapped word. Clicking the space between
        // the words lands after it (ties prefer the later position).
        assert_eq!(super::byte_pos_at_click("ab cde", 0, 2, 5), 3);
        assert_eq!(super::byte_pos_at_click("ab cde", 1, 0, 5), 6);
        assert_eq!(super::byte_pos_at_click("ab cde", 1, 3, 5), 6);
    }

    #[test]
    fn byte_pos_at_click_lands_after_markers() {
        // Clicking on a heading marker puts the caret after the "## " so
        // typing keeps the heading intact.
        assert_eq!(super::byte_pos_at_click("## titulo", 0, 0, 100), 3);
        assert_eq!(super::byte_pos_at_click("## titulo", 0, 1, 100), 4);
        assert_eq!(super::byte_pos_at_click("## titulo", 0, 6, 100), 9);
    }

    #[test]
    fn byte_pos_at_click_multibyte() {
        // 'á' is 2 bytes: clicking col 2 (on 'l') lands before it, col 3
        // (past it) lands after.
        assert_eq!(super::byte_pos_at_click("olá", 0, 2, 100), 2);
        assert_eq!(super::byte_pos_at_click("olá", 0, 3, 100), 4);
    }

    #[test]
    fn byte_pos_at_click_blank_line() {
        // Clicking the blank line between the newlines lands between them;
        // clicking the row below lands after both.
        assert_eq!(super::byte_pos_at_click("abc\n\nX", 1, 0, 20), 4);
        assert_eq!(super::byte_pos_at_click("abc\n\nX", 2, 0, 20), 5);
    }

    #[test]
    fn caret_row_follows_line_boundaries() {
        // caret_row is what keeps Left/Right horizontal: crossing a newline
        // changes the row, moving within a line does not.
        let value_w = 20u16;
        assert_eq!(super::caret_row("abc\ndef", 3, value_w), 0, "end of line 1");
        assert_eq!(super::caret_row("abc\ndef", 4, value_w), 1, "start of line 2");
        assert_eq!(super::caret_row("abc\ndef", 5, value_w), 1, "mid line 2");
        // A wrapped line behaves the same: the caret after a word that fills
        // the line is still on row 0; the next char wraps to row 1.
        assert_eq!(super::caret_row("abc def ghi", 11, 11), 0);
        assert_eq!(super::caret_row("abc def ghi j", 13, 11), 1);
    }

    #[test]
    fn cursor_drawn_at_wrap_boundary() {
        use crate::routes::rag::models::CreateDbFocus;
        use crate::routes::rag::view::RagView;
        use crate::theme::ThemeRegistry;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        // The caret at the end of a line that exactly fills the width sits at
        // column == value_w; the cursor must still be drawn there (it used to
        // vanish, making the caret appear to jump down on the next Right).
        let area = Rect::new(0, 0, 30, 40);
        let pad_w = (30 - 4) - 8;
        let value_w = pad_w - 7;
        let input_h = RagView::new().url_input.height(26);
        let form_y = 2 + input_h;
        let name_y = form_y + 3;

        let theme = ThemeRegistry::new().default_theme().clone();
        let mut view = RagView::new();
        view.toggle_create_db();
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "abc def ghi jkl".into();
        // Caret after "abc def ghi" (11 chars == value_w), before the space.
        view.db_description_cursor_pos = 11;

        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme);
        let (name_lines, _) = view.create_db_field_lines(pad_w);
        let desc_y = name_y + name_lines + 1;
        let value_x = 6 + 7; // pad (cx + 2) + label width

        let (render, ins) = super::markdown_render_text("abc def ghi jkl");
        let rp = super::render_byte_offset(&ins, 11);
        let (r, c) = wrapped_cursor_pos(&render[..rp], value_w);
        assert_eq!((r, c), (0, value_w), "caret at the wrap boundary");

        let cell = &buf[(value_x + c, desc_y + r)];
        assert_eq!(
            cell.bg,
            super::rgba_color(theme.text),
            "cursor must be drawn at the wrap boundary"
        );
    }

    #[test]
    fn render_first_char_after_leading_breaks_on_cursor_row() {
        use crate::routes::rag::models::CreateDbFocus;
        use crate::routes::rag::view::RagView;
        use crate::theme::ThemeRegistry;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        // Geometry mirrors render_create_db_form: title(1) + gap(1) + input,
        // then gap + model line + gap before the Name field, +1 gap before
        // the Description field.
        let area = Rect::new(0, 0, 100, 40);
        let input_h = RagView::new().url_input.height(92);
        let form_y = 2 + input_h;
        let name_y = form_y + 3; // gap + collapsed model line + gap
        let value_x = 4 + 2 + 7; // pad (cx + 2) + label width

        let mut view = RagView::new();
        view.toggle_create_db();
        view.create_db_focus = CreateDbFocus::Description;
        // One Shift+Enter on the empty field, then the first typed char.
        view.db_description_input = "\nX".into();
        view.db_description_cursor_pos = 2;
        let mut buf = Buffer::empty(area);
        let theme = ThemeRegistry::new().default_theme().clone();
        view.render(&mut buf, area, &theme);

        // Find the typed 'X' inside the Description field region only.
        let (_, desc_lines) = view.create_db_field_lines(88);
        let desc_y = name_y + 1 + 1; // name_lines(1) + gap
        let mut found = Vec::new();
        for y in desc_y..desc_y + desc_lines {
            for x in 0..area.width {
                if buf[(x, y)].symbol() == "X" {
                    found.push((x, y));
                }
            }
        }
        assert!(!found.is_empty(), "typed char must be drawn in the field");
        let (x, y) = found[0];
        // The char lands on the row BELOW the leading blank line — the row
        // the caret was on — never one line up.
        assert_eq!(y, desc_y + 1, "X must render on the caret row");
        assert_eq!(x, value_x, "X starts at the value's column 0");
    }
}
