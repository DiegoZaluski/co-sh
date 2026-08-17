//! RAG Knowledge Base view — core struct and lifecycle.
//!
//! The `RagView` struct holds all UI state for the RAG screen.
//! Input handling lives in `handlers.rs`, rendering lives in `render.rs`.
//!
//! Layout — two boxed sections:
//! 1. **Embed Content** — URL/path input with inline status + content preview overlay
//! 2. **Available Databases** — toggleable DB list with inline "+ Create New Database"
//!    form that appears inside the Embed Content box when Tab is pressed.

use std::collections::HashSet;

use super::models::{CreateDbFocus, EmbedModelEntry, RagMode};
use super::registry::RagRegistry;
use super::render::{FIELD_LABEL_W, MAX_VISIBLE_MODELS_IN_FORM};
use crate::component::cursor::Cursor;
use crate::component::rag_input::RagInput;
use crate::component::search_bar::SearchBar;
use crate::component::spinner::SpinnerState;
use cosh_tui::core::renderables::markdown::estimate_height;

// Layout constants

/// Percentage of available height used for Box 1 (Embed Content).
const BOX1_HEIGHT_PCT: u16 = 30;

/// Show the search/filter bar when DB count exceeds this threshold.
const DB_LIST_FILTER_THRESHOLD: usize = 5;

/// Number of visible DB rows kept during scroll clamping.
const DB_SCROLL_VISIBLE: usize = 20;

/// Minimum number of models always visible in the expanded selector.
const MIN_VISIBLE_MODELS: usize = 3;

/// Minimum height reserved for Box 1 (Embed Content) when no form is open
/// (title + gap + input(3 lines) + status + gap).
const BOX1_MIN_BASE: u16 = 7;

/// Bottom margin subtracted from available height for Box 1 max clamp.
const BOX1_BOTTOM_MARGIN: u16 = 8;

/// Terminal rows reserved for the model count calculation in the expanded selector.
const MODEL_COUNT_HEADROOM: u16 = 10;

// RagLayout

/// Pre-computed layout geometry for the RAG view.
///
/// Shared between rendering and mouse hit-testing to avoid
/// duplicating the box sizing logic.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RagLayout {
    /// Height of the URL/path input widget.
    pub input_h: u16,
    /// Number of visible model rows (0 if not expanded).
    pub db_model_count: usize,
    /// Total lines consumed by the create-DB form (0 if not shown).
    pub create_db_lines: u16,
    /// Height of box 1 (Embed Content section).
    pub box1_h: u16,
    /// Y position of box 2 (Available Databases section).
    pub box2_y: u16,
    /// Height of box 2.
    pub box2_h: u16,
    /// Height of the warning lines in box 2.
    pub warning_h: u16,
    /// Height of the filter line (0 or 1).
    pub filter_h: u16,
    /// Bottom padding in box 2.
    pub pad_bottom: u16,
    /// Gap under the header.
    pub header_gap: u16,
    /// Available height for the DB list in box 2.
    pub list_h: u16,
    /// Y position where the DB list starts.
    pub list_start_y: u16,
    /// Inner content width (area.width - 4).
    pub inner_w: u16,
    /// Inner content X position (area.x + 2).
    pub inner_x: u16,
    /// Content X position (inner_x + 2).
    pub cx: u16,
}

