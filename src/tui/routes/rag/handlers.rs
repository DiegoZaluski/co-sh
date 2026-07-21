//! Input handling for the RAG Knowledge Base view.
//!
//! Keyboard and mouse event handling, extracted from the original monolith.

use ratatui::layout::Rect;
use cosh_tui::core::types::MouseEvent;

use super::models::{RagAction, RagMode};
use super::view::RagView;

// ── Keyboard handling ──────────────────────────────────────────────────

impl RagView {
    pub fn handle_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> Option<RagAction> {
        // When the DB picker is open, route keys there first
        if self.show_db_picker {
            return self.handle_db_picker_key(key);
        }

        // When the Create DB form is expanded, route keys to it first
        if self.show_create_db {
            return self.handle_create_db_key(key);
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
                    if !self.has_selected_db() {
                        return Some(RagAction::ShowWarning(
                            "Select a database first to embed the content.".into()
                        ));
                    }
                    return Some(RagAction::FetchUrlOrPath(input));
                }
                RagMode::Previewing => {
                    if !self.content_preview.is_empty() {
                        let db_name = match &self.selected_db_for_embed {
                            Some(name) => name.clone(),
                            None => return Some(RagAction::ShowWarning(
                                "Select a database first to embed the content.".into()
                            )),
                        };
                        let db_desc = self.registry.find(&db_name)
                            .map(|db| db.description.clone())
                            .unwrap_or_default();
                        let model = self.available_models.get(self.selected_model_index).cloned();
                        return Some(RagAction::EmbedContent {
                            content: self.content_preview.clone(),
                            db_name,
                            db_description: db_desc,
                            model,
                        });
                    }
                    Some(RagAction::Consumed)
                }
                _ => Some(RagAction::Consumed),
            },
            ratatui::crossterm::event::KeyCode::Esc => {
                if self.show_db_picker {
                    self.close_db_picker();
                    return Some(RagAction::Consumed);
                }
                if self.mode == RagMode::Previewing {
                    // Previewing: just close the preview overlay
                    self.close_preview();
                    return Some(RagAction::ClosePreview);
                }
                // Idle or other: exit the RAG view entirely
                self.mode = RagMode::Idle;
                self.content_preview.clear();
                self.preview_scroll = 0;
                self.url_input.clear();
                Some(RagAction::Back)
            }
            ratatui::crossterm::event::KeyCode::Tab => {
                self.show_create_db = !self.show_create_db;
                self.show_db_picker = false; // close DB picker if open
                if !self.show_create_db {
                    self.selected_model_index = 0;
                    self.model_scroll_offset = 0;
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
                    super::registry::RagRegistry::save_active(&self.active_dbs);
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

    /// Handle keys when the Create DB form is focused.
    /// Handle keys when the Create DB form is focused.
    /// Returns CreateDb action when user confirms creation.
    /// Handle keys when the Create DB form is focused.
    /// Returns CreateDb action when user confirms creation.
    fn handle_create_db_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> Option<RagAction> {
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
                    // Keep selected model visible in scroll
                    let max_vis = 8usize;
                    if self.selected_model_index >= self.model_scroll_offset + max_vis {
                        self.model_scroll_offset =
                            self.selected_model_index.saturating_sub(max_vis.saturating_sub(1));
                    }
                    if self.selected_model_index < self.model_scroll_offset {
                        self.model_scroll_offset = self.selected_model_index;
                    }
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Enter => {
                if self.models_expanded {
                    // Collapse models
                    self.models_expanded = false;
                    Some(RagAction::Consumed)
                } else if !self.db_name_input.is_empty() {
                    // Create the DB
                    let name = self.db_name_input.clone();
                    let description = self.db_description_input.clone();
                    let embedder = self
                        .available_models
                        .get(self.selected_model_index)
                        .map(|m| match m {
                            super::models::EmbedModelEntry::Local(lm) => {
                                super::models::EmbedderConfig::Local { model: *lm }
                            }
                            super::models::EmbedModelEntry::Cloud(p, m) => {
                                super::models::EmbedderConfig::Cloud(
                                    super::models::CloudEmbedConfig {
                                        provider: p.clone(),
                                        model: m.clone(),
                                    }
                                )
                            }
                        })
                        .unwrap_or(super::models::EmbedderConfig::Local {
                            model: super::models::LocalEmbedModel::AllMiniLML6V2,
                        });
                    // Close form
                    self.show_create_db = false;
                    self.models_expanded = false;
                    self.selected_model_index = 0;
                    self.model_scroll_offset = 0;
                    self.db_name_input.clear();
                    self.db_description_input.clear();
                    Some(RagAction::CreateDb { name, description, embedder })
                } else {
                    Some(RagAction::ShowWarning("Enter a database name first.".into()))
                }
            }
            ratatui::crossterm::event::KeyCode::Tab
            | ratatui::crossterm::event::KeyCode::Esc => {
                self.show_create_db = false;
                self.models_expanded = false;
                self.selected_model_index = 0;
                self.model_scroll_offset = 0;
                self.db_name_input.clear();
                self.db_description_input.clear();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Char(ch) => {
                self.db_name_input.push(ch);
                self.db_name_cursor.note_activity();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Backspace => {
                self.db_name_input.pop();
                self.db_name_cursor.note_activity();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Delete => {
                let len = self.db_name_input.len();
                if len > 0 {
                    self.db_name_input.remove(0);
                    self.db_name_cursor.note_activity();
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Left
            | ratatui::crossterm::event::KeyCode::Right
            | ratatui::crossterm::event::KeyCode::Home
            | ratatui::crossterm::event::KeyCode::End => {
                Some(RagAction::Consumed)
            }
            _ => Some(RagAction::Consumed),
        }
    }

    /// Handle keys when the DB picker overlay is open.
    fn handle_db_picker_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> Option<RagAction> {
        let filtered = self.filtered_dbs();
        if filtered.is_empty() {
            // No DBs to pick — just close on any key
            self.close_db_picker();
            return Some(RagAction::Consumed);
        }
        match key {
            ratatui::crossterm::event::KeyCode::Up => {
                let total = filtered.len();
                let cur_idx = filtered
                    .iter()
                    .position(|db| Some(db.name.as_str()) == self.selected_db_for_embed.as_deref())
                    .unwrap_or(0);
                let new = if cur_idx == 0 { total - 1 } else { cur_idx - 1 };
                self.selected_db_for_embed = Some(filtered[new].name.clone());
                // Clamp scroll
                let scroll = &mut self.db_picker_scroll_offset;
                let vis = 10usize.min(total);
                if new >= *scroll + vis {
                    *scroll = new.saturating_sub(vis.saturating_sub(1));
                }
                if new < *scroll {
                    *scroll = new;
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Down => {
                let total = filtered.len();
                let cur_idx = filtered
                    .iter()
                    .position(|db| Some(db.name.as_str()) == self.selected_db_for_embed.as_deref())
                    .unwrap_or(0);
                let new = (cur_idx + 1) % total;
                self.selected_db_for_embed = Some(filtered[new].name.clone());
                // Clamp scroll
                let scroll = &mut self.db_picker_scroll_offset;
                let vis = 10usize.min(total);
                if new >= *scroll + vis {
                    *scroll = new.saturating_sub(vis.saturating_sub(1));
                }
                if new < *scroll {
                    *scroll = new;
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Enter => {
                // Pick the currently selected DB and close
                self.close_db_picker();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Esc => {
                self.close_db_picker();
                Some(RagAction::Consumed)
            }
            _ => Some(RagAction::Consumed),
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.url_input.handle_paste(text);
    }

    // ── Preview overlay geometry (shared with mouse handling) ──────────

    /// Compute the preview overlay rectangle for the given area.
    pub(crate) fn preview_overlay_rect(&self, area: Rect) -> Option<Rect> {
        if !self.is_preview_visible() {
            return None;
        }
        let overlay_w = (area.width * 85 / 100).max(40).min(area.width.saturating_sub(4));
        let overlay_h = (area.height * 80 / 100).max(10).min(area.height.saturating_sub(4));
        let overlay_x = area.x + (area.width - overlay_w) / 2;
        let overlay_y = area.y + (area.height - overlay_h) / 2;
        Some(Rect::new(overlay_x, overlay_y, overlay_w, overlay_h))
    }
}

// ── Mouse handling ─────────────────────────────────────────────────────

impl RagView {
    /// Public entry point. Returns `Some(index)` for a DB row click,
    /// `None` otherwise.
    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        self.find_db_row_for_mouse(mouse, area)
    }

    /// Public query for the create-DB button.
    pub fn is_create_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        !self.show_create_db && !self.show_db_picker && self.is_create_db_clicked(mouse, area)
    }

    /// Public query for the "Select Database" button.
    pub fn is_select_db_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        !self.show_create_db && !self.show_db_picker && !self.registry.dbs.is_empty()
            && self.is_select_db_btn_clicked(mouse, area)
    }

    /// Public query for DB picker row clicks.
    pub fn is_db_picker_row_click(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        if !self.show_db_picker {
            return None;
        }
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return None;
        }
        let inner_x = area.x + 2;
        let input_h = self.url_input.height();

        // The picker renders at cy, with fixed height 5 (matching create form)
        let picker_y = area.y + 2 + input_h;
        let picker_h = 5u16;

        let mx = mouse.x;
        let my = mouse.y;

        if my >= picker_y && my < picker_y + picker_h && mx >= inner_x && mx < inner_x + inner_w {
            let filtered = self.filtered_dbs();
            let list_start = picker_y + 1; // skip title line
            let row = my.saturating_sub(list_start) as usize;
            let idx = self.db_picker_scroll_offset + row;
            if idx < filtered.len() {
                return Some(idx);
            }
        }
        None
    }

    /// Public query for model line clicks.
    pub fn is_model_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        self.is_model_line_clicked(mouse, area)
    }

    /// Check if the mouse click is outside the preview overlay.
    /// If the preview is visible and the click is outside its bounds, return true.
    pub fn is_click_outside_preview(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let Some(overlay) = self.preview_overlay_rect(area) else {
            return false;
        };
        let mx = mouse.x;
        let my = mouse.y;
        mx < overlay.x
            || mx >= overlay.right()
            || my < overlay.y
            || my >= overlay.bottom()
    }

    /// Find which DB row was clicked (if any).
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
        let cx = area.x + 4;

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

    /// Check if the mouse click is on the "+ Create DB" button.
    fn is_create_db_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let input_h = self.url_input.height();
        let input_w = inner_w.saturating_sub(4);
        let cx = area.x + 4;
        let mx = mouse.x;
        let my = mouse.y;

        // Button is on the status line cy = area.y + 2 + input_h
        let btn_y = area.y + 2 + input_h;

        // Rightmost button: "+ Create DB"
        let btn = " +Create DB ";
        let btn_len = btn.len() as u16;
        let right_side_start = cx + input_w.saturating_sub(2);
        let btn_x = right_side_start.saturating_sub(btn_len);

        my == btn_y && mx >= btn_x && mx < btn_x + btn_len
    }

    /// Check if the mouse click is on the collapsed model line (to toggle expand).
    fn is_model_line_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db || self.models_expanded {
            return false;
        }
        let model_y = area.y + 2 + self.url_input.height();
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let cx = area.x + 4;
        let pad_w = inner_w.saturating_sub(8);
        let mx = mouse.x;
        let my = mouse.y;

        my == model_y && mx >= cx + 2 && mx < cx + 2 + pad_w
    }


    /// Check if the mouse click is on the "Esc to close" label in the Create DB form.
    fn is_form_esc_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db {
            return false;
        }
        let input_h = self.url_input.height();
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let cx = area.x + 4;
        let pad = cx + 2;
        let esc_label = "Esc to close";

        // The "Esc to close" is at the bottom of the form
        // Approximate Y position: after input (3) + model line (1) + name (1) + desc (1)
        let form_bottom = area.y + 2 + input_h + 3 + 1; // title gap + input + model + name + desc
        let my = mouse.y;
        let mx = mouse.x;

        my == form_bottom && mx >= pad && mx < pad + esc_label.len() as u16
    }    /// Check if mouse is on the "Select Database" button.
    fn is_select_db_btn_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let inner_w = area.width.saturating_sub(4);
        if inner_w < 10 {
            return false;
        }
        let input_h = self.url_input.height();
        let input_w = inner_w.saturating_sub(4);
        let cx = area.x + 4;
        let mx = mouse.x;
        let my = mouse.y;

        // Button is on the status line cy = area.y + 2 + input_h
        let btn_y = area.y + 2 + input_h;

        let sel_btn = " Select DB ";
        let sel_btn_len = sel_btn.len() as u16;
        let create_btn = " +Create DB ";
        let right_side_start = cx + input_w.saturating_sub(2);
        // Buttons are rendered right-to-left: first Create DB, then Select DB to its left
        // Select DB starts at: right_side_start - len(Create) - len(Select)
        let sel_btn_x = right_side_start
            .saturating_sub(create_btn.len() as u16)
            .saturating_sub(sel_btn_len);

        my == btn_y && mx >= sel_btn_x && mx < sel_btn_x + sel_btn_len
    }

    /// Dismiss the Create DB form on click outside any target (used by app.rs).
    pub fn is_dismiss_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db {
            return false;
        }
        // If click is not on form esc, not on model line, not on create button area,
        // and not on the form itself, it's a dismiss click.
        !self.is_create_db_clicked(mouse, area)
            && !self.is_model_line_clicked(mouse, area)
            && !self.is_form_esc_clicked(mouse, area)
    }
}
