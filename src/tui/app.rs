use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

use crate::component::agent_spinner::AgentSpinner;
use crate::component::prompt::PromptView;
use crate::config::{LlmConfig, TuiConfig};
use crate::keymap::KeyMap;
use crate::routes::add_provider::AddProviderView;
use crate::routes::home::{HomeAction, HomeView};
use crate::routes::session::SessionView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::sidebar::SidebarView;
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

const SIDEBAR_WIDTH: u16 = 24;
const FOOTER_HEIGHT: u16 = 1;

enum AppMode {
    Home,
    Session,
    InternalTools,
    AddProvider,
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
    llm_config: LlmConfig,
    stop_signal: Arc<AtomicBool>,
    terminal_focused: bool,
    agent_spinner: Option<AgentSpinner>,
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

    // ── Mouse drag / selection tracking ───────────────────────────────────────────
    /// Position where the mouse was pressed down (for detecting drag selections).
    mouse_down_pos: Option<(u16, u16)>,
    /// Whether a drag-selection is in progress.
    mouse_drag_active: bool,
    /// Visual highlight: anchor (sx,sy) and focus (x,y) — stored without normalisation
    /// so the renderer can apply flow-based selection highlighting (top line from `start_x`
    /// to end, bottom line from start to `end_x`, middle lines fully highlighted).
    drag_selection: Option<(u16, u16, u16, u16)>,

    // ── Auto-scroll on selection drag ─────────────────────────────────────────────
    /// When true, the render loop keeps running even without input events.
    live_requested: bool,
    /// Timestamp of the previous frame (for delta_time calculation).
    last_frame_time: std::time::Instant,
}

