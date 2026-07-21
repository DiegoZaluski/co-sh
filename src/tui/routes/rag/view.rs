//! RAG Knowledge Base view — core struct and lifecycle.
//!
//! The `RagView` struct holds all UI state for the RAG screen.
//! Input handling lives in `handlers.rs`, rendering lives in `render.rs`.
//!
//! Layout — two boxed sections:
//! 1. **Embed Content** — URL/path input (thin prompt-style) + spacious content preview
//! 2. **Available Databases** — toggleable DB list with inline \"+ Create New Database\"
//!    at the bottom (Tab expands the creation form inside this same box)

use std::collections::HashSet;

use super::models::{EmbedModelEntry, RagMode};
use super::registry::RagRegistry;
use crate::component::cursor::Cursor;
use crate::component::rag_input::RagInput;
use crate::component::search_bar::SearchBar;
use crate::component::spinner::SpinnerState;

// ── RagView ─────────────────────────────────────────────────────────────

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
    pub(crate) db_name_input: String,
    pub(crate) db_name_cursor: Cursor,
    pub(crate) db_description_input: String,
    pub(crate) db_description_cursor: Cursor,

    // DB registry + filter
    pub(crate) registry: RagRegistry,
    /// Receiver for async URL fetch result.
    pub(crate) fetch_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    pub(crate) selected_db_index: usize,
    pub(crate) dbs_scroll_offset: usize,
    pub(crate) active_dbs: HashSet<String>,
    pub(crate) db_filter: SearchBar,
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

    /// Close the preview overlay and return to Idle (stays in RAG view).
    pub fn close_preview(&mut self) {
        self.mode = RagMode::Idle;
        self.content_preview.clear();
        self.preview_scroll = 0;
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

    // ── DB helpers ─────────────────────────────────────────────────────

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
            self.model_scroll_offset = 0;
            self.db_name_input.clear();
            self.db_description_input.clear();
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
        if self.show_create_db {
            self.show_create_db = false;
            self.models_expanded = false;
            self.selected_model_index = 0;
            self.model_scroll_offset = 0;
            self.db_name_input.clear();
            self.db_description_input.clear();
        }
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

    // ── Internal helpers ───────────────────────────────────────────────

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
        let visible = 20usize.min(total);
        if self.selected_db_index >= self.dbs_scroll_offset + visible {
            self.dbs_scroll_offset =
                self.selected_db_index.saturating_sub(visible.saturating_sub(1));
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
