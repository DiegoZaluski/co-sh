//! Input handling for the RAG Knowledge Base view.
//!
//! Keyboard and mouse event handling, extracted from the original monolith.

use cosh_tui::core::types::MouseEvent;
use ratatui::layout::Rect;

use super::models::{CreateDbFocus, RagAction, RagMode};
use super::render::byte_pos_at_click;
use super::render::truncate_label;
use super::render::{
    DB_PICKER_VISIBLE, DB_ROW_H_PADDING, DESC_POPUP_HEIGHT_PCT, DESC_POPUP_MIN_H, DESC_POPUP_MIN_W,
    DESC_POPUP_WIDTH_PCT, FIELD_LABEL_W, MIN_CONTENT_WIDTH, PICKER_BOX_HEIGHT,
    PREVIEW_OVERLAY_HEIGHT_PCT, PREVIEW_OVERLAY_MIN_H, PREVIEW_OVERLAY_MIN_W,
    PREVIEW_OVERLAY_WIDTH_PCT, SHOW_DESC_BTN_TEXT, TRASH_EMOJI_WIDTH,
};
use super::view::RagView;
use crate::util::field_selection::DragSelection;

// Keyboard handling

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
                if self.show_desc_for_db.is_some() && self.desc_scroll > 0 {
                    self.desc_scroll = self.desc_scroll.saturating_sub(1);
                } else if self.mode == RagMode::Previewing && self.preview_scroll > 0 {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                } else if !self.registry.dbs.is_empty() {
                    let total = self.registry.dbs.len();
                    self.selected_db_index = if self.selected_db_index == 0 {
                        total - 1
                    } else {
                        self.selected_db_index - 1
                    };
                    self.clamp_db_scroll();
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Down => {
                if self.show_desc_for_db.is_some() {
                    let total_lines = self
                        .filtered_dbs()
                        .get(self.show_desc_for_db.unwrap_or(usize::MAX))
                        .map(|db| db.description.lines().count())
                        .unwrap_or(0);
                    if (self.desc_scroll as usize) < total_lines.saturating_sub(1) {
                        self.desc_scroll = self.desc_scroll.saturating_add(1);
                    }
                } else if self.mode == RagMode::Previewing
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
                        // No URL typed: Enter toggles the highlighted DB row,
                        // mirroring the mouse-click behaviour.
                        let idx = self.selected_db_index;
                        self.toggle_db(idx);
                        return Some(RagAction::Consumed);
                    }
                    if !self.has_selected_db() {
                        return Some(RagAction::ShowWarning(
                            "Select a database first to embed the content.".into(),
                        ));
                    }
                    self.url_input.clear();
                    return Some(RagAction::FetchUrlOrPath(input));
                }
                RagMode::Previewing => {
                    if !self.content_preview.is_empty() {
                        let db_name = match &self.selected_db_for_embed {
                            Some(name) => name.clone(),
                            None => {
                                return Some(RagAction::ShowWarning(
                                    "Select a database first to embed the content.".into(),
                                ));
                            }
                        };
                        let db_desc = self
                            .registry
                            .find(&db_name)
                            .map(|db| db.description.clone())
                            .unwrap_or_default();
                        let model = self
                            .available_models
                            .get(self.selected_model_index)
                            .cloned();
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
                if self.show_desc_for_db.is_some() {
                    self.show_desc_for_db = None;
                    return Some(RagAction::Consumed);
                }
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
                // Toggle the form; closing it keeps the draft buffer alive.
                self.toggle_create_db();
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
    /// Tab switches focus between Name and Description fields.
    /// All text input goes to the currently focused field.
    fn focused_input(&mut self) -> (&mut String, &mut usize) {
        match self.create_db_focus {
            CreateDbFocus::Name => (&mut self.db_name_input, &mut self.db_name_cursor_pos),
            CreateDbFocus::Description => (
                &mut self.db_description_input,
                &mut self.db_description_cursor_pos,
            ),
        }
    }

    fn handle_create_db_key(
        &mut self,
        key: ratatui::crossterm::event::KeyCode,
    ) -> Option<RagAction> {
        // Any form key discards a lingering drag selection.
        self.clear_field_selection();
        match key {
            key @ (ratatui::crossterm::event::KeyCode::Up
            | ratatui::crossterm::event::KeyCode::Down) => {
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
                    let max_vis = super::render::MAX_VISIBLE_MODELS_IN_FORM;
                    if self.selected_model_index >= self.model_scroll_offset + max_vis {
                        self.model_scroll_offset = self
                            .selected_model_index
                            .saturating_sub(max_vis.saturating_sub(1));
                    }
                    if self.selected_model_index < self.model_scroll_offset {
                        self.model_scroll_offset = self.selected_model_index;
                    }
                } else if let Some(area) = self.last_area {
                    // Collapsed form: Up/Down move the cursor between visual
                    // lines of the focused field (like the chat prompt).
                    self.note_cursor_activity();
                    let inner_w = area.width.saturating_sub(4);
                    let pad_w = inner_w.saturating_sub(8).max(1);
                    let value_w = pad_w.saturating_sub(FIELD_LABEL_W).max(1);
                    let (input, pos) = self.focused_input();
                    let new_pos = crate::util::word_ops::move_visual_line(
                        input,
                        *pos,
                        value_w as usize,
                        key == ratatui::crossterm::event::KeyCode::Up,
                    );
                    *pos = new_pos;
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Enter => {
                if self.models_expanded {
                    // Collapse models
                    self.models_expanded = false;
                    Some(RagAction::Consumed)
                } else if self.db_name_input.is_empty() {
                    Some(RagAction::ShowWarning(
                        "Enter a database name first.".into(),
                    ))
                } else if self.db_description_input.trim().is_empty() {
                    Some(RagAction::ShowWarning(
                        "Enter a database description first (required).".into(),
                    ))
                } else {
                    let name = self.db_name_input.trim().to_string();
                    // Validate table name before creating the DB.
                    if let Err(e) = cosh_recall::embed::validate_table_name(&name) {
                        let msg = e.to_string();
                        let clean = msg.strip_prefix("Database error: ").unwrap_or(&msg);
                        return Some(RagAction::ShowWarning(clean.to_string()));
                    }
                    let description = self.db_description_input.trim().to_string();
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
                                    },
                                )
                            }
                        })
                        .unwrap_or(super::models::EmbedderConfig::Local {
                            model: super::models::LocalEmbedModel::AllMiniLML6V2,
                        });
                    // Close the form and wipe the draft buffer: the database
                    // is being created for real.
                    self.reset_create_db_form();
                    self.clear_create_db_buffer();
                    Some(RagAction::CreateDb {
                        name,
                        description,
                        embedder,
                    })
                }
            }
            ratatui::crossterm::event::KeyCode::Tab => {
                // Toggle focus between Name and Description
                self.create_db_focus = match self.create_db_focus {
                    CreateDbFocus::Name => CreateDbFocus::Description,
                    CreateDbFocus::Description => CreateDbFocus::Name,
                };
                self.db_name_cursor.note_activity();
                self.db_description_cursor.note_activity();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Esc => {
                self.reset_create_db_form();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Char(ch) => {
                self.note_cursor_activity();
                let (input, pos) = self.focused_input();
                input.insert(*pos, ch);
                *pos += ch.len_utf8();
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Backspace => {
                self.note_cursor_activity();
                let (input, pos) = self.focused_input();
                if *pos > 0 {
                    let char_start = input.floor_char_boundary(*pos - 1);
                    input.remove(char_start);
                    *pos = char_start;
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Delete => {
                self.note_cursor_activity();
                let (input, pos) = self.focused_input();
                if *pos < input.len() {
                    let next = *pos + input[*pos..].chars().next().unwrap_or(' ').len_utf8();
                    input.drain(*pos..next);
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Left => {
                self.note_cursor_activity();
                let value_w = self.field_value_w();
                let (input, pos) = self.focused_input();
                if *pos > 0 {
                    let prev = input.floor_char_boundary(*pos - 1);
                    // Horizontal arrows move within the current line only;
                    // Up/Down move between lines. Without a rendered area the
                    // move is always allowed (plain char movement).
                    let same_line = match value_w {
                        Some(w) => {
                            super::render::caret_row(input, prev, w)
                                == super::render::caret_row(input, *pos, w)
                        }
                        None => true,
                    };
                    if same_line {
                        *pos = prev;
                    }
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Right => {
                self.note_cursor_activity();
                let value_w = self.field_value_w();
                let (input, pos) = self.focused_input();
                if *pos < input.len() {
                    let next = *pos + input[*pos..].chars().next().unwrap_or(' ').len_utf8();
                    let same_line = match value_w {
                        Some(w) => {
                            super::render::caret_row(input, *pos, w)
                                == super::render::caret_row(input, next, w)
                        }
                        None => true,
                    };
                    if same_line {
                        *pos = next;
                    }
                }
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::Home => {
                self.note_cursor_activity();
                let (_, pos) = self.focused_input();
                *pos = 0;
                Some(RagAction::Consumed)
            }
            ratatui::crossterm::event::KeyCode::End => {
                self.note_cursor_activity();
                let (input, pos) = self.focused_input();
                *pos = input.len();
                Some(RagAction::Consumed)
            }
            _ => Some(RagAction::Consumed),
        }
    }

    /// Wrap width of the focused field's value (label excluded), when the
    /// last rendered area is known.
    fn field_value_w(&self) -> Option<u16> {
        self.last_area.map(|area| {
            let inner_w = area.width.saturating_sub(4);
            let pad_w = inner_w.saturating_sub(8).max(1);
            pad_w.saturating_sub(FIELD_LABEL_W).max(1)
        })
    }

    /// Reset blink timers for both cursors (called after any input).
    fn note_cursor_activity(&mut self) {
        self.db_name_cursor.note_activity();
        self.db_description_cursor.note_activity();
    }

    /// Clamp the DB picker scroll offset so the newly selected index `new`
    /// stays within the visible window of size `DB_PICKER_VISIBLE`.
    fn clamp_picker_scroll(&mut self, new: usize, total: usize) {
        let vis = DB_PICKER_VISIBLE.min(total);
        if new >= self.db_picker_scroll_offset + vis {
            self.db_picker_scroll_offset = new.saturating_sub(vis.saturating_sub(1));
        }
        if new < self.db_picker_scroll_offset {
            self.db_picker_scroll_offset = new;
        }
    }

    /// Handle keys when the DB picker overlay is open.
    fn handle_db_picker_key(
        &mut self,
        key: ratatui::crossterm::event::KeyCode,
    ) -> Option<RagAction> {
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
                self.clamp_picker_scroll(new, total);
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
                self.clamp_picker_scroll(new, total);
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
        if self.show_create_db {
            // Paste into the focused create-db field (keeps newlines — the
            // description is multi-line markdown; strip \r like the prompt).
            self.note_cursor_activity();
            let cleaned = text.replace('\r', "");
            let (input, pos) = self.focused_input();
            input.insert_str(*pos, &cleaned);
            *pos += cleaned.len();
        } else {
            self.url_input.handle_paste(text);
        }
    }

    // Ctrl+Backspace / Ctrl+W: delete the word before the cursor. In the
    // create-db form this targets the focused field, otherwise the URL input.
    pub fn handle_ctrl_backspace(&mut self) {
        if self.show_create_db {
            self.note_cursor_activity();
            let (input, pos) = self.focused_input();
            let start = crate::util::word_ops::find_word_start(input, *pos);
            if start < *pos {
                input.drain(start..*pos);
                *pos = start;
            }
        } else {
            self.url_input.delete_word_before_cursor();
        }
    }

    // Ctrl+Left: jump to the start of the previous word.
    pub fn handle_ctrl_left(&mut self) {
        if self.show_create_db {
            self.note_cursor_activity();
            let (input, pos) = self.focused_input();
            let new_pos = crate::util::word_ops::find_word_start(input, *pos);
            if new_pos < *pos {
                *pos = new_pos;
            }
        } else {
            self.url_input.cursor_word_left();
        }
    }

    // Ctrl+Right: jump to the start of the next word.
    pub fn handle_ctrl_right(&mut self) {
        if self.show_create_db {
            self.note_cursor_activity();
            let (input, pos) = self.focused_input();
            let new_pos = crate::util::word_ops::find_word_end(input, *pos);
            if new_pos > *pos {
                *pos = new_pos;
            }
        } else {
            self.url_input.cursor_word_right();
        }
    }

    /// Insert a newline at the cursor in the focused create-db field
    /// (Ctrl+J / Shift+Enter, like the chat prompt). Returns `true` when a
    /// newline was inserted (form open), `false` otherwise.
    ///
    /// The form stops growing once it fills the box: further newlines are
    /// rejected so the text can never overflow off screen.
    pub fn handle_insert_newline(&mut self) -> bool {
        if !self.show_create_db {
            return false;
        }
        // The box is capped at `avail_h - BOX1_BOTTOM_MARGIN` rows and the
        // URL input shrinks to its 3-row minimum, so the form can use at
        // most `avail_h - BOX1_BOTTOM_MARGIN - CREATE_DB_FORM_BOTTOM_RESERVE`
        // rows. The reserve leaves one row of slack below the form so its
        // bottom border never touches box 1's bottom edge at full expansion.
        if let Some(area) = self.last_area {
            let inner_w = area.width.saturating_sub(4);
            let pad_w = inner_w.saturating_sub(8).max(1);
            let layout = self.compute_layout(area);
            let form_h_max = layout
                .box1_h
                .saturating_sub(super::render::CREATE_DB_FORM_BOTTOM_RESERVE);
            if self.create_db_mini_box_height(pad_w) >= form_h_max {
                return false;
            }
        }
        self.note_cursor_activity();
        let (input, pos) = self.focused_input();
        input.insert(*pos, '\n');
        *pos += 1;
        true
    }

    // Preview overlay geometry (shared with mouse handling)

    /// Compute the preview overlay rectangle for the given area.
    pub(crate) fn preview_overlay_rect(&self, area: Rect) -> Option<Rect> {
        if !self.is_preview_visible() {
            return None;
        }
        let overlay_w = (area.width * PREVIEW_OVERLAY_WIDTH_PCT / 100)
            .max(PREVIEW_OVERLAY_MIN_W)
            .min(area.width.saturating_sub(4));
        let overlay_h = (area.height * PREVIEW_OVERLAY_HEIGHT_PCT / 100)
            .max(PREVIEW_OVERLAY_MIN_H)
            .min(area.height.saturating_sub(4));
        let overlay_x = area.x + (area.width - overlay_w) / 2;
        let overlay_y = area.y + (area.height - overlay_h) / 2;
        Some(Rect::new(overlay_x, overlay_y, overlay_w, overlay_h))
    }
}

// Mouse handling
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
        !self.show_create_db
            && !self.show_db_picker
            && !self.registry.dbs.is_empty()
            && self.is_select_db_btn_clicked(mouse, area)
    }

    /// Public query for DB picker row clicks.
    pub fn is_db_picker_row_click(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        if !self.show_db_picker {
            return None;
        }
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return None;
        }
        let inner_x = area.x + 2;
        let input_h = self.url_input.height(area.width.saturating_sub(8));

        // The picker renders at cy, with fixed height matching create form
        let picker_y = area.y + 2 + input_h;
        let picker_h = PICKER_BOX_HEIGHT;

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

    /// Check if a mouse click is inside the DB picker bounding box.
    /// Used to avoid closing the picker when clicking inside it but not on a row.
    pub fn is_click_inside_db_picker(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_db_picker {
            return false;
        }
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return false;
        }
        let inner_x = area.x + 2;
        let input_h = self.url_input.height(area.width.saturating_sub(8));
        let picker_y = area.y + 2 + input_h;
        let picker_h = PICKER_BOX_HEIGHT;
        let mx = mouse.x;
        let my = mouse.y;
        my >= picker_y && my < picker_y + picker_h && mx >= inner_x && mx < inner_x + inner_w
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
        mx < overlay.x || mx >= overlay.right() || my < overlay.y || my >= overlay.bottom()
    }

    /// Find which DB row was clicked (if any).
    fn find_db_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let layout = self.compute_layout(area);
        let filtered = self.filtered_dbs();
        let max_visible = layout.list_h as usize;
        let render_count = max_visible.min(filtered.len());
        let row_max_w = layout.inner_w.saturating_sub(4);

        let my = mouse.y;
        let mx = mouse.x;

        if my >= layout.list_start_y && my < layout.list_start_y + render_count as u16 {
            let i = (my - layout.list_start_y) as usize;
            let idx = self.dbs_scroll_offset + i;
            if idx < filtered.len() && mx >= layout.cx + 2 && mx < layout.cx + 2 + row_max_w {
                return Some(idx);
            }
        }
        None
    }

    /// Check if the mouse click is on the "+ Create DB" button.
    fn is_create_db_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return false;
        }
        let input_h = self.url_input.height(area.width.saturating_sub(8));
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
        // The model line is the first form field, preceded by one gap line.
        let model_y = area.y + 2 + self.url_input.height(area.width.saturating_sub(8)) + 1;
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return false;
        }
        let cx = area.x + 4;
        let pad_w = inner_w.saturating_sub(8);
        let mx = mouse.x;
        let my = mouse.y;

        my == model_y && mx >= cx + 2 && mx < cx + 2 + pad_w
    }

    /// Check if mouse is on the "Select Database" button.
    fn is_select_db_btn_clicked(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return false;
        }
        let input_h = self.url_input.height(area.width.saturating_sub(8));
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

    /// Map a mouse position inside the Create DB form to the focused field
    /// and the byte offset of the clicked character (mirrors the prompt's
    /// `char_pos_at_mouse`, inverting the markdown word-wrap). Returns `None`
    /// when the pointer is not over a field (label, gaps, model line, ...).
    /// Row ranges of the two create-db fields within `area`.
    ///
    /// Returns `(name_y, name_lines, desc_y, desc_lines)` or `None` when the
    /// form is closed or the area is too narrow to lay out. Each field
    /// (model, name, description) is preceded by one gap line, and the
    /// Name/Description fields can span multiple wrapped rows.
    pub(crate) fn create_db_field_y_ranges(&self, area: Rect) -> Option<(u16, u16, u16, u16)> {
        if !self.show_create_db {
            return None;
        }
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return None;
        }
        let input_h = self.url_input.height(area.width.saturating_sub(8));
        let form_y = area.y + 2 + input_h;
        let model_line_count = if self.models_expanded {
            let max_vis = super::render::MAX_VISIBLE_MODELS_IN_FORM;
            let total = self.available_models.len();
            1 + max_vis.min(total) as u16
        } else {
            1
        };
        let (name_lines, desc_lines) = self.create_db_field_lines(inner_w.saturating_sub(8));
        let name_y = form_y + model_line_count + 2;
        let desc_y = name_y + name_lines + 1;
        Some((name_y, name_lines, desc_y, desc_lines))
    }

    pub(crate) fn field_byte_at(
        &self,
        mouse: &MouseEvent,
        area: Rect,
    ) -> Option<(CreateDbFocus, usize)> {
        let (name_y, name_lines, desc_y, desc_lines) = self.create_db_field_y_ranges(area)?;
        let inner_w = area.width.saturating_sub(4);
        let inner_x = area.x + 2;
        let mx = mouse.x;
        let my = mouse.y;

        // X bounds: must be within the form's inner area
        if !(mx >= inner_x && mx < inner_x + inner_w) {
            return None;
        }

        // Value area of each field: the label starts at pad = cx + 2 and the
        // markdown value begins right after the label (matches render).
        let pad = area.x + 4 + 2; // layout.cx + 2
        let pad_w = inner_w.saturating_sub(8);
        let value_w = pad_w.saturating_sub(FIELD_LABEL_W).max(1);
        let value_x = pad + FIELD_LABEL_W;

        if my >= name_y && my < name_y + name_lines {
            let row = my - name_y;
            let col = mx.saturating_sub(value_x).min(value_w);
            let byte = byte_pos_at_click(&self.db_name_input, row, col, value_w);
            return Some((CreateDbFocus::Name, byte));
        }
        if my >= desc_y && my < desc_y + desc_lines {
            let row = my - desc_y;
            let col = mx.saturating_sub(value_x).min(value_w);
            let byte = byte_pos_at_click(&self.db_description_input, row, col, value_w);
            return Some((CreateDbFocus::Description, byte));
        }
        None
    }

    /// Extend the active drag selection to the field position under the mouse,
    /// clamping to the field's start/end when the drag leaves its rows.
    /// Returns `true` when a selection is active (even if the byte did not move).
    pub fn extend_field_selection_at(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        let Some(sel) = &self.field_selection else {
            return false;
        };
        let focus = sel.field();
        let Some((name_y, name_lines, desc_y, desc_lines)) = self.create_db_field_y_ranges(area)
        else {
            return false;
        };
        let (field_top, field_bottom, input) = match focus {
            CreateDbFocus::Name => (name_y, name_y + name_lines, &self.db_name_input),
            CreateDbFocus::Description => (desc_y, desc_y + desc_lines, &self.db_description_input),
        };
        let byte = if mouse.y < field_top {
            0
        } else if mouse.y >= field_bottom {
            input.len()
        } else {
            // Clamp the x coordinate into the form's inner bounds so dragging
            // past the value edge still lands on the row's first/last cell.
            let inner_x = area.x + 2;
            let inner_w = area.width.saturating_sub(4);
            let mx = mouse.x.clamp(inner_x, inner_x + inner_w - 1);
            let clamped =
                MouseEvent::new(mouse.event_type, mouse.button, mx, mouse.y, mouse.modifiers);
            match self.field_byte_at(&clamped, area) {
                Some((_, b)) => b,
                None => return false,
            }
        };
        self.extend_field_selection(byte);
        match focus {
            CreateDbFocus::Name => self.db_name_cursor_pos = byte,
            CreateDbFocus::Description => self.db_description_cursor_pos = byte,
        }
        true
    }

    /// Handle a mouse click inside the Create DB form: switch focus to the clicked field
    /// and place the cursor at the clicked character.
    /// Returns `true` if the click was on a field (Name or Description), `false` otherwise.
    pub fn handle_create_db_field_click(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        let Some((focus, byte)) = self.field_byte_at(mouse, area) else {
            return false;
        };
        self.create_db_focus = focus;
        self.db_name_cursor.note_activity();
        self.db_description_cursor.note_activity();
        match focus {
            CreateDbFocus::Name => self.db_name_cursor_pos = byte,
            CreateDbFocus::Description => self.db_description_cursor_pos = byte,
        }
        // A plain click (no drag) leaves a zero-length anchor behind; drop it.
        self.field_selection = None;
        true
    }

    /// Begin a drag selection in a create-db field: focus it, place the caret
    /// at `byte` and anchor the selection there (mirrors the prompt's mouse
    /// press on the input).
    pub fn start_field_selection(&mut self, focus: CreateDbFocus, byte: usize) {
        self.create_db_focus = focus;
        self.note_cursor_activity();
        match focus {
            CreateDbFocus::Name => self.db_name_cursor_pos = byte,
            CreateDbFocus::Description => self.db_description_cursor_pos = byte,
        }
        self.field_selection = Some(DragSelection::anchor(focus, byte));
    }

    /// Extend the active drag selection to `byte` (the anchor stays fixed, so
    /// dragging backwards still selects the range in between).
    pub fn extend_field_selection(&mut self, byte: usize) {
        if let Some(sel) = self.field_selection.as_mut() {
            sel.extend(byte);
        }
    }

    pub fn has_field_selection(&self) -> bool {
        self.field_selection.is_some_and(|sel| sel.is_active())
    }

    /// Text covered by the active selection in the raw input of the field.
    pub fn selected_field_text(&self) -> String {
        let Some(sel) = &self.field_selection else {
            return String::new();
        };
        let input = match sel.field() {
            CreateDbFocus::Name => &self.db_name_input,
            CreateDbFocus::Description => &self.db_description_input,
        };
        sel.slice_of(sel.field(), input).unwrap_or_default()
    }

    pub fn clear_field_selection(&mut self) {
        self.field_selection = None;
    }

    /// Dismiss the Create DB form on click outside the form area (used by app.rs).
    pub fn is_dismiss_click(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db {
            return false;
        }
        // Only dismiss if the click is OUTSIDE the form's bounding box
        // (not just outside specific interactive elements).
        !self.is_click_inside_create_form(mouse, area)
    }

    /// Check if the mouse click is on a "show desc" button next to a DB row.
    /// Returns the filtered index of the DB whose button was clicked.
    pub fn is_show_desc_click(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let layout = self.compute_layout(area);
        let filtered = self.filtered_dbs();
        let max_visible = layout.list_h as usize;
        let render_count = max_visible.min(filtered.len());

        let mx = mouse.x;
        let my = mouse.y;

        if my >= layout.list_start_y && my < layout.list_start_y + render_count as u16 {
            let i = (my - layout.list_start_y) as usize;
            let idx = self.dbs_scroll_offset + i;
            if idx < filtered.len() {
                // Check if the click is on the "show desc" button area
                // Button is placed right after the truncated name
                let btn_w = SHOW_DESC_BTN_TEXT.len() as u16;
                let gap_w: u16 = 1;
                let avail_name_w = (layout.inner_w.saturating_sub(DB_ROW_H_PADDING) as usize)
                    .saturating_sub((btn_w + gap_w) as usize);
                let name_display = format!(" {}", filtered[idx].name);
                let name_trunc = truncate_label(&name_display, avail_name_w);
                let name_len = name_trunc.chars().count() as u16;
                let btn_x = layout.cx + 2 + name_len + gap_w;
                if mx >= btn_x && mx < btn_x + btn_w {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Check if the mouse click is on the 🗑 delete button next to a DB row.
    /// Returns the name of the DB whose delete button was clicked.
    pub fn is_delete_click(&self, mouse: &MouseEvent, area: Rect) -> Option<String> {
        let layout = self.compute_layout(area);
        let filtered = self.filtered_dbs();
        let max_visible = layout.list_h as usize;
        let render_count = max_visible.min(filtered.len());

        let mx = mouse.x;
        let my = mouse.y;

        if my >= layout.list_start_y && my < layout.list_start_y + render_count as u16 {
            let i = (my - layout.list_start_y) as usize;
            let idx = self.dbs_scroll_offset + i;
            if idx < filtered.len() {
                // Compute the 🗑 x position (matches render_box2)
                let btn_w = SHOW_DESC_BTN_TEXT.len() as u16;
                let gap_w: u16 = 1;
                let total_btns_w = btn_w + gap_w + TRASH_EMOJI_WIDTH;
                let avail_name_w = (layout.inner_w.saturating_sub(DB_ROW_H_PADDING) as usize)
                    .saturating_sub((total_btns_w + gap_w) as usize);
                let name_display = format!(" {}", filtered[idx].name);
                let name_trunc = truncate_label(&name_display, avail_name_w);
                let name_len = name_trunc.chars().count() as u16;
                let btn_x = layout.cx + 2 + name_len + gap_w;
                let trash_x = btn_x + btn_w + gap_w;

                if mx >= trash_x && mx < trash_x + TRASH_EMOJI_WIDTH {
                    return Some(filtered[idx].name.clone());
                }
            }
        }
        None
    }

    /// Compute the bounding rect of the description popup.
    pub(crate) fn desc_popup_rect(&self, area: Rect) -> Option<Rect> {
        let idx = self.show_desc_for_db?;
        let filtered = self.filtered_dbs();
        if idx >= filtered.len() {
            return None;
        }
        let overlay_w = (area.width * DESC_POPUP_WIDTH_PCT / 100)
            .max(DESC_POPUP_MIN_W)
            .min(area.width.saturating_sub(8));
        let overlay_h = (area.height * DESC_POPUP_HEIGHT_PCT / 100)
            .max(DESC_POPUP_MIN_H)
            .min(area.height.saturating_sub(8));
        let overlay_x = area.x + (area.width - overlay_w) / 2;
        let overlay_y = area.y + (area.height - overlay_h) / 2;
        Some(Rect::new(overlay_x, overlay_y, overlay_w, overlay_h))
    }

    /// Check if the mouse click is outside the description popup.
    pub fn is_click_outside_desc_popup(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let Some(overlay) = self.desc_popup_rect(area) else {
            return false;
        };
        let mx = mouse.x;
        let my = mouse.y;
        mx < overlay.x || mx >= overlay.right() || my < overlay.y || my >= overlay.bottom()
    }

    /// Check if a mouse click is inside the Create DB form's bounding box.
    fn is_click_inside_create_form(&self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.show_create_db {
            return false;
        }
        let inner_w = area.width.saturating_sub(4);
        if inner_w < MIN_CONTENT_WIDTH {
            return false;
        }
        let inner_x = area.x + 2;
        let input_h = self.url_input.height(area.width.saturating_sub(8));
        let form_y = area.y + 2 + input_h;
        let form_h = self.create_db_mini_box_height(inner_w.saturating_sub(8));
        let mx = mouse.x;
        let my = mouse.y;
        my >= form_y && my < form_y + form_h && mx >= inner_x && mx < inner_x + inner_w
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::CreateDbFocus;
    use super::super::render::CREATE_DB_FORM_BOTTOM_RESERVE;
    use super::super::view::RagView;

    fn open_form(view: &mut RagView) {
        view.toggle_create_db();
    }

    #[test]
    fn ctrl_word_navigation_in_name_field() {
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello world foo".into();
        view.db_name_cursor_pos = view.db_name_input.len();

        view.handle_ctrl_left();
        assert_eq!(view.db_name_cursor_pos, 12, "jump to start of 'foo'");
        view.handle_ctrl_left();
        assert_eq!(view.db_name_cursor_pos, 6, "jump to start of 'world'");
        view.handle_ctrl_right();
        assert_eq!(view.db_name_cursor_pos, 12, "jump to start of 'foo'");
        view.handle_ctrl_right();
        assert_eq!(view.db_name_cursor_pos, 15, "jump to end of 'foo'");
    }

    #[test]
    fn ctrl_backspace_deletes_word_in_focused_field() {
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello world".into();
        view.db_name_cursor_pos = view.db_name_input.len();

        view.handle_ctrl_backspace();
        assert_eq!(view.db_name_input, "hello ");
        assert_eq!(view.db_name_cursor_pos, 6);

        view.handle_ctrl_backspace();
        assert_eq!(view.db_name_input, "");
        assert_eq!(view.db_name_cursor_pos, 0);
    }

    #[test]
    fn ctrl_backspace_targets_description_when_focused() {
        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "foo bar baz".into();
        view.db_description_cursor_pos = view.db_description_input.len();

        view.handle_ctrl_backspace();
        assert_eq!(view.db_description_input, "foo bar ");
        assert_eq!(view.db_description_cursor_pos, 8);
    }

    #[test]
    fn ctrl_word_ops_fall_back_to_url_input_when_form_closed() {
        let mut view = RagView::new();
        view.url_input.text = "hello world".into();
        view.url_input.cursor_pos = view.url_input.text.len();

        view.handle_ctrl_backspace();
        assert_eq!(view.url_input.text, "hello ");
        assert_eq!(view.url_input.cursor_pos, 6);

        view.handle_ctrl_left();
        assert_eq!(view.url_input.cursor_pos, 0);
    }

    #[test]
    fn insert_newline_only_when_form_open() {
        let mut view = RagView::new();
        assert!(!view.handle_insert_newline(), "no-op when form closed");

        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "line1".into();
        view.db_description_cursor_pos = 5;
        assert!(view.handle_insert_newline());
        assert_eq!(view.db_description_input, "line1\n");
        assert_eq!(view.db_description_cursor_pos, 6);
    }

    #[test]
    fn insert_newline_stops_at_area_limit() {
        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.last_area = Some(ratatui::layout::Rect::new(0, 0, 100, 40));

        let mut inserted = 0;
        while view.handle_insert_newline() {
            inserted += 1;
        }
        assert!(inserted >= 1, "some newlines should be allowed");
        // The field must have grown with the newlines (box expands along).
        assert!(
            view.db_description_input.lines().count()
                + usize::from(view.db_description_input.ends_with('\n'))
                > 1,
            "the box should grow with each line break"
        );

        // The form must still fit within the box after the last insertion,
        // with the bottom reserve (slack row) preserved.
        let area = view.last_area.unwrap();
        let layout = view.compute_layout(area);
        let form_h_max = layout.box1_h.saturating_sub(CREATE_DB_FORM_BOTTOM_RESERVE);
        let pad_w = area.width.saturating_sub(4).saturating_sub(8).max(1);
        assert!(
            view.create_db_mini_box_height(pad_w) <= form_h_max,
            "form must not exceed the box"
        );
        // At the limit the form leaves at least one row of slack before the
        // box's bottom edge (the reserve is 1 more than title+gap+input min).
        assert!(
            view.create_db_mini_box_height(pad_w) < layout.box1_h.saturating_sub(2 + 3),
            "form must stop one row short of the box edge"
        );
    }

    #[test]
    fn typing_after_blank_lines_keeps_cursor_below() {
        use ratatui::crossterm::event::KeyCode;
        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "abc".into();
        view.db_description_cursor_pos = 3;
        // Two Shift+Enter: the cursor advances past each newline.
        assert!(view.handle_insert_newline());
        assert!(view.handle_insert_newline());
        assert_eq!(view.db_description_input, "abc\n\n");
        assert_eq!(view.db_description_cursor_pos, 5);
        // Typing must insert right after the newlines (where the cursor is),
        // not back at the top before the line breaks.
        view.handle_key(KeyCode::Char('X'));
        assert_eq!(view.db_description_input, "abc\n\nX");
        assert_eq!(view.db_description_cursor_pos, 6);
    }

    #[test]
    fn arrow_right_skips_multibyte_chars() {
        use ratatui::crossterm::event::KeyCode;
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "olá".into();
        view.db_name_cursor_pos = 2; // before 'á' (2 bytes)
        view.handle_key(KeyCode::Right);
        assert_eq!(view.db_name_cursor_pos, 4, "Right must skip the 2-byte 'á'");
        view.handle_key(KeyCode::Left);
        assert_eq!(view.db_name_cursor_pos, 2);
    }

    #[test]
    fn delete_removes_multibyte_char() {
        use ratatui::crossterm::event::KeyCode;
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "olá".into();
        view.db_name_cursor_pos = 2; // before 'á'
        view.handle_key(KeyCode::Delete);
        assert_eq!(view.db_name_input, "ol");
        assert_eq!(view.db_name_cursor_pos, 2);
    }

    #[test]
    fn click_positions_cursor_in_fields() {
        use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello".into();
        view.db_description_input = "world".into();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        // Geometry mirrors handle_create_db_field_click + render: the form
        // starts below the URL input (3 rows), the model line is 1 row, then
        // a gap, then the Name field; the value starts after the label.
        let input_h = view.url_input.height(92);
        let form_y = 2 + input_h;
        let name_y = form_y + 1 + 2;
        let value_x = 0 + 4 + 2 + 7; // pad (cx + 2) + label width
        let click = |x: u16, y: u16| {
            MouseEvent::new(
                MouseEventType::Up,
                MouseButton::Left,
                x,
                y,
                MouseModifiers::none(),
            )
        };

        // Click on the Name field at col 2 → focus Name, cursor at byte 2.
        assert!(view.handle_create_db_field_click(&click(value_x + 2, name_y), area));
        assert_eq!(view.create_db_focus, CreateDbFocus::Name);
        assert_eq!(view.db_name_cursor_pos, 2);

        // Click past the end of the name → cursor clamps to the end.
        assert!(view.handle_create_db_field_click(&click(value_x + 30, name_y), area));
        assert_eq!(view.db_name_cursor_pos, 5);

        // Click on the Description field at col 1 → focus desc, cursor byte 1.
        let (name_lines, _) = view.create_db_field_lines(88);
        let desc_y = name_y + name_lines + 1;
        assert!(view.handle_create_db_field_click(&click(value_x + 1, desc_y), area));
        assert_eq!(view.create_db_focus, CreateDbFocus::Description);
        assert_eq!(view.db_description_cursor_pos, 1);

        // Click on the model line (not a field row) → no focus change.
        assert!(!view.handle_create_db_field_click(&click(value_x + 1, form_y + 1), area));
        assert_eq!(view.create_db_focus, CreateDbFocus::Description);
    }

    #[test]
    fn drag_selection_selects_and_copies_text() {
        use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello world".into();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let input_h = view.url_input.height(92);
        let form_y = 2 + input_h;
        let name_y = form_y + 1 + 2;
        let value_x = 0 + 4 + 2 + 7; // pad (cx + 2) + label width
        let mouse = |x: u16, y: u16| {
            MouseEvent::new(
                MouseEventType::Drag,
                MouseButton::Left,
                x,
                y,
                MouseModifiers::none(),
            )
        };

        // Press at 'hello' → anchors the selection at byte 0.
        view.start_field_selection(CreateDbFocus::Name, 0);
        assert!(view.field_selection.is_some());
        assert!(
            !view.has_field_selection(),
            "anchor only, nothing selected yet"
        );

        // Drag to 'world' → extends to byte 0..11.
        assert!(view.extend_field_selection_at(&mouse(value_x + 11, name_y), area));
        assert_eq!(view.selected_field_text(), "hello world");
        assert!(view.has_field_selection());
        assert_eq!(view.db_name_cursor_pos, 11);

        // Dragging back before the anchor selects the reversed range.
        assert!(view.extend_field_selection_at(&mouse(value_x + 3, name_y), area));
        assert_eq!(view.selected_field_text(), "hel");

        view.clear_field_selection();
        assert!(!view.has_field_selection());
    }

    #[test]
    fn drag_selection_clamps_outside_field() {
        use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello".into();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let mouse = |x: u16, y: u16| {
            MouseEvent::new(
                MouseEventType::Drag,
                MouseButton::Left,
                x,
                y,
                MouseModifiers::none(),
            )
        };
        let input_h = view.url_input.height(92);
        let form_y = 2 + input_h;
        let name_y = form_y + 1 + 2;

        view.start_field_selection(CreateDbFocus::Name, 2);
        // Drag far above the Name field → clamps to byte 0.
        assert!(view.extend_field_selection_at(&mouse(0, 0), area));
        assert_eq!(view.selected_field_text(), "he");
        // Drag far below the form → clamps to the end of the input.
        assert!(view.extend_field_selection_at(&mouse(0, 39), area));
        assert_eq!(view.selected_field_text(), "llo");

        // No active selection → drag is a no-op.
        view.clear_field_selection();
        let value_x = 0 + 4 + 2 + 7;
        assert!(!view.extend_field_selection_at(&mouse(value_x + 1, name_y), area));
    }

    #[test]
    fn plain_click_clears_lingering_selection() {
        use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
        let mut view = RagView::new();
        open_form(&mut view);
        view.db_name_input = "hello".into();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let input_h = view.url_input.height(92);
        let name_y = 2 + input_h + 1 + 2;
        let value_x = 0 + 4 + 2 + 7;
        let click = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            value_x + 1,
            name_y,
            MouseModifiers::none(),
        );

        // A press that never became a drag leaves a zero-length anchor behind.
        view.start_field_selection(CreateDbFocus::Name, 1);
        assert!(view.field_selection.is_some());
        // The plain click path discards it.
        assert!(view.handle_create_db_field_click(&click, area));
        assert!(view.field_selection.is_none());
        assert_eq!(view.db_name_cursor_pos, 1);
    }

    #[test]
    fn up_down_move_visual_line_after_render() {
        use ratatui::buffer::Buffer;
        use ratatui::crossterm::event::KeyCode;
        use ratatui::layout::Rect;

        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "ab\ncd".into();
        view.db_description_cursor_pos = 4; // end

        // render() must store last_area — Up/Down need it to know the field
        // width; without it they are silent no-ops.
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 100, 40);
        view.render(&mut Buffer::empty(area), area, &theme);
        assert!(view.last_area.is_some(), "render must store the area");

        view.handle_key(KeyCode::Up);
        assert_eq!(view.db_description_cursor_pos, 1, "up to the previous line");
        view.handle_key(KeyCode::Down);
        assert_eq!(view.db_description_cursor_pos, 4, "down back to the end");
    }

    #[test]
    fn left_right_stay_on_visual_line() {
        use ratatui::buffer::Buffer;
        use ratatui::crossterm::event::KeyCode;
        use ratatui::layout::Rect;

        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "abc\ndef".into();
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 100, 40);
        view.render(&mut Buffer::empty(area), area, &theme);

        // Right at the end of the first line: stays (Down moves between lines).
        view.db_description_cursor_pos = 3;
        view.handle_key(KeyCode::Right);
        assert_eq!(
            view.db_description_cursor_pos, 3,
            "Right must not cross the newline"
        );
        // Left at the start of the second line: stays.
        view.db_description_cursor_pos = 4;
        view.handle_key(KeyCode::Left);
        assert_eq!(
            view.db_description_cursor_pos, 4,
            "Left must not cross the newline"
        );
        // Within a line both move sideways normally.
        view.db_description_cursor_pos = 1;
        view.handle_key(KeyCode::Right);
        assert_eq!(view.db_description_cursor_pos, 2);
        view.db_description_cursor_pos = 5;
        view.handle_key(KeyCode::Left);
        assert_eq!(view.db_description_cursor_pos, 4);
    }

    #[test]
    fn paste_into_focused_field_keeps_newlines() {
        let mut view = RagView::new();
        open_form(&mut view);
        view.create_db_focus = CreateDbFocus::Description;
        view.db_description_input = "ab".into();
        view.db_description_cursor_pos = 1;
        view.handle_paste("XY\nZ");
        assert_eq!(view.db_description_input, "aXY\nZb");
        assert_eq!(view.db_description_cursor_pos, 5);
    }
}
