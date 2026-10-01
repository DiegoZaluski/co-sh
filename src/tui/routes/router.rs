use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh::ModelEntry;
use cosh_tui::core::types::MouseEvent;

use crate::component::search_bar::SearchBar;
use crate::fallback::{FallbackEntry, PromptCorrectorFallback, default_fallbacks};
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;
use crate::util::list_selection::ListSelection;

const FOOTER_MARGIN: u16 = 3;
const SIDE_PADDING: u16 = 4;
// Title row, one blank gap row, the search bar, then the list.
const MODEL_LIST_TOP_OFFSET: u16 = 4;
const FALLBACK_LIST_TOP_OFFSET: u16 = 2;
const VISIBLE_COUNT: usize = 20;
const ROUTER_SECTION_GAP: u16 = 1;
const AUTO_ROUTER_MIN_HEIGHT: u16 = 6;
const TAB_BAR_HEIGHT: u16 = 1;
const ACP_LIST_TOP_OFFSET: u16 = 2;
// Title row, one blank gap row, the search bar, then the list.
const PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET: u16 = 4;
const TAB_AUTO_TITLE: &str = " Fallback Auto ";
const TAB_PROMPT_TITLE: &str = " Fallback Prompt Corrector ";
/// The router shares the header row with the app's "← esc" hint, which
/// occupies the five columns after the left padding. Tab titles start two
/// columns past it, and the row fill skips the hint's cells entirely so the
/// hint stays visible.
const TAB_BAR_LEFT_PAD: u16 = 8;

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

/// Perceived luminance of an RGB color on the theme's contrast pivot:
/// below it the theme surface is "dark".
fn luma(color: Color) -> f32 {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return 0.0,
    };
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
}

/// Nudge a block's background one step toward legibility for the hover
/// highlight: dark surfaces blend toward white, light ones toward black
/// (same pivot as `theme::contrast_color`). `Color::Reset` — the transparent
/// theme convention for "no fill" — is returned untouched so the wash is a
/// no-op there.
fn lightened(color: Color) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return color,
    };
    let toward = if luma(color) > 0.5 { 0.0 } else { 255.0 };
    // ~7.5% per channel: clearly visible next to the resting fill, still
    // reading as "the same block".
    const STEP: f32 = 0.075;
    let mix = |c: u8| -> u8 { (f32::from(c) + (toward - f32::from(c)) * STEP).round() as u8 };
    Color::Rgb(mix(r), mix(g), mix(b))
}

/// Paint the hover wash over one block. `idle_bg` is the block's resting
/// background fill; a cell is touched ONLY while its current bg still equals
/// that fill, so overlays painted with their own background (the primary
/// title bands) keep their color. `Cell::set_style` patches — `None` fields
/// keep the cell's values — so the glyphs the block already drew stay put
/// and only the background lightens. The cursor is the last known mouse
/// position (`None` when the pointer left the router).
fn paint_block_hover(buf: &mut Buffer, block: Rect, cursor: Option<(u16, u16)>, idle_bg: Color) {
    if block.is_empty() {
        return;
    }
    let hovered = cursor.is_some_and(|(cx, cy)| {
        cx >= block.x && cx < block.right() && cy >= block.y && cy < block.bottom()
    });
    if !hovered {
        return;
    }
    let style = Style::default().bg(lightened(idle_bg));
    for y in block.y..block.bottom() {
        for x in block.x..block.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && cell.bg == idle_bg
            {
                cell.set_style(style);
            }
        }
    }
}

/// Whether a click on a list row lands on the row's visible text. Rows are
/// rendered from `row_x` one cell per char, so the text occupies exactly
/// `text.chars().count()` columns; a click past it hits the row's empty
/// trailing space and means a focus switch only — never a selection. The
/// two-char selection prefix ("🞴 " / "  ") must be included in `text` so
/// the length matches what the render drew.
fn click_lands_on_text(mouse_x: u16, row_x: u16, text: &str) -> bool {
    mouse_x >= row_x && ((mouse_x - row_x) as usize) < text.chars().count()
}

// ── Row text builders ────────────────────────────────────────────
// Every list row's visible text is built here and ONLY here: the render
// draws these strings and the mouse hit-test measures a click's column
// against their exact length, so the two sides must never drift.

/// The two-column marker every row starts with: the 🞴 selection
/// indicator or a same-width blank.
fn row_marker(selected: bool) -> &'static str {
    if selected { "🞴 " } else { "  " }
}

/// `provider  model` rows — shared by the automatic list and the
/// correction tab's model catalog.
fn model_row_text(marker: &str, entry: &ModelEntry) -> String {
    format!("{marker}{}  {}", entry.provider, entry.model)
}

fn auto_fallback_row_text(marker: &str, position: usize, entry: &FallbackEntry) -> String {
    format!(
        "{marker}{}.  {}  {}",
        position + 1,
        entry.provider,
        entry.model
    )
}

fn acp_row_text(marker: &str, agent_name: &str, detected: bool) -> String {
    let availability = if detected { "" } else { " (not detected)" };
    format!("{marker}{agent_name}{availability}")
}

fn prompt_fallback_row_text(
    marker: &str,
    position: usize,
    entry: &PromptCorrectorFallback,
) -> String {
    let number = position + 1;
    match entry {
        PromptCorrectorFallback::Model { provider, model } => {
            format!("{marker}{number}. {provider}  {model}")
        }
        PromptCorrectorFallback::Acp { agent } => format!("{marker}{number}. ACP  {agent}"),
    }
}