impl RagView {
    /// Compute layout geometry shared between rendering and mouse handling.
    pub(crate) fn compute_layout(&self, area: ratatui::layout::Rect) -> RagLayout {
        let avail_h = area.height;
        let gap: u16 = 1;
        let bottom_gap: u16 = 0;

        let inner_w = area.width.saturating_sub(4);
        let mut input_h = self.url_input.height(inner_w.saturating_sub(4));
        let db_model_count = if self.show_create_db && self.models_expanded {
            self.available_models.len().min(max_visible_models(avail_h))
        } else {
            0
        };
        // Field width of the Name/Description markdown fields (same derivation
        // as render_create_db_form: input_w - 4 = inner_w - 8).
        let pad_w = inner_w.saturating_sub(8);
        let create_db_lines: u16 = if self.show_create_db {
            let (name_lines, desc_lines) = self.create_db_field_lines(pad_w);
            if self.models_expanded {
                6 + db_model_count as u16 + name_lines + desc_lines
            } else {
                6 + name_lines + desc_lines
            }
        } else if self.show_db_picker {
            5
        } else {
            1
        };
        let box1_h = {
            let ideal = avail_h.saturating_mul(BOX1_HEIGHT_PCT) / 100;
            ideal
                .max(BOX1_MIN_BASE + create_db_lines)
                .min(avail_h.saturating_sub(BOX1_BOTTOM_MARGIN))
        };
        // Clamp input_h to what box1 can actually hold.
        // Without form: box1 = title(1) + gap(1) + input(input_h) + status(1) + bottom_gap(1) = 4 + input_h
        // With form:    box1 = title(1) + gap(1) + input(input_h) + form(form_h) = 2 + input_h + form_h
        let max_input_h = if self.show_create_db || self.show_db_picker {
            let form_h = self.create_db_mini_box_height(pad_w);
            box1_h.saturating_sub(2 + form_h)
        } else {
            box1_h.saturating_sub(4)
        };
        input_h = input_h.min(max_input_h).max(3);

        let box2_y = area.y + box1_h + gap;
        let box2_available = avail_h.saturating_sub(box1_h + gap + bottom_gap);
        let box2_h = box2_available.max(5);

        let warning_h = 2u16;
        let filter_h = if self.registry.dbs.len() > DB_LIST_FILTER_THRESHOLD {
            1u16
        } else {
            0u16
        };
        let pad_bottom: u16 = 1;
        let header_gap: u16 = 1;
        let list_h = box2_h.saturating_sub(1 + header_gap + warning_h + filter_h + pad_bottom);
        let list_start_y = box2_y + 1 + header_gap + warning_h + filter_h;

        let inner_x = area.x + 2;
        let cx = inner_x + 2;

        RagLayout {
            input_h,
            db_model_count,
            create_db_lines,
            box1_h,
            box2_y,
            box2_h,
            warning_h,
            filter_h,
            pad_bottom,
            header_gap,
            list_h,
            list_start_y,
            inner_w,
            inner_x,
            cx,
        }
    }
}

// RagView

pub struct RagView {
    pub mode: RagMode,
    pub(crate) url_input: RagInput,
    pub(crate) content_preview: String,
    pub(crate) preview_scroll: usize,
    pub(crate) spinner: SpinnerState,

    // Create DB form (shown when `show_create_db` is true)
    pub(crate) show_create_db: bool,
    pub(crate) models_expanded: bool,
    pub(crate) selected_model_index: usize,
    pub(crate) model_scroll_offset: usize,
    pub(crate) available_models: Vec<EmbedModelEntry>,
    pub(crate) create_db_focus: CreateDbFocus,
    pub(crate) db_name_input: String,
    pub(crate) db_name_cursor: Cursor,
    pub(crate) db_name_cursor_pos: usize,
    pub(crate) db_description_input: String,
    pub(crate) db_description_cursor: Cursor,
    pub(crate) db_description_cursor_pos: usize,

    // DB registry + filter
    pub(crate) registry: RagRegistry,
    /// Receiver for async URL fetch result.
    pub(crate) fetch_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    pub(crate) selected_db_index: usize,
    pub(crate) dbs_scroll_offset: usize,
    pub(crate) active_dbs: HashSet<String>,
    pub(crate) db_filter: SearchBar,

    // Embed target DB selection
    pub(crate) selected_db_for_embed: Option<String>,
    pub(crate) show_db_picker: bool,
    pub(crate) db_picker_scroll_offset: usize,

    // Description popup
    pub(crate) show_desc_for_db: Option<usize>,
    pub(crate) desc_scroll: usize,

