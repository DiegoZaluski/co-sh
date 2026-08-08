use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton as CrosstermMouseButton,
    MouseEvent as CrosstermMouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use cosh::harness::HarnessEvent;
use cosh::harness::context_manager::CompactionEvent;

use crate::component::agent_spinner::AgentSpinner;
use crate::component::prompt::PromptView;
use crate::component::spinner_highlight::HighlightSpinner;
use crate::config::{LlmConfig, TuiConfig};
use crate::fallback;
use crate::keymap::KeyMap;
use crate::logo::{LOGO_CHAT, LOGO_WIDTH};
use crate::routes::add_provider::AddProviderView;
use crate::routes::home::footer::HomeFooterView;
use crate::routes::home::{HomeAction, HomeView};
use crate::routes::router::{FocusTarget, RouterView};
use crate::routes::session::SessionView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::right_panel::{
    RIGHT_PANEL_WIDTH, render_right_panel, should_show_right_panel,
};
use crate::routes::session::sidebar::{SidebarAction, SidebarView};
use crate::routes::tools::InternalToolsView;
use crate::session_store::{
    SessionStore, format_session_timestamp, generate_session_id, is_valid_session,
};
use crate::state::AppState;
use crate::theme::{Theme, ThemeRegistry};
use crate::types::SessionStatus;
use crate::ui::command_palette::CommandPalette;
use crate::ui::dialogs::{DialogAction, DialogState, DialogType};
use crate::ui::toast::ToastState;
use crate::util::selection;

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Render a 10-character budget bar like `▓▓▓▓▓░░░░░` from a 0-100 percentage.
fn render_budget_bar(pct: u8) -> String {
    const FILLED: char = '▓';
    const EMPTY: char = '░';
    const BAR_LEN: usize = 10;
    let filled = (pct as usize * BAR_LEN) / 100;
    let empty = BAR_LEN.saturating_sub(filled);
    std::iter::repeat_n(FILLED, filled)
        .chain(std::iter::repeat_n(EMPTY, empty))
        .collect()
}

const SIDEBAR_WIDTH: u16 = 22;
const FOOTER_HEIGHT: u16 = 1;

/// When the session is empty (no messages), the prompt is centered horizontally
/// with a width of `RATIO * main_area` but at least `MIN_WIDTH` characters wide.
const EMPTY_SESSION_PROMPT_MIN_WIDTH: u16 = 50;
const EMPTY_SESSION_PROMPT_RATIO: f64 = 0.4;

enum AppMode {
    Home,
    Session,
    InternalTools,
    AddProvider,
    Router,
    #[cfg(feature = "embed")]
    Rag,
}

#[allow(clippy::struct_excessive_bools)]
pub struct App {
    pub state: AppState,
    pub theme: Theme,
    pub theme_registry: ThemeRegistry,
    pub session_view: SessionView,
    pub prompt_view: PromptView,
    pub sidebar: SidebarView,
    pub dialog: DialogState,
    pub permission_dialog: PermissionDialog,
    pub question_dialog: QuestionDialog,
    pub home_view: HomeView,
    pub internal_tools_view: InternalToolsView,
    pub show_internal_tools: bool,
    pub add_provider_view: AddProviderView,
    pub show_add_provider: bool,
    pub router_view: RouterView,
    pub show_router: bool,
    #[cfg(feature = "embed")]
    pub rag_view: crate::routes::rag::RagView,
    #[cfg(feature = "embed")]
    pub show_rag: bool,
    pub keymap: KeyMap,
    pub config: TuiConfig,
    pub toast_state: ToastState,
    pub command_palette: CommandPalette,
    pub slash_menu: crate::ui::slash_menu::SlashMenu,
    pub should_quit: bool,
    pub tokio_handle: Handle,
    pub event_tx: mpsc::UnboundedSender<HarnessEvent>,
    event_rx: mpsc::UnboundedReceiver<HarnessEvent>,
    /// Sender for question answers back to the harness.
    answer_tx: mpsc::UnboundedSender<Result<Vec<cosh_tools::question::types::AnswerItem>, String>>,
    /// Sender for permission responses back to the harness.
    perm_tx: mpsc::UnboundedSender<cosh::harness::PermissionAction>,
    llm_config: LlmConfig,
    stop_signal: Arc<AtomicBool>,
    terminal_focused: bool,
    agent_spinner: Option<AgentSpinner>,
    /// Latest context manager info for the budget bar (None if no data yet).
    context_info: Option<cosh::harness::ContextDisplayInfo>,
    /// Stores the theme name that was active when the theme dialog opened (for cancel/restore)
    theme_dialog_original: Option<String>,
    /// Stores the model that was active when the model dialog opened (for cancel/restore)
    model_dialog_original: Option<String>,
    /// Stale-while-revalidate cache for model listings, keyed by provider.
    model_cache: crate::util::cache::StaleCache<String, Vec<cosh::ModelEntry>>,
    /// Generic preferences cache (theme, etc.) persisted as key-value pairs.
    prefs_cache: crate::util::cache::StaleCache<String, String>,
    /// Session persistence store (JSONL files on disk).
    session_store: SessionStore,
    /// When set, the current Confirm dialog is asking about deleting a session.
    pending_delete_session_id: Option<String>,
    /// When set, the current Confirm dialog is asking about deleting a RAG database.
    #[cfg(feature = "embed")]
    pending_delete_db_name: Option<String>,
    /// Handle for the async URL fetch task, aborted on Esc.
    #[cfg(feature = "embed")]
    rag_fetch_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle for the async embed task, aborted on Esc.
    #[cfg(feature = "embed")]
    rag_embed_handle: Option<tokio::task::JoinHandle<()>>,

    // Mouse drag / selection tracking
    /// Position where the mouse was pressed down (for detecting drag selections).
    mouse_down_pos: Option<(u16, u16)>,
    /// Whether a drag-selection is in progress.
    mouse_drag_active: bool,
    /// Visual highlight: anchor (sx,sy) and focus (x,y) — stored without normalisation
    /// so the renderer can apply flow-based selection highlighting (top line from `start_x`
    /// to end, bottom line from start to `end_x`, middle lines fully highlighted).
    drag_selection: Option<(u16, u16, u16, u16)>,

    // Auto-scroll on selection drag
    /// When true, the render loop keeps running even without input events.
    live_requested: bool,
    /// Timestamp of the previous frame (for delta_time calculation).
    last_frame_time: std::time::Instant,
    /// Last known mouse X position (for keyboard scroll targeting).
    last_mouse_x: u16,
    /// Timestamp of last scroll wheel event (for debouncing rapid scrolls).
    last_scroll_time: Instant,
    /// Whether the sidebar is focused to receive scroll events.
    /// Set to true when the user clicks inside the sidebar; false on outside clicks.
    sidebar_focused: bool,
}