/// Resolve a click inside a scrolled list to the absolute entry index.
/// `None` when the click is above the rows or past the rendered rows
/// (empty space under a short list) — such clicks mean "focus the box"
/// only, never a selection.
fn clicked_row_index(
    mouse_y: u16,
    list_top: u16,
    visible_rows: usize,
    total: usize,
    scroll: usize,
) -> Option<usize> {
    if mouse_y < list_top {
        return None;
    }
    let row = (mouse_y - list_top) as usize;
    if row >= visible_rows {
        return None;
    }
    let idx = scroll + row;
    (idx < total).then_some(idx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouterTab {
    /// Agent-loop fallback chain (the original router screen).
    Auto,
    /// Prompt-correction fallback chain.
    PromptCorrector,
}

impl RouterTab {
    pub const fn title(self) -> &'static str {
        match self {
            RouterTab::Auto => TAB_AUTO_TITLE,
            RouterTab::PromptCorrector => TAB_PROMPT_TITLE,
        }
    }

    const fn next(self) -> Self {
        match self {
            RouterTab::Auto => RouterTab::PromptCorrector,
            RouterTab::PromptCorrector => RouterTab::Auto,
        }
    }

    const fn prev(self) -> Self {
        self.next()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Models,
    Fallbacks,
    PromptCorrectorModels,
    PromptCorrectorAcp,
    PromptCorrectorFallbacks,
}

/// Router interaction that needs the owning [`App`](crate::app::App) to
/// persist the changed correction chain.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptCorrectorAction {
    Changed,
    Selected,
    Consumed,
}

/// Geometry for the automatic router above and the prompt-correction router
/// below it. Keeping render and mouse dispatch on this shared layout prevents
/// clicks from leaking into the section above.
struct RouterLayout {
    /// Clickable tab titles across the top row.
    tab_bar: Rect,
    automatic_models: Rect,
    automatic_fallbacks: Rect,
    /// ACP agent chooser: top of the left column (prompt-corrector tab).
    prompt_choices: Rect,
    prompt_acp: Rect,
    /// Model chooser: below the ACP chooser on the left column.
    prompt_models: Rect,
    prompt_fallbacks: Rect,
}

pub struct RouterView {
    pub selection: ListSelection,
    pub search_bar: SearchBar,
    pub fallbacks: Vec<FallbackEntry>,
    /// REST-model and ACP candidates in exactly the order the user chose.
    pub prompt_corrector_fallbacks: Vec<PromptCorrectorFallback>,
    /// Which of the two tab sections is currently displayed.
    pub active_tab: RouterTab,
    pub focus: FocusTarget,
    selected_fallback: usize,
    fallback_scroll_offset: usize,
    /// The correction route's model-chooser selection, independent of the
    /// automatic router above.
    pub prompt_model_selection: ListSelection,
    /// The correction route's model-chooser filter.
    pub prompt_model_search_bar: SearchBar,
    selected_acp_agent: usize,
    selected_prompt_corrector_fallback: usize,
    num_buffer: String,
    /// Last known mouse position (fed by `App` from Move events) driving
    /// the block hover highlight. `None` when the pointer left the router
    /// or the route was closed.
    hover_cell: Option<(u16, u16)>,
}

impl RouterView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            search_bar: SearchBar::new(),
            fallbacks: default_fallbacks(),
            prompt_corrector_fallbacks: Vec::new(),
            active_tab: RouterTab::Auto,
            focus: FocusTarget::Models,
            selected_fallback: 0,
            fallback_scroll_offset: 0,
            prompt_model_selection: ListSelection::new(),
            prompt_model_search_bar: SearchBar::new(),
            selected_acp_agent: 0,
            selected_prompt_corrector_fallback: 0,
            num_buffer: String::new(),
            hover_cell: None,
        }
    }

    /// Feed the last known mouse position so the block under the pointer
    /// lightens for the next frame. Call with `None` when the pointer leaves
    /// the router area.
    pub fn update_hover(&mut self, x: u16, y: u16) {
        self.hover_cell = Some((x, y));
    }

    /// Drop the hover highlight (pointer left the route, or the route closed).
    pub fn clear_hover(&mut self) {
        self.hover_cell = None;
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

    pub fn set_prompt_corrector_fallbacks(&mut self, fallbacks: Vec<PromptCorrectorFallback>) {
        self.prompt_corrector_fallbacks = fallbacks;
        self.clamp_prompt_corrector_fallback_selection();
    }

    /// Append a model to the correction chain, or remove it when it is
    /// already selected. Re-selecting a removed item appends it again, which
    /// gives the user a direct click-only way to adjust its fallback order.
    pub fn toggle_prompt_corrector_model(&mut self, provider: String, model: String) {
        if let Some(index) = self.prompt_corrector_fallbacks.iter().position(|entry| {
            matches!(entry, PromptCorrectorFallback::Model {
                provider: selected_provider,
                model: selected_model,
            } if selected_provider == &provider && selected_model == &model)
        }) {
            self.prompt_corrector_fallbacks.remove(index);
        } else {
            self.prompt_corrector_fallbacks
                .push(PromptCorrectorFallback::Model { provider, model });
        }
        self.clamp_prompt_corrector_fallback_selection();
    }

    fn toggle_prompt_corrector_acp(&mut self, agent: &str) {
        if let Some(index) = self
            .prompt_corrector_fallbacks
            .iter()
            .position(|entry| matches!(entry, PromptCorrectorFallback::Acp { agent: selected } if selected == agent))
        {
            self.prompt_corrector_fallbacks.remove(index);
        } else {
            self.prompt_corrector_fallbacks
                .push(PromptCorrectorFallback::Acp {
                    agent: agent.to_string(),
                });
        }
        self.clamp_prompt_corrector_fallback_selection();
    }

    /// Filter the correction route's model catalog the same way the
    /// automatic router filters its own list. Returns owned copies because
    /// the catalog is filtered (`!= "auto"`) before display.
    fn filtered_prompt_models(&self, models: &[ModelEntry]) -> Vec<ModelEntry> {
        let lower = self.prompt_model_search_bar.as_str().to_lowercase();
        models
            .iter()
            .filter(|entry| entry.model != "auto" && !entry.provider.is_empty())
            .filter(|m| {
                lower.is_empty()
                    || m.model.to_lowercase().contains(&lower)
                    || m.provider.to_lowercase().contains(&lower)
            })
            .cloned()
            .collect()
    }

    /// All registered ACP agents, installed harnesses first so the usable
    /// options lead the list. No filter: the registry is small and fixed.
    fn acp_agents_ordered(&self) -> Vec<&'static cosh_tools::subagent::acp::Agent> {
        let installed = cosh_tools::subagent::acp::detect_installed();
        let (mut present, mut missing): (Vec<_>, Vec<_>) = cosh_tools::subagent::acp::ACP_AGENTS
            .iter()
            .partition(|agent| installed.contains(&agent.name));
        present.append(&mut missing);
        present
    }

    /// Switch to the other tab section. Called by Left/Right keys and tab
    /// clicks; focus resets to the active tab's first list.
    pub fn set_active_tab(&mut self, tab: RouterTab) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        self.focus = match tab {
            RouterTab::Auto => FocusTarget::Models,
            RouterTab::PromptCorrector => FocusTarget::PromptCorrectorAcp,
        };
    }

    pub const fn active_tab(&self) -> RouterTab {
        self.active_tab
    }

    /// Switch to the next (Right) tab.
    pub fn select_next_tab(&mut self) {
        self.set_active_tab(self.active_tab.next());
    }

    /// Switch to the previous (Left) tab.
    pub fn select_prev_tab(&mut self) {
        self.set_active_tab(self.active_tab.prev());
    }

    pub const fn acp_picker_open(&self) -> bool {
        false
    }

    pub fn close_acp_picker(&mut self) {}

    pub fn toggle_acp_picker(&mut self) {
        self.focus = FocusTarget::PromptCorrectorAcp;
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusTarget::Models => FocusTarget::Fallbacks,
            FocusTarget::Fallbacks => FocusTarget::PromptCorrectorAcp,
            FocusTarget::PromptCorrectorAcp => FocusTarget::PromptCorrectorModels,
            FocusTarget::PromptCorrectorModels => FocusTarget::PromptCorrectorFallbacks,
            FocusTarget::PromptCorrectorFallbacks => FocusTarget::Models,
        };
    }

    pub fn select_next_acp_agent(&mut self) {
        let total = self.acp_agents_ordered().len();
        if total > 0 {
            self.selected_acp_agent = (self.selected_acp_agent + 1) % total;
        }
    }

    pub fn select_prev_acp_agent(&mut self) {
        let total = self.acp_agents_ordered().len();
        if total > 0 {
            self.selected_acp_agent = (self.selected_acp_agent + total - 1) % total;
        }
    }

    pub fn toggle_selected_acp_agent(&mut self) -> bool {
        let agents = self.acp_agents_ordered();
        let Some(agent) = agents.get(self.selected_acp_agent) else {
            return false;
        };
        let name = agent.name;
        self.toggle_prompt_corrector_acp(name);
        true
    }

    fn clamp_prompt_corrector_fallback_selection(&mut self) {
        self.selected_prompt_corrector_fallback = self
            .selected_prompt_corrector_fallback
            .min(self.prompt_corrector_fallbacks.len().saturating_sub(1));
    }

    pub fn select_next_prompt_corrector_acp(&mut self) {
        self.select_next_acp_agent();
    }

    pub fn select_prev_prompt_corrector_acp(&mut self) {
        self.select_prev_acp_agent();
    }

    /// Move the correction route's model selection down one row. The
    /// visible-count hint comes from the last render, mirroring the
    /// automatic router's list so the window scrolls instead of letting
    /// the selector run off-screen.
    pub fn select_next_prompt_corrector_model(&mut self, models: &[ModelEntry]) {
        let total = self.filtered_prompt_models(models).len();
        self.prompt_model_selection.select_next(total);
    }

    /// Move the correction route's model selection up one row (window
    /// scroll mirrors the automatic router's list).
    pub fn select_prev_prompt_corrector_model(&mut self, models: &[ModelEntry]) {
        let total = self.filtered_prompt_models(models).len();
        self.prompt_model_selection.select_prev(total);
    }

    /// Toggle the correction route's currently highlighted model.
    pub fn toggle_selected_prompt_corrector_model(&mut self, models: &[ModelEntry]) -> bool {
        let filtered = self.filtered_prompt_models(models);
        let Some(entry) = filtered.get(self.prompt_model_selection.selected_index) else {
            return false;
        };
        self.toggle_prompt_corrector_model(entry.provider.clone(), entry.model.clone());
        true
    }

    pub fn select_next_prompt_corrector_fallback(&mut self) {
        if !self.prompt_corrector_fallbacks.is_empty() {
            self.selected_prompt_corrector_fallback = (self.selected_prompt_corrector_fallback + 1)
                .min(self.prompt_corrector_fallbacks.len() - 1);
        }
    }

    pub fn select_prev_prompt_corrector_fallback(&mut self) {
        self.selected_prompt_corrector_fallback =
            self.selected_prompt_corrector_fallback.saturating_sub(1);
    }

    /// Remove the selected correction candidate. Unlike the automatic
    /// router, an empty correction route is valid and simply disables the
    /// operation until the user selects another candidate.
    pub fn remove_selected_prompt_corrector(&mut self) -> bool {
        if self.focus != FocusTarget::PromptCorrectorFallbacks
            || self.selected_prompt_corrector_fallback >= self.prompt_corrector_fallbacks.len()
        {
            return false;
        }
        self.prompt_corrector_fallbacks
            .remove(self.selected_prompt_corrector_fallback);
        self.clamp_prompt_corrector_fallback_selection();
        true
    }

    pub fn push_filter_char(&mut self, ch: char) {
        self.search_bar.push_char(ch);
        self.selection.clamp(self.filtered_models(&[]).len());
    }

    pub fn pop_filter_char(&mut self) {
        self.search_bar.pop_char();
        self.selection.clamp(self.filtered_models(&[]).len());
    }

    /// Type into the correction route's model filter.
    pub fn push_prompt_model_filter_char(&mut self, ch: char) {
        self.prompt_model_search_bar.push_char(ch);
        self.prompt_model_selection
            .clamp(self.filtered_prompt_models(&[]).len());
    }

    pub fn pop_prompt_model_filter_char(&mut self) {
        self.prompt_model_search_bar.pop_char();
        self.prompt_model_selection
            .clamp(self.filtered_prompt_models(&[]).len());
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
        if self.fallback_scroll_offset > 0 && self.fallback_scroll_offset >= self.fallbacks.len() {
            self.fallback_scroll_offset = self.fallbacks.len().saturating_sub(VISIBLE_COUNT);
        }
    }

    fn prompt_corrector_height(&self, body_h: u16) -> u16 {
        body_h
    }

    /// Tab bar on the first row; the active section fills the rest. Only
    /// one section is ever visible, so each list gets the full body height
    /// and no section is ever clipped by the other.
    fn compute_layout(&self, area: Rect) -> RouterLayout {
        let body_h = area.height.saturating_sub(FOOTER_MARGIN);
        let tab_bar = Rect::new(area.x, area.y, area.width, TAB_BAR_HEIGHT);
        let body = Rect::new(
            area.x,
            area.y + TAB_BAR_HEIGHT + ROUTER_SECTION_GAP,
            area.width,
            body_h.saturating_sub(TAB_BAR_HEIGHT + ROUTER_SECTION_GAP),
        );
        let half_w = body.width / 2;
        let (left, right) = if self.active_tab == RouterTab::Auto {
            (Rect::new(body.x, body.y, half_w, body.height), true)
        } else {
            (Rect::ZERO, false)
        };
        let (prompt_choices, prompt_fallbacks) = if self.active_tab == RouterTab::PromptCorrector {
            (
                Rect::new(body.x, body.y, half_w, body.height),
                Rect::new(body.x + half_w, body.y, body.width - half_w, body.height),
            )
        } else {
            (Rect::ZERO, Rect::ZERO)
        };
        RouterLayout {
            tab_bar,
            automatic_models: left,
            automatic_fallbacks: if right {
                Rect::new(body.x + half_w, body.y, body.width - half_w, body.height)
            } else {
                Rect::ZERO
            },
            prompt_choices,
            prompt_acp: Rect::new(
                prompt_choices.x,
                prompt_choices.y,
                prompt_choices.width,
                ACP_LIST_TOP_OFFSET
                    + cosh_tools::subagent::acp::ACP_AGENTS.len() as u16
                    + ROUTER_SECTION_GAP,
            ),
            prompt_models: Rect::new(
                prompt_choices.x,
                prompt_choices.y
                    + ACP_LIST_TOP_OFFSET
                    + cosh_tools::subagent::acp::ACP_AGENTS.len() as u16
                    + ROUTER_SECTION_GAP,
                prompt_choices.width,
                prompt_choices
                    .height
                    .saturating_sub(ACP_LIST_TOP_OFFSET + 1)
                    .saturating_sub(cosh_tools::subagent::acp::ACP_AGENTS.len() as u16),
            ),
            prompt_fallbacks,
        }
    }

    /// Handle clicks on the prompt-corrector tab. Clicking a tab title
    /// switches sections; the ACP rows toggle agents into the chain and the
    /// fallback rows select entries for removal. Selections trigger only
    /// when the click lands on the row's visible text (`models` is needed
    /// to resolve the model chooser rows); clicks past the text are plain
    /// focus switches.
    pub fn handle_prompt_corrector_mouse(
        &mut self,
        models: &[ModelEntry],
        mouse: &MouseEvent,
        area: Rect,
    ) -> Option<PromptCorrectorAction> {
        let layout = self.compute_layout(area);
        if mouse.x < area.x
            || mouse.x >= area.right()
            || mouse.y < area.y
            || mouse.y >= area.bottom()
        {
            return None;
        }

        // Only the prompt-corrector tab owns its rows. On the auto tab every
        // section rect collapses to `Rect::ZERO` (x = 0, width = 0), so the
        // column checks below would match any x and consume clicks that
        // belong to the automatic lists, leaving their boxes without focus.
        if self.active_tab != RouterTab::PromptCorrector {
            return None;
        }

        // Tab bar spans the full width: a click on a title switches tabs.
        if mouse.y >= layout.tab_bar.y && mouse.y < layout.tab_bar.bottom() {
            let clicked = self.tab_at(&layout, mouse.x);
            if let Some(tab) = clicked {
                self.set_active_tab(tab);
            }
            return Some(PromptCorrectorAction::Consumed);
        }

        // Left column: the ACP chooser list first, then the model chooser.
        let choice_x = layout.prompt_choices.x + SIDE_PADDING / 2;
        if mouse.x >= layout.prompt_choices.x && mouse.x < layout.prompt_choices.right() {
            if mouse.y >= layout.prompt_acp.y && mouse.y < layout.prompt_acp.bottom() {
                let agents = self.acp_agents_ordered();
                if let Some(agent_index) = clicked_row_index(
                    mouse.y,
                    layout.prompt_acp.y + ACP_LIST_TOP_OFFSET,
                    agents.len(),
                    agents.len(),
                    0,
                ) {
                    self.focus = FocusTarget::PromptCorrectorAcp;
                    // Selecting only on the row's text: past it the
                    // click is a focus switch, not a chain toggle.
                    let agent = agents[agent_index];
                    let text = acp_row_text(
                        "  ",
                        agent.name,
                        cosh_tools::subagent::acp::detect_installed().contains(&agent.name),
                    );
                    if click_lands_on_text(mouse.x, choice_x, &text) {
                        self.selected_acp_agent = agent_index;
                        if self.toggle_selected_acp_agent() {
                            return Some(PromptCorrectorAction::Changed);
                        }
                    }
                }
                return Some(PromptCorrectorAction::Consumed);
            }
            if mouse.y >= layout.prompt_models.y && mouse.y < layout.prompt_models.bottom() {
                self.focus = FocusTarget::PromptCorrectorModels;
                let filtered = self.filtered_prompt_models(models);
                let visible = layout
                    .prompt_models
                    .height
                    .saturating_sub(PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET)
                    as usize;
                if let Some(idx) = clicked_row_index(
                    mouse.y,
                    layout.prompt_models.y + PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET,
                    visible,
                    filtered.len(),
                    self.prompt_model_selection.scroll_offset,
                ) {
                    // Same selection rule as every other list: the click
                    // must land on the rendered row text. Within it, the
                    // click mirrors Enter — the model toggles into the
                    // correction route.
                    let text = model_row_text("  ", &filtered[idx]);
                    if click_lands_on_text(mouse.x, choice_x, &text) {
                        self.prompt_model_selection.selected_index = idx;
                        if self.toggle_selected_prompt_corrector_model(models) {
                            return Some(PromptCorrectorAction::Changed);
                        }
                    }
                }
                return Some(PromptCorrectorAction::Consumed);
            }
            return Some(PromptCorrectorAction::Consumed);
        }

        if mouse.x < layout.prompt_fallbacks.x || mouse.x >= layout.prompt_fallbacks.right() {
            return Some(PromptCorrectorAction::Consumed);
        }

        let list_top = layout.prompt_fallbacks.y + 4;
        let visible_rows = layout.prompt_fallbacks.height.saturating_sub(5) as usize;
        if let Some(idx) = clicked_row_index(
            mouse.y,
            list_top,
            self.prompt_corrector_fallbacks.len().min(visible_rows),
            self.prompt_corrector_fallbacks.len(),
            0,
        ) {
            self.focus = FocusTarget::PromptCorrectorFallbacks;
            let text = prompt_fallback_row_text("  ", idx, &self.prompt_corrector_fallbacks[idx]);
            if click_lands_on_text(mouse.x, layout.prompt_fallbacks.x + SIDE_PADDING / 2, &text) {
                self.selected_prompt_corrector_fallback = idx;
                return Some(PromptCorrectorAction::Selected);
            }
            return Some(PromptCorrectorAction::Consumed);
        }
        Some(PromptCorrectorAction::Consumed)
    }

    /// Which tab title sits under the given x coordinate, if any.
    fn tab_at(&self, layout: &RouterLayout, x: u16) -> Option<RouterTab> {
        let mut cursor = layout.tab_bar.x + TAB_BAR_LEFT_PAD;
        for tab in [RouterTab::Auto, RouterTab::PromptCorrector] {
            let width = tab.title().chars().count() as u16;
            if x >= cursor && x < cursor + width {
                return Some(tab);
            }
            cursor += width + 2;
        }
        None
    }

    pub fn handle_mouse(&mut self, models: &[ModelEntry], mouse: &MouseEvent, area: Rect) -> bool {
        let layout = self.compute_layout(area);
        let left_area = layout.automatic_models;
        let right_area = layout.automatic_fallbacks;
        let work_h = left_area.height;

        // The other tab owns its clicks; this handler never sees them
        // because `handle_prompt_corrector_mouse` runs first in App.
        if self.active_tab != RouterTab::Auto {
            return false;
        }

        // Tab bar spans the full width: a click on a title switches tabs.
        if mouse.y >= layout.tab_bar.y && mouse.y < layout.tab_bar.bottom() {
            if let Some(tab) = self.tab_at(&layout, mouse.x) {
                self.set_active_tab(tab);
                return true;
            }
            return false;
        }

        let text_pad = SIDE_PADDING / 2;

        if mouse.x >= left_area.x && mouse.x < left_area.right() {
            self.focus = FocusTarget::Models;
            let filtered = self.filtered_models(models);
            let visible =
                (work_h.saturating_sub(MODEL_LIST_TOP_OFFSET + 1) as usize).min(filtered.len());
            if let Some(idx) = clicked_row_index(
                mouse.y,
                left_area.y + MODEL_LIST_TOP_OFFSET,
                visible,
                filtered.len(),
                self.selection.scroll_offset,
            ) {
                // Selecting (which adds to the fallback chain) only when
                // the click lands on the row's rendered text; past it the
                // click is a directed focus switch and must not touch the
                // selection or the chain.
                let entry = filtered[idx];
                let text = model_row_text("  ", entry);
                if click_lands_on_text(mouse.x, left_area.x + text_pad, &text) {
                    self.selection.selected_index = idx;
                    self.add_selected_to_fallback(models);
                }
            }
            return true;
        }

        if mouse.x >= right_area.x && mouse.x < right_area.right() {
            self.focus = FocusTarget::Fallbacks;
            let visible = (work_h.saturating_sub(FALLBACK_LIST_TOP_OFFSET + 1) as usize)
                .min(self.fallbacks.len());
            if let Some(idx) = clicked_row_index(
                mouse.y,
                right_area.y + FALLBACK_LIST_TOP_OFFSET,
                visible,
                self.fallbacks.len(),
                self.fallback_scroll_offset,
            ) {
                // Same text-boundary rule as the models list: only the
                // row's string selects; empty row space just moves focus
                // to the box.
                let text = auto_fallback_row_text("  ", idx, &self.fallbacks[idx]);
                if click_lands_on_text(mouse.x, right_area.x + text_pad, &text) {
                    self.selected_fallback = idx;
                }
            }
            return true;
        }

        false
    }

    /// Draw the clickable tab bar: active tab highlighted, inactive muted.
    fn render_tab_bar(&self, buf: &mut Buffer, layout: &RouterLayout, theme: &Theme) {
        let primary = rgba_color(theme.primary);
        let contrast = crate::theme::contrast_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        let bg_full = rgba_color(theme.background);
        // Fill only past the "← esc" hint: `App::render` draws the hint on
        // this same row before the router view runs, and the frame-wide
        // background fill already covers the skipped cells.
        fill_rect(
            buf,
            layout.tab_bar.x + TAB_BAR_LEFT_PAD,
            layout.tab_bar.y,
            layout.tab_bar.width.saturating_sub(TAB_BAR_LEFT_PAD),
            layout.tab_bar.height,
            Style::default().bg(bg_full),
        );
        let mut x = layout.tab_bar.x + TAB_BAR_LEFT_PAD;
        for tab in [RouterTab::Auto, RouterTab::PromptCorrector] {
            let active = tab == self.active_tab;
            section_title(
                buf,
                x,
                layout.tab_bar.y,
                tab.title(),
                if active { primary } else { bg_full },
                if active { contrast } else { muted },
            );
            x += tab.title().chars().count() as u16 + 2;
        }
    }

    fn render_prompt_corrector(
        &mut self,
        buf: &mut Buffer,
        layout: &RouterLayout,
        theme: &Theme,
        models: &[ModelEntry],
    ) {
        if layout.prompt_choices.is_empty() {
            return;
        }

        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);
        let panel_bg = rgba_color(theme.background_panel);
        let text_pad = SIDE_PADDING / 2;

        self.render_tab_bar(buf, layout, theme);

        let choice_x = layout.prompt_choices.x + text_pad;
        let choice_w = layout.prompt_choices.width.saturating_sub(SIDE_PADDING);

        // ── ACP chooser (top of the left column, no filter) ──
        let acp_focused = self.focus == FocusTarget::PromptCorrectorAcp;
        // The block carries the prompt input box's `background_element` fill
        // so it reads as a delimited section above the model catalog (same
        // color the prompt uses to fake transparency over its inner area).
        // Transparent themes (`background_element` alpha 0 → `Color::Reset`)
        // skip the fill, mirroring the prompt's cap-band rule. Text styles
        // below re-apply the bg because `set_style` replaces it cell-wide.
        let acp_bg = rgba_color(theme.background_element);
        let acp_area = layout.prompt_acp;
        if acp_bg != Color::Reset {
            fill_rect(
                buf,
                acp_area.x,
                acp_area.y,
                acp_area.width,
                acp_area.height,
                Style::default().bg(acp_bg),
            );
        }
        let acp_row_bg = if acp_bg != Color::Reset {
            Style::default().bg(acp_bg)
        } else {
            Style::default()
        };
        let acp_title_style = acp_row_bg.fg(muted);
        draw_text_line(
            buf,
            "ACP Agents",
            choice_x,
            layout.prompt_acp.y,
            choice_w,
            acp_title_style,
        );

        let installed = cosh_tools::subagent::acp::detect_installed();
        let acp_list_top = layout.prompt_acp.y + ACP_LIST_TOP_OFFSET;
        for (index, agent) in self.acp_agents_ordered().into_iter().enumerate() {
            let y = acp_list_top + index as u16;
            if y >= layout.prompt_acp.bottom() {
                break;
            }
            let selected = acp_focused && index == self.selected_acp_agent;
            // Standard TUI selection: 🞴 marker + fg color only, never a
            // background wash. The section fill rides along so the text
            // never punches a hole in the `background_element` band.
            let style = if selected {
                Style::default().fg(primary)
            } else {
                Style::default().fg(fg)
            }
            .patch(acp_row_bg);
            let text = acp_row_text(
                row_marker(selected),
                agent.name,
                installed.contains(&agent.name),
            );
            draw_text_line(buf, &text, choice_x, y, choice_w, style);
        }
        // The ACP block owns its `background_element` band — hover lightens
        // exactly that band (and only its cells; overlays like row text
        // carry no bg of their own here, so the wash reads through).
        paint_block_hover(buf, layout.prompt_acp, self.hover_cell, acp_bg);

        // ── Model chooser (below the ACP list) ──
        let models_focused = self.focus == FocusTarget::PromptCorrectorModels;
        if !layout.prompt_models.is_empty() {
            draw_text_line(
                buf,
                "Models",
                choice_x,
                layout.prompt_models.y,
                choice_w,
                // Plain muted heading like the auto tab's "Available Models":
                // only the ACP block above carries the section fill.
                Style::default().fg(muted),
            );
            self.prompt_model_search_bar.render(
                buf,
                choice_x,
                layout.prompt_models.y + 2,
                choice_w,
                theme,
            );

            let model_list_top = layout.prompt_models.y + PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET;
            let filtered = self.filtered_prompt_models(models);
            let max_visible = layout
                .prompt_models
                .height
                .saturating_sub(PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET)
                as usize;
            // Same viewport hint the automatic router feeds its list: the
            // next select_next/prev keeps the selector inside this window.
            self.prompt_model_selection
                .set_visible_count(max_visible.max(1));
            let scroll = self.prompt_model_selection.scroll_offset;
            if filtered.is_empty() {
                let msg = if self.prompt_model_search_bar.is_empty() {
                    "No models loaded"
                } else {
                    "No matching models"
                };
                draw_text_line(
                    buf,
                    msg,
                    choice_x,
                    model_list_top,
                    choice_w,
                    Style::default().fg(muted),
                );
            } else {
                for i in 0..max_visible.min(filtered.len()) {
                    let idx = scroll + i;
                    if idx >= filtered.len() {
                        break;
                    }
                    let entry = &filtered[idx];
                    let y = model_list_top + i as u16;
                    if y >= layout.prompt_models.bottom() {
                        break;
                    }
                    let selected =
                        models_focused && idx == self.prompt_model_selection.selected_index;
                    let style = if selected {
                        Style::default().fg(primary)
                    } else {
                        Style::default().fg(fg)
                    };
                    let text = model_row_text(row_marker(selected), entry);
                    draw_text_line(buf, &text, choice_x, y, choice_w, style);
                }
            }
            // Hover for the model-chooser block: its resting surface is the
            // route background (the tab has no fill of its own here).
            paint_block_hover(
                buf,
                layout.prompt_models,
                self.hover_cell,
                rgba_color(theme.background),
            );
        }

        // ── Fallback chain (right column) ──
        let fallback_area = layout.prompt_fallbacks;
        if fallback_area.is_empty() {
            return;
        }
        fill_rect(
            buf,
            fallback_area.x,
            fallback_area.y,
            fallback_area.width,
            fallback_area.height,
            Style::default().bg(panel_bg),
        );
        section_title(
            buf,
            fallback_area.x + 1,
            fallback_area.y,
            " Fallback Chain ",
            primary,
            // Same title contrast the auto tab's Fallback Chain box uses.
            rgba_color(theme.background),
        );
        let fallback_x = fallback_area.x + text_pad;
        let fallback_w = fallback_area.width.saturating_sub(SIDE_PADDING);
        let list_top = fallback_area.y + 4;
        if self.prompt_corrector_fallbacks.is_empty() {
            if list_top < fallback_area.bottom() {
                draw_text_line(
                    buf,
                    "Pick an agent or model on the left to add",
                    fallback_x,
                    list_top,
                    fallback_w,
                    Style::default().fg(muted).bg(panel_bg),
                );
            }
            // The empty chain skips the row loop below; it still has to get
            // the hover wash before this early return.
            paint_block_hover(buf, fallback_area, self.hover_cell, panel_bg);
            return;
        }
        for (index, entry) in self.prompt_corrector_fallbacks.iter().enumerate() {
            let y = list_top + index as u16;
            if y >= fallback_area.bottom() {
                break;
            }
            let selected = self.focus == FocusTarget::PromptCorrectorFallbacks
                && index == self.selected_prompt_corrector_fallback;
            let style = if selected {
                Style::default().fg(primary)
            } else {
                Style::default().fg(fg)
            };
            // Order still matters in a chain, so the number stays — but the
            // row highlight is the standard 🞴 marker, no background wash.
            let text = prompt_fallback_row_text(row_marker(selected), index, entry);
            draw_text_line(buf, &text, fallback_x, y, fallback_w, style);
        }

        // Hover for the fallback-chain block, painted after its rows so the
        // wash covers the panel's exact final area (fill, title band, rows).
        paint_block_hover(buf, fallback_area, self.hover_cell, panel_bg);
    }

    /// Render the active tab. `&mut self` because rendering feeds the
    /// viewport height back into the list selections (`set_visible_count`),
    /// exactly as `ListSelection` documents.
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        models: &[ModelEntry],
        _now: std::time::SystemTime,
    ) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let layout = self.compute_layout(area);
        let left_area = layout.automatic_models;
        let right_area = layout.automatic_fallbacks;
        let work_h = left_area.height;

        let filtered = self.filtered_models(models);

        let panel_bg = rgba_color(theme.background_panel);
        let title_bg = primary;
        let title_fg = rgba_color(theme.background);

        let models_focused = self.focus == FocusTarget::Models;
        let fallbacks_focused = self.focus == FocusTarget::Fallbacks;

        let text_pad = SIDE_PADDING / 2;
        let title_pad = 1;

        if self.active_tab == RouterTab::PromptCorrector {
            // Only the correction section and the tab bar are visible.
            self.render_prompt_corrector(buf, &layout, theme, models);
            self.render_router_footer(buf, area, &layout, theme);
            return;
        }

        // ── Left panel: available models ──
        self.render_tab_bar(buf, &layout, theme);
        let title_style = Style::default().fg(muted);
        draw_text_line(
            buf,
            "Available Models",
            left_area.x + text_pad,
            left_area.y,
            left_area.width.saturating_sub(SIDE_PADDING),
            title_style,
        );

        let filter_y = left_area.y + 2;
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
                // Standard TUI selection: 🞴 marker + fg color only, never a
                // background wash — same pattern as the prompt-corrector tab.
                let style = if is_selected {
                    Style::default().fg(primary)
                } else {
                    Style::default().fg(fg)
                };

                let text = model_row_text(row_marker(is_selected), entry);
                draw_text_line(buf, &text, list_x, y, list_w, style);
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
                // Standard 🞴 selection: fg color only, order number kept
                // (the chain is tried top to bottom).
                let style = if is_selected {
                    Style::default().fg(primary)
                } else {
                    Style::default().fg(fg)
                };

                let entry_text = auto_fallback_row_text(row_marker(is_selected), i, fb);
                draw_text_line(buf, &entry_text, fb_list_x, y, fb_list_w, style);
            }
        }

        // Hover highlight for the exact blocks on this tab: wash the
        // available-models surface and the fallback-chain panel — rows,
        // titles and the primary title band included, everything that
        // still carries the block's resting background.
        let cursor = self.hover_cell;
        paint_block_hover(buf, left_area, cursor, rgba_color(theme.background));
        paint_block_hover(buf, right_area, cursor, panel_bg);

        self.render_router_footer(buf, area, &layout, theme);
    }

    /// Centered instruction line under the active section.
    fn render_router_footer(
        &self,
        buf: &mut Buffer,
        area: Rect,
        layout: &RouterLayout,
        theme: &Theme,
    ) {
        let muted = rgba_color(theme.text_muted);
        let footer_bg = rgba_color(theme.background);
        let text_pad = SIDE_PADDING / 2;
        // Clear the three footer rows below the section body.
        let footer_top = layout.tab_bar.bottom()
            + ROUTER_SECTION_GAP
            + layout
                .automatic_models
                .height
                .max(layout.prompt_choices.height);
        for y in footer_top..area.bottom() {
            for cx in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(footer_bg));
                }
            }
        }
        let footer_y = area.bottom().saturating_sub(FOOTER_MARGIN) + 2;

        let mut footer = String::from("Enter to add · type number before Enter to position");
        if self.has_num_buffer() && self.focus == FocusTarget::Models {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            x,
            y,
            MouseModifiers::none(),
        )
    }

    fn render_view(view: &mut RouterView, area: Rect, models: &[ModelEntry]) -> String {
        use crate::theme::ThemeRegistry;
        let theme = ThemeRegistry::new().default_theme().clone();
        let mut buffer = Buffer::empty(area);
        view.render(
            &mut buffer,
            area,
            &theme,
            models,
            std::time::SystemTime::now(),
        );
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    /// Render into a buffer pre-filled the way `App::render` fills the frame
    /// (every cell carrying the theme background). The hover wash only lights
    /// cells already carrying a block's resting background, so tests need that
    /// underlying fill to exist — the real app always provides it.
    fn render_buffer_with_frame_bg(
        view: &mut RouterView,
        area: Rect,
        models: &[ModelEntry],
        theme: &Theme,
    ) -> Buffer {
        let bg = rgba_color(theme.background);
        let mut blank = ratatui::buffer::Cell::EMPTY;
        blank.set_char(' ');
        blank.set_style(Style::default().bg(bg));
        let mut buffer = Buffer::filled(area, blank);
        view.render(
            &mut buffer,
            area,
            theme,
            models,
            std::time::SystemTime::now(),
        );
        buffer
    }

    fn test_model(provider: &str, model: &str) -> ModelEntry {
        ModelEntry {
            provider: provider.into(),
            model: model.into(),
        }
    }

    #[test]
    fn hover_lightens_the_models_block_under_the_pointer() {
        let mut view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        let models = vec![test_model("openrouter", "model-a")];
        let theme = ThemeRegistry::new().default_theme().clone();
        let bg = rgba_color(theme.background);

        let clean = render_buffer_with_frame_bg(&mut view, area, &models, &theme);
        let layout = view.compute_layout(area);
        let left = layout.automatic_models;
        let right = layout.automatic_fallbacks;
        assert!(!left.is_empty() && !right.is_empty());
        // Baseline: the block surface carries the plain frame background.
        assert_eq!(clean[(left.x + 2, left.y + 6)].bg, bg);

        view.update_hover(left.x + 2, left.y + 6);
        let lit = render_buffer_with_frame_bg(&mut view, area, &models, &theme);

        // The hovered block's body lightens away from the resting fill...
        assert_eq!(lit[(left.x + 2, left.y + 6)].bg, lightened(bg));
        // ...while the untouched fallback panel keeps its own fill.
        assert_eq!(
            lit[(right.x + 2, right.y + 2)].bg,
            rgba_color(theme.background_panel)
        );
    }

    #[test]
    fn hover_wash_covers_the_fallback_block_but_spares_the_title_band() {
        let mut view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        let models = vec![test_model("openrouter", "model-a")];
        let theme = ThemeRegistry::new().default_theme().clone();
        let panel_bg = rgba_color(theme.background_panel);

        view.update_hover(60, 20); // inside the fallback-chain panel
        let lit = render_buffer_with_frame_bg(&mut view, area, &models, &theme);

        let layout = view.compute_layout(area);
        let right = layout.automatic_fallbacks;
        assert!(!right.is_empty());
        // The title band is painted with `primary` as its bg: an overlay with
        // its own background, the wash must leave it alone.
        assert_eq!(lit[(right.x + 1, right.y)].bg, rgba_color(theme.primary));
        // A body cell (first fallback row's text) carries the lightened fill.
        assert_eq!(lit[(right.x + 2, right.y + 2)].bg, lightened(panel_bg));
    }

    #[test]
    fn hover_on_the_prompt_corrector_tab_lights_its_three_blocks() {
        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let area = Rect::new(0, 0, 100, 40);
        let theme = ThemeRegistry::new().default_theme().clone();
        let bg = rgba_color(theme.background);
        let element_bg = rgba_color(theme.background_element);

        let layout = view.compute_layout(area);
        let acp = layout.prompt_acp;
        let prompt_models = layout.prompt_models;
        let chain = layout.prompt_fallbacks;
        assert!(!acp.is_empty() && !prompt_models.is_empty() && !chain.is_empty());

        // Pointer on the ACP band: it lightens (its fill is the
        // `background_element` band), the model chooser below and the chain
        // panel keep their resting fills.
        view.update_hover(acp.x + 2, acp.y + 2);
        let lit = render_buffer_with_frame_bg(&mut view, area, &[], &theme);
        assert_eq!(lit[(acp.x + 2, acp.y + 2)].bg, lightened(element_bg));
        assert_eq!(lit[(prompt_models.x + 2, prompt_models.y + 8)].bg, bg);
        assert_eq!(
            lit[(chain.x + 2, chain.y + 5)].bg,
            rgba_color(theme.background_panel)
        );

        // Pointer on the model chooser: only that block lightens.
        view.update_hover(prompt_models.x + 2, prompt_models.y + 8);
        let lit = render_buffer_with_frame_bg(&mut view, area, &[], &theme);
        assert_eq!(
            lit[(prompt_models.x + 2, prompt_models.y + 8)].bg,
            lightened(bg)
        );
        assert_eq!(lit[(acp.x + 2, acp.y + 2)].bg, element_bg);

        // Pointer on the chain panel: it lightens, left column back to rest.
        view.update_hover(chain.x + 2, chain.y + 5);
        let lit = render_buffer_with_frame_bg(&mut view, area, &[], &theme);
        assert_eq!(
            lit[(chain.x + 2, chain.y + 5)].bg,
            lightened(rgba_color(theme.background_panel))
        );
        assert_eq!(lit[(prompt_models.x + 2, prompt_models.y + 8)].bg, bg);
    }

    #[test]
    fn leaving_the_router_clears_the_hover_state() {
        let mut view = RouterView::new();
        view.update_hover(1, 1);
        view.clear_hover();
        // Re-rendering after the clear must match the never-hovered frame.
        let area = Rect::new(0, 0, 100, 40);
        let models = vec![test_model("openrouter", "model-a")];
        let mut fresh = RouterView::new();
        assert_eq!(
            render_view(&mut view, area, &models),
            render_view(&mut fresh, area, &models)
        );
    }

    #[test]
    fn prompt_corrector_entries_keep_the_cross_field_selection_order() {
        let mut view = RouterView::new();

        view.toggle_prompt_corrector_model("openrouter".into(), "model-a".into());
        view.toggle_prompt_corrector_acp("codex");
        view.toggle_prompt_corrector_model("groq".into(), "model-b".into());

        assert_eq!(
            view.prompt_corrector_fallbacks,
            vec![
                PromptCorrectorFallback::Model {
                    provider: "openrouter".into(),
                    model: "model-a".into(),
                },
                PromptCorrectorFallback::Acp {
                    agent: "codex".into(),
                },
                PromptCorrectorFallback::Model {
                    provider: "groq".into(),
                    model: "model-b".into(),
                },
            ]
        );

        // Selecting an existing item removes it. Selecting it again appends
        // it, giving click-only order adjustment across both fields.
        view.toggle_prompt_corrector_acp("codex");
        view.toggle_prompt_corrector_acp("codex");
        assert!(matches!(
            view.prompt_corrector_fallbacks.last(),
            Some(PromptCorrectorFallback::Acp { agent }) if agent == "codex"
        ));
    }

    #[test]
    fn tabs_show_one_section_at_a_time_and_both_titles_render() {
        let view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);

        // Auto tab: automatic panels visible, prompt corrector collapsed.
        assert_eq!(view.active_tab, RouterTab::Auto);
        assert!(!layout.automatic_models.is_empty());
        assert!(layout.prompt_choices.is_empty());

        // Switch: prompt corrector visible full-height, automatic collapsed.
        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let layout = view.compute_layout(area);
        assert!(layout.automatic_models.is_empty());
        assert!(!layout.prompt_choices.is_empty());
        // The section gets the full body height (no clipping by the other).
        assert_eq!(
            layout.prompt_choices.height,
            layout
                .automatic_models
                .height
                .max(layout.prompt_choices.height)
        );

        // Both tab titles always render in the bar.
        let rendered = render_view(&mut view, area, &[]);
        assert!(rendered.contains("Fallback Auto"));
        assert!(rendered.contains("Fallback Prompt Corrector"));
    }

    #[test]
    fn tab_titles_render_only_the_active_section() {
        let mut view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        let rendered = render_view(&mut view, area, &[]);
        // Auto tab: no ACP rows leak into the view.
        assert!(!rendered.contains("ACP Agents"));

        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let rendered = render_view(&mut view, area, &[]);
        assert!(rendered.contains("ACP Agents"));
        assert!(rendered.contains("Fallback Chain"));
        // ACP needs no filter row: the search prompt must not appear there.
        assert!(!rendered.contains("Filter:"));
    }

    #[test]
    fn left_right_keys_and_clicks_switch_tabs() {
        let mut view = RouterView::new();
        view.select_next_tab();
        assert_eq!(view.active_tab, RouterTab::PromptCorrector);
        assert!(matches!(view.focus, FocusTarget::PromptCorrectorAcp));
        view.select_next_tab();
        assert_eq!(view.active_tab, RouterTab::Auto);
        assert!(matches!(view.focus, FocusTarget::Models));
        view.select_prev_tab();
        assert_eq!(view.active_tab, RouterTab::PromptCorrector);

        // Clicking the inactive title switches back.
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let auto_x = layout.tab_bar.x + TAB_BAR_LEFT_PAD + 2; // inside " Fallback Auto "
        assert!(matches!(
            view.handle_prompt_corrector_mouse(&[], &click(auto_x, layout.tab_bar.y), area),
            Some(PromptCorrectorAction::Consumed)
        ));
        assert_eq!(view.active_tab, RouterTab::Auto);

        // A click below the titles never switches tabs (the body panels own
        // that row and consume it).
        view.set_active_tab(RouterTab::Auto);
        let layout = view.compute_layout(area);
        let _ = view.handle_mouse(&[], &click(auto_x, layout.tab_bar.y + 1), area);
        assert_eq!(view.active_tab, RouterTab::Auto);
    }

    #[test]
    fn auto_tab_clicks_focus_the_automatic_boxes() {
        // Regression: on the auto tab the prompt-corrector handler used to
        // consume every body click (the collapsed section rects sit at x=0,
        // so the fallback-column check matched any x), and the automatic
        // boxes never took focus.
        let mut view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let body_y = layout.automatic_models.y + 6;

        // A body click on the auto tab must not be swallowed by the
        // prompt-corrector handler.
        let right_x = layout.automatic_fallbacks.x + 2;
        assert!(
            view.handle_prompt_corrector_mouse(&[], &click(right_x, body_y), area)
                .is_none()
        );
        // And the automatic router takes the click: the right (fallbacks)
        // box gains focus.
        assert!(view.handle_mouse(&[], &click(right_x, body_y), area));
        assert!(matches!(view.focus, FocusTarget::Fallbacks));

        // The left (models) box gains focus the same way.
        let left_x = layout.automatic_models.x + 2;
        assert!(view.handle_mouse(&[], &click(left_x, body_y), area));
        assert!(matches!(view.focus, FocusTarget::Models));
    }

    #[test]
    fn prompt_corrector_acp_rows_toggle_from_clicks() {
        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let list_top = layout.prompt_acp.y + ACP_LIST_TOP_OFFSET;

        assert!(matches!(
            view.handle_prompt_corrector_mouse(
                &[],
                &click(layout.prompt_choices.x + 2, list_top),
                area,
            ),
            Some(PromptCorrectorAction::Changed)
        ));
        assert_eq!(view.prompt_corrector_fallbacks.len(), 1);
        assert!(matches!(view.focus, FocusTarget::PromptCorrectorAcp));
    }

    #[test]
    fn prompt_corrector_model_rows_toggle_from_clicks() {
        // Regression: clicking the correction tab's model list only moved
        // focus — Enter toggled the highlighted model but the mouse could
        // not. A click on the row text must mirror Enter.
        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let models = vec![
            ModelEntry {
                provider: "openrouter".into(),
                model: "model-a".into(),
            },
            ModelEntry {
                provider: "groq".into(),
                model: "model-b".into(),
            },
        ];
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let choice_x = layout.prompt_choices.x + SIDE_PADDING / 2;
        let list_top = layout.prompt_models.y + PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET;

        // Click on the row text: the model toggles into the correction
        // route, like pressing Enter on the highlighted row.
        assert!(matches!(
            view.handle_prompt_corrector_mouse(&models, &click(choice_x + 3, list_top), area),
            Some(PromptCorrectorAction::Changed)
        ));
        assert_eq!(view.prompt_corrector_fallbacks.len(), 1);
        assert!(matches!(view.focus, FocusTarget::PromptCorrectorModels));

        // Click past the row text: focus switch only, nothing toggles.
        assert!(matches!(
            view.handle_prompt_corrector_mouse(&models, &click(choice_x + 40, list_top), area),
            Some(PromptCorrectorAction::Consumed)
        ));
        assert!(matches!(view.focus, FocusTarget::PromptCorrectorModels));
        assert_eq!(view.prompt_corrector_fallbacks.len(), 1);

        // Clicking the same text again toggles the entry back out.
        assert!(matches!(
            view.handle_prompt_corrector_mouse(&models, &click(choice_x + 3, list_top), area),
            Some(PromptCorrectorAction::Changed)
        ));
        assert!(view.prompt_corrector_fallbacks.is_empty());
    }

    #[test]
    fn acp_clicks_past_the_row_text_only_move_focus() {
        let mut view = RouterView::new();
        view.set_active_tab(RouterTab::PromptCorrector);
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let choice_x = layout.prompt_choices.x + SIDE_PADDING / 2;
        let list_top = layout.prompt_acp.y + ACP_LIST_TOP_OFFSET;
        let name = view.acp_agents_ordered()[0].name;
        let text_len = format!("  {name}").chars().count() as u16;

        assert!(matches!(
            view.handle_prompt_corrector_mouse(
                &[],
                &click(choice_x + text_len + 3, list_top),
                area,
            ),
            Some(PromptCorrectorAction::Consumed)
        ));
        assert!(matches!(view.focus, FocusTarget::PromptCorrectorAcp));
        assert!(view.prompt_corrector_fallbacks.is_empty());
    }

    #[test]
    fn auto_tab_click_selection_is_bounded_by_the_row_text() {
        // Regression: a click used to apply to the whole row, so clicking
        // the empty space after a model's name added it to the fallback
        // chain — ambiguous with the click-to-switch-focus gesture. A
        // selection must be bound to the exact rendered string; anything
        // past it behaves like a directed Tab (focus moves, nothing else).
        let mut view = RouterView::new();
        // `new()` seeds persisted defaults; the test wants a clean chain.
        view.fallbacks.clear();
        let models = vec![
            ModelEntry {
                provider: "openrouter".into(),
                model: "model-a".into(),
            },
            ModelEntry {
                provider: "groq".into(),
                model: "model-b".into(),
            },
        ];
        let area = Rect::new(0, 0, 100, 40);
        let layout = view.compute_layout(area);
        let text_pad = SIDE_PADDING / 2;
        let list_x = layout.automatic_models.x + text_pad;
        let row_y = layout.automatic_models.y + MODEL_LIST_TOP_OFFSET;
        let text_len = "  openrouter  model-a".chars().count() as u16;

        // Click on the row's empty trailing space: focus moves to the box
        // but the chain stays untouched.
        assert!(view.handle_mouse(&models, &click(list_x + text_len + 2, row_y), area));
        assert!(matches!(view.focus, FocusTarget::Models));
        assert!(view.fallbacks.is_empty());

        // Click on the text itself: the model is selected and added.
        assert!(view.handle_mouse(&models, &click(list_x + 2, row_y), area));
        assert_eq!(view.fallbacks.len(), 1);

        // Same rule for the fallback rows: two entries make the selection
        // index observable. Past the text the click only moves focus; on
        // the text it selects the entry.
        assert!(view.handle_mouse(&models, &click(list_x + 2, row_y + 1), area));
        assert_eq!(view.fallbacks.len(), 2);
        let fb_x = layout.automatic_fallbacks.x + text_pad;
        let fb_y = layout.automatic_fallbacks.y + FALLBACK_LIST_TOP_OFFSET;
        let fb_text_len = "  2.  groq  model-b".chars().count() as u16;

        // Adding moved the selection to the newest entry (row 1); a click
        // past row 1's text must leave that selection untouched.
        assert!(view.handle_mouse(&models, &click(fb_x + fb_text_len + 2, fb_y + 1), area));
        assert!(matches!(view.focus, FocusTarget::Fallbacks));
        assert_eq!(view.selected_fallback, 1);

        // Click on row 0's text selects that entry.
        assert!(view.handle_mouse(&models, &click(fb_x + 2, fb_y), area));
        assert_eq!(view.selected_fallback, 0);
    }

    #[test]
    fn prompt_corrector_enter_toggles_model_without_any_dialog() {
        let mut view = RouterView::new();
        let models = vec![
            ModelEntry {
                provider: "openrouter".into(),
                model: "model-a".into(),
            },
            ModelEntry {
                provider: "groq".into(),
                model: "model-b".into(),
            },
        ];

        view.focus = FocusTarget::PromptCorrectorModels;
        assert!(view.toggle_selected_prompt_corrector_model(&models));
        assert_eq!(view.prompt_corrector_fallbacks.len(), 1);
        // Toggling the same entry again removes it.
        assert!(view.toggle_selected_prompt_corrector_model(&models));
        assert!(view.prompt_corrector_fallbacks.is_empty());

        // ACP agents are ordered installed-first with no filter involved.
        let agents = view.acp_agents_ordered();
        assert_eq!(agents.len(), cosh_tools::subagent::acp::ACP_AGENTS.len());
    }

    #[test]
    fn selected_prompt_corrector_rows_are_removed_with_backspace_behavior() {
        let mut view = RouterView::new();
        view.toggle_prompt_corrector_model("openrouter".into(), "model-a".into());
        view.toggle_prompt_corrector_acp("codex");
        view.focus = FocusTarget::PromptCorrectorFallbacks;

        assert!(view.remove_selected_prompt_corrector());
        assert_eq!(
            view.prompt_corrector_fallbacks,
            vec![PromptCorrectorFallback::Acp {
                agent: "codex".into(),
            }]
        );
    }

    /// Regression: scrolling the correction route's model list must slide
    /// the window (like the auto tab), not park the selector off-screen.
    #[test]
    fn prompt_corrector_model_list_scrolls_with_the_selection() {
        let mut view = RouterView::new();
        let area = Rect::new(0, 0, 100, 40);
        // 30 models, viewport of 5 rows: walking down must move the scroll
        // offset once the selection passes the window edge.
        let models: Vec<ModelEntry> = (0..30)
            .map(|i| ModelEntry {
                provider: format!("p{i}"),
                model: format!("m{i}"),
            })
            .collect();
        view.focus = FocusTarget::PromptCorrectorModels;
        view.set_active_tab(RouterTab::PromptCorrector);

        // Feed the viewport hint exactly as the render does.
        let layout = view.compute_layout(area);
        let max_visible = layout
            .prompt_models
            .height
            .saturating_sub(PROMPT_CORRECTOR_CHOOSER_TOP_OFFSET) as usize;
        view.prompt_model_selection
            .set_visible_count(max_visible.max(1));

        for _ in 0..max_visible {
            view.select_next_prompt_corrector_model(&models);
        }
        assert_eq!(
            view.prompt_model_selection.scroll_offset, 1,
            "selection past the window edge must scroll the list"
        );

        // And the rendered window must actually show the selected row.
        let rendered = render_view(&mut view, area, &models);
        assert!(rendered.contains("🞴"), "the selector must stay visible");
    }
}