    // Async embed result channel
    pub(crate) embed_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    pub(crate) embed_success: bool,
    pub(crate) embed_error: Option<String>,
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
            model_scroll_offset: 0,
            available_models,
            create_db_focus: CreateDbFocus::Name,
            db_name_input: String::new(),
            db_name_cursor: Cursor::new(),
            db_name_cursor_pos: 0,
            db_description_input: String::new(),
            db_description_cursor: Cursor::new(),
            db_description_cursor_pos: 0,
            registry,
            fetch_rx: None,
            selected_db_index: 0,
            dbs_scroll_offset: 0,
            active_dbs,
            db_filter: SearchBar::new(),
            selected_db_for_embed: None,
            show_db_picker: false,
            db_picker_scroll_offset: 0,
            show_desc_for_db: None,
            desc_scroll: 0,
            embed_rx: None,
            embed_success: false,
            embed_error: None,
        }
    }

    // State transitions

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
        // Keep the Create DB draft buffer intact — it is only wiped once a DB
        // is actually created (see clear_create_db_buffer).
        self.show_create_db = false;
        // Keep selected_db_for_embed (user may want to embed more content into same DB)
        self.registry = RagRegistry::load();
    }

    pub fn set_error(&mut self) {
        self.mode = RagMode::Idle;
    }

    /// Close the preview overlay and return to Idle (stays in RAG view).
    pub fn close_preview(&mut self) {
        self.mode = RagMode::Idle;
        self.content_preview.clear();
        self.preview_scroll = 0;
        // Keep selected_db_for_embed (user doesn't need to re-select for next fetch)
    }

    pub fn set_fetch_rx(&mut self, rx: std::sync::mpsc::Receiver<Result<String, String>>) {
        self.fetch_rx = Some(rx);
    }

    pub fn set_embed_rx(&mut self, rx: std::sync::mpsc::Receiver<Result<(), String>>) {
        self.embed_rx = Some(rx);
    }

    pub fn take_embed_completed(&mut self) -> bool {
        std::mem::take(&mut self.embed_success)
    }

    pub fn take_embed_error(&mut self) -> Option<String> {
        self.embed_error.take()
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
        // Check if async embed completed
        if let Some(rx) = &self.embed_rx {
            if let Ok(result) = rx.try_recv() {
                self.embed_rx = None;
                match result {
                    Ok(()) => {
                        self.embedding_done();
                        self.embed_success = true;
                    }
                    Err(e) => {
                        self.set_error();
                        self.embed_error = Some(e);
                    }
                }
            }
        }
    }

    pub fn active_dbs(&self) -> &HashSet<String> {
        &self.active_dbs
    }

    // DB helpers

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
        self.show_db_picker = false; // close DB picker if open
        self.models_expanded = false;
        if self.show_create_db {
            // Reopening the form keeps any draft the user already typed;
            // the draft is only cleared once the DB is actually created.
            self.create_db_focus = CreateDbFocus::Name;
        }
    }

    pub fn toggle_models_expanded(&mut self) {
        self.models_expanded = !self.models_expanded;
        self.model_scroll_offset = 0;
    }

    pub fn form_open(&self) -> bool {
        self.show_create_db
    }

    pub fn close_form(&mut self) {
        self.reset_create_db_form();
    }

    /// Close the Create DB form without discarding the draft.
    ///
    /// The typed name/description (and picked model) act as a live buffer:
    /// dismissing the form by accident (Esc, click outside, Tab) keeps the
    /// draft so the user can reopen the form and continue where they left off.
    pub(crate) fn reset_create_db_form(&mut self) {
        self.show_create_db = false;
        self.models_expanded = false;
    }

    /// Wipe the Create DB draft buffer.
    ///
    /// Called only after the database has actually been created, so the next
    /// form session starts empty.
    pub(crate) fn clear_create_db_buffer(&mut self) {
        self.selected_model_index = 0;
        self.model_scroll_offset = 0;
        self.db_name_input.clear();
        self.db_description_input.clear();
        self.db_name_cursor_pos = 0;
        self.db_description_cursor_pos = 0;
        self.create_db_focus = CreateDbFocus::Name;
    }

    /// Display text of the Name field (placeholder while empty).
    pub(crate) fn name_display_text(&self) -> &str {
        if self.db_name_input.is_empty() {
            "Enter database name..."
        } else {
            &self.db_name_input
        }
    }

    /// Display text of the Description field (placeholder while empty).
    pub(crate) fn desc_display_text(&self) -> &str {
        if self.db_description_input.is_empty() {
            "Briefly describe what this DB contains..."
        } else {
            &self.db_description_input
        }
    }

    /// Display lines occupied by the Name and Description fields at width
    /// `pad_w`, counting the markdown wrapping done by the renderer.
    ///
    /// The labels are drawn outside the markdown content (so they don't
    /// interfere with cmark's block parsing), so only the value wraps — the
    /// value has `FIELD_LABEL_W` fewer columns available.
    pub(crate) fn create_db_field_lines(&self, pad_w: u16) -> (u16, u16) {
        let value_w = pad_w.saturating_sub(FIELD_LABEL_W).max(1);
        let name_lines = markdown_field_lines(self.name_display_text(), value_w);
        let desc_lines = markdown_field_lines(self.desc_display_text(), value_w);
        (name_lines, desc_lines)
    }

    pub fn db_count(&self) -> usize {
        self.registry.dbs.len()
    }

    /// Whether the content preview overlay is currently visible.
    pub fn is_preview_visible(&self) -> bool {
        self.mode == RagMode::Previewing && !self.content_preview.is_empty()
    }

    /// Whether the spinner animation is active (fetching or embedding).
    pub fn is_spinner_active(&self) -> bool {
        matches!(self.mode, RagMode::Fetching | RagMode::Embedding)
    }

    // DB picker helpers

    pub fn toggle_db_picker(&mut self) {
        self.show_db_picker = !self.show_db_picker;
        self.show_create_db = false; // close create-db form if open
        self.db_picker_scroll_offset = 0;
    }

    pub fn close_db_picker(&mut self) {
        self.show_db_picker = false;
        self.db_picker_scroll_offset = 0;
    }

    pub fn toggle_desc_popup(&mut self, filtered_idx: usize) {
        // Toggle: if already showing this DB, close; otherwise show
        if self.show_desc_for_db == Some(filtered_idx) {
            self.show_desc_for_db = None;
        } else {
            self.show_desc_for_db = Some(filtered_idx);
        }
    }

    pub fn close_desc_popup(&mut self) {
        self.show_desc_for_db = None;
        self.desc_scroll = 0;
    }

    pub fn select_db_for_embed(&mut self, db_name: &str) {
        self.selected_db_for_embed = Some(db_name.to_string());
        self.show_db_picker = false;
    }

    pub fn has_selected_db(&self) -> bool {
        self.selected_db_for_embed.is_some()
    }

    pub fn selected_db_display(&self) -> String {
        match &self.selected_db_for_embed {
            Some(name) => name.clone(),
            None => String::new(),
        }
    }

    // Internal helpers

    pub(crate) fn max_preview_scroll(&self) -> usize {
        self.content_preview
            .lines()
            .count()
            .saturating_sub(1)
            .max(0)
    }

    pub(crate) fn clamp_db_scroll(&mut self) {
        let total = self.registry.dbs.len();
        if total == 0 {
            self.dbs_scroll_offset = 0;
            return;
        }
        let visible = DB_SCROLL_VISIBLE.min(total);
        if self.selected_db_index >= self.dbs_scroll_offset + visible {
            self.dbs_scroll_offset = self
                .selected_db_index
                .saturating_sub(visible.saturating_sub(1));
        }
        if self.selected_db_index < self.dbs_scroll_offset {
            self.dbs_scroll_offset = self.selected_db_index;
        }
    }

    pub(crate) fn filtered_dbs(&self) -> Vec<&super::models::RagDb> {
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
}

impl Default for RagView {
    fn default() -> Self {
        Self::new()
    }
}

// Free helpers

/// Max number of models to show in the expanded selector (depends on terminal height).
pub(crate) fn max_visible_models(avail_h: u16) -> usize {
    let cap = (avail_h.saturating_sub(MODEL_COUNT_HEADROOM) / 2) as usize;
    cap.clamp(MIN_VISIBLE_MODELS, MAX_VISIBLE_MODELS_IN_FORM)
}

/// Rows a markdown paragraph occupies at `pad_w` columns, using the same
/// wrap logic as the markdown renderer (so the form box fits the fields).
fn markdown_field_lines(text: &str, pad_w: u16) -> u16 {
    if text.is_empty() {
        return 1;
    }
    estimate_height(text, pad_w.max(1)).max(1)
}