impl App {
    pub fn new(cwd: String) -> Self {
        let mut state = AppState::new();
        state.working_directory = cwd;

        // Load persisted sessions from disk
        let session_store = SessionStore::new();
        for summary in session_store.list_sessions() {
            if let Some(session) = session_store.load_session(&summary.session_id) {
                state.add_session(session);
            }
        }

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (answer_tx, _answer_rx) = mpsc::unbounded_channel();

        let theme_registry = ThemeRegistry::new();
        let prefs_cache = crate::util::cache::StaleCache::new("preferences.json");

        // Load saved theme from preferences cache, if available
        let saved_theme: Option<String> = prefs_cache.get(&"theme".to_string()).cloned();
        let theme = saved_theme
            .as_deref()
            .and_then(|name| theme_registry.get(name))
            .cloned()
            .unwrap_or_else(|| theme_registry.default_theme().clone());

        App {
            state,
            theme_registry,
            theme,
            session_view: SessionView::new(),
            home_view: HomeView::new(),
            internal_tools_view: InternalToolsView::new(),
            show_internal_tools: false,
            add_provider_view: AddProviderView::new(),
            show_add_provider: false,
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
            model_cache: crate::util::cache::StaleCache::new("model.json"),
            prefs_cache,
            session_store,
            should_quit: false,
            tokio_handle: Handle::current(),
            event_tx,
            event_rx,
            answer_tx,
            llm_config: LlmConfig::from_env(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            terminal_focused: true,
            agent_spinner: None,
            mouse_down_pos: None,
            mouse_drag_active: false,
            drag_selection: None,
            live_requested: false,
            last_frame_time: std::time::Instant::now(),
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

        if !cached_models.is_empty() {
            self.dialog.replace(DialogType::ModelList {
                models: cached_models,
                current: current.clone(),
                filter: String::new(),
            });
        } else {
            // Show a loading state first
            self.dialog.replace(DialogType::ModelList {
                models: vec![],
                current: current.clone(),
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
            let dialog_tx_clone = dialog_tx.clone();

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

        // Group by provider
        let mut grouped: HashMap<&str, Vec<ModelEntry>> = HashMap::new();
        for entry in models {
            grouped
                .entry(entry.provider.as_str())
                .or_default()
                .push(entry.clone());
        }

        // Update cache for each provider with results
        for (provider, provider_models) in grouped {
            self.model_cache
                .finish_revalidation(provider.to_string(), provider_models);
        }

        // Clear any remaining revalidation flags (providers that failed or
        // returned no results) — preserves old cached data for those providers.
        self.model_cache.clear_all_revalidation();
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

    #[allow(clippy::too_many_lines)]
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

    #[allow(clippy::too_many_lines)]
    fn handle_model_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_model_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Get the models from the dialog
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    // Build filtered indices
                    let filtered_indices: Vec<usize> = if filter.is_empty() {
                        (0..models.len()).collect()
                    } else {
                        let lower = filter.to_lowercase();
                        models
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| m.model.to_lowercase().contains(&lower))
                            .map(|(i, _)| i)
                            .collect()
                    };

                    if !filtered_indices.is_empty()
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        let current_pos =
                            filtered_indices.iter().position(|&i| i == d_mut.selected);
                        if let Some(current_idx) = current_pos {
                            d_mut.selected = if current_idx == 0 {
                                *filtered_indices.last().unwrap()
                            } else {
                                filtered_indices[current_idx - 1]
                            };
                        } else {
                            // If current selection is not in filtered list, select first
                            d_mut.selected = filtered_indices[0];
                        }
                    }
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    // Build filtered indices
                    let filtered_indices: Vec<usize> = if filter.is_empty() {
                        (0..models.len()).collect()
                    } else {
                        let lower = filter.to_lowercase();
                        models
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| m.model.to_lowercase().contains(&lower))
                            .map(|(i, _)| i)
                            .collect()
                    };

                    if !filtered_indices.is_empty()
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        let current_pos =
                            filtered_indices.iter().position(|&i| i == d_mut.selected);
                        if let Some(current_idx) = current_pos {
                            if current_idx + 1 < filtered_indices.len() {
                                d_mut.selected = filtered_indices[current_idx + 1];
                            } else {
                                d_mut.selected = filtered_indices[0];
                            }
                        } else {
                            // If current selection is not in filtered list, select first
                            d_mut.selected = filtered_indices[0];
                        }
                    }
                }
                true
            }
            KeyCode::Enter => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, .. } = &d.dialog_type
                    && !models.is_empty()
                {
                    let selected_idx = d.selected.min(models.len().saturating_sub(1));
                    let selected_entry = &models[selected_idx];
                    self.llm_config.model = Some(selected_entry.model.clone());
                    self.llm_config.provider = selected_entry.provider.clone();
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
                let _is_empty = {
                    let Some(d) = self.dialog.current_mut() else {
                        return true;
                    };
                    let DialogType::ModelList { filter, .. } = &mut d.dialog_type else {
                        return true;
                    };
                    filter.pop();
                    d.selected = 0;
                    d.cursor.note_activity();
                    filter.is_empty()
                };
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

    fn mode(&self) -> AppMode {
        if self.show_internal_tools {
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

        while !self.should_quit {
            let now = std::time::Instant::now();
            let delta = now.duration_since(self.last_frame_time);
            self.last_frame_time = now;
            let delta_secs = delta.as_secs_f64();

            terminal.draw(|frame| {
                self.render(frame, delta_secs);
            })?;

            if self.live_requested {
                // When auto-scroll is active, don't block on event::poll.
                if event::poll(Duration::from_millis(8))? {
                    if self.handle_events()? {
                        break;
                    }
                }
            } else if self.handle_events()? {
                break;
            }

            self.poll_events();
        }

        restore_terminal()?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, frame: &mut Frame<'_>, delta_time: f64) {
        // Sync live_requested from session_view auto-scroll state.
        if self.session_view.is_auto_scrolling {
            self.live_requested = true;
        } else {
            self.live_requested = false;
        }
        let area = frame.area();

        {
            let buf = frame.buffer_mut();

            let bg_color = rgba_color(self.theme.background);
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(Style::default().bg(bg_color));
                        cell.set_char(' ');
                    }
                }
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

            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );

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

            let prompt_h = if is_session {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4))
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

            let prompt_area_y = footer_y.saturating_sub(prompt_h);

            // Spinner line (1 row when the agent loop is active)
            let spinner_h = u16::from(
                is_session
                    && self.state.status == crate::types::SessionStatus::Working
                    && self.agent_spinner.is_some(),
            );
            let spinner_area_y = prompt_area_y.saturating_sub(spinner_h);

            // Question dialog inline (between messages and spinner), only during session
            let question_area_y = spinner_area_y.saturating_sub(question_h);
            let session_bottom = question_area_y;

            let prompt_area = Rect::new(
                main_area.x + 2,
                prompt_area_y,
                main_area.width.saturating_sub(4),
                prompt_h,
            );
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
                AppMode::Session => {
                    self.prompt_view.focus();
                    self.prompt_view.cursor.terminal_focused = self.terminal_focused;
                    self.session_view.drag_selection = self.drag_selection;
                    self.session_view.tool_state.advance_spinner();

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
                    // Question dialog rendered inline between messages and prompt (like OpenCode)
                    if self.question_dialog.visible {
                        self.question_dialog.render(buf, question_area, &self.theme);
                    }
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

            let show_home = !matches!(self.mode(), AppMode::InternalTools | AppMode::AddProvider);
            FooterView::render_with_mode(
                buf,
                Rect::new(main_area.x, footer_y, main_area.width, 1),
                &self.state,
                &self.theme,
                show_home,
            );
            let now = std::time::SystemTime::now();
            self.toast_state.render(buf, area, &self.theme);
            // Sync terminal_focused to the dialog cursor so ThemeList/ModelList/ApiKeyInput
            // all respect the terminal focus state (blur when user clicks outside).
            if let Some(d) = self.dialog.current_mut() {
                d.cursor.terminal_focused = self.terminal_focused;
            }
            self.dialog.render(buf, area, &self.theme, now);
            self.permission_dialog.render(buf, area, &self.theme);
            self.command_palette.render(buf, area, &self.theme);
            self.slash_menu.render(buf, prompt_area, &self.theme);
        }
    }

    #[allow(clippy::too_many_lines)]
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

                    // Shift/Ctrl/Alt+Enter inserts a newline instead of sending.
                    if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::NONE {
                        self.prompt_view.note_activity();
                        let pos = self.prompt_view.cursor_pos;
                        self.prompt_view.input.insert(pos, '\n');
                        self.prompt_view.cursor_pos = pos + 1;
                        return Ok(false);
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
                                        self.state.add_session(crate::types::Session {
                                            id,
                                            title,
                                            created_at: now_ms,
                                            messages: vec![],
                                        });
                                        self.state.current_session_id =
                                            Some(self.state.sessions.last().unwrap().id.clone());
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
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.show_internal_tools = false;
                                return Ok(false);
                            }
                            _ => {}
                        }
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
                            self.session_view.scroll_y = (self.session_view.scroll_y - 3).max(0);
                        }
                        Some(crate::keymap::Action::ScrollDown) => {
                            self.session_view.scroll_y = (self.session_view.scroll_y + 3).max(0);
                        }
                        Some(crate::keymap::Action::ScrollUpPage) => {
                            self.session_view.scroll_y = (self.session_view.scroll_y - 10).max(0);
                        }
                        Some(crate::keymap::Action::ScrollDownPage) => {
                            self.session_view.scroll_y = (self.session_view.scroll_y + 10).max(0);
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
                                    self.should_quit = true;
                                } else {
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
                                self.state.add_session(crate::types::Session {
                                    id: id.clone(),
                                    title,
                                    created_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis()
                                        as u64,
                                    messages: vec![],
                                });
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
                            }

                            self.stop_signal.store(false, Ordering::Relaxed);
                            self.state.status = crate::types::SessionStatus::Working;
                            self.agent_spinner = Some(AgentSpinner::new("Working", &self.theme));

                            let event_tx = self.event_tx.clone();
                            let provider = self.llm_config.provider.clone();
                            let model = self.llm_config.model.clone();
                            let stop_signal = self.stop_signal.clone();
                            let input = msg;
                            let cwd = self.state.working_directory.clone();
                            let mode = self.state.mode;

                            // Create a fresh answer channel for this agent loop invocation
                            let (answer_tx, answer_rx) = mpsc::unbounded_channel();
                            self.answer_tx = answer_tx;

                            let disabled_tools = self.internal_tools_view.disabled.clone();

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

                                        let connector = match Connector::new(&provider) {
                                            Ok(c) => c,
                                            Err(e) => {
                                                let _ = event_tx.send(HarnessEvent::Error(
                                                    format!("connector: {e}"),
                                                ));
                                                return;
                                            }
                                        };

                                        let connector = if let Some(ref m) = model {
                                            connector.with_model(m)
                                        } else {
                                            connector
                                        };

                                        let mut harness =
                                            Harness::new(connector, &cwd, disabled_tools)
                                                .with_mode(mode)
                                                .with_history(&history);
                                        harness.format_header_context();
                                        harness
                                            .run_agent_loop(
                                                &input,
                                                event_tx,
                                                answer_rx,
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
                            } else if self.permission_dialog.visible {
                                self.permission_dialog.visible = false;
                            } else if self.dialog.visible() {
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
                            } else if self.permission_dialog.visible {
                                self.permission_dialog.visible = false;
                            } else if self.dialog.visible() {
                                self.dialog.pop();
                            } else if matches!(self.mode(), AppMode::Session) {
                                self.state.current_session_id = None;
                            } else if matches!(self.mode(), AppMode::AddProvider) {
                                self.show_add_provider = false;
                            } else if matches!(self.mode(), AppMode::Home) {
                                self.dialog.show(DialogType::Confirm {
                                    message: "Quit cosh?".into(),
                                });
                                if let Some(d) = self.dialog.current_mut() {
                                    d.selected = 1;
                                }
                            }
                        }
                        Some(crate::keymap::Action::ScrollToTop) => {
                            self.session_view.scroll_y = 0;
                        }
                        Some(crate::keymap::Action::ScrollToBottom) => {
                            self.session_view.scroll_y = self.state.max_scroll();
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
                            use cosh::harness::Mode;
                            self.state.mode = match self.state.mode {
                                Mode::Build => Mode::Ask,
                                Mode::Ask => Mode::Build,
                            };
                        }
                        Some(crate::keymap::Action::HistoryUp) => {
                            self.prompt_view.note_activity();
                            self.prompt_view.history_up();
                        }
                        Some(crate::keymap::Action::HistoryDown) => {
                            self.prompt_view.note_activity();
                            self.prompt_view.history_down();
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
                                        if self.prompt_view.input.is_empty() {
                                            self.session_view.scroll_y =
                                                (self.session_view.scroll_y - 3).max(0);
                                        } else {
                                            self.prompt_view.note_activity();
                                            self.prompt_view.cursor_up(
                                                self.prompt_view.input_text_width.get().max(1),
                                            );
                                        }
                                    }
                                    KeyCode::Down => {
                                        if self.prompt_view.input.is_empty() {
                                            self.session_view.scroll_y =
                                                (self.session_view.scroll_y + 3).max(0);
                                        } else {
                                            self.prompt_view.note_activity();
                                            self.prompt_view.cursor_down(
                                                self.prompt_view.input_text_width.get().max(1),
                                            );
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
                                                self.prompt_view.cursor_pos = self
                                                    .prompt_view
                                                    .input
                                                    .floor_char_boundary(
                                                        self.prompt_view.cursor_pos + 1,
                                                    )
                                                    .min(len);
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
                                        self.prompt_view.note_activity();
                                        let pos = self.prompt_view.cursor_pos;
                                        let len = self.prompt_view.input.len();
                                        if pos < len {
                                            let next = self
                                                .prompt_view
                                                .input
                                                .floor_char_boundary(pos + 1)
                                                .min(len);
                                            self.prompt_view.input.drain(pos..next);
                                        }
                                    }
                                    KeyCode::PageUp => {
                                        self.session_view.scroll_y =
                                            (self.session_view.scroll_y - 10).max(0);
                                    }
                                    KeyCode::PageDown => {
                                        self.session_view.scroll_y =
                                            (self.session_view.scroll_y + 10).max(0);
                                    }
                                    KeyCode::Backspace => {
                                        // Ctrl+Backspace = delete word before cursor
                                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                                            self.prompt_view.delete_word_before_cursor();
                                        } else {
                                            self.prompt_view.note_activity();
                                            let pos = self.prompt_view.cursor_pos;
                                            if pos > 0 {
                                                // Use floor_char_boundary to safely handle multi-byte chars
                                                // (e.g. á, é, emoji). remove() panics if called at a
                                                // non-char-boundary position.
                                                let char_start = self
                                                    .prompt_view
                                                    .input
                                                    .floor_char_boundary(pos - 1);
                                                self.prompt_view.input.remove(char_start);
                                                self.prompt_view.cursor_pos = char_start;
                                            }
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
                                            self.session_view.scroll_y =
                                                (self.session_view.scroll_y + 3).max(0);
                                            return Ok(false);
                                        }
                                        if ch == 'k' && self.prompt_view.input.is_empty() {
                                            self.session_view.scroll_y =
                                                (self.session_view.scroll_y - 3).max(0);
                                            return Ok(false);
                                        }

                                        // Insert character normally
                                        let pos = self.prompt_view.cursor_pos;
                                        self.prompt_view.input.insert(pos, ch);
                                        // Use len_utf8() so cursor stays on a valid UTF-8 boundary
                                        // for multi-byte chars (e.g. á, é, emoji).
                                        self.prompt_view.cursor_pos = pos + ch.len_utf8();

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
                } else {
                    self.prompt_view.note_activity();
                    // Strip newlines/carriage returns so paste doesn't trigger submission
                    let cleaned: String =
                        text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                    let pos = self.prompt_view.cursor_pos;
                    self.prompt_view.input.insert_str(pos, &cleaned);
                    self.prompt_view.cursor_pos = pos + cleaned.len();
                    self.slash_menu.update(&self.prompt_view.input);
                }
            }
            Event::Mouse(crossterm_mouse) => {
                self.handle_mouse_event(crossterm_mouse)?;
            }
        }

        Ok(false)
    }

    #[allow(clippy::too_many_lines)]
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
                }

                HarnessEvent::ToolResult { output } => {
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                        {
                            tp.status = ToolStatus::Completed;
                            tp.output = Some(output.clone());
                            break;
                        }
                    }
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
                }

                HarnessEvent::Reasoning { text } => {
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    let part = Part::Reasoning(ReasoningPart {
                        text: text.clone(),
                        collapsed: true,
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
                }

                HarnessEvent::Done => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;

                    // Persist session to disk if it has valid dialog
                    if let Some(session) = self.state.current_session()
                        && is_valid_session(session)
                    {
                        self.session_store.save_session(session);
                    }
                }

                HarnessEvent::Stopped => {
                    self.state.status = SessionStatus::Idle;
                    self.agent_spinner = None;
                    self.toast_state.show(ToastOptions {
                        title: Some("Interrupted".into()),
                        message: "Agent loop was stopped.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 3000,
                    });

                    // Persist session to disk even when stopped (partial dialog is still valuable)
                    if let Some(session) = self.state.current_session()
                        && is_valid_session(session)
                    {
                        self.session_store.save_session(session);
                    }
                }

                HarnessEvent::Error(msg) => {
                    self.state.status = SessionStatus::Retry {
                        message: msg.clone(),
                        action: None,
                    };
                    self.agent_spinner = None;
                    self.toast_state.show(ToastOptions {
                        title: Some("Error".into()),
                        message: msg.clone(),
                        variant: ToastVariant::Error,
                        duration_ms: 5000,
                    });

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

                    // Update the dialog with the loaded models
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ModelList {
                            models: dialog_models,
                            current: dialog_current,
                            ..
                        } = &mut d.dialog_type
                    {
                        *dialog_models = models;
                        *dialog_current = current;
                    }
                }

                HarnessEvent::QuestionRequest { questions } => {
                    // Show the question dialog with real questions from the harness
                    self.question_dialog.show_questions(questions);
                }
            }
        }
    }

    /// Handle a crossterm mouse event by converting it to a cosh-tui `MouseEvent`
    /// and dispatching to the appropriate component based on current layout.
    #[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
    fn handle_mouse_event(&mut self, evt: CrosstermMouseEvent) -> io::Result<bool> {
        let x = evt.column;
        let y = evt.row;

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

        // ── Selection / drag tracking ─────────────────────────────────────────
        // We must handle Down and Drag events for the prompt area INSIDE this match
        // because they return early below and never reach the component dispatch section.
        match (event_type, button) {
            (MouseEventType::Down, MouseButton::Left) => {
                self.mouse_down_pos = Some((x, y));
                self.mouse_drag_active = false;
                self.drag_selection = None;

                // If the click is inside the prompt area, start a text selection.
                if matches!(self.mode(), AppMode::Session)
                    && let Some(prompt_area) = self.compute_prompt_area()
                    && x >= prompt_area.x
                    && x < prompt_area.right()
                    && y >= prompt_area.y
                    && y < prompt_area.bottom()
                {
                    self.prompt_view.focus();
                    if let Some(pos) = self.prompt_view.char_pos_at_mouse(x, y, prompt_area) {
                        self.prompt_view.cursor_pos = pos;
                        self.prompt_view.sel_start = Some(pos);
                        self.prompt_view.sel_end = Some(pos);
                    }
                    return Ok(true);
                }
            }
            (MouseEventType::Drag, MouseButton::Left) => {
                if self.mouse_down_pos.is_some() {
                    self.mouse_drag_active = true;
                    // Update visual selection rectangle.
                    if let Some((sx, sy)) = self.mouse_down_pos {
                        // Store anchor (sx,sy) and focus (x,y) WITHOUT normalising,
                        // so the renderer can apply flow-based selection highlighting.
                        self.drag_selection = Some((sx, sy, x, y));
                    }
                    // If drag is within the prompt area, extend the text selection.
                    if matches!(self.mode(), AppMode::Session)
                        && self.prompt_view.sel_start.is_some()
                        && let Some(prompt_area) = self.compute_prompt_area()
                        && y >= prompt_area.y
                        && y < prompt_area.bottom()
                        && let Some(pos) = self.prompt_view.char_pos_at_mouse(x, y, prompt_area)
                    {
                        self.prompt_view.cursor_pos = pos;
                        self.prompt_view.sel_end = Some(pos);
                    }

                    // Update auto-scroll on selection drag in the session view.
                    if matches!(self.mode(), AppMode::Session) {
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

        // ── Mouse wheel scrolling ───────────────────────────────────────────
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
                } else if matches!(self.mode(), AppMode::Session) {
                    self.session_view.scroll_y = (self.session_view.scroll_y - 3).max(0);
                } else if matches!(self.mode(), AppMode::AddProvider) {
                    let list_area = 20;
                    self.add_provider_view.select_prev(list_area);
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
                } else if matches!(self.mode(), AppMode::Session) {
                    self.session_view.scroll_y = (self.session_view.scroll_y + 3).max(0);
                } else if matches!(self.mode(), AppMode::AddProvider) {
                    let list_area = 20;
                    self.add_provider_view.select_next(list_area);
                }
                return Ok(true);
            }
            _ => {}
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
                            self.should_quit = true;
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
            let prompt_h = self
                .prompt_view
                .required_height(main_area.width.saturating_sub(4));
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
            let consumed = self.question_dialog.handle_mouse(&mouse, question_area);
            if consumed {
                if self.question_dialog.submitted {
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
        if self.sidebar.open {
            let sidebar_area = Rect::new(0, 0, SIDEBAR_WIDTH, self.terminal_height());
            if let Some(session_id) = self.sidebar.handle_mouse(&mouse, sidebar_area, &self.state) {
                self.state.current_session_id = Some(session_id);
                return Ok(true);
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
                        let title = format_session_timestamp(now_ms);
                        self.state.add_session(crate::types::Session {
                            id: format!("{now_ms}"),
                            title,
                            created_at: now_ms,
                            messages: vec![],
                        });
                        self.state.current_session_id =
                            Some(self.state.sessions.last().unwrap().id.clone());
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
                main_area.height.saturating_sub(3),
            );
            if let Some(clicked_idx) = self.internal_tools_view.handle_mouse(&mouse, tools_area) {
                self.internal_tools_view.selected_index = clicked_idx;
                self.internal_tools_view.toggle_current();
                return Ok(true);
            }
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
                self.add_provider_view.selected_index = clicked_idx;
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
            let footer_y = main_area.bottom().saturating_sub(1);
            let prompt_area_y = footer_y.saturating_sub(prompt_h);
            let prompt_area = Rect::new(
                main_area.x + 2,
                prompt_area_y,
                main_area.width.saturating_sub(4),
                prompt_h,
            );
            if x >= prompt_area.x
                && x < prompt_area.right()
                && y >= prompt_area.y
                && y < prompt_area.bottom()
            {
                self.prompt_view.focus();
                return Ok(true);
            }
        }

        Ok(true)
    }

    /// Compute the prompt area rectangle (same calculation as in `render()`).
    fn compute_prompt_area(&self) -> Option<Rect> {
        if !matches!(self.mode(), AppMode::Session) {
            return None;
        }
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
        let footer_y = main_area.bottom().saturating_sub(1);
        let prompt_area_y = footer_y.saturating_sub(prompt_h);
        Some(Rect::new(
            main_area.x + 2,
            prompt_area_y,
            main_area.width.saturating_sub(4),
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

/// Save a provider API key to the user's shell profile for persistence.
/// Delegates security to the OS by writing to the shell config file.
fn save_provider_api_key(provider: &str, env_var: &str, api_key: &str) {
    // Determine shell config file from $SHELL environment variable
    let shell = std::env::var("SHELL").unwrap_or_default();
    let config_file: Option<std::path::PathBuf> = if shell.ends_with("zsh") {
        std::env::var("ZDOTDIR").ok().map_or_else(
            || {
                Some(
                    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                        .join(".zshrc"),
                )
            },
            |zd| Some(std::path::PathBuf::from(zd).join(".zshrc")),
        )
    } else if shell.ends_with("bash") {
        Some(std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".bashrc"))
    } else if shell.ends_with("fish") {
        Some(
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".config/fish/config.fish"),
        )
    } else {
        // Fallback to .profile
        Some(std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".profile"))
    };

    if let Some(path) = config_file {
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
}
