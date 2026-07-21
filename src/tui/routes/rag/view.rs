//! RAG Knowledge Base view.
//!
//! Layout — two boxed sections:
//! 1. **Embed Content** — URL/path input (thin prompt-style) + spacious content preview
//! 2. **Available Databases** — toggleable DB list with inline "+ Create New Database"
//!    at the bottom (Tab expands the creation form inside this same box)

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;

use cosh_tui::core::types::MouseEvent;

use crate::component::cursor::Cursor;
use crate::component::rag_input::RagInput;
use crate::component::search_bar::SearchBar;
use crate::component::spinner::SpinnerState;
use crate::theme::Theme;

use super::rag_registry::{EmbedModelEntry, RagRegistry};

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

// ── Modes ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RagMode {
    Idle,
    Fetching,
    Previewing,
    Embedding,
}

// ── RagView ─────────────────────────────────────────────────────────────

pub struct RagView {
    mode: RagMode,
    url_input: RagInput,
    content_preview: String,
    preview_scroll: usize,
    spinner: SpinnerState,

    // Create DB form (shown when `show_create_db` is true)
    show_create_db: bool,
    models_expanded: bool,
    selected_model_index: usize,
    available_models: Vec<EmbedModelEntry>,
    db_name_input: String,
    db_name_cursor: Cursor,
    db_description_input: String,
    db_description_cursor: Cursor,

    // DB registry + filter
    registry: RagRegistry,
    /// Receiver for async URL fetch result.
    fetch_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    selected_db_index: usize,
    dbs_scroll_offset: usize,
    active_dbs: HashSet<String>,
    db_filter: SearchBar,
}

impl RagView {
    pub fn new() -> Self {
        let registry = RagRegistry::load();
        let active_dbs = RagRegistry::load_active();
        let available_models = EmbedModelEntry::all_available();
        Self {
            mode: RagMode::Idle,
            url_input: RagInput::new(),
            content_preview: String::new(),
            preview_scroll: 0,
            spinner: SpinnerState::new(),
            show_create_db: false,
            models_expanded: false,
            selected_model_index: 0,
            available_models,
            db_name_input: String::new(),
            db_name_cursor: Cursor::new(),
            db_description_input: String::new(),
            db_description_cursor: Cursor::new(),
            registry,
            fetch_rx: None,
            selected_db_index: 0,
            dbs_scroll_offset: 0,
            active_dbs,
            db_filter: SearchBar::new(),
        }
    }

