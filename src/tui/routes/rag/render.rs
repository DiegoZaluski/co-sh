//! Rendering logic for the RAG Knowledge Base view.
//!
//! All drawing code extracted from the original monolithic view.rs.

use std::time::SystemTime;

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::component::cursor::CursorState;
use crate::theme::Theme;

use super::models::{CreateDbFocus, RagMode};
use super::view::RagView;

// ── Helpers ─────────────────────────────────────────────────────────────

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

// ── Render implementation ───────────────────────────────────────────────

impl RagView {
    #[allow(clippy::too_many_lines)]
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let warning = rgba_color(theme.warning);
        let success = rgba_color(theme.success);
        let primary = rgba_color(theme.primary);
        let panel_bg = rgba_color(theme.background_panel);
        let title_bg = primary;
        let title_fg = rgba_color(theme.background);

        let inner_x = area.x + 2;
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return;
        }

        let avail_h = area.height;
        let gap: u16 = 1;
        let bottom_gap: u16 = 0;

        // ── Layout (fixed proportional split) ──────────────────────────
        let input_h = self.url_input.height(); // 3
        let db_model_count = if self.show_create_db && self.models_expanded {
            self.available_models.len().min(max_visible_models(avail_h))
        } else {
            0
        };
        let create_db_lines: u16 = if self.show_create_db {
            if self.models_expanded {
                4 + db_model_count as u16 + 1
            } else {
                5
            }
        } else if self.show_db_picker {
            5
        } else {
            1
        };
        let box1_h = {
            let ideal = avail_h.saturating_mul(30) / 100;
            ideal
                .max(7 + create_db_lines)
                .min(avail_h.saturating_sub(8))
        };
        let box2_available = avail_h.saturating_sub(box1_h + gap + bottom_gap);
        let box2_h = box2_available.max(5);

        let mut y = area.y;

        // ══════════ BOX 1: Embed Content ══════════════════════════════
        self.render_box1(
            buf,
            inner_x,
            inner_w,
            box1_h,
            &mut y,
            theme,
            fg,
            muted,
            warning,
            success,
            primary,
            panel_bg,
            title_bg,
            title_fg,
            input_h,
            db_model_count as usize,
            create_db_lines,
        );

        // ══════════ BOX 2: Available Databases ════════════════════════
        if y < area.bottom() {
            self.render_box2(
                buf, area, inner_x, inner_w, box2_h, &mut y, theme, fg, muted, warning, success,
                primary, panel_bg, title_bg, title_fg,
            );
        }

        // ══════════ OVERLAY: Content Preview popup ═════════════════════
        self.render_preview_overlay(buf, area, theme, fg, muted, success, primary);
    }

    // ── Box 1: Embed Content ───────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn render_box1(
        &mut self,
        buf: &mut Buffer,
        inner_x: u16,
        inner_w: u16,
        box1_h: u16,
        y: &mut u16,
        _theme: &Theme,
        fg: Color,
        muted: Color,
        warning: Color,
        success: Color,
        primary: Color,
        panel_bg: Color,
        title_bg: Color,
        title_fg: Color,
        input_h: u16,
        _db_model_count: usize,
        _create_db_lines: u16,
    ) {
        let title = " Embed Content ";
        fill_rect(
            buf,
            inner_x,
            *y,
            inner_w,
            box1_h,
            Style::default().bg(panel_bg),
        );
        section_title(buf, inner_x + 1, *y, title, title_bg, title_fg);

        let cx = inner_x + 2;
        let mut cy = *y + 2;

        // ── URL/path input ────────────────────────────────────────────
        let input_w = inner_w.saturating_sub(4);
        self.url_input.render(buf, cx, cy, input_w, _theme);

        // ── DB indicator inside the input box (line 2, below placeholder) ──
        let input_bg = rgba_color(_theme.background_element);
        if let Some(db_name) = &self.selected_db_for_embed {
            draw_text_line(
                buf,
                &format!("DB {db_name}"),
                cx + 2,
                cy + 2,
                input_w.saturating_sub(4),
                Style::default().fg(fg).bg(input_bg),
            );
        } else {
            draw_text_line(
                buf,
                "No DB selected",
                cx + 2,
                cy + 2,
                input_w.saturating_sub(4),
                Style::default().fg(warning).bg(input_bg),
            );
        }

        cy += input_h;

        // ── Preview / status ──────────────────────────────────────────
        match self.mode {
            RagMode::Fetching => {
                let ch = self.spinner.current_char();
                if let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(success));
                }
                draw_text_line(
                    buf,
                    " Fetching content",
                    cx + 2,
                    cy,
                    input_w.saturating_sub(2),
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
            RagMode::Embedding => {
                let ch = self.spinner.current_char();
                if let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(success));
                }
                draw_text_line(
                    buf,
                    " Embedding content...",
                    cx + 2,
                    cy,
                    input_w.saturating_sub(2),
                    Style::default().fg(muted),
                );
            }
            RagMode::Idle => {}
        }

        // ── Action buttons (right side, only when form/picker not open) ──
        if !self.show_create_db && !self.show_db_picker {
            let right_side_start = cx + input_w.saturating_sub(2);
            let mut btn_x = right_side_start;

            // "+Create DB" button (rightmost) — primary bg
            let create_btn = " +Create DB ";
            btn_x = btn_x.saturating_sub(create_btn.len() as u16);
            section_title(buf, btn_x, cy, create_btn, title_bg, title_fg);

            // "Select DB" button (second from right) — success bg for visual distinction
            if !self.registry.dbs.is_empty() {
                let sel_btn = " Select DB ";
                btn_x = btn_x.saturating_sub(sel_btn.len() as u16);
                section_title(buf, btn_x, cy, sel_btn, success, title_fg);
            }
        }

        // ── DB picker (replaces button line when open) ────────────────
        if self.show_db_picker && !self.show_create_db {
            self.render_db_picker(
                buf, cx, cy, input_w, _theme, fg, muted, success, primary, title_bg, title_fg,
            );
        }

        // ── Create New Database form (replaces button line when open) ──
        if self.show_create_db {
            self.render_create_db_form(
                buf, cx, cy, input_w, _theme, fg, muted, warning, primary, title_fg, panel_bg,
            );
        }

        *y = *y + box1_h + 1;
    }

    // ── Create DB form ─────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
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
        let mini_box_h = self.create_db_mini_box_height();
        let bg_term = rgba_color(theme.background);
        fill_rect(
            buf,
            cx,
            cy,
            input_w,
            mini_box_h,
            Style::default().bg(bg_term),
        );

        let mut form_y = cy;
        let pad = cx + 2;
        let pad_w = input_w.saturating_sub(4);

        // ── Model selector ─────────────────────────────────────────────
        if self.models_expanded {
            draw_text_line(buf, "Model", pad, form_y, pad_w, Style::default().fg(muted));
            form_y += 1;

            let max_vis = max_visible_models_for_form();
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
                let max_label_w = pad_w.saturating_sub(3) as usize;
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
            let max_label_w = pad_w.saturating_sub(20) as usize;
            let truncated = truncate_label(&model_lbl, max_label_w);
            let coll = format!("Model: {truncated}  (Click to expand)");
            draw_text_line(buf, &coll, pad, form_y, pad_w, Style::default().fg(primary));
            form_y += 1;
        }

        // ── Name & Description ─────────────────────────────────────────
        let now = SystemTime::now();
        let name_focused = self.create_db_focus == CreateDbFocus::Name;
        let desc_focused = self.create_db_focus == CreateDbFocus::Description;

        let nd = if self.db_name_input.is_empty() {
            "Enter database name..."
        } else {
            &self.db_name_input
        };
        let name_text = format!("Name:  {nd}");
        let max_name_w = pad_w as usize;
        let name_trunc = truncate_label(&name_text, max_name_w);
        let name_style = if name_focused {
            Style::default()
                .fg(fg)
                .bg(rgba_color(theme.background_element))
        } else {
            Style::default().fg(fg)
        };
        draw_text_line(buf, &name_trunc, pad, form_y, pad_w, name_style);
        // Draw cursor on Name field using Cursor component's state for blink
        if name_focused {
            let cursor_x = pad + 6 + self.db_name_cursor_pos as u16; // "Name:  " = 6 chars
            if cursor_x < pad + pad_w {
                if let Some(cell) = buf.cell_mut((cursor_x, form_y)) {
                    match self.db_name_cursor.current_state(now) {
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
        }
        form_y += 1;

        let dd = if self.db_description_input.is_empty() {
            "Briefly describe what this DB contains..."
        } else {
            &self.db_description_input
        };
        let desc_text = format!("Desc:  {dd}");
        let desc_trunc = truncate_label(&desc_text, max_name_w);
        let desc_style = if desc_focused {
            Style::default()
                .fg(fg)
                .bg(rgba_color(theme.background_element))
        } else {
            Style::default().fg(muted)
        };
        draw_text_line(buf, &desc_trunc, pad, form_y, pad_w, desc_style);
        // Draw cursor on Description field using Cursor component's state for blink
        if desc_focused {
            let cursor_x = pad + 6 + self.db_description_cursor_pos as u16; // "Desc:  " = 6 chars
            if cursor_x < pad + pad_w {
                if let Some(cell) = buf.cell_mut((cursor_x, form_y)) {
                    match self.db_description_cursor.current_state(now) {
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
        }
    }

    pub(crate) fn create_db_mini_box_height(&self) -> u16 {
        if self.models_expanded {
            let max_vis = 8u16;
            4 + max_vis + 1
        } else {
            5
        }
    }

    // ── Box 2: Available Databases ─────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn render_box2(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        inner_x: u16,
        inner_w: u16,
        box2_h: u16,
        y: &mut u16,
        _theme: &Theme,
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
        let warning_h = 2u16;
        let filter_h = if self.registry.dbs.len() > 5 {
            1u16
        } else {
            0u16
        };
        let pad_bottom: u16 = 1;
        let header_gap: u16 = 1;
        let list_h = box2_h.saturating_sub(1 + header_gap + warning_h + filter_h + pad_bottom);

        let content_h = 1 + header_gap + warning_h + filter_h + list_h + pad_bottom;
        fill_rect(
            buf,
            inner_x,
            *y,
            inner_w,
            content_h,
            Style::default().bg(panel_bg),
        );
        section_title(buf, inner_x + 1, *y, title, title_bg, title_fg);

        let cx = inner_x + 2;
        let mut cy = *y + 1; // title row

        cy += 1; // gap

        // ── Warning ───────────────────────────────────────────────────
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

        // ── SearchBar filter ──────────────────────────────────────────
        if filter_h > 0 {
            self.db_filter
                .render(buf, cx, cy, inner_w.saturating_sub(4), _theme);
            cy += 1;
        }

        // ── DB list ───────────────────────────────────────────────────
        let filtered = self.filtered_dbs();
        let max_visible = list_h as usize;

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

                let btn_text = " show desc ";
                let btn_w = btn_text.len() as u16; // 12
                let trash_w: u16 = 2; // 🗑 wide emoji takes 2 cells
                let gap: u16 = 1;
                let total_btns_w = btn_w + gap + trash_w;
                let avail_name_w = (inner_w.saturating_sub(6) as usize)
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
                    btn_text,
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

        // ── Description popup (simple overlay with just the description text) ──
        self.render_desc_popup(buf, area, _theme, fg);
    }

    fn render_desc_popup(&self, buf: &mut Buffer, area: Rect, _theme: &Theme, fg: Color) {
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

        // Compute a centered overlay with scrollable content
        let overlay_w = (area.width * 70 / 100)
            .max(30)
            .min(area.width.saturating_sub(8));
        let overlay_h = (area.height * 50 / 100)
            .max(5)
            .min(area.height.saturating_sub(8));
        let overlay_x = area.x + (area.width - overlay_w) / 2;
        let overlay_y = area.y + (area.height - overlay_h) / 2;

        // Fill background with main background color so it contrasts with panel_bg
        let bg_color = rgba_color(_theme.background);
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

    // ── Preview overlay ────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
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

        let overlay_w = (area.width * 85 / 100)
            .max(40)
            .min(area.width.saturating_sub(4));
        let overlay_h = (area.height * 80 / 100)
            .max(10)
            .min(area.height.saturating_sub(4));
        let overlay_x = area.x + (area.width - overlay_w) / 2;
        let overlay_y = area.y + (area.height - overlay_h) / 2;

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

    fn draw_overlay_border(
        buf: &mut Buffer,
        overlay_x: u16,
        overlay_y: u16,
        overlay_w: u16,
        overlay_h: u16,
        border_color: Color,
    ) {
        let max_x = overlay_x + overlay_w - 1;
        let max_y = overlay_y + overlay_h - 1;

        for x in (overlay_x + 1)..max_x {
            if let Some(cell) = buf.cell_mut((x, overlay_y)) {
                cell.set_char('\u{2500}');
                cell.set_style(Style::default().fg(border_color));
            }
            if let Some(cell) = buf.cell_mut((x, max_y)) {
                cell.set_char('\u{2500}');
                cell.set_style(Style::default().fg(border_color));
            }
        }
        for yb in (overlay_y + 1)..max_y {
            if let Some(cell) = buf.cell_mut((overlay_x, yb)) {
                cell.set_char('\u{2502}');
                cell.set_style(Style::default().fg(border_color));
            }
            if let Some(cell) = buf.cell_mut((max_x, yb)) {
                cell.set_char('\u{2502}');
                cell.set_style(Style::default().fg(border_color));
            }
        }
        // Rounded corners
        for (xx, yy, ch) in [
            (overlay_x, overlay_y, '\u{256D}'),
            (max_x, overlay_y, '\u{256E}'),
            (overlay_x, max_y, '\u{2570}'),
            (max_x, max_y, '\u{256F}'),
        ] {
            if let Some(cell) = buf.cell_mut((xx, yy)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(border_color));
            }
        }
    }

    // ── DB Picker (inline form, identical to create DB form) ─────────

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
        // Match create_db_mini_box_height() collapsed height: 5 lines
        let picker_h = 5u16;
        let bg_term = rgba_color(theme.background);
        fill_rect(buf, cx, cy, input_w, picker_h, Style::default().bg(bg_term));

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

        let max_vis = 4usize;
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

// ── Free helpers ────────────────────────────────────────────────────────

/// Max number of models to show in the expanded selector (depends on terminal height).
fn max_visible_models(avail_h: u16) -> usize {
    // Show at most 8 models, or fewer if the terminal is very small.
    let cap = (avail_h.saturating_sub(10) / 2) as usize;
    cap.clamp(3, 8)
}

/// Max visible models in the form's expanded list (fits inside the box).
const MAX_VISIBLE_MODELS_IN_FORM: usize = 8;

/// Max visible models in the form's expanded list (fits inside the box).
fn max_visible_models_for_form() -> usize {
    MAX_VISIBLE_MODELS_IN_FORM
}