impl App {
    pub fn new(cwd: String) -> Self {
        let mut state = AppState::new();
        state.working_directory = cwd;

        // Load session summaries (header-only, lightweight).
        // Full sessions are loaded lazily into the LRU cache on demand.
        let session_store = SessionStore::new();
        state.session_summaries = session_store.list_sessions();

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (answer_tx, _answer_rx) = mpsc::unbounded_channel();
        let (perm_tx, _perm_rx) = mpsc::unbounded_channel();

        let theme_registry = ThemeRegistry::new();
        let prefs_cache = crate::util::cache::StaleCache::new("", "preferences.json");

        // Load saved fallback chain from preferences cache
        let saved_fallbacks = fallback::load_fallbacks(&prefs_cache);

        // Load saved disabled tools from preferences cache
        let saved_disabled_tools = crate::routes::tools::load_disabled_tools(&prefs_cache);

        // Load saved theme from preferences cache, if available
        let saved_theme: Option<String> = prefs_cache.get(&"theme".to_string()).cloned();
        let theme = saved_theme
            .as_deref()
            .and_then(|name| theme_registry.get(name))
            .cloned()
            .unwrap_or_else(|| theme_registry.default_theme().clone());

        Self {
            state,
            theme_registry,
            theme,
            session_view: SessionView::new(),
            home_view: HomeView::new(),
            internal_tools_view: {
                let mut v = InternalToolsView::new();
                v.disabled = saved_disabled_tools;
                v
            },
            show_internal_tools: false,
            add_provider_view: AddProviderView::new(),
            show_add_provider: false,
            router_view: {
                let mut rv = RouterView::new();
                rv.set_fallbacks(saved_fallbacks);
                rv
            },
            show_router: false,
            #[cfg(feature = "embed")]
            rag_view: crate::routes::rag::RagView::new(),
            #[cfg(feature = "embed")]
            show_rag: false,
            prompt_view: PromptView::new(),
            sidebar: SidebarView::new(),
            dialog: DialogState::new(),
            permission_dialog: PermissionDialog::new(),
            question_dialog: QuestionDialog::new(),
            keymap: KeyMap::default_vim(),
            config: TuiConfig::default(),
            toast_state: ToastState::new(),
            command_palette: CommandPalette::new(),
            slash_menu: crate::ui::slash_menu::SlashMenu::new(),
            theme_dialog_original: None,
            model_dialog_original: None,
            model_cache: crate::util::cache::StaleCache::new("cache", "model.json"),
            prefs_cache,
            session_store,
            pending_delete_session_id: None,
            #[cfg(feature = "embed")]
            pending_delete_db_name: None,
            #[cfg(feature = "embed")]
            rag_fetch_handle: None,
            #[cfg(feature = "embed")]
            rag_embed_handle: None,
            should_quit: false,
            tokio_handle: Handle::current(),
            event_tx,
            event_rx,
            answer_tx,
            perm_tx,
            llm_config: LlmConfig::from_env(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            terminal_focused: true,
            agent_spinner: None,
            context_info: None,
            mouse_down_pos: None,
            mouse_drag_active: false,
            drag_selection: None,
            live_requested: false,
            last_frame_time: std::time::Instant::now(),
            last_mouse_x: 0,
            last_scroll_time: Instant::now(),
            sidebar_focused: false,
        }
    }

    pub fn show_welcome_toast(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some("cosh".to_string()),
            message: "Welcome! Press Ctrl+P for commands.".to_string(),
            variant: ToastVariant::Info,
            duration_ms: 5000,
        });
    }

    fn open_theme_dialog(&mut self) {
        let mut themes: Vec<String> = self
            .theme_registry
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        themes.sort_by_key(|a| a.to_lowercase());

        let current = self
            .theme_registry
            .names()
            .iter()
            .find(|&&name| self.theme_registry.get(name) == Some(&self.theme))
            .map_or_else(|| "opencode".to_string(), |&s| s.to_string());

        // Store the theme name so we can restore on cancel
        self.theme_dialog_original = Some(current.clone());

        self.dialog.replace(DialogType::ThemeList {
            themes,
            current,
            filter: String::new(),
        });
    }

    fn open_model_dialog(&mut self) {
        use cosh::ModelEntry;
        use cosh_sdk::connector::Connector;
        use cosh_sdk::connector::known_providers_with_env;

        let current = self.llm_config.model.clone().unwrap_or_default();

        // Store the current model so we can restore on cancel
        self.model_dialog_original = Some(current.clone());

        // Collect providers and check if their API key is still configured.
        // If a provider's env var is missing, invalidate its cache entry so
        // stale models don't appear as available options.
        let providers_to_check: Vec<&str> = {
            let mut active = Vec::new();
            for (provider, env_var) in known_providers_with_env() {
                if std::env::var(env_var).is_ok() {
                    active.push(provider);
                } else {
                    // Provider no longer configured — purge cached models
                    self.model_cache.invalidate(&provider.to_string());
                }
            }
            active
        };

        // Try to populate the dialog from cache first (instant, no network)
        let mut cached_models: Vec<ModelEntry> = Vec::new();
        for &provider in &providers_to_check {
            if let Some(models) = self.model_cache.get(&provider.to_string()) {
                cached_models.extend(models.iter().cloned());
            }
        }
        // Deduplicate cached models as a safety net — the cache should
        // already be unique after update_model_cache runs, but this
        // protects against stale on-disk data from older versions.
        {
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            cached_models.retain(|m| seen.insert((m.provider.clone(), m.model.clone())));
        }

        let auto_entry = ModelEntry {
            provider: String::new(),
            model: "auto".to_string(),
        };
        let auto_current = if current == "auto" {
            current.clone()
        } else {
            String::new()
        };
        if !cached_models.is_empty() {
            let mut models_with_auto = vec![auto_entry];
            models_with_auto.extend(cached_models);
            self.dialog.replace(DialogType::ModelList {
                models: models_with_auto,
                current: auto_current,
                filter: String::new(),
            });
        } else {
            self.dialog.replace(DialogType::ModelList {
                models: vec![auto_entry],
                current: auto_current,
                filter: String::new(),
            });
        }

        // Check if any provider is already being revalidated — if so,
        // a background fetch is already in progress, skip spawning another.
        let any_revalidating = providers_to_check
            .iter()
            .any(|p| self.model_cache.is_revalidating(&p.to_string()));

        if !any_revalidating {
            // Mark all providers as revalidating to prevent redundant fetches
            for &provider in &providers_to_check {
                self.model_cache.start_revalidation(provider.to_string());
            }

            // Always revalidate in background (stale-while-revalidate)
            let dialog_tx = self.event_tx.clone();
            let dialog_tx_clone = dialog_tx;

            self.tokio_handle.spawn(async move {
                let mut all_models: Vec<ModelEntry> = Vec::new();

                for provider in providers_to_check {
                    if let Ok(connector) = Connector::new(provider)
                        && let Ok(output) = connector.list_models().await
                    {
                        for model_info in output.models() {
                            all_models.push(ModelEntry {
                                provider: provider.to_string(),
                                model: model_info.id().to_string(),
                            });
                        }
                    }
                }

                let _ = dialog_tx_clone.send(HarnessEvent::ModelsLoaded {
                    models: all_models,
                    current,
                });
            });
        }
    }

    /// Group models by provider and update the cache for each provider.
    fn update_model_cache(&mut self, models: &[cosh::ModelEntry]) {
        use cosh::ModelEntry;
        use std::collections::HashMap;
        use std::collections::HashSet;

        // Group by provider
        let mut grouped: HashMap<&str, Vec<ModelEntry>> = HashMap::new();
        for entry in models {
            grouped
                .entry(entry.provider.as_str())
                .or_default()
                .push(entry.clone());
        }

        // Deduplicate models per provider: some APIs may return the same
        // model ID multiple times (Mistral, transient API glitches, etc.).
        // Using a per-provider HashSet avoids O(n²) on each Vec.
        for provider_models in grouped.values_mut() {
            let mut seen: HashSet<String> = HashSet::new();
            provider_models.retain(|m| seen.insert(m.model.clone()));
        }

        // Update cache for each provider with results
        for (provider, provider_models) in grouped {
            self.model_cache
                .finish_revalidation(provider.to_string(), provider_models);
        }

        // Providers still in the revalidation set failed or returned nothing
        // (expired API key, network error, etc.). Invalidate their cache so
        // stale models don't appear as available options.
        let failed: Vec<String> = self.model_cache.drain_revalidation();
        for provider in &failed {
            self.model_cache.invalidate(provider);
        }
    }

    fn is_theme_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ThemeList { .. })
            )
    }

    fn is_model_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ModelList { .. })
            )
    }

    fn handle_theme_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_theme_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Compute filtered indices and move selection up
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty()
                    && let Some(d) = self.dialog.current_mut()
                {
                    d.selected = if d.selected == 0 {
                        filtered.len() - 1
                    } else {
                        d.selected.saturating_sub(1)
                    };
                    // Preview theme on move
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Down => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty()
                    && let Some(d) = self.dialog.current_mut()
                {
                    d.selected = (d.selected + 1).min(filtered.len() - 1);
                    // Preview theme on move
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Enter => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty() {
                    let name = filtered[self
                        .dialog
                        .current()
                        .map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)))]
                    .clone();
                    if let Some(t) = self.theme_registry.get(&name) {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                    // Persist theme choice so it survives restarts
                    self.prefs_cache
                        .finish_revalidation("theme".to_string(), name);
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Restore original theme
                if let Some(ref orig) = self.theme_dialog_original
                    && let Some(t) = self.theme_registry.get(orig)
                {
                    self.theme = t.clone();
                    self.config.theme_gen += 1;
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                let is_empty = {
                    let Some(d) = self.dialog.current_mut() else {
                        return true;
                    };
                    let DialogType::ThemeList { filter, .. } = &mut d.dialog_type else {
                        return true;
                    };
                    filter.pop();
                    d.selected = 0;
                    d.cursor.note_activity();
                    filter.is_empty()
                };
                if is_empty {
                    // Restore original theme when filter becomes empty (matches opencode)
                    if let Some(ref orig) = self.theme_dialog_original
                        && let Some(t) = self.theme_registry.get(orig)
                    {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                } else {
                    self.apply_filtered_theme_preview();
                }
                true
            }
            KeyCode::Char(ch) => {
                self.theme_dialog_push_filter(ch);
                true
            }
            _ => false,
        }
    }

    /// Get the list of filtered theme names from the current dialog
    fn theme_dialog_filtered(&self) -> Vec<String> {
        self.dialog.current().map_or(Vec::new(), |d| {
            if let DialogType::ThemeList { themes, filter, .. } = &d.dialog_type {
                if filter.is_empty() {
                    themes.clone()
                } else {
                    let lower = filter.to_lowercase();
                    themes
                        .iter()
                        .filter(|t| t.to_lowercase().contains(&lower))
                        .cloned()
                        .collect()
                }
            } else {
                Vec::new()
            }
        })
    }

    /// Preview the currently selected theme from the filtered list
    fn apply_filtered_theme_preview(&mut self) {
        let filtered = self.theme_dialog_filtered();
        let sel = self
            .dialog
            .current()
            .map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)));
        if sel < filtered.len()
            && let Some(t) = self.theme_registry.get(&filtered[sel])
        {
            self.theme = t.clone();
            self.config.theme_gen += 1;
        }
    }

    /// Add a character to the theme filter and reset selection
    fn theme_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ThemeList { filter, .. } = &mut d.dialog_type {
                filter.push(ch);
            }
            d.selected = 0;
            d.cursor.note_activity();
        }
        self.apply_filtered_theme_preview();
    }

    fn is_confirm_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::Confirm { .. })
            )
    }

    fn is_shortcuts_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::Shortcuts { .. })
            )
    }

    fn is_apikey_input_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ApiKeyInput { .. })
            )
    }

    fn handle_apikey_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_apikey_input_visible() {
            return false;
        }

        // Update blink timestamps on any interaction
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }

        match key {
            KeyCode::Enter => {
                let should_save = self.dialog.current().is_some_and(|d| {
                    if let DialogType::ApiKeyInput { input, .. } = &d.dialog_type {
                        !input.is_empty()
                    } else {
                        false
                    }
                });
                if should_save
                    && let Some(d) = self.dialog.current()
                    && let DialogType::ApiKeyInput {
                        provider,
                        env_var,
                        input,
                        ..
                    } = &d.dialog_type
                {
                    save_provider_api_key(provider, env_var, input);
                    // Invalidate model cache for this provider so the next
                    // dialog open fetches fresh models with the new key.
                    self.model_cache.invalidate(&provider.to_string());
                    // SAFETY: Setting env vars is safe in a single-threaded CLI context
                    unsafe {
                        std::env::set_var(env_var, input);
                    }
                }
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Left => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput { cursor_pos, .. } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    *cursor_pos -= 1;
                }
                true
            }
            KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos < input.len()
                {
                    *cursor_pos += 1;
                }
                true
            }
            KeyCode::Home => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput { cursor_pos, .. } = &mut d.dialog_type
                {
                    *cursor_pos = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                {
                    *cursor_pos = input.len();
                }
                true
            }
            KeyCode::Delete => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos < input.len()
                {
                    let next = input.floor_char_boundary(*cursor_pos + 1).min(input.len());
                    input.drain(*cursor_pos..next);
                }
                true
            }
            KeyCode::Backspace => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    let char_start = input.floor_char_boundary(*cursor_pos - 1);
                    input.remove(char_start);
                    *cursor_pos = char_start;
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    } = &mut d.dialog_type
                {
                    input.insert(*cursor_pos, ch);
                    *cursor_pos += ch.len_utf8();
                }
                true
            }
            _ => false,
        }
    }

    fn handle_confirm_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_confirm_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Left | KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected ^= 1;
                }
                true
            }
            _ => false,
        }
    }

    fn handle_model_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_model_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Build flat entries in the same grouped-by-provider order as the render
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    let new_selected = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if flat_entries.is_empty() {
                            None
                        } else {
                            let old = d.selected;
                            let next = if old == 0 || old >= flat_entries.len() {
                                flat_entries.len() - 1
                            } else {
                                old - 1
                            };
                            Some(next)
                        }
                    };
                    if let Some(ns) = new_selected
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        d_mut.selected = ns;
                    }
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    // Compute new selection in a separate scope so the borrow drops
                    // before calling current_mut().
                    let new_selected = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if flat_entries.is_empty() {
                            None
                        } else {
                            let old = d.selected;
                            let next = if old + 1 >= flat_entries.len() {
                                0
                            } else {
                                old + 1
                            };
                            Some(next)
                        }
                    };
                    if let Some(ns) = new_selected
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        d_mut.selected = ns;
                    }
                }
                true
            }
            KeyCode::Enter => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                    && !models.is_empty()
                {
                    let selection = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if !flat_entries.is_empty() {
                            let selected_idx = d.selected.min(flat_entries.len().saturating_sub(1));
                            let entry = flat_entries[selected_idx];
                            Some((entry.model.clone(), entry.provider.clone()))
                        } else {
                            None
                        }
                    };
                    if let Some((model, provider)) = selection {
                        if model == "auto" {
                            self.llm_config.model = Some("auto".to_string());
                            self.llm_config.provider = String::new();
                        } else {
                            self.llm_config.model = Some(model);
                            self.llm_config.provider = provider;
                        }
                    }
                }
                self.model_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Restore original model
                if let Some(ref orig) = self.model_dialog_original {
                    self.llm_config.model = if orig.is_empty() {
                        None
                    } else {
                        Some(orig.clone())
                    };
                }
                self.model_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                let Some(d) = self.dialog.current_mut() else {
                    return true;
                };
                let DialogType::ModelList { filter, .. } = &mut d.dialog_type else {
                    return true;
                };
                filter.pop();
                d.selected = 0;
                d.cursor.note_activity();
                true
            }
            KeyCode::Char(ch) => {
                self.model_dialog_push_filter(ch);
                true
            }
            _ => false,
        }
    }

    /// Add a character to the model filter and reset selection
    fn model_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ModelList { filter, .. } = &mut d.dialog_type {
                filter.push(ch);
            }
            d.selected = 0;
            d.cursor.note_activity();
        }
    }

    fn collect_cached_models(&self) -> Vec<cosh::ModelEntry> {
        let mut models = Vec::new();
        for (provider, _) in cosh_sdk::connector::known_providers_with_env() {
            if std::env::var(cosh_sdk::connector::get_provider_env_var(provider).unwrap_or(""))
                .is_ok()
                && let Some(cached) = self.model_cache.get(&provider.to_string())
            {
                models.extend(cached.iter().cloned());
            }
        }
        models
    }

    /// Compute the flat list of models in the same grouped-by-provider order
    /// used by the dialog render, so navigation and rendering stay in sync.
    fn model_dialog_flat_entries<'a>(
        models: &'a [cosh::ModelEntry],
        filter: &str,
    ) -> Vec<&'a cosh::ModelEntry> {
        use std::collections::BTreeMap;
        let mut grouped: BTreeMap<String, Vec<&'a cosh::ModelEntry>> = BTreeMap::new();
        for entry in models {
            if filter.is_empty() || entry.model.to_lowercase().contains(&filter.to_lowercase()) {
                grouped
                    .entry(entry.provider.clone())
                    .or_default()
                    .push(entry);
            }
        }
        grouped.values().flatten().copied().collect()
    }

    // RAG helper methods (cfg-gated at method level, always compiles)
    #[cfg(feature = "embed")]
    fn is_rag_mode(&self) -> bool {
        matches!(self.mode(), AppMode::Rag)
    }

    #[cfg(not(feature = "embed"))]
    fn is_rag_mode(&self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn rag_spinner_active(&self) -> bool {
        self.rag_view.is_spinner_active()
    }

    #[cfg(not(feature = "embed"))]
    fn rag_spinner_active(&self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn render_rag_view(&mut self, buf: &mut ratatui::buffer::Buffer, session_area: Rect) {
        self.prompt_view.blur();
        self.rag_view.advance_spinner();
        // Show toast when embed completes
        if self.rag_view.take_embed_completed() {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("RAG".into()),
                message: "Content embedded successfully.".into(),
                variant: ToastVariant::Success,
                duration_ms: 4000,
            });
        }
        if let Some(err) = self.rag_view.take_embed_error() {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("RAG".into()),
                message: err,
                variant: ToastVariant::Error,
                duration_ms: 6000,
            });
        }
        let tools_area = Rect::new(
            session_area.x,
            session_area.y,
            session_area.width,
            session_area.height.saturating_sub(1),
        );
        self.rag_view.render(buf, tools_area, &self.theme);
    }

    #[cfg(not(feature = "embed"))]
    fn render_rag_view(&mut self, _buf: &mut ratatui::buffer::Buffer, _session_area: Rect) {}

    #[cfg(feature = "embed")]
    fn handle_rag_confirm_delete(&mut self) -> bool {
        if let Some(db_name) = self.pending_delete_db_name.take() {
            self.rag_view.registry.remove(&db_name);
            self.rag_view.active_dbs.remove(&db_name);
            crate::routes::rag::registry::RagRegistry::save_active(&self.rag_view.active_dbs);
            if self.rag_view.selected_db_for_embed.as_deref() == Some(&db_name) {
                self.rag_view.selected_db_for_embed = None;
            }
            self.dialog.pop();
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    fn handle_rag_confirm_delete(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn clear_rag_pending_state(&mut self) {
        self.pending_delete_db_name = None;
    }

    #[cfg(not(feature = "embed"))]
    fn clear_rag_pending_state(&mut self) {}

    #[cfg(feature = "embed")]
    fn handle_rag_cancel_action(&mut self) {
        if matches!(self.mode(), AppMode::Rag) {
            self.show_rag = false;
        }
    }

    #[cfg(not(feature = "embed"))]
    fn handle_rag_cancel_action(&mut self) {}

    #[cfg(feature = "embed")]
    fn recall_suffix(&self) -> String {
        self.rag_view
            .registry
            .build_tool_suffix(self.rag_view.active_dbs())
    }

    #[cfg(not(feature = "embed"))]
    fn recall_suffix(&self) -> String {
        String::new()
    }

    #[cfg(feature = "embed")]
    fn maybe_disable_recall_tool(&self, disabled: &mut std::collections::HashSet<String>) {
        if self.rag_view.registry.dbs.is_empty() {
            disabled.insert("recall_search".into());
        }
    }

    #[cfg(not(feature = "embed"))]
    fn maybe_disable_recall_tool(&self, _disabled: &mut std::collections::HashSet<String>) {}

    #[cfg(feature = "embed")]
    fn recall_dbs_vec(&self) -> Vec<cosh::harness::tools::RecallDb> {
        use cosh::harness::tools::{RecallDb, RecallEmbedderConfig};
        self.rag_view
            .registry
            .dbs
            .iter()
            .filter(|db| self.rag_view.active_dbs.contains(&db.name))
            .map(|db| RecallDb {
                name: db.name.clone(),
                uri: db.uri.clone(),
                table_name: db.name.clone(),
                embedder: match &db.embedder {
                    crate::routes::rag::models::EmbedderConfig::Local { model } => {
                        RecallEmbedderConfig::Local {
                            model_name: serde_json::to_value(model)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default(),
                        }
                    }
                    crate::routes::rag::models::EmbedderConfig::Cloud(c) => {
                        RecallEmbedderConfig::Cloud {
                            provider: c.provider.clone(),
                            model: c.model.clone(),
                            dim: db.embedder.vector_dim(),
                        }
                    }
                },
            })
            .collect()
    }

    #[cfg(feature = "embed")]
    fn handle_rag_key_event(&mut self, key: KeyCode) -> bool {
        if self.is_rag_mode() && !self.dialog.visible() {
            use crate::routes::rag::RagAction;
            match self.rag_view.handle_key(key) {
                Some(RagAction::Back) => {
                    // Abort any in-flight async operations so the app doesn't
                    // accumulate stale background tasks after leaving RAG mode.
                    if let Some(h) = self.rag_fetch_handle.take() {
                        h.abort();
                        log::debug!("[tui_rag_app] Aborted fetch task on Esc");
                    }
                    if let Some(h) = self.rag_embed_handle.take() {
                        h.abort();
                        log::debug!("[tui_rag_app] Aborted embed task on Esc");
                    }
                    self.rag_view.fetch_rx = None;
                    self.rag_view.embed_rx = None;
                    self.show_rag = false;
                }
                Some(RagAction::ClosePreview) => {}
                Some(RagAction::FetchUrlOrPath(input)) => {
                    self.rag_view.start_fetch(&input);
                    if input.starts_with("http://") || input.starts_with("https://") {
                        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
                        let url = input.clone();
                        let url_for_log = url.clone();
                        let handle = self.tokio_handle.spawn(async move {
                            use cosh_tools::web::{WebFetch, fetch as web_fetch_fn};
                            let fetch_input = WebFetch { url };
                            let result = web_fetch_fn(&fetch_input).await;
                            let _ = tx.send(result);
                        });
                        self.rag_fetch_handle = Some(handle);
                        log::debug!("[tui_rag_app] Spawned fetch task for URL: {url_for_log}");
                        self.rag_view.set_fetch_rx(rx);
                    } else {
                        match std::fs::read_to_string(&input) {
                            Ok(content) => self.rag_view.content_fetched(content),
                            Err(_) => self.rag_view.set_error(),
                        }
                    }
                }
                Some(RagAction::CreateDb {
                    name,
                    description,
                    embedder,
                }) => {
                    use crate::routes::rag::models::RagDb;
                    let db = RagDb {
                        name: name.clone(),
                        uri: crate::routes::rag::registry::RagRegistry::db_uri(&name),
                        description: description.clone(),
                        embedder,
                        created_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    };
                    let mut registry = crate::routes::rag::registry::RagRegistry::load();
                    registry.upsert(db);
                    self.rag_view.selected_db_for_embed = Some(name.clone());
                    self.rag_view.active_dbs.insert(name.clone());
                    crate::routes::rag::registry::RagRegistry::save_active(
                        &self.rag_view.active_dbs,
                    );
                    self.rag_view.registry = crate::routes::rag::registry::RagRegistry::load();
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("Database Created".into()),
                        message: format!("'{name}' is now selected for embedding."),
                        variant: ToastVariant::Success,
                        duration_ms: 4000,
                    });
                }
                Some(RagAction::ShowWarning(msg)) => {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("RAG".into()),
                        message: msg,
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                }
                Some(RagAction::EmbedContent {
                    content,
                    db_name,
                    db_description: _,
                    model: _,
                }) => {
                    // Look up the DB from the registry to get its URI and embedder config
                    let db_entry = self.rag_view.registry.find(&db_name).cloned();
                    if let Some(db) = db_entry {
                        self.rag_view.start_embedding();
                        let uri = db.uri.clone();
                        let table_name = db.name.clone();
                        let embedder_config = db.embedder.clone();
                        let embed_content = content.clone();
                        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
                        let handle = self.tokio_handle.spawn(async move {
                            let result =
                                embed_document(&uri, &table_name, &embedder_config, &embed_content)
                                    .await;
                            let _ = tx.send(result);
                        });
                        self.rag_embed_handle = Some(handle);
                        log::debug!("[tui_rag_app] Spawned embed task for DB: {db_name}");
                        self.rag_view.set_embed_rx(rx);
                    } else {
                        use crate::ui::toast::{ToastOptions, ToastVariant};
                        self.toast_state.show(ToastOptions {
                            title: Some("RAG".into()),
                            message: format!("Database '{db_name}' not found."),
                            variant: ToastVariant::Error,
                            duration_ms: 4000,
                        });
                    }
                }
                _ => {}
            }
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    fn handle_rag_key_event(&mut self, _key: KeyCode) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn handle_rag_paste(&mut self, text: &str) {
        self.rag_view.handle_paste(text);
    }

    #[cfg(not(feature = "embed"))]
    fn handle_rag_paste(&mut self, _text: &str) {}

    #[cfg(feature = "embed")]
    fn try_rag_scroll_up(&mut self) -> bool {
        if !self.dialog.visible() {
            self.rag_view.handle_key(KeyCode::Up);
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    fn try_rag_scroll_up(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn try_rag_scroll_down(&mut self) -> bool {
        if !self.dialog.visible() {
            self.rag_view.handle_key(KeyCode::Down);
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    fn try_rag_scroll_down(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    fn handle_rag_mouse_click(&mut self, mouse: &MouseEvent) -> bool {
        let area = self.terminal_size();
        let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
        let main_area = Rect::new(
            area.x + sidebar_w,
            area.y,
            area.width.saturating_sub(sidebar_w),
            area.height,
        );
        let tools_area = Rect::new(
            main_area.x,
            area.y + 1,
            main_area.width,
            main_area.height.saturating_sub(4),
        );
        // DB picker row click (select DB for embed)
        if self.rag_view.show_db_picker {
            if let Some(picker_idx) = self.rag_view.is_db_picker_row_click(mouse, tools_area) {
                let pick_name = self
                    .rag_view
                    .filtered_dbs()
                    .get(picker_idx)
                    .map(|db| db.name.clone());
                if let Some(ref name) = pick_name {
                    self.rag_view.select_db_for_embed(name);
                }
                return true;
            }
            if !self.rag_view.is_click_inside_db_picker(mouse, tools_area) {
                self.rag_view.close_db_picker();
                return true;
            }
            return true;
        }
        // "Select Database" button click
        if self.rag_view.is_select_db_click(mouse, tools_area) {
            self.rag_view.toggle_db_picker();
            return true;
        }
        // "show desc" button click
        if let Some(desc_idx) = self.rag_view.is_show_desc_click(mouse, tools_area) {
            self.rag_view.toggle_desc_popup(desc_idx);
            return true;
        }
        // delete button click
        if let Some(db_name) = self.rag_view.is_delete_click(mouse, tools_area) {
            self.pending_delete_db_name = Some(db_name);
            self.dialog.show(DialogType::Confirm {
                message: "Delete this database?".into(),
            });
            if let Some(d) = self.dialog.current_mut() {
                d.selected = 1;
            }
            return true;
        }
        // toggle DB active
        if let Some(clicked_idx) = self.rag_view.handle_mouse(mouse, tools_area) {
            self.rag_view.toggle_db(clicked_idx);
            return true;
        }
        // Create DB button
        if self.rag_view.is_create_click(mouse, tools_area) {
            self.rag_view.toggle_create_db();
            return true;
        }
        // Model line toggle
        if self.rag_view.is_model_click(mouse, tools_area) {
            self.rag_view.toggle_models_expanded();
            return true;
        }
        // Click outside preview
        if self.rag_view.is_preview_visible()
            && self.rag_view.is_click_outside_preview(mouse, tools_area)
        {
            self.rag_view.close_preview();
            return true;
        }
        // Click outside description popup
        if self.rag_view.show_desc_for_db.is_some()
            && self.rag_view.is_click_outside_desc_popup(mouse, tools_area)
        {
            self.rag_view.close_desc_popup();
            return true;
        }
        // Create DB form field click
        if self.rag_view.show_create_db
            && self
                .rag_view
                .handle_create_db_field_click(mouse, tools_area)
        {
            return true;
        }
        // Dismiss Create DB form
        if self.rag_view.is_dismiss_click(mouse, tools_area) {
            self.rag_view.close_form();
            return true;
        }
        // Click on the URL input box → position cursor at the click location
        {
            let inner_w = tools_area.width.saturating_sub(4);
            let input_w = inner_w.saturating_sub(4);
            let input_h = self.rag_view.url_input.height(input_w);
            let input_x = tools_area.x + 4; // = layout.cx
            let input_y = tools_area.y + 2;
            let mx = mouse.x;
            let my = mouse.y;
            if my >= input_y && my < input_y + input_h && mx >= input_x && mx < input_x + input_w {
                self.rag_view.url_input.focus();
                if let Some(pos) = self.rag_view.url_input.char_pos_at_mouse(
                    mx,
                    my,
                    Rect::new(input_x, input_y, input_w, input_h),
                ) {
                    self.rag_view.url_input.cursor_pos = pos;
                    self.rag_view.url_input.cursor.note_activity();
                }
                return true;
            }
        }

        false
    }

    #[cfg(not(feature = "embed"))]
    fn handle_rag_mouse_click(&mut self, _mouse: &MouseEvent) -> bool {
        false
    }

    fn mode(&self) -> AppMode {
        #[cfg(feature = "embed")]
        if self.show_rag {
            return AppMode::Rag;
        }
        if self.show_router {
            AppMode::Router
        } else if self.show_internal_tools {
            AppMode::InternalTools
        } else if self.show_add_provider {
            AppMode::AddProvider
        } else if self.state.current_session().is_some() {
            AppMode::Session
        } else {
            AppMode::Home
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        let mut terminal = init_terminal()?;
        // Target 30 fps during streaming to give agents time to produce tokens
        // before we spend cycles re-rendering (reduces jank, lowers CPU usage).
        let frame_interval = Duration::from_micros(33_333); // ~30 fps

        while !self.should_quit {
            // ESC sovereign: pre-render event check
            // During streaming, terminal.draw() can take hundreds of milliseconds.
            // Do a quick non-blocking poll for pending events BEFORE spending time
            // on rendering. If events are available, handle them via the full
            // handle_events() path (which is fast because event::poll() returns
            // immediately when events are already buffered). This ensures ESC and
            // other critical keys are processed with minimal latency.
            if self.live_requested && event::poll(Duration::from_millis(0))? {
                if self.handle_events()? {
                    break;
                }
                // If stop was requested, skip this render to respond instantly.
                if self.stop_signal.load(Ordering::Relaxed) {
                    self.poll_events();
                    continue;
                }
            }

            // Frame-rate limiting during streaming
            // Skip rendering if not enough time has elapsed. This reduces CPU usage
            // and prevents jitter from rendering too frequently (which would compete
            // with the agent's token production). Events are still polled.
            if self.live_requested {
                let elapsed = self.last_frame_time.elapsed();
                if elapsed < frame_interval {
                    let wait = frame_interval.saturating_sub(elapsed);
                    // Still poll events while waiting (non-blocking)
                    if event::poll(Duration::from_millis(0))? && self.handle_events()? {
                        break;
                    }
                    // If stop was requested, drain events and skip render
                    if self.stop_signal.load(Ordering::Relaxed) {
                        self.poll_events();
                        continue;
                    }
                    // Sleep for the remaining frame interval
                    std::thread::sleep(wait);
                }
            }

            let now = std::time::Instant::now();
            let delta = now.duration_since(self.last_frame_time);
            self.last_frame_time = now;
            let delta_secs = delta.as_secs_f64();

            terminal.draw(|frame| {
                self.render(frame, delta_secs);
            })?;

            if self.live_requested {
                // When auto-scroll is active, don't block on event::poll.
                if event::poll(Duration::from_millis(8))? && self.handle_events()? {
                    break;
                }
            } else if self.handle_events()? {
                break;
            }

            self.poll_events();
        }

        restore_terminal()?;
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame<'_>, delta_time: f64) {
        // Sync live_requested — keeps the render loop running smoothly.
        // Session: during streaming, sticky scroll needs continuous re-rendering.
        // RAG: when the spinner is active (fetching/embedding), enable live mode
        //      so event::poll uses 8ms instead of 50ms, keeping animation smooth.
        // Tool spinners: keep live mode while any tool spinner is active/finishing
        // so the beam sweep animation advances every frame, even when the session
        // is idle (no streaming).
        let has_active_spinner = self
            .session_view
            .tool_state
            .tool_spinners
            .iter()
            .any(|(_, s)| !s.is_idle());
        let mut live = self.session_view.is_auto_scrolling
            || (self.state.status == crate::types::SessionStatus::Working
                && self.session_view.is_sticky_bottom);
        live = live || self.rag_spinner_active();
        live = live || has_active_spinner;
        // A running compaction stopwatch must tick every frame.
        live = live || self.has_running_compaction();
        self.live_requested = live;
        let area = frame.area();

        {
            let buf = frame.buffer_mut();

            let bg_color = rgba_color(self.theme.background);
            let _bg_start = Instant::now();
            // Optimized background fill: set char + style per cell, but
            // skip the CellDiffOption::None that the original code had.
            // The default CellDiffOption::Update detects char/style
            // changes correctly, reducing overhead by ~33%.
            let empty_style = Style::default().bg(bg_color);
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol(" ");
                        cell.set_style(empty_style);
                    }
                }
            }
            let _bg_us = _bg_start.elapsed().as_micros();
            if _bg_us > 200 {
                log::debug!(
                    "[PERF] bg_fill: {_bg_us}us area={}x{}",
                    area.width,
                    area.height
                );
            }

            let header_style = Style::default().fg(rgba_color(self.theme.text_muted));
            let title_chars: Vec<char> = "cosh".chars().collect();
            for (i, ch) in title_chars.iter().enumerate() {
                if let Some(cell) = buf.cell_mut((area.x + 1 + i as u16, area.y)) {
                    cell.set_char(*ch);
                    cell.set_style(header_style);
                }
            }

            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };

            let right_panel_w = if matches!(self.mode(), AppMode::Session)
                && (should_show_right_panel(area.width, &self.state.right_panel))
            {
                RIGHT_PANEL_WIDTH
            } else {
                0
            };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w + right_panel_w),
                area.height,
            );

            // Context info bar — only after the user has sent at least one
            // message. Shows the live token count to the left of the budget bar.
            let has_content = self
                .state
                .current_session()
                .is_some_and(|s| !s.messages.is_empty());
            if matches!(self.mode(), AppMode::Session) && has_content {
                let pct = self.context_info.as_ref().map_or(0, |info| info.budget_pct);
                let tokens = self
                    .context_info
                    .as_ref()
                    .map_or(0, |info| info.total_tokens);

                let token_str = format!("{tokens} tok");
                let budget_str = format!("{}{:>3}%", render_budget_bar(pct), pct);
                let gap: u16 = 2;

                let display_w =
                    (token_str.chars().count() + gap as usize + budget_str.chars().count()) as u16;
                let right_x = main_area.right().saturating_sub(display_w + 1);

                // Token counter — to the left of the budget bar.
                for (i, ch) in token_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((right_x + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(rgba_color(self.theme.text_muted)));
                    }
                }

                // Budget bar (with conditional color)
                let offset = token_str.chars().count() as u16 + gap;
                let budget_style = if pct >= 90 {
                    Style::default().fg(rgba_color(self.theme.error))
                } else if pct >= 70 {
                    Style::default().fg(rgba_color(self.theme.warning))
                } else {
                    Style::default().fg(rgba_color(self.theme.text_muted))
                };
                for (i, ch) in budget_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((right_x + offset + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(budget_style);
                    }
                }
            }

            // Right panel (independent of sidebar state)
            if right_panel_w > 0 {
                render_right_panel(
                    buf,
                    Rect::new(
                        area.right().saturating_sub(right_panel_w),
                        area.y,
                        right_panel_w,
                        area.height,
                    ),
                    &mut self.state.right_panel,
                    &self.theme,
                    area.width,
                );
            }

            if self.sidebar.open {
                self.sidebar.render(
                    buf,
                    Rect::new(area.x, area.y, sidebar_w, area.height),
                    &self.state,
                    &self.theme,
                );
            }

            let footer_y = main_area.bottom().saturating_sub(1);
            let is_session = matches!(self.mode(), AppMode::Session);

            // When question or permission dialog is visible, hide prompt and spinner (like OpenCode)
            let hide_prompt_and_spinner =
                is_session && (self.question_dialog.visible || self.permission_dialog.visible);

            // Detect empty session — no messages yet (like OpenCode initial state)
            let is_empty_session = is_session
                && !hide_prompt_and_spinner
                && self
                    .state
                    .current_session()
                    .is_none_or(|s| s.messages.is_empty());

            let full_w = main_area.width.saturating_sub(4);
            let (prompt_area_x, prompt_area_w) = if is_empty_session {
                let narrow = std::cmp::max(
                    EMPTY_SESSION_PROMPT_MIN_WIDTH,
                    (main_area.width as f64 * EMPTY_SESSION_PROMPT_RATIO) as u16,
                )
                .min(full_w);
                (main_area.x + (main_area.width - narrow) / 2, narrow)
            } else {
                (main_area.x + 2, full_w)
            };

            let prompt_h = if is_session && !hide_prompt_and_spinner {
                self.prompt_view.required_height(prompt_area_w)
            } else {
                0
            };

            // Question dialog inline (between messages and prompt), only during session
            let question_h = if is_session && self.question_dialog.visible {
                self.question_dialog
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            // Permission dialog (same position as question, mutually exclusive)
            let permission_h = if is_session && self.permission_dialog.visible {
                self.permission_dialog
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };

            // Logo block: logo (6 rows) + gap before prompt (1)
            let logo_block_h = if is_empty_session {
                LOGO_CHAT.len() as u16 + 1
            } else {
                0
            };

            let (prompt_area_y, logo_start_y) = if is_empty_session && prompt_h > 0 {
                let header_y = area.y + 1;
                let total_block_h = logo_block_h + prompt_h;
                let available = footer_y.saturating_sub(header_y);
                let top_spacer = available.saturating_sub(total_block_h) / 2;
                let start_y = header_y + top_spacer;
                (start_y + logo_block_h, start_y)
            } else {
                (footer_y.saturating_sub(prompt_h), 0)
            };

            // Spinner line (1 row when the agent loop is active, hidden when questions are visible)
            let spinner_h = u16::from(
                is_session
                    && !hide_prompt_and_spinner
                    && self.state.status == crate::types::SessionStatus::Working
                    && self.agent_spinner.is_some(),
            );
            let spinner_area_y = prompt_area_y.saturating_sub(spinner_h);

            // Question dialog inline (between messages and spinner), only during session
            let question_area_y = spinner_area_y.saturating_sub(question_h);
            let permission_area_y = spinner_area_y.saturating_sub(permission_h);
            let prompt_padding: u16 = 1;
            // session bottom is below whichever dialog is visible (mutually exclusive, never both)
            let session_bottom = question_area_y
                .min(permission_area_y)
                .saturating_sub(prompt_padding);

            let prompt_area = Rect::new(prompt_area_x, prompt_area_y, prompt_area_w, prompt_h);
            let spinner_area = Rect::new(
                main_area.x + 2,
                spinner_area_y,
                main_area.width.saturating_sub(4),
                spinner_h,
            );
            let question_area = Rect::new(
                main_area.x + 2,
                question_area_y,
                main_area.width.saturating_sub(4),
                question_h,
            );
            let permission_area = Rect::new(
                main_area.x + 2,
                permission_area_y,
                main_area.width.saturating_sub(4),
                permission_h,
            );
            let session_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                session_bottom.saturating_sub(area.y + 1),
            );

            match self.mode() {
                AppMode::Home => {
                    self.prompt_view.blur();
                    self.home_view.render(buf, session_area, &self.theme);
                }
                AppMode::InternalTools => {
                    self.prompt_view.blur();
                    let tools_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.internal_tools_view
                        .render(buf, tools_area, &self.theme);
                }
                AppMode::AddProvider => {
                    self.prompt_view.blur();
                    self.add_provider_view.search_bar.cursor.terminal_focused =
                        self.terminal_focused;
                    let tools_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.add_provider_view.render(buf, tools_area, &self.theme);
                }
                AppMode::Router => {
                    self.prompt_view.blur();
                    let router_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    let all_models = self.collect_cached_models();
                    self.router_view.render(
                        buf,
                        router_area,
                        &self.theme,
                        &all_models,
                        std::time::SystemTime::now(),
                    );
                }
                #[cfg(feature = "embed")]
                AppMode::Rag => {
                    self.render_rag_view(buf, session_area);
                }
                AppMode::Session => {
                    // Blur prompt when question/permission dialog is visible (like OpenCode)
                    if self.question_dialog.visible || self.permission_dialog.visible {
                        self.prompt_view.blur();
                    }

                    // Render static logo above the prompt on empty session
                    if is_empty_session && logo_start_y > 0 {
                        let cx = main_area.x + main_area.width / 2;
                        let lx = cx.saturating_sub(LOGO_WIDTH as u16 / 2);
                        let logo_style = Style::default().fg(rgba_color(self.theme.primary));
                        for (row, line) in LOGO_CHAT.iter().enumerate() {
                            let ly = logo_start_y + row as u16;
                            for (col, ch) in line.chars().enumerate() {
                                let cx_pos = lx + col as u16;
                                if cx_pos >= area.right() || ch == ' ' {
                                    continue;
                                }
                                if let Some(cell) = buf.cell_mut((cx_pos, ly)) {
                                    cell.set_char(ch);
                                    cell.set_style(logo_style);
                                }
                            }
                        }
                    }
                    self.prompt_view.cursor.terminal_focused = self.terminal_focused;
                    self.session_view.drag_selection = self.drag_selection;
                    self.session_view
                        .tool_state
                        .advance_tool_spinners(delta_time);

                    // Advance the agent spinner when working
                    if self.state.status == crate::types::SessionStatus::Working
                        && let Some(spinner) = &mut self.agent_spinner
                    {
                        spinner.advance();
                    }

                    let unique_agents = self.state.unique_agents();
                    let agent_colors = crate::types::AgentColors::from_theme(&self.theme);
                    self.session_view.render(
                        buf,
                        session_area,
                        &self.state,
                        &self.theme,
                        &self.config,
                        delta_time,
                    );
                    // Question/permission dialog rendered inline between messages and prompt
                    if self.question_dialog.visible {
                        self.question_dialog.render(buf, question_area, &self.theme);
                    } else if self.permission_dialog.visible {
                        self.permission_dialog
                            .render(buf, permission_area, &self.theme);
                    }
                    // Hide spinner and prompt when dialog is visible (like OpenCode)
                    if !self.question_dialog.visible && !self.permission_dialog.visible {
                        // Agent spinner rendered above the prompt when the loop is active
                        if let Some(spinner) = &self.agent_spinner
                            && self.state.status == crate::types::SessionStatus::Working
                        {
                            spinner.render(buf, spinner_area.x + 1, spinner_area.y);
                        }
                        let model_name = self.llm_config.model.as_deref().unwrap_or("");
                        self.prompt_view.render(
                            buf,
                            prompt_area,
                            &self.state,
                            &self.theme,
                            &agent_colors,
                            &unique_agents,
                            std::time::SystemTime::now(),
                            model_name,
                        );
                    }
                }
            }

            let footer_area = Rect::new(main_area.x, footer_y, main_area.width, 1);
            match self.mode() {
                AppMode::Home => {
                    HomeFooterView::render(buf, footer_area, &self.theme);
                }
                AppMode::Session => {
                    let hide_text = self.question_dialog.visible || self.permission_dialog.visible;
                    FooterView::render_with_mode(
                        buf,
                        footer_area,
                        &self.state,
                        &self.theme,
                        hide_text,
                    );
                }
                _ => {}
            }
            let now = std::time::SystemTime::now();
            self.toast_state.render(buf, area, &self.theme);
            // Sync terminal_focused to the dialog cursor so ThemeList/ModelList/ApiKeyInput
            // all respect the terminal focus state (blur when user clicks outside).
            if let Some(d) = self.dialog.current_mut() {
                d.cursor.terminal_focused = self.terminal_focused;
            }
            self.dialog.render(buf, area, &self.theme, now);

            self.command_palette.render(buf, area, &self.theme);
            self.slash_menu.render(buf, prompt_area, &self.theme);
        }
    }

    fn handle_events(&mut self) -> io::Result<bool> {
        self.toast_state.tick(50);

        if !event::poll(Duration::from_millis(50))? {
            return Ok(false);
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    // Escape clears selection if there is one.
                    if key.code == KeyCode::Esc && self.prompt_view.has_selection() {
                        self.prompt_view.clear_selection();
                        return Ok(false);
                    }

                    // Escape also unfocuses the sidebar.
                    if key.code == KeyCode::Esc && self.sidebar_focused {
                        self.sidebar_focused = false;
                        return Ok(false);
                    }

                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        // If there is text selected in the prompt, copy it instead of quitting.
                        if matches!(self.mode(), AppMode::Session)
                            && self.prompt_view.has_selection()
                        {
                            let text = self.prompt_view.selected_text();
                            selection::copy_selection(&text, &mut self.toast_state);
                            self.prompt_view.clear_selection();
                            return Ok(false);
                        }
                        self.pending_delete_session_id = None;
                        self.dialog.show(DialogType::Confirm {
                            message: "Quit cosh?".into(),
                        });
                        if let Some(d) = self.dialog.current_mut() {
                            d.selected = 1;
                        }
                        return Ok(false);
                    }

                    // Check theme dialog FIRST, before action lookup
                    if self.is_theme_dialog_visible() && self.handle_theme_dialog_key(key.code) {
                        return Ok(false);
                    }

                    // Check model dialog SECOND, before action lookup
                    if self.is_model_dialog_visible() && self.handle_model_dialog_key(key.code) {
                        return Ok(false);
                    }

                    // Check question dialog THIRD (inline)
                    if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                        let consumed = self.question_dialog.handle_key(key.code);
                        if consumed {
                            // Check if user submitted answers (Enter on confirm tab)
                            if self.question_dialog.submitted {
                                let answers = self.question_dialog.build_answers();
                                let _ = self.answer_tx.send(Ok(answers));
                                self.question_dialog.visible = false;
                                self.question_dialog.submitted = false;
                                // Re-focus prompt when question is answered (like OpenCode)
                                self.prompt_view.focus();
                            } else if !self.question_dialog.visible {
                                // Dialog was dismissed via Esc (like OpenCode) — stop agent loop + send rejection
                                self.stop_signal.store(true, Ordering::Relaxed);
                                let _ = self
                                    .answer_tx
                                    .send(Err("User dismissed the question dialog".into()));
                                self.prompt_view.focus();
                            }
                            return Ok(false);
                        }
                    }

                    // Check Confirm dialog for arrow navigation
                    if self.is_confirm_dialog_visible() && self.handle_confirm_dialog_key(key.code)
                    {
                        return Ok(false);
                    }

                    // Check ApiKeyInput dialog
                    if self.is_apikey_input_visible() {
                        let handled = self.handle_apikey_dialog_key(key.code);
                        if handled {
                            return Ok(false);
                        }
                    }

                    // Check Shortcuts dialog for scrolling
                    if self.is_shortcuts_dialog_visible() {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_sub(1);
                                }
                                return Ok(false);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_add(1);
                                }
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // Check permission dialog for keyboard navigation
                    if self.permission_dialog.visible {
                        match key.code {
                            KeyCode::Up => {
                                self.permission_dialog.selected =
                                    (self.permission_dialog.selected + 2) % 3;
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                self.permission_dialog.selected =
                                    (self.permission_dialog.selected + 1) % 3;
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                let action = match self.permission_dialog.selected {
                                    0 => cosh::harness::PermissionAction::Allow,
                                    1 => cosh::harness::PermissionAction::AllowOnce,
                                    _ => cosh::harness::PermissionAction::Deny,
                                };
                                self.permission_dialog.visible = false;
                                let _ = self.perm_tx.send(action);
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.permission_dialog.visible = false;
                                let _ = self.perm_tx.send(cosh::harness::PermissionAction::Deny);
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // Shift/Ctrl/Alt+Enter inserts a newline instead of sending.
                    if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::NONE {
                        self.prompt_view.note_activity();
                        let pos = self.prompt_view.cursor_pos;
                        self.prompt_view.input.insert(pos, '\n');
                        self.prompt_view.cursor_pos = pos + 1;
                        return Ok(false);
                    }

                    // Sidebar-focused arrow key scrolling (runs for ALL modes)
                    // Must come before mode-specific handlers (Home, InternalTools,
                    // Session) which also consume Up/Down before the action dispatch.
                    if self.sidebar_focused && self.sidebar.open {
                        match key.code {
                            KeyCode::Up => {
                                self.sidebar.select_prev(self.state.session_summaries.len());
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                self.sidebar.select_next(self.state.session_summaries.len());
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                match self.sidebar.handle_key(key.code, &self.state) {
                                    SidebarAction::SwitchTo(session_id) => {
                                        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
                                        self.finalize_stale_compaction_lines();
                                        self.state
                                            .switch_to_session(session_id, &self.session_store);
                                        self.finalize_stale_compaction_lines();
                                        return Ok(false);
                                    }
                                    SidebarAction::RequestDelete(_) | SidebarAction::None => {}
                                }
                            }
                            _ => {}
                        }
                    }

                    let action = self.keymap.lookup(key.code, key.modifiers).cloned();

                    // Home mode: navigation keys (skip when dialog is visible)
                    if matches!(self.mode(), AppMode::Home) && !self.dialog.visible() {
                        match key.code {
                            KeyCode::Up => {
                                self.home_view.select_prev();
                            }
                            KeyCode::Down => {
                                self.home_view.select_next();
                            }
                            KeyCode::Enter => {
                                match self.home_view.selected_action() {
                                    HomeAction::NewSession => {
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as u64;
                                        let id = format!("{now_ms}");
                                        let title = format_session_timestamp(now_ms);
                                        self.state.add_empty_session(id.clone(), title, now_ms);
                                        self.state.current_session_id = Some(id);
                                        self.prompt_view.focus();
                                    }
                                    HomeAction::ToggleSidebar => {
                                        self.sidebar.open = !self.sidebar.open;
                                    }
                                    HomeAction::OpenInternalTools => {
                                        self.show_internal_tools = true;
                                    }
                                    HomeAction::OpenShortcuts => {
                                        self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                                    }
                                    HomeAction::OpenAddProvider => {
                                        self.show_add_provider = true;
                                    }
                                    HomeAction::OpenModelRouter => {
                                        // Refresh fallbacks from prefs cache and models from model cache
                                        let saved = fallback::load_fallbacks(&self.prefs_cache);
                                        self.router_view.set_fallbacks(saved);
                                        self.show_router = true;
                                    }
                                    #[cfg(feature = "embed")]
                                    HomeAction::OpenRag => {
                                        self.show_rag = true;
                                    }
                                }
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // InternalTools mode: navigation and toggle keys
                    // Each matched arm returns early so unmatched keys fall through
                    // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
                    if matches!(self.mode(), AppMode::InternalTools) && !self.dialog.visible() {
                        match key.code {
                            KeyCode::Up => {
                                let list_area = 20; // max visible items estimate based on terminal
                                self.internal_tools_view.select_prev(list_area);
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                let list_area = 20;
                                self.internal_tools_view.select_next(list_area);
                                return Ok(false);
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                self.internal_tools_view.toggle_current();
                                crate::routes::tools::save_disabled_tools(
                                    &mut self.prefs_cache,
                                    &self.internal_tools_view.disabled,
                                );
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.show_internal_tools = false;
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // RAG mode: handle Ctrl+Backspace, Ctrl+Left, Ctrl+Right
                    // before passing key.code (which loses modifier info).
                    #[cfg(feature = "embed")]
                    if self.is_rag_mode() && !self.dialog.visible() {
                        match key.code {
                            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.rag_view.handle_ctrl_backspace();
                                return Ok(false);
                            }
                            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.rag_view.handle_ctrl_left();
                                return Ok(false);
                            }
                            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.rag_view.handle_ctrl_right();
                                return Ok(false);
                            }
                            // Ctrl+W = delete word before cursor (universal terminal shortcut)
                            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.rag_view.handle_ctrl_backspace();
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // Router mode: navigation and add/remove fallbacks
                    if matches!(self.mode(), AppMode::Router) && !self.dialog.visible() {
                        let all_models = self.collect_cached_models();
                        match key.code {
                            KeyCode::Up => {
                                if self.router_view.focus == FocusTarget::Fallbacks {
                                    self.router_view.select_prev_fallback();
                                } else {
                                    self.router_view.select_prev(&all_models);
                                }
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                if self.router_view.focus == FocusTarget::Fallbacks {
                                    self.router_view.select_next_fallback();
                                } else {
                                    self.router_view.select_next(&all_models);
                                }
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                if self.router_view.focus == FocusTarget::Models {
                                    self.router_view.add_selected_to_fallback(&all_models);
                                    fallback::save_fallbacks(
                                        &mut self.prefs_cache,
                                        &self.router_view.fallbacks,
                                    );
                                }
                                return Ok(false);
                            }
                            KeyCode::Backspace | KeyCode::Delete => {
                                if self.router_view.focus == FocusTarget::Fallbacks {
                                    if self.router_view.remove_selected_fallback().is_some() {
                                        fallback::save_fallbacks(
                                            &mut self.prefs_cache,
                                            &self.router_view.fallbacks,
                                        );
                                    }
                                } else if !self.router_view.search_bar.is_empty() {
                                    self.router_view.pop_filter_char();
                                } else if let Some(selected) =
                                    self.router_view.selected_model(&all_models)
                                {
                                    let idx = self.router_view.fallbacks.iter().position(|f| {
                                        f.provider == selected.provider && f.model == selected.model
                                    });
                                    if let Some(i) = idx {
                                        self.router_view.remove_fallback(i);
                                        fallback::save_fallbacks(
                                            &mut self.prefs_cache,
                                            &self.router_view.fallbacks,
                                        );
                                    }
                                }
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.router_view.clear_num_buffer();
                                self.show_router = false;
                                return Ok(false);
                            }
                            KeyCode::Char(ch) => {
                                if self.router_view.focus == FocusTarget::Models {
                                    if ch.is_ascii_digit() {
                                        self.router_view.handle_number_input(ch);
                                    } else {
                                        self.router_view.push_filter_char(ch);
                                    }
                                }
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // RAG mode: handle via RagView
                    if self.handle_rag_key_event(key.code) {
                        return Ok(false);
                    }

                    // AddProvider mode: navigation and select
                    // Each matched arm returns early so unmatched keys fall through
                    // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
                    if matches!(self.mode(), AppMode::AddProvider) && !self.dialog.visible() {
                        match key.code {
                            KeyCode::Up => {
                                let list_area = 20;
                                self.add_provider_view.select_prev(list_area);
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                let list_area = 20;
                                self.add_provider_view.select_next(list_area);
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                if let Some((provider, env_var)) =
                                    self.add_provider_view.selected_provider()
                                {
                                    self.dialog.show(DialogType::ApiKeyInput {
                                        provider: provider.to_string(),
                                        env_var: env_var.to_string(),
                                        input: String::new(),
                                        cursor_pos: 0,
                                    });
                                }
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.show_add_provider = false;
                                return Ok(false);
                            }
                            KeyCode::Char(ch) => {
                                let list_area = 20;
                                self.add_provider_view.push_filter_char(ch, list_area);
                                return Ok(false);
                            }
                            KeyCode::Backspace => {
                                let list_area = 20;
                                self.add_provider_view.pop_filter_char(list_area);
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }

                    // If slash menu is visible, arrow keys should move selection there
                    if self.slash_menu.visible {
                        match key.code {
                            KeyCode::Up => self.slash_menu.select_prev(),
                            KeyCode::Down => self.slash_menu.select_next(),
                            KeyCode::Enter => {
                                self.prompt_view.note_activity();
                                if let Some(cmd) = self.slash_menu.get_selected_command() {
                                    if cmd.name == "themes" {
                                        self.open_theme_dialog();
                                    } else if cmd.name == "models" {
                                        self.open_model_dialog();
                                    } else {
                                        let cmd_name = format!("/{} ", cmd.name);
                                        self.prompt_view.input = cmd_name;
                                        self.prompt_view.cursor_pos = self.prompt_view.input.len();
                                    }
                                    self.slash_menu.visible = false;
                                }
                            }
                            KeyCode::Esc => {
                                self.prompt_view.note_activity();
                                self.prompt_view.input.clear();
                                self.prompt_view.pasted_parts.clear();
                                self.prompt_view.cursor_pos = 0;
                                self.slash_menu.visible = false;
                            }
                            KeyCode::Backspace => {
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.delete_word_before_cursor();
                                    self.slash_menu.update(&self.prompt_view.input);
                                } else {
                                    self.prompt_view.note_activity();
                                    if !self.prompt_view.input.is_empty() {
                                        self.prompt_view.input.pop();
                                        self.prompt_view.cursor_pos = self.prompt_view.input.len();
                                        self.prompt_view.reset_history_index();
                                        self.slash_menu.update(&self.prompt_view.input);
                                    }
                                }
                            }
                            KeyCode::Char(ch) => {
                                self.prompt_view.note_activity();
                                self.prompt_view.input.push(ch);
                                self.prompt_view.cursor_pos += ch.len_utf8();
                                let was_visible = self.slash_menu.visible;
                                self.slash_menu.update(&self.prompt_view.input);
                                if was_visible
                                    && !self.slash_menu.visible
                                    && self.prompt_view.input.starts_with('/')
                                {
                                    self.prompt_view.input.remove(0);
                                    self.prompt_view.cursor_pos =
                                        self.prompt_view.cursor_pos.saturating_sub(1);
                                }
                            }
                            _ => {}
                        }
                        return Ok(false);
                    }

                    match action {
                        Some(crate::keymap::Action::ScrollUp) => {
                            if self.sidebar_focused && self.sidebar.open {
                                self.sidebar.select_prev(self.state.session_summaries.len());
                            } else if Self::is_in_right_panel(
                                self.last_mouse_x,
                                self.terminal_size(),
                            ) {
                                self.state.right_panel.scroll_up(3);
                            } else {
                                let vh = self.session_view.visible_height.max(1);
                                let delta = -(vh as f64 / 5.0);
                                self.session_view.scroll_by_raw(delta);
                                self.session_view.reset_scroll_accumulator();
                            }
                        }
                        Some(crate::keymap::Action::ScrollDown) => {
                            if self.sidebar_focused && self.sidebar.open {
                                self.sidebar.select_next(self.state.session_summaries.len());
                            } else if Self::is_in_right_panel(
                                self.last_mouse_x,
                                self.terminal_size(),
                            ) {
                                self.state.right_panel.scroll_down(3);
                            } else {
                                let vh = self.session_view.visible_height.max(1);
                                let delta = vh as f64 / 5.0;
                                self.session_view.scroll_by_raw(delta);
                                self.session_view.reset_scroll_accumulator();
                            }
                        }
                        Some(crate::keymap::Action::ScrollUpPage) => {
                            if self.sidebar_focused && self.sidebar.open {
                                self.sidebar
                                    .select_first(self.state.session_summaries.len());
                            } else if Self::is_in_right_panel(
                                self.last_mouse_x,
                                self.terminal_size(),
                            ) {
                                let vh = self.state.right_panel.visible_height.max(1);
                                self.state.right_panel.scroll_up(vh / 2);
                            } else {
                                let vh = self.session_view.visible_height.max(1);
                                let delta = -(vh as f64 / 2.0);
                                self.session_view.scroll_by_raw(delta);
                                self.session_view.reset_scroll_accumulator();
                            }
                        }
                        Some(crate::keymap::Action::ScrollDownPage) => {
                            if self.sidebar_focused && self.sidebar.open {
                                self.sidebar.select_last(self.state.session_summaries.len());
                            } else if Self::is_in_right_panel(
                                self.last_mouse_x,
                                self.terminal_size(),
                            ) {
                                let vh = self.state.right_panel.visible_height.max(1);
                                self.state.right_panel.scroll_down(vh / 2);
                            } else {
                                let vh = self.session_view.visible_height.max(1);
                                let delta = vh as f64 / 2.0;
                                self.session_view.scroll_by_raw(delta);
                                self.session_view.reset_scroll_accumulator();
                            }
                        }
                        Some(crate::keymap::Action::ToggleSidebar) => {
                            self.sidebar.open = !self.sidebar.open;
                        }
                        Some(crate::keymap::Action::ToggleHelp) => {
                            self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                        }
                        Some(
                            crate::keymap::Action::NextSession
                            | crate::keymap::Action::PrevSession
                            | crate::keymap::Action::FocusInput
                            | crate::keymap::Action::Quit
                            | crate::keymap::Action::NextAgent
                            | crate::keymap::Action::PrevAgent,
                        ) => {
                            // TBD
                        }
                        Some(
                            crate::keymap::Action::SendMessage | crate::keymap::Action::Confirm,
                        ) => {
                            if let Some(dialog) = self.dialog.current()
                                && matches!(dialog.dialog_type, DialogType::Confirm { .. })
                            {
                                if dialog.selected == 0 {
                                    if let Some(session_id) = self.pending_delete_session_id.take()
                                    {
                                        self.state.remove_session(&session_id);
                                        self.session_store.delete_session(&session_id);
                                        self.dialog.pop();
                                    } else if self.handle_rag_confirm_delete() {
                                        // handled
                                    } else {
                                        self.should_quit = true;
                                    }
                                } else {
                                    self.pending_delete_session_id = None;
                                    self.clear_rag_pending_state();
                                    self.dialog.pop();
                                }
                                return Ok(false);
                            }
                            self.prompt_view.note_activity();
                            if self.state.status == crate::types::SessionStatus::Working {
                                return Ok(false);
                            }

                            let msg = self.prompt_view.send_message();
                            if msg.trim().is_empty() {
                                return Ok(false);
                            }

                            if self.state.current_session_id.is_none() {
                                let id = generate_session_id();
                                let title: String = msg.chars().take(40).collect();
                                self.state.add_empty_session(
                                    id.clone(),
                                    title,
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis() as u64,
                                );
                                self.state.current_session_id = Some(id);
                            }

                            if let Some(session) = self.state.current_session_mut() {
                                session.messages.push(crate::types::Message {
                                    id: format!("msg-{}", session.messages.len()),
                                    role: crate::types::MessageRole::User,
                                    parts: vec![crate::types::Part::Text(crate::types::TextPart {
                                        text: msg.clone(),
                                        synthetic: false,
                                    })],
                                    created_at: 0,
                                    agent: None,
                                    model: None,
                                });

                                // Scroll to bottom when user sends a message (matches OpenCode's `toBottom()` on submit)
                                self.session_view.scroll_to_bottom();
                            }

                            self.stop_signal.store(false, Ordering::Relaxed);
                            self.state.status = crate::types::SessionStatus::Working;
                            self.agent_spinner = Some(AgentSpinner::new("Working", &self.theme));

                            let event_tx = self.event_tx.clone();
                            let provider = self.llm_config.provider.clone();
                            let model = self.llm_config.model.clone();
                            let fallbacks = self.router_view.fallbacks.clone();
                            // Auto-rotate: move the first working fallback to the front so the
                            // next message tries the provider that actually worked before wasting
                            // time on failing ones. Rotation is in-memory only (not persisted).
                            let fallbacks = if model.as_deref() == Some("auto") {
                                let working = fallbacks.iter().position(|fb| {
                                    cosh_sdk::connector::Connector::new(&fb.provider).is_ok()
                                });
                                match working {
                                    Some(0) | None => fallbacks,
                                    Some(idx) => {
                                        let mut rotated = fallbacks;
                                        rotated.rotate_left(idx);
                                        self.router_view.fallbacks = rotated.clone();
                                        rotated
                                    }
                                }
                            } else {
                                fallbacks
                            };
                            let stop_signal = self.stop_signal.clone();
                            let input = msg;
                            let cwd = self.state.working_directory.clone();
                            let mode = self.state.mode;

                            // Create a fresh answer channel for this agent loop invocation
                            let (answer_tx, answer_rx) = mpsc::unbounded_channel();
                            self.answer_tx = answer_tx;

                            // Create a fresh permission channel for this agent loop invocation
                            let (perm_tx, perm_rx) = mpsc::unbounded_channel();
                            self.perm_tx = perm_tx;

                            let mut disabled_tools = self.internal_tools_view.disabled.clone();

                            // RAG recall context
                            // 1) Description suffix (what the model sees in the tool doc)
                            #[cfg(feature = "embed")]
                            let recall_suffix = self.recall_suffix();
                            #[cfg(not(feature = "embed"))]
                            let _recall_suffix = String::new();

                            // 2) DB registry (what the dispatch uses to resolve db_name → connect)
                            #[cfg(feature = "embed")]
                            let recall_dbs: Vec<
                                cosh::harness::tools::RecallDb,
                            > = self.recall_dbs_vec();
                            #[cfg(not(feature = "embed"))]
                            let _recall_dbs = std::vec::Vec::<()>::new();

                            // 3) Auto-exclude tool if no databases exist at all
                            self.maybe_disable_recall_tool(&mut disabled_tools);

                            let event_tx_panic = event_tx.clone();
                            // Build conversation history from existing session messages
                            let history: Vec<(String, String)> = self
                                .state
                                .current_session()
                                .map(|s| {
                                    s.messages
                                        .iter()
                                        .filter_map(|m| {
                                            let role = match m.role {
                                                crate::types::MessageRole::User => "user",
                                                crate::types::MessageRole::Assistant => "assistant",
                                            };
                                            let text: String = m
                                                .parts
                                                .iter()
                                                .filter_map(|p| match p {
                                                    crate::types::Part::Text(t) => {
                                                        Some(t.text.as_str())
                                                    }
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            if text.is_empty() {
                                                None
                                            } else {
                                                Some((role.to_owned(), text))
                                            }
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();

                            // Load companion context state (.ctx file) for session resumption
                            let ctx_bytes: Option<Vec<u8>> = self
                                .state
                                .current_session_id
                                .as_ref()
                                .and_then(|id| self.session_store.load_ctx(id));

                            std::thread::spawn(move || {
                                use std::panic::AssertUnwindSafe;
                                use tokio::runtime::Builder;

                                let rt = match Builder::new_current_thread().enable_all().build() {
                                    Ok(rt) => rt,
                                    Err(e) => {
                                        let _ = event_tx
                                            .send(HarnessEvent::Error(format!("runtime: {e}")));
                                        return;
                                    }
                                };

                                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                    rt.block_on(async {
                                        use cosh::harness::Harness;
                                        use cosh_sdk::connector::Connector;

                                            let connector;
                                            let mut remaining: Vec<(String, String)> = Vec::new();
                                            if model.as_deref() == Some("auto") {
                                                let mut last_err = String::new();
                                                let mut found = None;
                                                for (i, fb) in fallbacks.iter().enumerate() {
                                                    match Connector::new(&fb.provider) {
                                                        Ok(c) => {
                                                            let c = c.with_model(&fb.model);
                                                            found = Some((c, i));
                                                            break;
                                                        }
                                                        Err(e) => {
                                                            last_err = format!("connector for {}: {e}", fb.provider);
                                                        }
                                                    }
                                                }
                                                match found {
                                                    Some((c, idx)) => {
                                                        remaining = fallbacks[idx + 1..]
                                                            .iter()
                                                            .map(|fb| (fb.provider.clone(), fb.model.clone()))
                                                            .collect();
                                                        connector = c;
                                                    }
                                                    None => {
                                                        let _ = event_tx.send(HarnessEvent::Error(
                                                            format!("auto: no fallback available ({last_err})"),
                                                        ));
                                                        return;
                                                    }
                                                }
                                            } else {
                                                match Connector::new(&provider) {
                                                    Ok(c) => {
                                                        connector = if let Some(ref m) = model {
                                                            c.with_model(m)
                                                        } else {
                                                            c
                                                        };
                                                    }
                                                    Err(e) => {
                                                        let _ = event_tx.send(HarnessEvent::Error(
                                                            format!("connector: {e}"),
                                                        ));
                                                        return;
                                                    }
                                                }
                                            }

                                        let mut harness =
                                            Harness::new(connector, &cwd, disabled_tools)
                                                .with_mode(mode)
                                                .with_history(&history)
                                                .with_fallbacks(remaining);

                                        // Restore compressed context state from .ctx companion file
                                        if let Some(ref ctx_bytes) = ctx_bytes {
                                            use cosh::harness::ContextManagerState;
                                            if let Ok(state) = bincode::deserialize::<ContextManagerState>(ctx_bytes) {
                                                harness.context_manager.restore_state(&state);
                                            }
                                        }
                                        #[cfg(feature = "embed")]
                                        harness.set_recall_context(recall_suffix);
                                        #[cfg(feature = "embed")]
                                        harness.set_recall_dbs(recall_dbs);
                                        harness.format_header_context();
                                        harness
                                            .run_agent_loop(
                                                &input,
                                                event_tx,
                                                answer_rx,
                                                perm_rx,
                                                stop_signal,
                                            )
                                            .await;
                                    });
                                }));

                                if let Err(panic) = result {
                                    let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                                        s.to_string()
                                    } else if let Some(s) = panic.downcast_ref::<String>() {
                                        s.clone()
                                    } else {
                                        "unknown panic".to_string()
                                    };
                                    let _ = event_tx_panic
                                        .send(HarnessEvent::Error(format!("panic: {msg}")));
                                }
                            });
                        }
                        Some(crate::keymap::Action::Interrupt) => {
                            if self.state.status == crate::types::SessionStatus::Working {
                                self.stop_signal.store(true, Ordering::Relaxed);
                            } else if self.question_dialog.visible {
                                self.question_dialog.visible = false;
                                let _ = self
                                    .answer_tx
                                    .send(Err("User dismissed the question dialog".into()));
                                self.prompt_view.focus();
                            } else if self.dialog.visible() {
                                self.pending_delete_session_id = None;
                                self.clear_rag_pending_state();
                                self.dialog.pop();
                            }
                        }
                        Some(crate::keymap::Action::Cancel) => {
                            if self.state.status == crate::types::SessionStatus::Working {
                                self.stop_signal.store(true, Ordering::Relaxed);
                                return Ok(false);
                            }
                            if self.question_dialog.visible {
                                self.question_dialog.visible = false;
                                let _ = self
                                    .answer_tx
                                    .send(Err("User dismissed the question dialog".into()));
                                self.prompt_view.focus();
                            } else if self.dialog.visible() {
                                self.pending_delete_session_id = None;
                                self.clear_rag_pending_state();
                                self.dialog.pop();
                            } else if matches!(self.mode(), AppMode::Session) {
                                self.state.current_session_id = None;
                                self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
                            } else if matches!(self.mode(), AppMode::AddProvider) {
                                self.show_add_provider = false;
                            } else if self.is_rag_mode() {
                                self.handle_rag_cancel_action();
                            } else if matches!(self.mode(), AppMode::Home) {
                                self.pending_delete_session_id = None;
                                self.dialog.show(DialogType::Confirm {
                                    message: "Quit cosh?".into(),
                                });
                                if let Some(d) = self.dialog.current_mut() {
                                    d.selected = 1;
                                }
                            }
                        }
                        Some(crate::keymap::Action::ScrollToTop) => {
                            if Self::is_in_right_panel(self.last_mouse_x, self.terminal_size()) {
                                self.state.right_panel.reset_scroll();
                            } else {
                                self.session_view.scroll_to(0);
                            }
                        }
                        Some(crate::keymap::Action::ScrollToBottom) => {
                            if Self::is_in_right_panel(self.last_mouse_x, self.terminal_size()) {
                                self.state.right_panel.scroll_to_bottom();
                            } else {
                                self.session_view.scroll_to_bottom();
                            }
                        }
                        Some(crate::keymap::Action::ToggleConceal) => {
                            self.config.conceal = !self.config.conceal;
                        }
                        Some(crate::keymap::Action::ToggleThinking) => {
                            self.config.thinking_mode = !self.config.thinking_mode;
                        }
                        Some(crate::keymap::Action::ToggleToolDetails) => {
                            self.config.show_tool_details = !self.config.show_tool_details;
                        }
                        Some(crate::keymap::Action::ToggleGenericToolOutput) => {
                            self.config.show_generic_tool_output =
                                !self.config.show_generic_tool_output;
                        }
                        Some(crate::keymap::Action::ToggleTimestamps) => {
                            self.config.show_timestamps = !self.config.show_timestamps;
                        }
                        Some(crate::keymap::Action::ToggleMode) => {
                            if matches!(self.mode(), AppMode::Session) {
                                use cosh::harness::Mode;
                                self.state.mode = match self.state.mode {
                                    Mode::Build => Mode::Ask,
                                    Mode::Ask => Mode::Yolo,
                                    Mode::Yolo => Mode::Build,
                                };
                            }
                        }
                        Some(crate::keymap::Action::HistoryUp) => {
                            self.prompt_view.note_activity();
                            let user_msgs = self
                                .state
                                .current_session()
                                .map(PromptView::user_message_texts)
                                .unwrap_or_default();
                            self.prompt_view.history_up(&user_msgs);
                        }
                        Some(crate::keymap::Action::HistoryDown) => {
                            self.prompt_view.note_activity();
                            let user_msgs = self
                                .state
                                .current_session()
                                .map(PromptView::user_message_texts)
                                .unwrap_or_default();
                            self.prompt_view.history_down(&user_msgs);
                        }
                        Some(crate::keymap::Action::ToggleCommandPalette) => {
                            self.command_palette.toggle();
                        }
                        None => {
                            if self.command_palette.visible {
                                match key.code {
                                    KeyCode::Up => {
                                        self.command_palette.select_prev();
                                    }
                                    KeyCode::Down => {
                                        self.command_palette.select_next();
                                    }
                                    KeyCode::Backspace => {
                                        self.command_palette.pop_char();
                                    }
                                    KeyCode::Char(ch) => {
                                        self.command_palette.push_char(ch);
                                    }
                                    KeyCode::Esc => {
                                        self.command_palette.visible = false;
                                    }
                                    _ => {}
                                }
                                return Ok(false);
                            }

                            if self.slash_menu.visible {
                                match key.code {
                                    KeyCode::Up => self.slash_menu.select_prev(),
                                    KeyCode::Down => self.slash_menu.select_next(),
                                    KeyCode::Enter => {
                                        self.prompt_view.note_activity();
                                        if let Some(cmd) = self.slash_menu.get_selected_command() {
                                            if cmd.name == "themes" {
                                                self.open_theme_dialog();
                                            } else if cmd.name == "models" {
                                                self.open_model_dialog();
                                            } else {
                                                let cmd_name = format!("/{} ", cmd.name);
                                                self.prompt_view.input = cmd_name;
                                                self.prompt_view.cursor_pos =
                                                    self.prompt_view.input.len();
                                            }
                                            self.slash_menu.visible = false;
                                        }
                                    }
                                    KeyCode::Esc => {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.input.clear();
                                        self.prompt_view.pasted_parts.clear();
                                        self.prompt_view.cursor_pos = 0;
                                        self.slash_menu.visible = false;
                                    }
                                    KeyCode::Backspace => {
                                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                                            self.prompt_view.delete_word_before_cursor();
                                            self.slash_menu.update(&self.prompt_view.input);
                                        } else {
                                            self.prompt_view.note_activity();
                                            if !self.prompt_view.input.is_empty() {
                                                self.prompt_view.input.pop();
                                                self.prompt_view.cursor_pos =
                                                    self.prompt_view.input.len();
                                                self.slash_menu.update(&self.prompt_view.input);
                                            }
                                        }
                                    }
                                    KeyCode::Char(ch) => {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.input.push(ch);
                                        self.prompt_view.cursor_pos += ch.len_utf8();
                                        let was_visible = self.slash_menu.visible;
                                        self.slash_menu.update(&self.prompt_view.input);
                                        // If menu closed (e.g., user typed space), remove the leading "/"
                                        if was_visible
                                            && !self.slash_menu.visible
                                            && self.prompt_view.input.starts_with('/')
                                        {
                                            self.prompt_view.input.remove(0);
                                            self.prompt_view.cursor_pos =
                                                self.prompt_view.cursor_pos.saturating_sub(1);
                                        }
                                    }
                                    _ => {}
                                }
                                return Ok(false);
                            }

                            if matches!(self.mode(), AppMode::Session) {
                                match key.code {
                                    KeyCode::Up => {
                                        if self.prompt_view.is_focused {
                                            if self.prompt_view.input.is_empty()
                                                || self.prompt_view.history_index != -1
                                            {
                                                let user_msgs = self
                                                    .state
                                                    .current_session()
                                                    .map(PromptView::user_message_texts)
                                                    .unwrap_or_default();
                                                self.prompt_view.history_up(&user_msgs);
                                            } else {
                                                self.prompt_view.note_activity();
                                                self.prompt_view.cursor_up(
                                                    self.prompt_view.input_text_width.get().max(1),
                                                );
                                            }
                                        } else if Self::is_in_right_panel(
                                            self.last_mouse_x,
                                            self.terminal_size(),
                                        ) {
                                            self.state.right_panel.scroll_up(3);
                                        } else {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = -(vh as f64 / 5.0);
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                        }
                                    }
                                    KeyCode::Down => {
                                        if self.prompt_view.is_focused {
                                            if self.prompt_view.input.is_empty()
                                                || self.prompt_view.history_index != -1
                                            {
                                                let user_msgs = self
                                                    .state
                                                    .current_session()
                                                    .map(PromptView::user_message_texts)
                                                    .unwrap_or_default();
                                                self.prompt_view.history_down(&user_msgs);
                                            } else {
                                                self.prompt_view.note_activity();
                                                self.prompt_view.cursor_down(
                                                    self.prompt_view.input_text_width.get().max(1),
                                                );
                                            }
                                        } else if Self::is_in_right_panel(
                                            self.last_mouse_x,
                                            self.terminal_size(),
                                        ) {
                                            self.state.right_panel.scroll_down(3);
                                        } else {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = vh as f64 / 5.0;
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                        }
                                    }
                                    KeyCode::Left => {
                                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                                            self.prompt_view.cursor_word_left();
                                        } else {
                                            self.prompt_view.note_activity();
                                            if self.prompt_view.cursor_pos > 0 {
                                                self.prompt_view.cursor_pos =
                                                    self.prompt_view.input.floor_char_boundary(
                                                        self.prompt_view.cursor_pos - 1,
                                                    );
                                            }
                                        }
                                    }
                                    KeyCode::Right => {
                                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                                            self.prompt_view.cursor_word_right();
                                        } else {
                                            self.prompt_view.note_activity();
                                            let len = self.prompt_view.input.len();
                                            if self.prompt_view.cursor_pos < len {
                                                let c = self.prompt_view.input
                                                    [self.prompt_view.cursor_pos..]
                                                    .chars()
                                                    .next()
                                                    .unwrap();
                                                self.prompt_view.cursor_pos += c.len_utf8();
                                            }
                                        }
                                    }
                                    KeyCode::Home => {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.cursor_pos = 0;
                                    }
                                    KeyCode::End => {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.cursor_pos = self.prompt_view.input.len();
                                    }
                                    KeyCode::Delete => {
                                        self.prompt_view.delete();
                                    }
                                    KeyCode::PageUp => {
                                        if Self::is_in_right_panel(
                                            self.last_mouse_x,
                                            self.terminal_size(),
                                        ) {
                                            let vh = self.state.right_panel.visible_height.max(1);
                                            self.state.right_panel.scroll_up(vh / 2);
                                        } else {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = -(vh as f64 / 2.0);
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                        }
                                    }
                                    KeyCode::PageDown => {
                                        if Self::is_in_right_panel(
                                            self.last_mouse_x,
                                            self.terminal_size(),
                                        ) {
                                            let vh = self.state.right_panel.visible_height.max(1);
                                            self.state.right_panel.scroll_down(vh / 2);
                                        } else {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = vh as f64 / 2.0;
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                        }
                                    }
                                    KeyCode::Backspace => {
                                        // Ctrl+Backspace = delete word before cursor
                                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                                            self.prompt_view.delete_word_before_cursor();
                                        } else {
                                            self.prompt_view.backspace();
                                        }
                                    }
                                    KeyCode::Char(ch) => {
                                        self.prompt_view.note_activity();

                                        // Ctrl+J is the universal newline (^J = \n) — works in every terminal
                                        if ch == 'j'
                                            && key.modifiers.contains(KeyModifiers::CONTROL)
                                        {
                                            let pos = self.prompt_view.cursor_pos;
                                            self.prompt_view.input.insert(pos, '\n');
                                            self.prompt_view.cursor_pos = pos + 1;
                                            return Ok(false);
                                        }

                                        // Ctrl+W = delete word before cursor (universal terminal shortcut)
                                        if ch == 'w'
                                            && key.modifiers.contains(KeyModifiers::CONTROL)
                                        {
                                            self.prompt_view.delete_word_before_cursor();
                                            return Ok(false);
                                        }

                                        // Vim-style scroll only when prompt is empty
                                        // (otherwise these chars are typed normally)
                                        if ch == 'j' && self.prompt_view.input.is_empty() {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = vh as f64 / 5.0;
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                            return Ok(false);
                                        }
                                        if ch == 'k' && self.prompt_view.input.is_empty() {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = -(vh as f64 / 5.0);
                                            self.session_view.scroll_by_raw(delta);
                                            self.session_view.reset_scroll_accumulator();
                                            return Ok(false);
                                        }

                                        // Insert character normally
                                        let pos = self.prompt_view.cursor_pos;
                                        self.prompt_view.input.insert(pos, ch);
                                        // Use len_utf8() so cursor stays on a valid UTF-8 boundary
                                        // for multi-byte chars (e.g. á, é, emoji).
                                        self.prompt_view.cursor_pos = pos + ch.len_utf8();

                                        // Typing modifies input, exit history browsing
                                        self.prompt_view.reset_history_index();

                                        // Check if "/" menu should open
                                        self.slash_menu.update(&self.prompt_view.input);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            Event::FocusGained => {
                self.terminal_focused = true;
            }
            Event::FocusLost => {
                self.terminal_focused = false;
            }
            Event::Resize(_w, _h) => {}
            Event::Paste(text) => {
                // If ApiKeyInput dialog is visible, paste into the dialog input
                if self.is_apikey_input_visible() {
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ApiKeyInput {
                            input, cursor_pos, ..
                        } = &mut d.dialog_type
                    {
                        let cleaned: String =
                            text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                        input.insert_str(*cursor_pos, &cleaned);
                        *cursor_pos += cleaned.len();
                        d.cursor.note_activity();
                    }
                } else if self.is_rag_mode() {
                    self.handle_rag_paste(&text);
                } else {
                    self.prompt_view.note_activity();
                    self.prompt_view.handle_paste(&text);
                    self.slash_menu.update(&self.prompt_view.input);
                }
            }
            Event::Mouse(crossterm_mouse) => {
                self.handle_mouse_event(crossterm_mouse)?;
            }
        }

        Ok(false)
    }

    /// Append a one-shot compaction notice line (phases 2-4) to the current
    /// session chat.
    fn push_compaction_line(
        session: &mut crate::types::Session,
        phase: crate::types::CompactionPhase,
    ) {
        use crate::types::{CompactionPart, Message, MessageRole, Part};
        session.messages.push(Message {
            id: format!("msg-ctx-{}", session.messages.len()),
            role: MessageRole::Assistant,
            parts: vec![Part::Compaction(CompactionPart::done(phase))],
            created_at: 0,
            agent: None,
            model: None,
        });
    }

    /// Handle a compaction-phase notification from the harness: start the
    /// pipeline stopwatch line, finalize it (freezing the elapsed time), or
    /// append a one-shot phase notice.
    ///
    /// NOTE on the stopwatch: it measures from when THIS function processes
    /// `PipelineStarted` to when it processes `PipelineFinished`, so channel
    /// queue delay is included. That is the intended design ("the TUI times
    /// it, the context manager only notifies") — do not "fix" it to use
    /// harness-side timestamps.
    fn handle_compaction_event(&mut self, event: CompactionEvent) {
        use crate::types::{CompactionPart, CompactionPhase, Message, MessageRole, Part};
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        match event {
            CompactionEvent::PipelineStarted => {
                session.messages.push(Message {
                    id: format!("msg-ctx-{}", session.messages.len()),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Compaction(CompactionPart::running(
                        CompactionPhase::Pipeline,
                    ))],
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
            CompactionEvent::PipelineFinished => {
                Self::finalize_compaction_line(session, CompactionPhase::Pipeline);
            }
            CompactionEvent::DraftsEvicted => {
                Self::push_compaction_line(session, CompactionPhase::Drafts);
            }
        }
    }

    /// Freeze the still-running stopwatch line of the given phase. The
    /// pipeline and the LLM compaction share this lifecycle: `Started` opens
    /// a running line, its terminal event freezes it.
    fn finalize_compaction_line(
        session: &mut crate::types::Session,
        phase: crate::types::CompactionPhase,
    ) {
        use crate::types::Part;
        let now = crate::types::now_ms();
        let running = session.messages.iter_mut().rev().find(|m| {
            matches!(&m.parts[..], [Part::Compaction(c)]
                if c.phase == phase && c.is_running())
        });
        if let Some(msg) = running
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.elapsed_ms = Some(now.saturating_sub(c.started_at));
        }
    }

    /// Handle an LLM-compaction lifecycle event (phase 3, the last-resort
    /// fallback driven by the harness): `Started` opens the running
    /// "Summarizing" box in the chat, `Finished`/`Failed` freezes it.
    fn handle_llm_compaction_event(&mut self, event: cosh::harness::events::LlmCompactionEvent) {
        use crate::types::{CompactionPart, CompactionPhase, Message, MessageRole, Part};
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        match event {
            cosh::harness::events::LlmCompactionEvent::Started => {
                session.messages.push(Message {
                    id: format!("msg-ctx-{}", session.messages.len()),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Compaction(CompactionPart::running(
                        CompactionPhase::Llm,
                    ))],
                    created_at: 0,
                    agent: None,
                    model: None,
                });
            }
            cosh::harness::events::LlmCompactionEvent::Finished
            | cosh::harness::events::LlmCompactionEvent::Failed => {
                Self::finalize_compaction_line(session, CompactionPhase::Llm);
            }
        }
    }

    /// Accumulate a streamed summary token into the running "Summarizing"
    /// box (the newest running LLM-compaction line in the current session).
    /// Tokens append to the box's text so the user watches the summary being
    /// written live, exactly like the main agent's tokens stream into the
    /// chat.
    fn handle_llm_compaction_token(&mut self, text: &str) {
        use crate::types::{CompactionPhase, Part};
        if text.is_empty() {
            return;
        }
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let target = session.messages.iter_mut().rev().find(|m| {
            matches!(&m.parts[..], [Part::Compaction(c)]
                if c.phase == CompactionPhase::Llm && c.is_running())
        });
        if let Some(msg) = target
            && let Some(Part::Compaction(c)) = msg.parts.last_mut()
        {
            c.text.push_str(text);
        }
    }

    /// Close any still-running compaction lines in the current session so they
    /// never tick forever (e.g. a session restored from disk with a line that
    /// was mid-pipeline when the process died, or a loop interrupted right
    /// after `PipelineStarted`).
    fn finalize_stale_compaction_lines(&mut self) {
        let now = crate::types::now_ms();
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        for msg in &mut session.messages {
            if let Some(crate::types::Part::Compaction(c)) = msg.parts.last_mut()
                && c.is_running()
            {
                c.elapsed_ms = Some(now.saturating_sub(c.started_at));
            }
        }
    }

    /// True when the current session has a still-running compaction line — the
    /// render loop must stay live so the stopwatch ticks every frame.
    fn has_running_compaction(&self) -> bool {
        self.state.current_session().is_some_and(|s| {
            s.messages.iter().any(|m| {
                m.parts
                    .iter()
                    .any(|p| matches!(p, crate::types::Part::Compaction(c) if c.is_running()))
            })
        })
    }

    fn poll_events(&mut self) {
        use crate::types::{
            Message, MessageRole, Part, ReasoningPart, SessionStatus, TextPart, ToolPart,
            ToolStatus,
        };
        use crate::ui::toast::{ToastOptions, ToastVariant};

        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                HarnessEvent::Token { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => {
                            match msg.parts.last_mut() {
                                Some(Part::Text(tp)) => tp.text.push_str(&text),
                                _ => msg.parts.push(Part::Text(TextPart {
                                    text: text.clone(),
                                    synthetic: false,
                                })),
                            }
                        }
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Text(TextPart {
                                text: text.clone(),
                                synthetic: false,
                            })],
                            created_at: 0,
                            agent: None,
                            model: None,
                        }),
                    }
                }

                HarnessEvent::ToolCall { tool, input } => {
                    // Track plan_todo_write calls for right panel TODO list
                    if tool == "plan_todo_write" {
                        self.state.right_panel.pending_todo_update_count += 1;
                    }
                    // Also track plan_todo_cross_off for checkmarks
                    if tool == "plan_todo_cross_off" {
                        self.state.right_panel.pending_todo_update_count += 1;
                    }
                    // Start PTY tracking for bash calls
                    if tool == "bash_run" {
                        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        self.state.right_panel.start_pty(command.to_string(), None);
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Start PTY tracking for subagent calls
                    if tool == "subagent_call" {
                        let agent = input.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = input.get("input").and_then(|v| v.as_str()).unwrap_or("");
                        // Remove previous PTY entries for this specific agent only,
                        // so different agents (e.g. opencode vs claude) can coexist.
                        let subagent_prefix = format!("subagent: {agent}");
                        self.state
                            .right_panel
                            .pty_sessions
                            .retain(|s| !s.command.starts_with(&subagent_prefix));
                        let cmd = format!("subagent: {agent}");
                        self.state.right_panel.start_pty(cmd, None);
                        // Show the input message as the first line of the dialogue,
                        // visually prefixed to indicate it came from the main agent.
                        if !msg.is_empty() {
                            self.state
                                .right_panel
                                .update_last_pty(format!("→ cosh: {msg}\n"));
                        }
                        self.state.right_panel.scroll_to_bottom();
                    }

                    // Save a clone of input before it moves into the ToolPart
                    let input_clone = input.clone();

                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    let part = Part::Tool(ToolPart {
                        tool: tool.clone(),
                        input,
                        output: None,
                        status: ToolStatus::Running,
                        tool_call_id: None,
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                    });
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => msg.parts.push(part),
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![part],
                            created_at: 0,
                            agent: None,
                            model: None,
                        }),
                    }
                    // Session borrow dropped; create spinner for the new tool call.
                    // This ensures the beam is visible even if ToolResult arrives
                    // before the next render (fast tools like read).
                    {
                        let display = crate::routes::session::tool_render::tool_display(&tool);
                        let part_idx = self
                            .state
                            .current_session()
                            .and_then(|s| s.messages.last())
                            .map(|m| m.parts.len().saturating_sub(1))
                            .unwrap_or(0);
                        let tool_id = format!("{}_{}", display, part_idx);
                        let text = {
                            let temp_part = ToolPart {
                                tool: tool.clone(),
                                input: input_clone,
                                output: None,
                                status: ToolStatus::Running,
                                tool_call_id: None,
                                is_start: false,
                                is_streaming: false,
                                cached_line_count: None,
                            };
                            crate::routes::session::tool_render::tool_inline_text(&temp_part)
                        };
                        let highlight = crate::routes::session::tool_render::tool_color(display)
                            .map(|c| {
                                let (r, g, b) = match c {
                                    Color::Rgb(r, g, b) => (r, g, b),
                                    _ => (128, 128, 128),
                                };
                                RGBA::from_ints(r, g, b, 255)
                            })
                            .unwrap_or(RGBA::from_ints(128, 128, 128, 255));
                        let base = self.theme.text;
                        let mut spinner = HighlightSpinner::new(&text, highlight, base);
                        spinner.set_beam_pos(0.0);
                        self.session_view
                            .tool_state
                            .tool_spinners
                            .insert(tool_id, spinner);
                    }
                }

                HarnessEvent::ToolResult { output } => {
                    // Try to parse as TODO output to update right panel
                    if self.state.right_panel.pending_todo_update_count > 0 {
                        self.state.right_panel.pending_todo_update_count -= 1;
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output)
                            && let Some(groups) = val
                                .get("list")
                                .and_then(|l| l.get("groups"))
                                .and_then(|g| g.as_array())
                        {
                            let todos: Vec<_> = groups
                                .iter()
                                .flat_map(|g| {
                                    g.get("items")
                                        .and_then(|items| items.as_array())
                                        .into_iter()
                                        .flatten()
                                })
                                .map(|item| {
                                    let status = item
                                        .get("status")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("Pending");
                                    let description = item
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    crate::routes::session::right_panel::types::TodoItem {
                                        status: match status {
                                            "InProgress" => "in_progress",
                                            "Completed" => "completed",
                                            "Cancelled" => "cancelled",
                                            _ => "pending",
                                        }
                                        .to_string(),
                                        content: description.to_string(),
                                    }
                                })
                                .collect();
                            self.state.right_panel.set_todos(todos);
                        }
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };

                    // Find and complete the running tool part, capture its name
                    let mut completed_tool_name: Option<String> = None;
                    'find_running: for msg in session.messages.iter_mut().rev() {
                        for part in msg.parts.iter_mut().rev() {
                            if let Part::Tool(tp) = part
                                && tp.status == ToolStatus::Running
                            {
                                completed_tool_name = Some(tp.tool.clone());
                                tp.status = ToolStatus::Completed;
                                tp.output = Some(output.clone());
                                break 'find_running;
                            }
                        }
                    }

                    // Deduplicate plan_todo_write: only the LAST completed one keeps its output.
                    // Previous completed plan_todo_write parts get cleared so they render
                    // inline ("☰ TODO Write") instead of as full block TODOs.
                    if completed_tool_name.as_deref() == Some("plan_todo_write") {
                        let mut found_current = false;
                        for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.tool == "plan_todo_write"
                                {
                                    if !found_current {
                                        // Skip the current (latest) plan_todo_write
                                        found_current = true;
                                    } else if tp.status == ToolStatus::Completed {
                                        // Clear output of previous completed plan_todo_write
                                        tp.output = None;
                                    }
                                }
                            }
                            // Only search the current assistant message
                            if msg.role == crate::types::MessageRole::User {
                                break;
                            }
                        }
                    }

                    if !self.state.right_panel.is_scrolled_up() {
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Harmless for subagent_call (PTY already Completed via finished:true,
                    // complete_last_pty is a no-op for non-Running sessions).
                    // Required for bash_run which only completes via ToolResult.
                    self.state.right_panel.complete_last_pty(output.clone());
                }

                HarnessEvent::ToolError { error } => {
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                        {
                            tp.status = ToolStatus::Failed(error.clone());
                            break;
                        }
                    }
                    self.state.right_panel.fail_last_pty(error.clone());
                }
                HarnessEvent::ToolOutput {
                    tool,
                    output,
                    finished,
                } => {
                    // Streaming find results (glob/grep matches) are appended to
                    // the running tool part so the chat shows a live counter.
                    // Everything else (bash, subagent) streams into the right
                    // panel PTY.
                    if matches!(tool.as_str(), "find_glob" | "find_grep") {
                        let Some(session) = self.state.current_session_mut() else {
                            continue;
                        };
                        let mut appended = false;
                        'find_part: for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.status == ToolStatus::Running
                                    && tp.tool == tool
                                {
                                    let out = tp.output.get_or_insert_with(String::new);
                                    let added_lines = output.lines().count() as u32;
                                    out.push_str(&output);
                                    // Update cached line count for efficient display
                                    tp.cached_line_count = Some(
                                        tp.cached_line_count
                                            .unwrap_or(0)
                                            .saturating_add(added_lines),
                                    );
                                    appended = true;
                                    break 'find_part;
                                }
                            }
                        }
                        if !appended {
                            log::debug!(
                                "ToolOutput for {tool} with no running part; dropping chunk"
                            );
                        }
                        continue;
                    }
                    // Update right panel PTY with streaming output
                    self.state.right_panel.update_last_pty(output.clone());
                    // Auto-follow if user is at the bottom
                    if !self.state.right_panel.is_scrolled_up() {
                        self.state.right_panel.scroll_to_bottom();
                    }
                    if finished {
                        self.state.right_panel.complete_last_pty(output.clone());
                    }
                }

                HarnessEvent::Reasoning { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => {
                            match msg.parts.last_mut() {
                                Some(Part::Reasoning(rp)) => rp.text.push_str(&text),
                                _ => msg.parts.push(Part::Reasoning(ReasoningPart {
                                    text: text.clone(),
                                    collapsed: true,
                                })),
                            }
                        }
                        _ => session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Reasoning(ReasoningPart {
                                text: text.clone(),
                                collapsed: true,
                            })],
                            created_at: 0,
                            agent: None,
                            model: None,
                        }),
                    }
                }

                HarnessEvent::Done { context_state } => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;
                    // Safety net: a pipeline line interrupted at its start must
                    // not stay running (the stopwatch would tick forever).
                    self.finalize_stale_compaction_lines();

                    // Persist session to disk if it has valid dialog
                    if let Some(id) = self.state.current_session_id.clone()
                        && let Some(session) = self.state.session_cache.get(&id)
                        && is_valid_session(session)
                    {
                        self.session_store.save_session(session);
                        self.session_store.save_ctx(&id, &context_state);
                        self.state.ensure_session_summary(&id);
                    }
                }

                HarnessEvent::Stopped { context_state } => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;
                    self.finalize_stale_compaction_lines();
                    self.toast_state.show(ToastOptions {
                        title: Some("Interrupted".into()),
                        message: "Agent loop was stopped.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 3000,
                    });

                    // Persist session to disk even when stopped (partial dialog is still valuable)
                    if let Some(id) = self.state.current_session_id.clone()
                        && let Some(session) = self.state.session_cache.get(&id)
                        && is_valid_session(session)
                    {
                        self.session_store.save_session(session);
                        self.session_store.save_ctx(&id, &context_state);
                        self.state.ensure_session_summary(&id);
                    }
                }
                HarnessEvent::ContextInfo { info } => {
                    self.context_info = Some(info);
                }

                HarnessEvent::Compaction { event } => {
                    self.handle_compaction_event(event);
                }

                HarnessEvent::LlmCompaction { event } => {
                    self.handle_llm_compaction_event(event);
                }

                HarnessEvent::LlmCompactionToken { text } => {
                    self.handle_llm_compaction_token(&text);
                }

                HarnessEvent::Error(msg) => {
                    self.state.status = SessionStatus::Retry {
                        message: msg.clone(),
                        action: None,
                    };
                    self.agent_spinner = None;

                    // Push error as an assistant message so it appears inline in the chat
                    let error_text = format!("Error: {msg}");
                    if let Some(session) = self.state.current_session_mut() {
                        session.messages.push(Message {
                            id: format!("msg-err-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Text(TextPart {
                                text: error_text,
                                synthetic: false,
                            })],
                            created_at: 0,
                            agent: None,
                            model: self.llm_config.model.clone(),
                        });
                    }
                }

                HarnessEvent::ModelsLoaded { models, current } => {
                    // Update cache with the freshly fetched models
                    self.update_model_cache(&models);

                    // Prepend "auto" entry
                    let auto_entry = cosh::ModelEntry {
                        provider: String::new(),
                        model: "auto".to_string(),
                    };
                    let mut models_with_auto = vec![auto_entry];
                    models_with_auto.extend(models);

                    let auto_current = if current == "auto" {
                        current
                    } else {
                        String::new()
                    };

                    // Update the dialog with the loaded models
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ModelList {
                            models: dialog_models,
                            current: dialog_current,
                            ..
                        } = &mut d.dialog_type
                    {
                        *dialog_models = models_with_auto;
                        *dialog_current = auto_current;
                    }
                }

                HarnessEvent::QuestionRequest { questions } => {
                    // Show the question dialog with real questions from the harness
                    self.question_dialog.show_questions(questions);
                    // Blur the prompt when questions appear (like OpenCode hides the prompt)
                    self.prompt_view.blur();
                }

                HarnessEvent::PermissionRequest {
                    tool,
                    description,
                    args,
                } => {
                    // Show the permission dialog with details from the harness
                    self.permission_dialog.request =
                        Some(crate::routes::session::permission::PermissionRequest {
                            tool,
                            description,
                            args,
                        });
                    self.permission_dialog.visible = true;
                    // Default to "Deny" (index 2) for safety
                    self.permission_dialog.selected = 2;
                }
            }
        }
    }

    /// Check if a mouse x-coordinate is within the right panel area.
    fn is_in_right_panel(x: u16, terminal_size: Rect) -> bool {
        if terminal_size.width < 100 {
            return false;
        }
        let right_panel_x = terminal_size
            .width
            .saturating_sub(crate::routes::session::right_panel::RIGHT_PANEL_WIDTH);
        x >= right_panel_x
    }

    /// Handle a crossterm mouse event by converting it to a cosh-tui `MouseEvent`
    /// and dispatching to the appropriate component based on current layout.
    #[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
    fn handle_mouse_event(&mut self, evt: CrosstermMouseEvent) -> io::Result<bool> {
        let x = evt.column;
        let y = evt.row;
        self.last_mouse_x = x;

        let modifiers = MouseModifiers {
            shift: evt.modifiers.contains(KeyModifiers::SHIFT),
            alt: evt.modifiers.contains(KeyModifiers::ALT),
            ctrl: evt.modifiers.contains(KeyModifiers::CONTROL),
        };

        let (button, event_type) = match evt.kind {
            MouseEventKind::Down(btn) | MouseEventKind::Drag(btn) => {
                let b = match btn {
                    CrosstermMouseButton::Left => MouseButton::Left,
                    CrosstermMouseButton::Right => MouseButton::Right,
                    CrosstermMouseButton::Middle => MouseButton::Middle,
                };
                let t = match evt.kind {
                    MouseEventKind::Down(_) => MouseEventType::Down,
                    _ => MouseEventType::Drag,
                };
                (b, t)
            }
            MouseEventKind::Up(btn) => {
                let b = match btn {
                    CrosstermMouseButton::Left => MouseButton::Left,
                    CrosstermMouseButton::Right => MouseButton::Right,
                    CrosstermMouseButton::Middle => MouseButton::Middle,
                };
                (b, MouseEventType::Up)
            }
            MouseEventKind::Moved => (MouseButton::Left, MouseEventType::Move),
            MouseEventKind::ScrollDown => (MouseButton::Left, MouseEventType::ScrollDown),
            MouseEventKind::ScrollUp => (MouseButton::Left, MouseEventType::ScrollUp),
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                return Ok(true);
            }
        };

        // Selection / drag tracking
        // We must handle Down and Drag events for the prompt area INSIDE this match
        // because they return early below and never reach the component dispatch section.
        match (event_type, button) {
            (MouseEventType::Down, MouseButton::Left) => {
                self.mouse_down_pos = Some((x, y));
                self.mouse_drag_active = false;
                self.drag_selection = None;

                // Reset session selection values until we know this is NOT a prompt click.
                // They will be set below for non-prompt clicks.
                self.session_view.selection_anchor_content_y = 0;
                self.session_view.selection_focus_content_y = 0;
                self.session_view.mouse_down_scroll_y = self.session_view.scroll_y;

                // If the click is inside the prompt area, start a text selection.
                // We must skip the session-content-space computation below so that
                // drag_selection / selection_*_content_y never get values from the
                // prompt row (which sits below the session viewport and would cause
                // a spurious full-width highlight bar at the bottom of the session area).
                if matches!(self.mode(), AppMode::Session)
                    && let Some(prompt_area) = self.compute_prompt_area()
                    && x >= prompt_area.x
                    && x < prompt_area.right()
                    && y >= prompt_area.y
                    && y < prompt_area.bottom()
                {
                    self.prompt_view.focus();
                    self.prompt_view.note_activity();
                    if let Some(pos) = self.prompt_view.char_pos_at_mouse(x, y, prompt_area) {
                        self.prompt_view.cursor_pos = pos;
                        self.prompt_view.sel_start = Some(pos);
                        self.prompt_view.sel_end = Some(pos);
                    }
                    return Ok(true);
                }
                // Click outside prompt area → blur for scroll mode
                if matches!(self.mode(), AppMode::Session) {
                    self.prompt_view.blur();
                }

                // Store anchor and focus in content space so the visual highlight
                // moves with content during auto-scroll drag.  Only reached when
                // the click is NOT inside the prompt area.
                if let Some(session_area) = self.session_view.session_area {
                    let vp_top = i32::from(session_area.1);
                    let content_y = (y as i32) - vp_top + self.session_view.mouse_down_scroll_y;
                    self.session_view.selection_anchor_content_y = content_y;
                    self.session_view.selection_focus_content_y = content_y;
                }
            }
            (MouseEventType::Drag, MouseButton::Left) => {
                if self.mouse_down_pos.is_some() {
                    self.mouse_drag_active = true;

                    // Determine whether we are dragging inside the prompt area.
                    // We need prompt_area to be in scope below, so compute it first.
                    let prompt_area = self.compute_prompt_area();
                    let is_prompt_drag = matches!(self.mode(), AppMode::Session)
                        && self.prompt_view.sel_start.is_some()
                        && prompt_area.is_some_and(|pa| y >= pa.y && y < pa.bottom());

                    if !is_prompt_drag {
                        // Update visual selection rectangle (session content).
                        if let Some((sx, sy)) = self.mouse_down_pos {
                            // Store anchor (sx,sy) and focus (x,y) WITHOUT normalising,
                            // so the renderer can apply flow-based selection highlighting.
                            self.drag_selection = Some((sx, sy, x, y));
                            // Store focus in content space so the visual highlight follows
                            // content during auto-scroll drag.
                            if let Some(session_area) = self.session_view.session_area {
                                let vp_top = i32::from(session_area.1);
                                self.session_view.selection_focus_content_y =
                                    (y as i32) - vp_top + self.session_view.scroll_y;
                            }
                        }
                    }

                    // If drag is within the prompt area, extend the text selection.
                    if is_prompt_drag
                        && let Some(pa) = prompt_area
                        && let Some(pos) = self.prompt_view.char_pos_at_mouse(x, y, pa)
                    {
                        self.prompt_view.cursor_pos = pos;
                        self.prompt_view.sel_end = Some(pos);
                    }

                    // Update auto-scroll on selection drag in the session view
                    // (only when NOT dragging in the prompt).
                    if matches!(self.mode(), AppMode::Session) && !is_prompt_drag {
                        self.session_view.update_auto_scroll(x, y);
                    }
                }
                return Ok(true);
            }
            (MouseEventType::Up, MouseButton::Left) => {
                // Stop auto-scroll on any mouse up.
                self.session_view.stop_auto_scroll();
                let _rect = self.drag_selection.take();
                let drag_start = self.mouse_down_pos.take();
                let is_drag =
                    self.mouse_drag_active || drag_start.is_some_and(|(sx, sy)| sx != x || sy != y);
                self.mouse_drag_active = false;

                if is_drag {
                    // Auto-copy prompt selection on mouse release after drag.
                    if self.prompt_view.has_selection() {
                        let text = self.prompt_view.selected_text();
                        selection::copy_selection(&text, &mut self.toast_state);
                        self.prompt_view.clear_selection();
                        return Ok(true);
                    }

                    // Extract selected text from the session view by drag region.
                    if matches!(self.mode(), AppMode::Session)
                        && let Some((sx, sy)) = drag_start
                    {
                        let area = self.terminal_size();
                        let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
                        let main_area = Rect::new(
                            area.x + sidebar_w,
                            area.y,
                            area.width.saturating_sub(sidebar_w),
                            area.height,
                        );
                        let prompt_h = self
                            .prompt_view
                            .required_height(main_area.width.saturating_sub(4));
                        let question_h = if self.question_dialog.visible {
                            self.question_dialog
                                .required_height(main_area.width.saturating_sub(4))
                        } else {
                            0
                        };
                        let spinner_h = u16::from(
                            matches!(self.state.status, SessionStatus::Working)
                                && self.agent_spinner.is_some(),
                        );
                        let footer_y = main_area.bottom().saturating_sub(1);
                        let prompt_area_y = footer_y.saturating_sub(prompt_h);
                        let spinner_area_y = prompt_area_y.saturating_sub(spinner_h);
                        let question_area_y = spinner_area_y.saturating_sub(question_h);
                        let session_bottom = question_area_y.saturating_sub(1);
                        let session_area = Rect::new(
                            main_area.x,
                            area.y + 1,
                            main_area.width,
                            session_bottom.saturating_sub(area.y + 1),
                        );

                        let margin = 2u16;
                        let inner_area = Rect::new(
                            session_area.x + margin,
                            session_area.y,
                            session_area.width.saturating_sub(margin * 2),
                            session_area.height,
                        );
                        let max_w = inner_area.width.saturating_sub(6);

                        if let Some(session) = self.state.current_session() {
                            self.session_view.build_text_regions(
                                session,
                                inner_area,
                                max_w,
                                &self.config,
                                &self.theme,
                            );
                        }

                        // Pass anchor (sx,sy) and focus (x,y) directly for flow selection.
                        let text = self.session_view.get_text_in_region(sx, sy, x, y);
                        if !text.is_empty() {
                            selection::copy_selection(&text, &mut self.toast_state);
                            return Ok(true);
                        }
                    }
                }
            }
            _ => {}
        }

        // Auto-scroll stops on any mouse action (up, scroll, etc.) outside of drag.
        if event_type != MouseEventType::Drag {
            self.session_view.stop_auto_scroll();
        }

        // Mouse wheel scrolling
        // Debounce: ignore scroll events that arrive within 50ms of the last one.
        // Different terminal emulators emit different numbers of events per physical
        // scroll tick (e.g. tmux/kitty emit 2-3, gnome-terminal emits 1). Without
        // debouncing, fast-emitters cause list navigation to skip items.
        let now = Instant::now();
        let scroll_elapsed = now.duration_since(self.last_scroll_time);
        if scroll_elapsed >= Duration::from_millis(50) {
            self.last_scroll_time = now;
            match event_type {
                MouseEventType::ScrollUp => {
                    if let Some(d) = self.dialog.current_mut() {
                        match &d.dialog_type {
                            DialogType::ModelList { .. } => {
                                self.handle_model_dialog_key(KeyCode::Up);
                            }
                            DialogType::ThemeList { .. } => {
                                self.handle_theme_dialog_key(KeyCode::Up);
                            }
                            _ => {}
                        }
                    } else if self.sidebar_focused && self.sidebar.open && x < SIDEBAR_WIDTH {
                        self.sidebar.select_prev(self.state.session_summaries.len());
                    } else if matches!(self.mode(), AppMode::Session)
                        && Self::is_in_right_panel(x, self.terminal_size())
                    {
                        self.state.right_panel.scroll_up(3);
                    } else if matches!(self.mode(), AppMode::Session) {
                        self.session_view.scroll_y = (self.session_view.scroll_y - 3).max(0);
                    } else if matches!(self.mode(), AppMode::Home) {
                        self.home_view.select_prev();
                    } else if matches!(self.mode(), AppMode::InternalTools) {
                        let list_area = 20;
                        self.internal_tools_view.select_prev(list_area);
                    } else if matches!(self.mode(), AppMode::Router) {
                        if self.router_view.focus == FocusTarget::Fallbacks {
                            self.router_view.select_prev_fallback();
                        } else {
                            let all_models = self.collect_cached_models();
                            self.router_view.select_prev(&all_models);
                        }
                    } else if matches!(self.mode(), AppMode::AddProvider) {
                        let list_area = 20;
                        self.add_provider_view.select_prev(list_area);
                    } else if self.try_rag_scroll_up() {
                    }
                    return Ok(true);
                }
                MouseEventType::ScrollDown => {
                    if let Some(d) = self.dialog.current_mut() {
                        match &d.dialog_type {
                            DialogType::ModelList { .. } => {
                                self.handle_model_dialog_key(KeyCode::Down);
                            }
                            DialogType::ThemeList { .. } => {
                                self.handle_theme_dialog_key(KeyCode::Down);
                            }
                            _ => {}
                        }
                    } else if self.sidebar_focused && self.sidebar.open && x < SIDEBAR_WIDTH {
                        self.sidebar.select_next(self.state.session_summaries.len());
                    } else if matches!(self.mode(), AppMode::Session)
                        && Self::is_in_right_panel(x, self.terminal_size())
                    {
                        self.state.right_panel.scroll_down(3);
                    } else if matches!(self.mode(), AppMode::Session) {
                        self.session_view.scroll_y = (self.session_view.scroll_y + 3).max(0);
                    } else if matches!(self.mode(), AppMode::Home) {
                        self.home_view.select_next();
                    } else if matches!(self.mode(), AppMode::InternalTools) {
                        let list_area = 20;
                        self.internal_tools_view.select_next(list_area);
                    } else if matches!(self.mode(), AppMode::Router) {
                        if self.router_view.focus == FocusTarget::Fallbacks {
                            self.router_view.select_next_fallback();
                        } else {
                            let all_models = self.collect_cached_models();
                            self.router_view.select_next(&all_models);
                        }
                    } else if matches!(self.mode(), AppMode::AddProvider) {
                        let list_area = 20;
                        self.add_provider_view.select_next(list_area);
                    } else if self.try_rag_scroll_down() {
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }

        // Only handle left-click UP events (standard "click" action)
        if event_type != MouseEventType::Up || button != MouseButton::Left {
            return Ok(true);
        }

        let mouse = MouseEvent::new(event_type, button, x, y, modifiers);

        // 1. Dialogs (highest z-order)
        if self.dialog.visible() {
            let area = self.terminal_size();
            match self.dialog.handle_mouse(&mouse, area, &self.theme) {
                DialogAction::Confirmed if self.is_confirm_dialog_visible() => {
                    if let Some(d) = self.dialog.current() {
                        if d.selected == 0 {
                            if let Some(session_id) = self.pending_delete_session_id.take() {
                                self.state.remove_session(&session_id);
                                self.session_store.delete_session(&session_id);
                            } else if self.handle_rag_confirm_delete() {
                            } else {
                                self.should_quit = true;
                            }
                        } else {
                            self.pending_delete_session_id = None;
                            self.clear_rag_pending_state();
                        }
                        self.dialog.pop();
                    }
                    return Ok(true);
                }
                DialogAction::Confirmed => {
                    if let Some(d) = self.dialog.current() {
                        match &d.dialog_type {
                            DialogType::ThemeList { .. } => {
                                self.apply_filtered_theme_preview();
                                self.theme_dialog_original = None;
                                // Persist theme choice from the dialog selection
                                let filtered = self.theme_dialog_filtered();
                                if let Some(d) = self.dialog.current() {
                                    let sel = d.selected.min(filtered.len().saturating_sub(1));
                                    if sel < filtered.len() {
                                        self.prefs_cache.finish_revalidation(
                                            "theme".to_string(),
                                            filtered[sel].clone(),
                                        );
                                    }
                                }
                            }
                            DialogType::ModelList { .. } => {
                                let models =
                                    if let DialogType::ModelList { models, .. } = &d.dialog_type {
                                        models.clone()
                                    } else {
                                        vec![]
                                    };
                                let selected_idx = d.selected.min(models.len().saturating_sub(1));
                                if let Some(entry) = models.get(selected_idx) {
                                    self.llm_config.model = Some(entry.model.clone());
                                    self.llm_config.provider.clone_from(&entry.provider);
                                }
                                self.model_dialog_original = None;
                            }
                            DialogType::ApiKeyInput {
                                provider,
                                env_var,
                                input,
                                ..
                            } if !input.is_empty() => {
                                save_provider_api_key(provider, env_var, input);
                                // Invalidate model cache for this provider so the next
                                // dialog open fetches fresh models with the new key.
                                self.model_cache.invalidate(&provider.to_string());
                                // SAFETY: Setting env vars is safe in a single-threaded CLI context
                                unsafe {
                                    std::env::set_var(env_var, input);
                                }
                            }
                            _ => {}
                        }
                    }
                    self.dialog.pop();
                    return Ok(true);
                }
                DialogAction::Dismissed => {
                    // Restore original if needed
                    if self.is_theme_dialog_visible()
                        && let Some(ref orig) = self.theme_dialog_original
                        && let Some(t) = self.theme_registry.get(orig)
                    {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                    if self.is_model_dialog_visible()
                        && let Some(ref orig) = self.model_dialog_original
                    {
                        self.llm_config.model = if orig.is_empty() {
                            None
                        } else {
                            Some(orig.clone())
                        };
                    }
                    self.theme_dialog_original = None;
                    self.model_dialog_original = None;
                    self.dialog.pop();
                    return Ok(true);
                }
                DialogAction::Consumed => {
                    // Selection changed, apply preview for theme dialog
                    if self.is_theme_dialog_visible() {
                        self.apply_filtered_theme_preview();
                    }
                    return Ok(true);
                }
                DialogAction::None => {}
            }
        }

        // 2. Command palette
        if self.command_palette.visible {
            let area = self.terminal_size();
            if self.command_palette.handle_mouse(&mouse, area, &self.theme) {
                return Ok(true);
            }
        }

        // 3. Slash menu
        if self.slash_menu.visible && matches!(self.mode(), AppMode::Session) {
            let is_session = matches!(self.mode(), AppMode::Session);
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let prompt_h = if is_session {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            let prompt_area = Rect::new(
                main_area.x + 2,
                main_area
                    .bottom()
                    .saturating_sub(1)
                    .saturating_sub(prompt_h),
                main_area.width.saturating_sub(4),
                prompt_h,
            );
            if self
                .slash_menu
                .handle_mouse(&mouse, prompt_area, &self.theme)
            {
                self.prompt_view.note_activity();
                if let Some(cmd) = self.slash_menu.get_selected_command() {
                    if cmd.name == "themes" {
                        self.open_theme_dialog();
                    } else if cmd.name == "models" {
                        self.open_model_dialog();
                    } else {
                        let cmd_name = format!("/{} ", cmd.name);
                        self.prompt_view.input = cmd_name;
                        self.prompt_view.cursor_pos = self.prompt_view.input.len();
                    }
                    self.slash_menu.visible = false;
                }
                return Ok(true);
            }
        }

        // 4. Question dialog (inline, between session and prompt)
        if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            // When question dialog is visible, prompt is hidden (like OpenCode)
            let prompt_h = if self.question_dialog.visible {
                0
            } else {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4))
            };
            let question_h = self
                .question_dialog
                .required_height(main_area.width.saturating_sub(4));
            let footer_y = main_area.bottom().saturating_sub(1);
            let prompt_area_y = footer_y.saturating_sub(prompt_h);
            let question_area_y = prompt_area_y.saturating_sub(question_h);
            let question_area = Rect::new(
                main_area.x + 2,
                question_area_y,
                main_area.width.saturating_sub(4),
                question_h,
            );
            // Don't dispatch to question dialog if text selection is in progress
            if !self.mouse_drag_active && self.drag_selection.is_none() {
                let consumed = self.question_dialog.handle_mouse(&mouse, question_area);
                if consumed && self.question_dialog.submitted {
                    let answers = self.question_dialog.build_answers();
                    let _ = self.answer_tx.send(Ok(answers));
                    self.question_dialog.visible = false;
                    self.question_dialog.submitted = false;
                }
                return Ok(true);
            }
        }

        // 5. Permission dialog
        if self.permission_dialog.visible {
            let area = self.terminal_size();
            if self
                .permission_dialog
                .handle_mouse(&mouse, area, &self.theme)
                .is_some()
            {
                return Ok(true);
            }
        }

        // 6. Sidebar
        // Focus management: clicking the sidebar focuses it for scroll;
        // clicking anywhere else unfocuses it.
        if matches!(event_type, MouseEventType::Down) || matches!(event_type, MouseEventType::Up) {
            self.sidebar_focused = self.sidebar.open && x < SIDEBAR_WIDTH;
        }

        if self.sidebar.open {
            let sidebar_area = Rect::new(0, 0, SIDEBAR_WIDTH, self.terminal_height());
            match self.sidebar.handle_mouse(&mouse, sidebar_area, &self.state) {
                SidebarAction::SwitchTo(session_id) => {
                    self.state.right_panel =
                        crate::routes::session::right_panel::types::RightPanelState::new();
                    self.finalize_stale_compaction_lines();
                    self.state
                        .switch_to_session(session_id, &self.session_store);
                    self.finalize_stale_compaction_lines();
                    return Ok(true);
                }
                SidebarAction::RequestDelete(session_id) => {
                    self.pending_delete_session_id = Some(session_id);
                    self.dialog.show(DialogType::Confirm {
                        message: "Delete this session?".into(),
                    });
                    if let Some(d) = self.dialog.current_mut() {
                        d.selected = 1;
                    }
                    return Ok(true);
                }
                SidebarAction::None => {}
            }
        }

        // 7. Session view (tool expand/collapse)
        if matches!(self.mode(), AppMode::Session) {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let prompt_h = self
                .prompt_view
                .required_height(main_area.width.saturating_sub(4));
            let question_h = if self.question_dialog.visible {
                self.question_dialog
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            let spinner_h = u16::from(
                matches!(self.state.status, SessionStatus::Working) && self.agent_spinner.is_some(),
            );
            let footer_y = main_area.bottom().saturating_sub(1);
            let prompt_area_y = footer_y.saturating_sub(prompt_h);
            let spinner_area_y = prompt_area_y.saturating_sub(spinner_h);
            let question_area_y = spinner_area_y.saturating_sub(question_h);
            let session_bottom = question_area_y;
            let session_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                session_bottom.saturating_sub(area.y + 1),
            );
            if self
                .session_view
                .handle_mouse(&mouse, session_area, &self.state, &self.config)
            {
                return Ok(true);
            }
        }

        // 8. Home view (same area computation as render: skip header row + footer)
        if matches!(self.mode(), AppMode::Home) && !self.dialog.visible() {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let footer_y = main_area.bottom().saturating_sub(1);
            let session_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                footer_y.saturating_sub(area.y + 1),
            );
            if let Some(action) = self.home_view.handle_mouse(&mouse, session_area) {
                match action {
                    crate::routes::home::HomeAction::NewSession => {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let id = format!("{now_ms}");
                        let title = format_session_timestamp(now_ms);
                        self.state.add_empty_session(id.clone(), title, now_ms);
                        self.state.current_session_id = Some(id);
                        self.prompt_view.focus();
                    }
                    crate::routes::home::HomeAction::ToggleSidebar => {
                        self.sidebar.open = !self.sidebar.open;
                    }
                    crate::routes::home::HomeAction::OpenInternalTools => {
                        self.show_internal_tools = true;
                    }
                    crate::routes::home::HomeAction::OpenShortcuts => {
                        self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                    }
                    crate::routes::home::HomeAction::OpenAddProvider => {
                        self.show_add_provider = true;
                    }
                    crate::routes::home::HomeAction::OpenModelRouter => {
                        let saved = fallback::load_fallbacks(&self.prefs_cache);
                        self.router_view.set_fallbacks(saved);
                        self.show_router = true;
                    }
                    #[cfg(feature = "embed")]
                    crate::routes::home::HomeAction::OpenRag => {
                        self.show_rag = true;
                    }
                }
                return Ok(true);
            }
        }

        // 8c. Router view — mouse click on a model row adds it to fallback chain
        if matches!(self.mode(), AppMode::Router) && !self.dialog.visible() {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let tools_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                main_area.height.saturating_sub(1),
            );
            let all_models = self.collect_cached_models();
            if self
                .router_view
                .handle_mouse(&all_models, &mouse, tools_area)
            {
                fallback::save_fallbacks(&mut self.prefs_cache, &self.router_view.fallbacks);
                if self.router_view.focus == FocusTarget::Fallbacks
                    && !self.router_view.fallbacks.is_empty()
                {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("Backspace to remove".into()),
                        message: "Select an item and press Backspace".into(),
                        variant: ToastVariant::Info,
                        duration_ms: 3000,
                    });
                }
                return Ok(true);
            }
        }

        // 8b. Internal Tools view — mouse click on a tool row toggles it
        if matches!(self.mode(), AppMode::InternalTools) && !self.dialog.visible() {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let tools_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                main_area.height.saturating_sub(4),
            );
            if let Some(clicked_idx) = self.internal_tools_view.handle_mouse(&mouse, tools_area) {
                self.internal_tools_view.selection.selected_index = clicked_idx;
                self.internal_tools_view.toggle_current();
                crate::routes::tools::save_disabled_tools(
                    &mut self.prefs_cache,
                    &self.internal_tools_view.disabled,
                );
                return Ok(true);
            }
        } // 8ba. Rag view — mouse click on DB list row or Create DB button
        if self.handle_rag_mouse_click(&mouse) {
            return Ok(true);
        }

        // 8c. AddProvider view — mouse click on a provider row opens API key input
        if matches!(self.mode(), AppMode::AddProvider) && !self.dialog.visible() {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let tools_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                main_area.height.saturating_sub(3),
            );
            if let Some(clicked_idx) = self.add_provider_view.handle_mouse(&mouse, tools_area) {
                self.add_provider_view.selection.selected_index = clicked_idx;
                if let Some((provider, env_var)) = self.add_provider_view.selected_provider() {
                    self.dialog.show(DialogType::ApiKeyInput {
                        provider: provider.to_string(),
                        env_var: env_var.to_string(),
                        input: String::new(),
                        cursor_pos: 0,
                    });
                }
                return Ok(true);
            }
        }

        // 9. Prompt area - click/drag to focus and select text
        if matches!(self.mode(), AppMode::Session)
            && let Some(prompt_area) = self.compute_prompt_area()
        {
            if x >= prompt_area.x
                && x < prompt_area.right()
                && y >= prompt_area.y
                && y < prompt_area.bottom()
            {
                self.prompt_view.focus();
                self.prompt_view.note_activity();
                return Ok(true);
            }
            // Click outside prompt → blur for scroll mode
            self.prompt_view.blur();
        }

        Ok(true)
    }

    /// Compute the prompt area rectangle (same calculation as in `render()`).
    /// Must match the render logic exactly so mouse clicks land on the
    /// visual prompt position, including the empty-session centered layout.
    fn compute_prompt_area(&self) -> Option<Rect> {
        if !matches!(self.mode(), AppMode::Session) {
            return None;
        }
        // When question or permission dialog is visible, prompt is hidden
        if self.question_dialog.visible || self.permission_dialog.visible {
            return None;
        }
        let area = self.terminal_size();
        let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };

        let right_panel_w = if matches!(self.mode(), AppMode::Session)
            && (should_show_right_panel(area.width, &self.state.right_panel))
        {
            RIGHT_PANEL_WIDTH
        } else {
            0
        };

        let main_area = Rect::new(
            area.x + sidebar_w,
            area.y,
            area.width.saturating_sub(sidebar_w + right_panel_w),
            area.height,
        );
        let footer_y = main_area.bottom().saturating_sub(1);

        let is_empty_session = self
            .state
            .current_session()
            .is_none_or(|s| s.messages.is_empty());

        let full_w = main_area.width.saturating_sub(4);
        let (prompt_area_x, prompt_area_w) = if is_empty_session {
            let narrow = std::cmp::max(
                EMPTY_SESSION_PROMPT_MIN_WIDTH,
                (main_area.width as f64 * EMPTY_SESSION_PROMPT_RATIO) as u16,
            )
            .min(full_w);
            (main_area.x + (main_area.width - narrow) / 2, narrow)
        } else {
            (main_area.x + 2, full_w)
        };

        let prompt_h = self.prompt_view.required_height(prompt_area_w);

        let logo_block_h = if is_empty_session {
            LOGO_CHAT.len() as u16 + 1
        } else {
            0
        };

        let prompt_area_y = if is_empty_session && prompt_h > 0 {
            let header_y = area.y + 1;
            let total_block_h = logo_block_h + prompt_h;
            let available = footer_y.saturating_sub(header_y);
            let top_spacer = available.saturating_sub(total_block_h) / 2;
            header_y + top_spacer + logo_block_h
        } else {
            footer_y.saturating_sub(prompt_h)
        };

        Some(Rect::new(
            prompt_area_x,
            prompt_area_y,
            prompt_area_w,
            prompt_h,
        ))
    }

    #[allow(clippy::unused_self)]
    fn terminal_size(&self) -> Rect {
        // We don't store the terminal size, but ratatui's Terminal::size is not accessible here.
        // Use a reasonable fallback: assume crossterm's terminal size.
        let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
        Rect::new(0, 0, w, h)
    }

    fn terminal_height(&self) -> u16 {
        self.terminal_size().height
    }
}

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableFocusChange,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    )?;
    // Enable keyboard enhancement on all platforms (Windows Terminal + Kitty protocol)
    // This allows detecting modifier+key combinations like Ctrl+Backspace.
    let _ = crossterm::execute!(
        stdout,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ),
    );
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    let _ = crossterm::execute!(stdout, crossterm::event::PopKeyboardEnhancementFlags,);
    stdout.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}