    // ── Input handling ────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> Option<RagAction> {
        // When the Create DB form is expanded, route keys to it first
        if self.show_create_db {
            match key {
                ratatui::crossterm::event::KeyCode::Up
                | ratatui::crossterm::event::KeyCode::Down => {
                    if self.models_expanded && !self.available_models.is_empty() {
                        let total = self.available_models.len();
                        self.selected_model_index = match key {
                            ratatui::crossterm::event::KeyCode::Up => {
                                if self.selected_model_index == 0 {
                                    total - 1
                                } else {
                                    self.selected_model_index - 1
                                }
                            }
                            _ => (self.selected_model_index + 1) % total,
                        };
                    }
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Enter => {
                    if self.models_expanded {
                        self.models_expanded = false;
                    }
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Tab
                | ratatui::crossterm::event::KeyCode::Esc => {
                    self.show_create_db = false;
                    self.models_expanded = false;
                    self.selected_model_index = 0;
                    self.db_name_input.clear();
                    self.db_description_input.clear();
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Char(ch) => {
                    self.db_name_input.push(ch);
                    self.db_name_cursor.note_activity();
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Backspace => {
                    self.db_name_input.pop();
                    self.db_name_cursor.note_activity();
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Delete => {
                    let len = self.db_name_input.len();
                    if len > 0 {
                        self.db_name_input.remove(0);
                        self.db_name_cursor.note_activity();
                    }
                    return Some(RagAction::Consumed);
                }
                ratatui::crossterm::event::KeyCode::Left
                | ratatui::crossterm::event::KeyCode::Right
                | ratatui::crossterm::event::KeyCode::Home
                | ratatui::crossterm::event::KeyCode::End => {
                    return Some(RagAction::Consumed);
                }
                _ => return Some(RagAction::Consumed),
            }
        }

        match key {
            ratatui::crossterm::event::KeyCode::Up => {
                if self.mode == RagMode::Previewing && self.preview_scroll > 0 {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                } else if !self.registry.dbs.is_empty() {
                    let total = self.registry.dbs.len();
                    self.selected_db_index =
                        if self.selected_db_index == 0 { total - 1 } else { self.selected_db_index - 1 };
                    self.clamp_db_scroll();
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Down => {
                if self.mode == RagMode::Previewing
                    && self.preview_scroll < self.max_preview_scroll()
                {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                } else if !self.registry.dbs.is_empty() {
                    let total = self.registry.dbs.len();
                    self.selected_db_index = (self.selected_db_index + 1) % total;
                    self.clamp_db_scroll();
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Enter => match self.mode {
                RagMode::Idle => {
                    let input = self.url_input.as_str().to_string();
                    if input.is_empty() {
                        return Some(RagAction::Consumed);
                    }
                    return Some(RagAction::FetchUrlOrPath(input));
                }
                RagMode::Previewing => {
                    if !self.content_preview.is_empty() && !self.db_name_input.is_empty() {
                        let model = self.available_models.get(self.selected_model_index).cloned();
                        return Some(RagAction::EmbedContent {
                            content: self.content_preview.clone(),
                            db_name: self.db_name_input.clone(),
                            db_description: self.db_description_input.clone(),
                            model,
                        });
                    }
                    Some(RagAction::Consumed)
                }
                _ => Some(RagAction::Consumed),
            },
            ratatui::crossterm::event::KeyCode::Esc => {
                if self.mode == RagMode::Previewing || self.mode == RagMode::Idle {
                    self.mode = RagMode::Idle;
                    self.content_preview.clear();
                    self.preview_scroll = 0;
                    self.url_input.clear();
                    return Some(RagAction::Back);
                }
                Some(RagAction::Back)
            }
            ratatui::crossterm::event::KeyCode::Tab => {
                self.show_create_db = !self.show_create_db;
                if !self.show_create_db {
                    self.selected_model_index = 0;
                    self.db_name_input.clear();
                    self.db_description_input.clear();
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Char(' ') => {
                if !self.registry.dbs.is_empty() && self.selected_db_index < self.registry.dbs.len() {
                    let name = &self.registry.dbs[self.selected_db_index].name;
                    if !self.active_dbs.remove(name) {
                        self.active_dbs.insert(name.clone());
                    }
                    RagRegistry::save_active(&self.active_dbs);
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Home => {
                self.url_input.cursor_home();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::End => {
                self.url_input.cursor_end();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Left => {
                self.url_input.cursor_left();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Right => {
                self.url_input.cursor_right();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Delete => {
                self.url_input.delete_forward();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Backspace => {
                self.url_input.pop_char();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Char(ch) => {
                self.url_input.push_char(ch);
                Some(RagAction::Consumed)
            }
            _ => Some(RagAction::Consumed),
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.url_input.handle_paste(text);
    }

    // ── State transitions ─────────────────────────────────────────────

    pub fn start_fetch(&mut self, _source: &str) {
        self.mode = RagMode::Fetching;
        self.spinner = SpinnerState::new();
    }
    pub fn content_fetched(&mut self, content: String) {
        self.mode = RagMode::Previewing;
        self.content_preview = content;
        self.preview_scroll = 0;
    }
    pub fn start_embedding(&mut self) {
        self.mode = RagMode::Embedding;
        self.spinner = SpinnerState::new();
    }
    pub fn embedding_done(&mut self) {
        self.mode = RagMode::Idle;
        self.content_preview.clear();
        self.preview_scroll = 0;
        self.url_input.clear();
        self.db_name_input.clear();
        self.db_description_input.clear();
        self.show_create_db = false;
        self.registry = RagRegistry::load();
    }
    pub fn set_error(&mut self) {
        self.mode = RagMode::Idle;
    }
    pub fn set_fetch_rx(&mut self, rx: std::sync::mpsc::Receiver<Result<String, String>>) {
        self.fetch_rx = Some(rx);
    }

    pub fn advance_spinner(&mut self) {
        self.spinner.advance();
        // Check if async URL fetch completed
        if let Some(rx) = &self.fetch_rx {
            if let Ok(result) = rx.try_recv() {
                self.fetch_rx = None;
                match result {
                    Ok(content) => {
                        self.mode = RagMode::Previewing;
                        self.content_preview = content;
                        self.preview_scroll = 0;
                    }
                    Err(_e) => {
                        self.mode = RagMode::Idle;
                    }
                }
            }
        }
    }
    pub fn active_dbs(&self) -> &HashSet<String> {
        &self.active_dbs
    }

    // ── Helpers ───────────────────────────────────────────────────────

    fn max_preview_scroll(&self) -> usize {
        // Generous upper bound for scroll; the actual visible count is
        // determined during render from the proportional preview_h.
        self.content_preview
            .lines()
            .count()
            .saturating_sub(1)
            .max(0)
    }

    fn clamp_db_scroll(&mut self) {
        let total = self.registry.dbs.len();
        if total == 0 {
            self.dbs_scroll_offset = 0;
            return;
        }
        let visible = 20usize.min(total);
        if self.selected_db_index >= self.dbs_scroll_offset + visible {
            self.dbs_scroll_offset =
                self.selected_db_index.saturating_sub(visible.saturating_sub(1));
        }
        if self.selected_db_index < self.dbs_scroll_offset {
            self.dbs_scroll_offset = self.selected_db_index;
        }
    }

    fn filtered_dbs(&self) -> Vec<&super::rag_registry::RagDb> {
        let filter = self.db_filter.as_str();
        if filter.is_empty() {
            return self.registry.dbs.iter().collect();
        }
        let lower = filter.to_lowercase();
        self.registry
            .dbs
            .iter()
            .filter(|db| {
                db.name.to_lowercase().contains(&lower)
                    || db.description.to_lowercase().contains(&lower)
            })
            .collect()
    }

    // ── Rendering ─────────────────────────────────────────────────────

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
        let bottom_gap: u16 = 0; // Box 2 uses bottom space, never steals from Box 1

        // ── Layout (fixed proportional split for ALL modes) ────────────
        // Box 1 gets ~30%, Box 2 gets the rest.
        let input_h = self.url_input.height(); // 3
        let db_model_count = if self.show_create_db && self.models_expanded {
            self.available_models.len().min(8)
        } else {
            0
        };
        let create_db_lines: u16 = if self.show_create_db {
            if self.models_expanded {
                4 + db_model_count as u16 + 1  // header + N models + name + desc + esc + padding
            } else {
                5  // current model + name + desc + esc + padding
            }
        } else {
            1
        };
        let box1_h = {
            let ideal = avail_h.saturating_mul(30) / 100;
            ideal.max(7 + create_db_lines).min(avail_h.saturating_sub(8))
        };
        // Box 2 — Available Databases: remaining space
        let box2_available = avail_h.saturating_sub(box1_h + gap + bottom_gap);
        let box2_h = box2_available.max(5);

        let mut y = area.y;

        // ══════════ BOX 1: Embed Content ══════════════════════════════
        {
            let title = " Embed Content ";
            fill_rect(
                buf,
                inner_x,
                y,
                inner_w,
                box1_h,
                Style::default().bg(panel_bg),
            );
            section_title(buf, inner_x + 1, y, title, title_bg, title_fg);

            let cx = inner_x + 2;
            let mut cy = y + 2; // gap row below title

            // ── URL/path input (thin, prompt-style) ───────────────────
            let input_w = inner_w.saturating_sub(4);
            self.url_input.render(buf, cx, cy, input_w, theme);
            cy += input_h;

            // ── Preview / status ──────────────────────────────────────
            match self.mode {
                RagMode::Fetching => {
                    let ch = self.spinner.current_char();
                    if let Some(cell) = buf.cell_mut((cx, cy)) {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(success));
                    }
                    draw_text_line(
                        buf,
                        " Fetching content...",
                        cx + 2,
                        cy,
                        input_w.saturating_sub(2),
                        Style::default().fg(muted),
                    );
                }
                RagMode::Previewing => {
                    // Preview is rendered as an overlay popup (see end of render()).
                    // Here we just show a status hint in Box 1.
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
                RagMode::Idle => {
                    // Nothing below input — placeholder already shows hint
                }
            }

            // ── Create New Database (mini-box with terminal bg) ────────────
            if self.show_create_db {
                // Fill mini-box with terminal bg to visually separate from panel
                let mini_box_h = create_db_lines;
                fill_rect(buf, cx, cy, input_w, mini_box_h, Style::default().bg(rgba_color(theme.background)));

                let pad = cx + 2;
                let pad_w = input_w.saturating_sub(4);

                // ── Model selector (collapsed or expanded) ──────────────────
                if self.models_expanded {
                    draw_text_line(buf, "Model", pad, cy, pad_w, Style::default().fg(muted));
                    cy += 1;
                    let show_count = db_model_count;
                    for i in 0..show_count {
                        if i >= self.available_models.len() {
                            break;
                        }
                        let is_sel = i == self.selected_model_index;
                        let lbl = self.available_models[i].label();
                        let prefix = if is_sel { "\u{25cf} " } else { "  " };
                        let line = format!("{prefix}{lbl}");

                        if is_sel {
                            fill_rect(buf, cx, cy, input_w, 1, Style::default().bg(primary));
                            draw_text_line(
                                buf,
                                &line,
                                pad,
                                cy,
                                pad_w,
                                Style::default().fg(title_fg).bg(primary),
                            );
                        } else {
                            draw_text_line(
                                buf,
                                &line,
                                pad,
                                cy,
                                pad_w,
                                Style::default().fg(fg),
                            );
                        }
                        cy += 1;
                    }
                } else {
                    // Collapsed: show current model inline
                    let model_lbl = self
                        .available_models
                        .get(self.selected_model_index)
                        .map(|m| m.label())
                        .unwrap_or_else(|| "No models".to_string());
                    let coll = format!("Model: {model_lbl}  (Click to expand)");
                    draw_text_line(buf, &coll, pad, cy, pad_w, Style::default().fg(primary));
                    cy += 1;
                }

                // ── Name & Description fields with padding ─────────────────
                let nd = if self.db_name_input.is_empty() {
                    "Enter database name..."
                } else {
                    &self.db_name_input
                };
                let name_text = format!("Name:  {nd}");
                draw_text_line(buf, &name_text, pad, cy, pad_w, Style::default().fg(fg));
                cy += 1;

                let dd = if self.db_description_input.is_empty() {
                    "Briefly describe what this DB contains..."
                } else {
                    &self.db_description_input
                };
                let desc_text = format!("Desc:  {dd}");
                draw_text_line(buf, &desc_text, pad, cy, pad_w, Style::default().fg(muted));

                draw_text_line(buf, "Esc to close", pad, cy + 1, pad_w, Style::default().fg(muted));
            } else {
                let btn = " + Create New Database  (Tab)";
                let btn_x = cx + input_w.saturating_sub(btn.len() as u16 + 2);
                section_title(buf, btn_x, cy, btn, title_bg, title_fg);
            }

            y = y + box1_h + gap;
        }

        // ══════════ BOX 2: Available Databases + Create DB ════════════
        if y < area.bottom() {
            let title = " Available Databases ";
            let warning_h = 2u16;
            let filter_h = if self.registry.dbs.len() > 5 { 1u16 } else { 0u16 };
            let pad_bottom: u16 = 1;
            let gap: u16 = 1;  // row between title and warning
            let list_h = box2_h.saturating_sub(1 + gap + warning_h + filter_h + pad_bottom);
            let content_h = 1 + gap + warning_h + filter_h + list_h + pad_bottom;

            fill_rect(
                buf,
                inner_x,
                y,
                inner_w,
                content_h,
                Style::default().bg(panel_bg),
            );
            section_title(buf, inner_x + 1, y, title, title_bg, title_fg);

            let cx = inner_x + 2;
            let mut cy = y + 1;  // title row

            // Gap row (empty, rendered via fill_rect's panel_bg)

            // ── Gap row between title and warning ─────────────────────
            cy += 1;

            // ── Warning ───────────────────────────────────────────────
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

            // ── SearchBar filter (only when >5 DBs) ───────────────────
            if filter_h > 0 {
                self.db_filter.render(buf, cx, cy, inner_w.saturating_sub(4), theme);
                cy += 1;
            }

            // ── DB list ───────────────────────────────────────────────
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
                    draw_text_line(
                        buf,
                        &format!(" {} \u{2014} {}", db.name, db.description),
                        cx + 2,
                        db_y,
                        inner_w.saturating_sub(6),
                        Style::default().fg(rc),
                    );
                }
            }
        }

        // ══════════ OVERLAY: Content Preview popup ═════════════════════
        if self.mode == RagMode::Previewing && !self.content_preview.is_empty() {
            let overlay_w = (area.width * 85 / 100).max(40).min(area.width.saturating_sub(4));
            let overlay_h = (area.height * 80 / 100).max(10).min(area.height.saturating_sub(4));
            let overlay_x = area.x + (area.width - overlay_w) / 2;
            let overlay_y = area.y + (area.height - overlay_h) / 2;

            // Background fill
            let bg_color = rgba_color(theme.background_element);
            fill_rect(buf, overlay_x, overlay_y, overlay_w, overlay_h, Style::default().bg(bg_color));

            // Border
            let border_color = rgba_color(theme.border_active);
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

            let content_x = overlay_x + 2;
            let content_w = overlay_w.saturating_sub(4);
            let mut content_y = overlay_y + 1;

            // Title line: "Content Preview" + "esc" right-aligned
            let title_style = Style::default()
                .fg(rgba_color(theme.text))
                .add_modifier(ratatui::style::Modifier::BOLD);
            draw_text_line(buf, "Content Preview", content_x, content_y, content_w, title_style);
            draw_text_line(
                buf,
                "esc",
                content_x + content_w.saturating_sub("esc".len() as u16),
                content_y,
                content_w,
                Style::default().fg(muted),
            );
            content_y += 1;

            // Separator line
            for x in content_x..content_x + content_w {
                if let Some(cell) = buf.cell_mut((x, content_y)) {
                    cell.set_char('\u{2500}');
                    cell.set_style(Style::default().fg(border_color));
                }
            }
            content_y += 1;

            // Content area (scrollable)
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

            // Footer with hints
            let footer_y2 = max_y - 1;
            let footer_text = if total_lines > max_content_h as usize {
                let pct = scroll.saturating_mul(100) / max_scroll.max(1);
                format!("\u{2191}\u{2193} scroll  {pct}%  Enter to embed  Esc")
            } else {
                "Enter to embed  Esc to close".to_string()
            };
            draw_text_line(
                buf,
                &footer_text,
                content_x,
                footer_y2,
                content_w,
                Style::default().fg(muted),
            );
        }
    }
}

impl Default for RagView {
    fn default() -> Self {
        Self::new()
    }
}

// ── Mouse handling (immutable query, caller handles mutation) ──

impl RagView {
    /// Check if the mouse click is on a DB list row. Returns `Some(index)`
    /// of the clicked row (relative to the full registry).
    fn find_db_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let avail_h = area.height;
        let gap: u16 = 1;
        let bottom_gap: u16 = 0;
        let db_model_count = if self.show_create_db && self.models_expanded {
            self.available_models.len().min(8)
        } else {
            0
        };
        let create_db_lines: u16 = if self.show_create_db {
            if self.models_expanded {
                4 + db_model_count as u16 + 1
            } else {
                5
            }
        } else {
            1
        };
        let box1_h = {
            let ideal = avail_h.saturating_mul(30) / 100;
            ideal.max(7 + create_db_lines).min(avail_h.saturating_sub(8))
        };

        let box2_y = area.y + box1_h + gap;
        let box2_available = avail_h.saturating_sub(box1_h + gap + bottom_gap);
        let box2_h = box2_available.max(5);

        let warning_h = 2u16;
        let filter_h = if self.registry.dbs.len() > 5 { 1u16 } else { 0u16 };
        let pad_bottom: u16 = 1;
        let header_gap: u16 = 1;
        let list_h = box2_h.saturating_sub(1 + header_gap + warning_h + filter_h + pad_bottom);
        let list_start_y = box2_y + 1 + header_gap + warning_h + filter_h;

        let filtered = self.filtered_dbs();
        let max_visible = list_h as usize;
        let render_count = max_visible.min(filtered.len());
        let inner_w = area.width.saturating_sub(4);
        let row_max_w = inner_w.saturating_sub(4);
        let cx = area.x + 4;  // = inner_x + 2

        let my = mouse.y;
        let mx = mouse.x;

        if my >= list_start_y && my < list_start_y + render_count as u16 {
            let i = (my - list_start_y) as usize;
            let idx = self.dbs_scroll_offset + i;
            if idx < filtered.len() && mx >= cx && mx < cx + row_max_w {
                return Some(idx);
            }
        }
        None
    }

    /// Check if the mouse click is on the "+ Create New Database" button.
    fn is_create_db_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if self.show_create_db {
            return false;
        }
        let avail_h = area.height;
        let input_h = self.url_input.height();
        let create_db_lines: u16 = 1;  // button mode (not expanded)
        let box1_h = {
            let ideal = avail_h.saturating_mul(30) / 100;
            ideal.max(7 + create_db_lines).min(avail_h.saturating_sub(8))
        };
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let input_w = inner_w.saturating_sub(4);
        let cx = area.x + 4;

        let btn = " + Create New Database  (Tab)";
        let btn_len = btn.len() as u16;
        let btn_x = cx + input_w.saturating_sub(btn_len + 2);
        let mx = mouse.x;
        let my = mouse.y;

        // Anywhere below the input area, within x-range of button text
        let box1_content_top = area.y + 2 + input_h;
        let box1_bottom = area.y + box1_h;
        my >= box1_content_top && my < box1_bottom
            && mx >= btn_x && mx < btn_x + btn_len
    }

    /// Public entry point. Returns `Some(index)` for a DB row click,
    /// `None` otherwise. The Create DB button is queried separately.
    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        self.find_db_row_for_mouse(mouse, area)
    }

    /// Public query for the create-DB button (separate from DB row clicks).
    pub fn is_create_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        self.is_create_db_clicked(mouse, area)
    }

    /// Toggle the active state of a DB at the given registry index.
    pub fn toggle_db(&mut self, registry_idx: usize) {
        self.selected_db_index = registry_idx;
        let filtered = self.filtered_dbs();
        if registry_idx < filtered.len() {
            let name = filtered[registry_idx].name.clone();
            if !self.active_dbs.remove(&name) {
                self.active_dbs.insert(name);
            }
            RagRegistry::save_active(&self.active_dbs);
        }
    }

    pub fn toggle_create_db(&mut self) {
        self.show_create_db = !self.show_create_db;
        self.models_expanded = false;
        if !self.show_create_db {
            self.selected_model_index = 0;
            self.db_name_input.clear();
            self.db_description_input.clear();
        }
    }

    /// Returns true when the Create DB form is expanded.
    pub fn form_open(&self) -> bool {
        self.show_create_db
    }

    /// Close the Create DB form (no-op if already closed).
    pub fn close_form(&mut self) {
        if self.show_create_db {
            self.show_create_db = false;
            self.models_expanded = false;
            self.selected_model_index = 0;
            self.db_name_input.clear();
            self.db_description_input.clear();
        }
    }

    pub fn db_count(&self) -> usize {
        self.registry.dbs.len()
    }

    /// Check if the mouse click is on the collapsed model line (to toggle expand).
    fn is_model_line_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db || self.models_expanded {
            return false;
        }
        let input_h = self.url_input.height();
        let model_y = area.y + 2 + input_h;  // after title gap + input
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let input_w = inner_w.saturating_sub(4);
        let cx = area.x + 4;  // = inner_x + 2
        let pad_w = input_w.saturating_sub(4);
        let mx = mouse.x;
        let my = mouse.y;

        my == model_y && mx >= cx + 2 && mx < cx + 2 + pad_w
    }

    /// Public query for model line clicks (used by app.rs mouse handler).
    pub fn is_model_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        self.is_model_line_clicked(mouse, area)
    }

    /// Toggle the expanded/collapsed state of the model list.
    pub fn toggle_models_expanded(&mut self) {
        self.models_expanded = !self.models_expanded;
    }
}

#[derive(Debug, Clone)]
pub enum RagAction {
    Consumed,
    Back,
    FetchUrlOrPath(String),
    EmbedContent {
        content: String,
        db_name: String,
        db_description: String,
        model: Option<EmbedModelEntry>,
    },
}
