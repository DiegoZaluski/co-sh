use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh::ModelEntry;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::MouseEvent;

use crate::component::search_bar::SearchBar;
use crate::fallback::{FallbackEntry, default_fallbacks};
use crate::theme::Theme;
use crate::util::list_selection::ListSelection;

const FOOTER_MARGIN: u16 = 3;
const SIDE_PADDING: u16 = 4;
const MODEL_LIST_TOP_OFFSET: u16 = 3;
const FALLBACK_LIST_TOP_OFFSET: u16 = 2;
const VISIBLE_COUNT: usize = 20;

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

fn primary_contrast_fg(theme: &Theme) -> Color {
    let (pr, pg, pb, _) = theme.primary.to_ints();
    let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
    if lum > 0.5 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Models,
    Fallbacks,
}

pub struct RouterView {
    pub selection: ListSelection,
    pub search_bar: SearchBar,
    pub fallbacks: Vec<FallbackEntry>,
    pub focus: FocusTarget,
    selected_fallback: usize,
    fallback_scroll_offset: usize,
    num_buffer: String,
}

impl RouterView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            search_bar: SearchBar::new(),
            fallbacks: default_fallbacks(),
            focus: FocusTarget::Models,
            selected_fallback: 0,
            fallback_scroll_offset: 0,
            num_buffer: String::new(),
        }
    }

    fn filtered_models<'a>(&self, models: &'a [ModelEntry]) -> Vec<&'a ModelEntry> {
        if self.search_bar.is_empty() {
            return models.iter().collect();
        }
        let lower = self.search_bar.as_str().to_lowercase();
        models
            .iter()
            .filter(|m| {
                m.model.to_lowercase().contains(&lower)
                    || m.provider.to_lowercase().contains(&lower)
            })
            .collect()
    }

    pub fn set_fallbacks(&mut self, fallbacks: Vec<FallbackEntry>) {
        self.fallbacks = fallbacks;
        self.selected_fallback = self
            .selected_fallback
            .min(self.fallbacks.len().saturating_sub(1));
        self.fallback_scroll_offset = 0;
    }

    pub fn push_filter_char(&mut self, ch: char) {
        self.search_bar.push_char(ch);
        self.selection.clamp(self.filtered_models(&[]).len());
    }

    pub fn pop_filter_char(&mut self) {
        self.search_bar.pop_char();
        self.selection.clamp(self.filtered_models(&[]).len());
    }

    pub fn select_next(&mut self, models: &[ModelEntry]) {
        let total = self.filtered_models(models).len();
        self.selection.set_visible_count(VISIBLE_COUNT);
        self.selection.select_next(total);
    }

    pub fn select_prev(&mut self, models: &[ModelEntry]) {
        let total = self.filtered_models(models).len();
        self.selection.set_visible_count(VISIBLE_COUNT);
        self.selection.select_prev(total);
    }

    pub fn select_next_fallback(&mut self) {
        if self.fallbacks.is_empty() {
            return;
        }
        self.selected_fallback =
            (self.selected_fallback + 1).min(self.fallbacks.len().saturating_sub(1));
        // Scroll to keep the selected item visible
        if self.selected_fallback >= self.fallback_scroll_offset + VISIBLE_COUNT {
            self.fallback_scroll_offset = self.selected_fallback.saturating_sub(VISIBLE_COUNT - 1);
        }
    }

    pub fn select_prev_fallback(&mut self) {
        if self.fallbacks.is_empty() {
            return;
        }
        self.selected_fallback = self.selected_fallback.saturating_sub(1);
        // Scroll to keep the selected item visible
        if self.selected_fallback < self.fallback_scroll_offset {
            self.fallback_scroll_offset = self.selected_fallback;
        }
    }

    pub fn remove_selected_fallback(&mut self) -> Option<usize> {
        if self.fallbacks.len() <= 1 {
            return None;
        }
        let idx = self.selected_fallback;
        if idx < self.fallbacks.len() {
            self.fallbacks.remove(idx);
            self.selected_fallback = self
                .selected_fallback
                .min(self.fallbacks.len().saturating_sub(1));
            // Clamp scroll offset after removal
            if self.fallback_scroll_offset > 0
                && self.fallback_scroll_offset >= self.fallbacks.len()
            {
                self.fallback_scroll_offset = self.fallbacks.len().saturating_sub(VISIBLE_COUNT);
            }
            Some(idx)
        } else {
            None
        }
    }

    pub fn handle_number_input(&mut self, ch: char) -> bool {
        if ch.is_ascii_digit() {
            self.num_buffer.push(ch);
            true
        } else {
            false
        }
    }

    pub fn has_num_buffer(&self) -> bool {
        !self.num_buffer.is_empty()
    }

    pub fn clear_num_buffer(&mut self) {
        self.num_buffer.clear();
    }

    pub fn selected_model<'a>(&self, models: &'a [ModelEntry]) -> Option<&'a ModelEntry> {
        let filtered = self.filtered_models(models);
        filtered.get(self.selection.selected_index).copied()
    }

    pub fn clamp(&mut self, total: usize) {
        self.selection.clamp(total);
    }

    fn already_in_fallback(&self, provider: &str, model: &str) -> Option<usize> {
        self.fallbacks
            .iter()
            .position(|f| f.provider == provider && f.model == model)
    }

    pub fn add_selected_to_fallback(&mut self, models: &[ModelEntry]) -> Option<usize> {
        let (provider, model) = {
            let entry = self.selected_model(models)?;
            (entry.provider.clone(), entry.model.clone())
        };

        let raw: usize = self.num_buffer.parse().unwrap_or(0);
        self.num_buffer.clear();

        let pos = if raw > 0 {
            (raw - 1).min(self.fallbacks.len())
        } else {
            self.fallbacks.len()
        };

        let inserted = if let Some(existing) = self.already_in_fallback(&provider, &model) {
            if existing == pos {
                self.selected_fallback = pos;
                return Some(pos);
            }
            self.fallbacks.remove(existing);
            let pos = if existing < pos {
                pos.saturating_sub(1)
            } else {
                pos
            };
            let pos = pos.min(self.fallbacks.len());
            self.fallbacks
                .insert(pos, FallbackEntry { provider, model });
            pos
        } else {
            let new_entry = FallbackEntry { provider, model };
            self.fallbacks.insert(pos, new_entry);
            pos
        };
        self.selected_fallback = inserted;
        // Ensure the newly added/repositioned fallback is visible
        if self.selected_fallback >= self.fallback_scroll_offset + VISIBLE_COUNT {
            self.fallback_scroll_offset = self.selected_fallback.saturating_sub(VISIBLE_COUNT - 1);
        } else if self.selected_fallback < self.fallback_scroll_offset {
            self.fallback_scroll_offset = self.selected_fallback;
        }
        Some(inserted)
    }

    pub fn remove_fallback(&mut self, index: usize) {
        if self.fallbacks.len() > 1 && index < self.fallbacks.len() {
            self.fallbacks.remove(index);
        }
        // Clamp scroll offset after removal
        if self.fallback_scroll_offset > 0
            && self.fallback_scroll_offset >= self.fallbacks.len()
        {
            self.fallback_scroll_offset = self.fallbacks.len().saturating_sub(VISIBLE_COUNT);
        }
    }

    fn compute_layout(&self, area: Rect) -> (Rect, Rect, u16) {
        let work_h = area.height.saturating_sub(FOOTER_MARGIN);
        let work_area = Rect::new(area.x, area.y, area.width, work_h);
        let panel_w = work_area.width / 2;
        let left = Rect::new(work_area.x, work_area.y, panel_w, work_area.height);
        let right = Rect::new(
            work_area.x + panel_w,
            work_area.y,
            work_area.width - panel_w,
            work_area.height,
        );
        (left, right, work_h)
    }

    pub fn handle_mouse(&mut self, models: &[ModelEntry], mouse: &MouseEvent, area: Rect) -> bool {
        let (left_area, right_area, work_h) = self.compute_layout(area);

        if mouse.x >= left_area.x && mouse.x < left_area.right() {
            self.focus = FocusTarget::Models;
            let filtered = self.filtered_models(models);
            if !filtered.is_empty() {
                let list_top = left_area.y + MODEL_LIST_TOP_OFFSET;
                let max_visible = (work_h.saturating_sub(MODEL_LIST_TOP_OFFSET + 1)) as usize;
                let row = mouse.y.saturating_sub(list_top) as usize;
                if row < max_visible {
                    let idx = self.selection.scroll_offset + row;
                    if idx < filtered.len() {
                        self.selection.selected_index = idx;
                        self.add_selected_to_fallback(models);
                        return true;
                    }
                }
            }
            return true;
        }

        if mouse.x >= right_area.x && mouse.x < right_area.right() {
            self.focus = FocusTarget::Fallbacks;
            if !self.fallbacks.is_empty() {
                let list_top = right_area.y + FALLBACK_LIST_TOP_OFFSET;
                let max_visible = (work_h.saturating_sub(FALLBACK_LIST_TOP_OFFSET + 1)) as usize;
                let row = mouse.y.saturating_sub(list_top) as usize;
                if row < max_visible {
                    let idx = self.fallback_scroll_offset + row;
                    if idx < self.fallbacks.len() {
                        self.selected_fallback = idx;
                        return true;
                    }
                }
            }
            return true;
        }

        false
    }

    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        models: &[ModelEntry],
        _now: std::time::SystemTime,
    ) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);
        let accent = rgba_color(theme.accent);
        let bg_full = rgba_color(theme.background);

        let (left_area, right_area, work_h) = self.compute_layout(area);

        let filtered = self.filtered_models(models);

        let panel_bg = rgba_color(theme.background_panel);
        let title_bg = primary;
        let title_fg = rgba_color(theme.background);

        let models_focused = self.focus == FocusTarget::Models;
        let fallbacks_focused = self.focus == FocusTarget::Fallbacks;

        let text_pad = SIDE_PADDING / 2;
        let title_pad = 1;

        // ── Left panel: available models ──
        let title_style = Style::default().fg(muted);
        draw_text_line(
            buf,
            "Available Models",
            left_area.x + text_pad,
            left_area.y,
            left_area.width.saturating_sub(SIDE_PADDING),
            title_style,
        );

        let filter_y = left_area.y + 1;
        let filter_x = left_area.x + text_pad;
        let filter_w = left_area.width.saturating_sub(SIDE_PADDING);
        self.search_bar
            .render(buf, filter_x, filter_y, filter_w, theme);

        let list_top = left_area.y + MODEL_LIST_TOP_OFFSET;
        let list_x = left_area.x + text_pad;
        let list_w = left_area.width.saturating_sub(SIDE_PADDING);

        if filtered.is_empty() {
            let msg = if self.search_bar.is_empty() {
                "No models loaded"
            } else {
                "No matching models"
            };
            let msg_x = left_area.x + (left_area.width.saturating_sub(msg.len() as u16)) / 2;
            draw_text_line(
                buf,
                msg,
                msg_x,
                list_top,
                left_area.width.saturating_sub(text_pad),
                Style::default().fg(muted),
            );
        } else {
            let max_visible = (work_h.saturating_sub(MODEL_LIST_TOP_OFFSET + 1)) as usize;
            let max_visible = max_visible.min(filtered.len()).max(1);

            for i in 0..max_visible {
                let idx = self.selection.scroll_offset + i;
                if idx >= filtered.len() {
                    break;
                }
                let entry = filtered[idx];
                let y = list_top + i as u16;
                if y >= left_area.bottom() {
                    break;
                }

                let is_selected = models_focused && idx == self.selection.selected_index;
                let row_bg = if is_selected { primary } else { bg_full };
                for cx in list_x..list_x + list_w {
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(row_bg));
                    }
                }

                let name_fg = if is_selected {
                    primary_contrast_fg(theme)
                } else {
                    fg
                };

                let text = format!("{}  {}", entry.provider, entry.model);
                draw_text_line(
                    buf,
                    &text,
                    list_x,
                    y,
                    list_w,
                    Style::default().fg(name_fg).bg(row_bg),
                );
            }
        }

        // ── Right panel: fallback chain (styled box) ──
        fill_rect(
            buf,
            right_area.x,
            right_area.y,
            right_area.width,
            right_area.height,
            Style::default().bg(panel_bg),
        );

        section_title(
            buf,
            right_area.x + title_pad,
            right_area.y,
            " Fallback Chain ",
            title_bg,
            title_fg,
        );

        let fallback_list_top = right_area.y + FALLBACK_LIST_TOP_OFFSET;
        let fb_list_x = right_area.x + text_pad;
        let fb_list_w = right_area.width.saturating_sub(SIDE_PADDING);

        if self.fallbacks.is_empty() {
            draw_text_line(
                buf,
                "Select a model on the left to add",
                fb_list_x,
                fallback_list_top,
                fb_list_w,
                Style::default().fg(muted).bg(panel_bg),
            );
        } else {
            let max_visible = (work_h.saturating_sub(FALLBACK_LIST_TOP_OFFSET + 1)) as usize;
            let scroll = self.fallback_scroll_offset;

            for (i, fb) in self
                .fallbacks
                .iter()
                .enumerate()
                .skip(scroll)
                .take(max_visible)
            {
                let y = fallback_list_top + (i - scroll) as u16;
                if y >= right_area.bottom() {
                    break;
                }

                let is_selected = fallbacks_focused && i == self.selected_fallback;
                let entry_bg = if is_selected { primary } else { panel_bg };

                let entry_fg = if is_selected {
                    primary_contrast_fg(theme)
                } else {
                    fg
                };

                for cx in fb_list_x..fb_list_x + fb_list_w {
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(entry_bg));
                    }
                }

                let num_str = format!("{}.", i + 1);
                draw_text_line(
                    buf,
                    &num_str,
                    fb_list_x,
                    y,
                    fb_list_w,
                    Style::default().fg(accent).bg(entry_bg),
                );

                let entry_text = format!(" {}  {}", fb.provider, fb.model);
                let entry_x = fb_list_x + num_str.len() as u16 + 1;
                draw_text_line(
                    buf,
                    &entry_text,
                    entry_x,
                    y,
                    fb_list_w.saturating_sub(num_str.len() as u16 + 1),
                    Style::default().fg(entry_fg).bg(entry_bg),
                );
            }
        }

        // ── Footer instructions ──
        let footer_bg = rgba_color(theme.background);
        for y in left_area.bottom()..left_area.bottom() + 3 {
            for cx in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(footer_bg));
                }
            }
        }
        let footer_y = left_area.bottom() + 2;

        let mut footer = String::from("Enter to add · type number before Enter to position");
        if self.has_num_buffer() {
            footer = format!(
                "Position: {} · Enter to confirm · Esc to cancel",
                self.num_buffer
            );
        }
        let footer_x = area.x + (area.width.saturating_sub(footer.len() as u16)) / 2;
        draw_text_line(
            buf,
            &footer,
            footer_x,
            footer_y,
            area.width.saturating_sub(text_pad),
            Style::default().fg(muted).bg(footer_bg),
        );
    }
}