/// Save a provider API key to `.env` in the project root (CWD).
///
/// NOTE: This is a temporary development-only mechanism. It will be replaced
/// by the `keyring` crate for proper system keychain integration in the future.
fn save_provider_api_key(provider: &str, env_var: &str, api_key: &str) {
    let path = std::path::PathBuf::from(".env");
    let export_line = format!("export {env_var}=\"{api_key}\"\n");
    let comment_line = format!("# cosh: {provider} API key\n");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = write!(file, "\n{comment_line}{export_line}");
    } else {
        // Silently fail - env var is still set for the current process
    }
}

// Embedding helpers (feature-gated)

#[cfg(feature = "embed")]
async fn embed_document(
    uri: &str,
    table_name: &str,
    embedder_config: &crate::routes::rag::models::EmbedderConfig,
    content: &str,
) -> Result<(), String> {
    use cosh_recall::embed::Rag;

    let dim = embedder_config.vector_dim();
    let embedder = match embedder_config {
        crate::routes::rag::models::EmbedderConfig::Local { model } => {
            create_local_embedder(*model)?
        }
        crate::routes::rag::models::EmbedderConfig::Cloud(c) => {
            create_cloud_embedder(&c.provider, &c.model, dim)?
        }
    };

    let rag = Rag::connect(uri, table_name, embedder)
        .await
        .map_err(|e| format!("Failed to connect to database: {e}"))?;

    rag.ingest("content", content)
        .await
        .map_err(|e| format!("Failed to embed content: {e}"))?;

    Ok(())
}

#[cfg(feature = "embed")]
fn create_local_embedder(
    model: crate::routes::rag::models::LocalEmbedModel,
) -> Result<cosh_recall::embed::Embedder, String> {
    use cosh_recall::embed::Embedder;
    let fast_model = model.to_fastembed_model();
    Embedder::try_new_local(fast_model).map_err(|e| format!("Failed to create local embedder: {e}"))
}

#[cfg(all(feature = "embed", feature = "cloud"))]
fn create_cloud_embedder(
    provider: &str,
    model: &str,
    dim: usize,
) -> Result<cosh_recall::embed::Embedder, String> {
    use cosh_recall::embed::Embedder;
    use cosh_sdk::connector::Connector;
    let connector = Connector::new(provider)
        .map_err(|e| format!("Failed to create connector: {e}"))?
        .with_model(model);
    Ok(Embedder::new_cloud(connector, dim))
}

#[cfg(all(feature = "embed", not(feature = "cloud")))]
fn create_cloud_embedder(
    _provider: &str,
    _model: &str,
    _dim: usize,
) -> Result<cosh_recall::embed::Embedder, String> {
    Err("Cloud embedding requires the 'cloud' feature (enable with --features cloud)".into())
}
