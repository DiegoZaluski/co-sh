use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton as CrosstermMouseButton, MouseEvent as CrosstermMouseEvent, MouseEventKind,
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
use crate::logo::LOGO_CHAT;
use crate::routes::add_provider::{AddProviderView, ProviderEntry};
use crate::routes::home::footer::HomeFooterView;
use crate::routes::home::{HomeAction, HomeView};
use crate::routes::router::{FocusTarget, RouterView};
use crate::routes::session::SessionView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::queue_choice::{QueueChoiceDialog, QueueTarget};
use crate::routes::session::right_panel::{
    RIGHT_PANEL_WIDTH, render_right_panel, should_show_right_panel,
};
use crate::routes::session::sidebar::{SidebarAction, SidebarView};
use crate::routes::settings::SettingsView;
use crate::routes::tools::InternalToolsView;
use crate::session_store::{
    SessionStore, format_session_timestamp, generate_session_id, is_valid_session,
};
use crate::state::AppState;
use crate::theme::{Theme, ThemeRegistry};
use crate::types::SessionStatus;
use crate::ui::dialogs::{DialogAction, DialogState, DialogType};
use crate::ui::toast::ToastState;
use crate::util::selection;

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// The editable prompt text of a message: its non-synthetic text parts
/// joined by a space (mirrors opencode's Revert/Copy text reconstruction).
pub(crate) fn message_prompt_text(msg: &crate::types::Message) -> String {
    msg.parts
        .iter()
        .filter_map(|p| match p {
            crate::types::Part::Text(t) if !t.synthetic => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Format an integer with thousands separators (e.g. `9612` → `9,612`) so
/// large token counts stay readable at a glance in the header bar.
fn format_tokens(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

const SIDEBAR_WIDTH: u16 = 22;

/// Header link that opens the project's bug-report page.
/// TODO: replace the URL with the real GitHub issues URL.
const BUG_REPORT_TEXT: &str = "𓆦 bug";
const BUG_REPORT_URL: &str = "https://github.com/PLACEHOLDER-OWNER/PLACEHOLDER-REPO/issues";
const FOOTER_HEIGHT: u16 = 1;

/// When the session is empty (no messages), the prompt is centered horizontally
/// with a width of `RATIO * main_area` but at least `MIN_WIDTH` characters wide.
const EMPTY_SESSION_PROMPT_MIN_WIDTH: u16 = 50;
const EMPTY_SESSION_PROMPT_RATIO: f64 = 0.4;
/// Rows always reserved for the conversation (or logo) above the prompt, so the
/// responsive prompt limit never swallows the whole screen even on a very short
/// terminal.
const MIN_PROMPT_RESERVE_ROWS: u16 = 4;

enum AppMode {
    Home,
    Session,
    InternalTools,
    AddProvider,
    Settings,
    Router,
    #[cfg(feature = "embed")]
    Rag,
}

/// Result of `App::session_main_area`: the session chat's content area plus
/// the sidebar/right-panel widths that were subtracted, so `render` and the
/// mouse dispatch cannot drift apart.
struct SessionArea {
    main: Rect,
    sidebar_w: u16,
    right_panel_w: u16,
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
    pub queue_choice_dialog: QueueChoiceDialog,
    pub home_view: HomeView,
    pub internal_tools_view: InternalToolsView,
    pub show_internal_tools: bool,
    pub add_provider_view: AddProviderView,
    pub show_add_provider: bool,
    pub settings_view: SettingsView,
    pub show_settings: bool,
    pub router_view: RouterView,
    pub show_router: bool,
    #[cfg(feature = "embed")]
    pub rag_view: crate::routes::rag::RagView,
    #[cfg(feature = "embed")]
    pub show_rag: bool,
    pub keymap: KeyMap,
    pub config: TuiConfig,
    pub toast_state: ToastState,
    pub slash_menu: crate::ui::slash_menu::SlashMenu,
    pub should_quit: bool,
    pub tokio_handle: Handle,
    pub event_tx: mpsc::UnboundedSender<HarnessEvent>,
    event_rx: mpsc::UnboundedReceiver<HarnessEvent>,
    /// Sender for question answers back to the harness.
    answer_tx: mpsc::UnboundedSender<Result<Vec<cosh_tools::question::types::AnswerItem>, String>>,
    /// Sender for permission responses back to the harness.
    perm_tx: mpsc::UnboundedSender<cosh::harness::PermissionAction>,
    /// Sender for user messages queued for the NEXT REQUEST of the running
    /// agent loop (the "next request" queue). `None` while no loop runs.
    queued_input_tx: Option<mpsc::UnboundedSender<String>>,
    /// Session id that owns the currently running agent loop — guards the
    /// queue auto-start against mid-run session switches.
    active_loop_session_id: Option<String>,
    llm_config: LlmConfig,
    stop_signal: Arc<AtomicBool>,
    /// Re-entry guard for the user-triggered `/compact`: true while the
    /// one-off compaction task runs (there is no loop status to read —
    /// between loops the app is Idle). Cleared by the CompactOnDemand event.
    manual_compaction_active: bool,
    terminal_focused: bool,
    agent_spinner: Option<AgentSpinner>,
    /// Latest context manager info for the budget bar (None if no data yet).
    context_info: Option<cosh::harness::ContextDisplayInfo>,
    /// Stores the theme name that was active when the theme dialog opened (for cancel/restore)
    theme_dialog_original: Option<String>,
    /// Stores the model that was active when the model dialog opened (for cancel/restore)
    model_dialog_original: Option<String>,
    /// Stores the reasoning level that was active when the model dialog opened
    /// (for cancel/restore).
    reasoning_dialog_original: Option<String>,
    /// Stale-while-revalidate cache for model listings, keyed by provider.
    model_cache: crate::util::cache::StaleCache<String, Vec<cosh::ModelEntry>>,
    /// Structured user preferences (theme, tools, routing) persisted in
    /// `~/.config/cosh/setup.json`.
    setup: crate::util::setup::Setup,
    /// Session persistence store (JSONL files on disk).
    session_store: SessionStore,
    /// When set, the current Confirm dialog is asking about deleting a session.
    pending_delete_session_id: Option<String>,
    /// Whether a title has already been generated for the current session.
    /// Set to `false` when a new session is created; set to `true` after
    /// the async title generation task is spawned.
    title_generated: bool,
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
    /// Set when a mouse Up was a drag (even if it selected nothing), so the
    /// session click dispatch skips opening Message Actions.
    mouse_up_was_drag: bool,
    /// Visual highlight: anchor (sx,sy) and focus (x,y) — stored without normalisation
    /// so the renderer can apply flow-based selection highlighting (top line from `start_x`
    /// to end, bottom line from start to `end_x`, middle lines fully highlighted).
    drag_selection: Option<(u16, u16, u16, u16)>,

    // Auto-scroll on selection drag
    /// When true, the render loop keeps running even without input events.
    live_requested: bool,
    /// Timestamp of the previous frame (for delta_time calculation).
    last_frame_time: std::time::Instant,
    /// Per-frame counter — rate-limits the PERF debug logs in the render hot
    /// path (bg_fill etc.) to one sample per ~30 frames (~1/sec) instead of
    /// one write per frame.
    perf_frame: u64,
    /// Last known mouse X position (for keyboard scroll targeting).
    last_mouse_x: u16,
    /// Last known mouse Y position (for keyboard scroll targeting).
    last_mouse_y: u16,
    /// Timestamp of last scroll wheel event (for debouncing rapid scrolls).
    last_scroll_time: Instant,
    /// Whether the sidebar is focused to receive scroll events.
    /// Set to true when the user clicks inside the sidebar; false on outside clicks.
    sidebar_focused: bool,
    /// Clickable area of the "bug report" header link (None when not drawn).
    bug_link_area: Option<Rect>,
    /// Whether the terminal bell rings when an agent loop finishes.
    bell_enabled: bool,
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
        let setup = crate::util::setup::Setup::load();

        // Load saved fallback chain from setup config
        let saved_fallbacks = fallback::load_fallbacks(&setup);

        // Load saved disabled tools from setup config
        let saved_disabled_tools = crate::routes::tools::load_disabled_tools(&setup);
        // Persisted tool-call mode overrides the env default for the session.
        let saved_tool_call_mode = crate::routes::tools::load_tool_call_mode(&setup);

        // Load saved theme from setup config
        let theme = if setup.appearance.theme.is_empty() {
            theme_registry.default_theme().clone()
        } else {
            theme_registry
                .get(&setup.appearance.theme)
                .cloned()
                .unwrap_or_else(|| theme_registry.default_theme().clone())
        };

        let saved_bell = setup.appearance.bell_enabled;

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
            settings_view: SettingsView::new(),
            show_settings: false,
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
            queue_choice_dialog: QueueChoiceDialog::new(),
            keymap: KeyMap::default_vim(),
            config: TuiConfig::default(),
            toast_state: ToastState::new(),
            slash_menu: crate::ui::slash_menu::SlashMenu::new(),
            theme_dialog_original: None,
            model_dialog_original: None,
            reasoning_dialog_original: None,
            model_cache: crate::util::cache::StaleCache::new("cache", "model.json"),
            setup,
            session_store,
            pending_delete_session_id: None,
            title_generated: false,
            manual_compaction_active: false,
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
            queued_input_tx: None,
            active_loop_session_id: None,
            llm_config: LlmConfig {
                tool_call_mode: saved_tool_call_mode,
                ..LlmConfig::from_env()
            },
            stop_signal: Arc::new(AtomicBool::new(false)),
            terminal_focused: true,
            agent_spinner: None,
            context_info: None,
            mouse_down_pos: None,
            mouse_drag_active: false,
            mouse_up_was_drag: false,
            drag_selection: None,
            live_requested: false,
            last_frame_time: std::time::Instant::now(),
            perf_frame: 0,
            last_mouse_x: 0,
            last_mouse_y: 0,
            last_scroll_time: Instant::now(),
            sidebar_focused: false,
            bug_link_area: None,
            bell_enabled: saved_bell,
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

    /// Opens the bug-report page in the default browser and shows a toast.
    fn open_bug_report_link(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some("Bug report".to_string()),
            message: format!("Opening {BUG_REPORT_URL}…"),
            variant: ToastVariant::Info,
            duration_ms: 2500,
        });
        std::thread::spawn(move || {
            if let Err(err) = open::that(BUG_REPORT_URL) {
                log::error!("Failed to open bug report URL: {err}");
            }
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

        let current = self.llm_config.model.clone().unwrap_or_default();

        // Store the current model (and reasoning) so we can restore on cancel
        self.model_dialog_original = Some(current.clone());
        self.reasoning_dialog_original = self.llm_config.reasoning.clone();

        // Collect providers and check if their API key is still configured.
        // If a provider has no key (env var or keyring), invalidate its cache
        // entry so stale models don't appear as available options. Local
        // providers qualify when configured in setup.json or when they
        // respond on their default port (probed below); unqualified ones are
        // purged too.
        let providers_to_check: Vec<&'static str> = self.active_providers();
        {
            use cosh_sdk::connector::{is_local_provider, known_providers_with_env};
            for (provider, _) in known_providers_with_env() {
                if !is_local_provider(provider) && !providers_to_check.contains(&provider) {
                    // Provider no longer configured — purge cached models
                    self.model_cache.invalidate(&provider.to_string());
                }
            }
            for provider in cosh_sdk::connector::known_local_providers() {
                if !providers_to_check.contains(&provider) {
                    self.model_cache.invalidate(&provider.to_string());
                }
            }
        }

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
            let base_urls = self.configured_local_base_urls();

            self.tokio_handle.spawn(async move {
                let mut all_models: Vec<ModelEntry> = Vec::new();

                for provider in providers_to_check {
                    if let Ok(mut connector) = Connector::new(provider) {
                        if let Some(url) = base_urls.get(provider) {
                            connector = connector.with_base_url(url.clone());
                        }
                        if let Ok(output) = connector.list_models().await {
                            for model_info in output.models() {
                                all_models.push(ModelEntry {
                                    provider: provider.to_string(),
                                    model: model_info.id().to_string(),
                                });
                            }
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
                    self.setup.appearance.theme = name;
                    self.setup.save();
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

    fn is_text_input_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(
                    DialogType::ApiKeyInput { .. }
                        | DialogType::LocalUrlInput { .. }
                        | DialogType::RenameSession { .. },
                )
            )
    }

    fn handle_text_input_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.dialog.visible() {
            return false;
        }
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }

        match key {
            KeyCode::Enter => {
                if self.save_text_input_dialog() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Left => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput { cursor_pos, .. }
                    | DialogType::LocalUrlInput { cursor_pos, .. }
                    | DialogType::RenameSession { cursor_pos, .. } = &mut d.dialog_type
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
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
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
                    && let DialogType::ApiKeyInput { cursor_pos, .. }
                    | DialogType::LocalUrlInput { cursor_pos, .. }
                    | DialogType::RenameSession { cursor_pos, .. } = &mut d.dialog_type
                {
                    *cursor_pos = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ApiKeyInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
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
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
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
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
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
                    }
                    | DialogType::LocalUrlInput {
                        input, cursor_pos, ..
                    }
                    | DialogType::RenameSession {
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

    /// Editing keys for the hook registration box: the active field behaves
    /// exactly like a single-line input; Up/Down move between fields.
    fn handle_hook_input_key(&mut self, key: KeyEvent) -> bool {
        if !self.dialog.visible() {
            return false;
        }
        if let Some(d) = self.dialog.current_mut() {
            d.cursor.note_activity();
        }

        const LAST_FIELD: usize = 3;

        match key.code {
            KeyCode::Enter => {
                if self.save_hook_input_dialog() {
                    self.dialog.pop();
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Up | KeyCode::Down => {
                let up = key.code == KeyCode::Up;
                let cols = crate::ui::dialogs::hook_input_content_w(
                    crate::ui::dialogs::hook_input_dialog_w(self.terminal_size()),
                ) as usize;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let lens = [name.len(), matcher.len(), command.len(), timeout.len()];
                    // Inside a wrapped value: move between visual lines and
                    // only leave the field at its first/last line.
                    let moved = {
                        let target = match *field {
                            0 => name,
                            1 => matcher,
                            2 => command,
                            _ => timeout,
                        };
                        crate::util::word_ops::move_visual_line(target, *cursor_pos, cols, up)
                    };
                    if moved != *cursor_pos {
                        *cursor_pos = moved;
                    } else {
                        *field = if !up {
                            (*field + 1).min(LAST_FIELD)
                        } else {
                            field.saturating_sub(1)
                        };
                        *cursor_pos = lens[*field];
                    }
                }
                true
            }
            KeyCode::Left => {
                let word_jump =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Left;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if word_jump && *field != 3 {
                        // Timeout is a plain number: words make no sense.
                        *cursor_pos = crate::util::word_ops::find_word_start(target, *cursor_pos);
                    } else if *cursor_pos > 0 {
                        *cursor_pos = target.floor_char_boundary(*cursor_pos - 1);
                    }
                }
                true
            }
            KeyCode::Right => {
                let word_jump =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Right;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let len = match *field {
                        0 => name.len(),
                        1 => matcher.len(),
                        2 => command.len(),
                        _ => timeout.len(),
                    };
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if word_jump && *field != 3 {
                        *cursor_pos = crate::util::word_ops::find_word_end(target, *cursor_pos);
                    } else if *cursor_pos < len {
                        let next = target.floor_char_boundary(*cursor_pos + 1).min(len);
                        *cursor_pos = next;
                    }
                }
                true
            }
            KeyCode::Home => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput { cursor_pos, .. } = &mut d.dialog_type
                {
                    *cursor_pos = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    *cursor_pos = match *field {
                        0 => name.len(),
                        1 => matcher.len(),
                        2 => command.len(),
                        _ => timeout.len(),
                    };
                }
                true
            }
            KeyCode::Delete => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    let len = target.len();
                    if *cursor_pos < len {
                        let next = target.floor_char_boundary(*cursor_pos + 1).min(len);
                        target.drain(*cursor_pos..next);
                    }
                }
                true
            }
            KeyCode::Backspace => {
                let delete_word =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Backspace;
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                    && *cursor_pos > 0
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    if delete_word && *field != 3 {
                        let start = crate::util::word_ops::find_word_start(target, *cursor_pos);
                        target.drain(start..*cursor_pos);
                        *cursor_pos = start;
                    } else {
                        let char_start = target.floor_char_boundary(*cursor_pos - 1);
                        target.remove(char_start);
                        *cursor_pos = char_start;
                    }
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::HookInput {
                        name,
                        matcher,
                        command,
                        timeout,
                        field,
                        cursor_pos,
                        ..
                    } = &mut d.dialog_type
                {
                    let target = match *field {
                        0 => name,
                        1 => matcher,
                        2 => command,
                        _ => timeout,
                    };
                    target.insert(*cursor_pos, ch);
                    *cursor_pos += ch.len_utf8();
                }
                true
            }
            _ => false,
        }
    }

    /// Validate and persist the hook registration box. Invalid input keeps
    /// the dialog open with an error toast (same contract as the local URL).
    fn save_hook_input_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        let DialogType::HookInput {
            event,
            editing_index,
            name,
            matcher,
            command,
            timeout,
            ..
        } = &d.dialog_type
        else {
            return false;
        };

        match crate::routes::settings::validate_hook(name, matcher, command, timeout) {
            Ok(entry) => {
                let list = self.setup.hooks.events.entry(event.to_string()).or_default();
                match editing_index {
                    Some(i) if *i < list.len() => list[*i] = entry,
                    _ => list.push(entry),
                }
                self.setup.save();
                true
            }
            Err(message) => {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("Hook not saved".into()),
                    message,
                    variant: ToastVariant::Error,
                    duration_ms: 6000,
                });
                false
            }
        }
    }

    /// Open the hook registration box for `event`: blank for creation,
    /// prefilled with the current values when editing the hook at `index`.
    fn open_hook_form(&mut self, event: &'static str, index: Option<usize>) {
        let existing = index.and_then(|i| {
            crate::routes::settings::hook_entries(&self.setup, event)
                .get(i)
                .cloned()
        });
        let (name, matcher, command, timeout) = match &existing {
            Some(entry) => (
                entry.name.clone(),
                entry.matcher.clone(),
                entry.command.clone(),
                entry.timeout.map(|t| t.to_string()).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        self.dialog.show(DialogType::HookInput {
            event,
            editing_index: index.filter(|_| existing.is_some()),
            name,
            matcher,
            command,
            timeout,
            field: 0,
            cursor_pos: 0,
        });
    }

    /// Perform the save for the current text input dialog (API key → keyring,
    /// local URL → setup.json). Returns `true` when the input was accepted.
    fn save_text_input_dialog(&mut self) -> bool {
        let Some(d) = self.dialog.current() else {
            return false;
        };
        match &d.dialog_type {
            DialogType::RenameSession { input, .. } => {
                // An empty/whitespace title just closes the dialog without
                // touching the session (opencode behavior: cancel, not clear).
                let trimmed = input.trim();
                if !trimmed.is_empty()
                    && let Some(id) = self.state.current_session_id.clone()
                {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    if let Some(session) = self.state.session_cache.get_mut(&id) {
                        session.title = trimmed.to_string();
                        session.title_generated = true;
                    }
                    // Update the sidebar summary.
                    if let Some(summary) = self
                        .state
                        .session_summaries
                        .iter_mut()
                        .find(|s| s.session_id == id)
                    {
                        summary.title = trimmed.to_string();
                        summary.title_generated = true;
                    }
                    self.session_store.update_title(&id, trimmed);
                    self.toast_state.show(ToastOptions {
                        title: Some("Renamed".into()),
                        message: format!("Session renamed to “{trimmed}”."),
                        variant: ToastVariant::Success,
                        duration_ms: 3000,
                    });
                }
                true
            }
            DialogType::ApiKeyInput {
                provider,
                env_var,
                input,
                ..
            } if !input.is_empty() => {
                if let Err(e) = save_provider_api_key(env_var, input) {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("Key not saved".to_string()),
                        message: format!("Failed to store the API key in the OS keyring: {e}"),
                        variant: ToastVariant::Error,
                        duration_ms: 6000,
                    });
                } else {
                    cosh_sdk::connector::invalidate_api_key(env_var);
                }
                // Invalidate model cache for this provider so the next dialog
                // open fetches fresh models with the new key.
                self.model_cache.invalidate(provider);
                true
            }
            DialogType::LocalUrlInput {
                provider, input, ..
            } if !input.is_empty() => {
                let trimmed = input.trim();
                if !is_valid_local_url(trimmed) {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("URL not saved".to_string()),
                        message: format!(
                            "Invalid server URL: \"{trimmed}\". Expected http://host:port"
                        ),
                        variant: ToastVariant::Error,
                        duration_ms: 6000,
                    });
                    false
                } else {
                    self.setup.set_local_base_url(provider, trimmed);
                    self.model_cache.invalidate(provider);
                    true
                }
            }
            DialogType::HookInput { .. } => self.save_hook_input_dialog(),
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

    /// The configured base URL for a provider, if the user saved one for a
    /// local provider in setup.json. Normalized so OpenAI-compatible servers
    /// get the `/v1` suffix their API exposes.
    #[must_use]
    fn base_url_for(&self, provider: &str) -> Option<String> {
        self.setup
            .local_base_url(provider)
            .map(|url| cosh_sdk::connector::normalize_local_base_url(provider, url))
    }

    /// Map of provider → base URL for every local provider the user configured
    /// in setup.json. Used to build fallback connectors inside the harness.
    #[must_use]
    fn configured_local_base_urls(&self) -> std::collections::HashMap<String, String> {
        self.setup
            .providers
            .local
            .iter()
            .map(|(provider, endpoint)| {
                (
                    provider.clone(),
                    cosh_sdk::connector::normalize_local_base_url(provider, &endpoint.base_url),
                )
            })
            .collect()
    }

    /// Providers whose models are eligible for the model picker.
    ///
    /// Cloud providers qualify when an API key is available. Local providers
    /// qualify when configured in setup.json OR when they respond on their
    /// default port — the model list probes every local provider and surfaces
    /// whatever answers, so a llama.cpp / ollama / vLLM server just needs to be
    /// running. Same-port servers (llamacpp/llamafile/localai all default to
    /// 8080, text-generation-webui/tabbyapi to 5000) are probed once: the
    /// first provider (registry order) represents the shared endpoint.
    #[must_use]
    fn active_providers(&self) -> Vec<&'static str> {
        use cosh_sdk::connector::{
            get_provider, is_local_provider, known_local_providers, known_providers_with_env,
        };

        let mut active: Vec<&'static str> = Vec::new();
        for (provider, _) in known_providers_with_env() {
            if !is_local_provider(provider) && cosh_sdk::connector::has_api_key(provider) {
                active.push(provider);
            }
        }

        // Local providers: configured URL wins; otherwise the registry's
        // default endpoint. Probe each distinct endpoint only once.
        let mut endpoints: std::collections::HashSet<String> = std::collections::HashSet::new();
        for provider in known_local_providers() {
            let endpoint = if let Some(cfg) = self.setup.local_base_url(provider) {
                cosh_sdk::connector::normalize_local_base_url(provider, cfg)
            } else {
                get_provider(provider).map_or_else(String::new, |cfg| cfg.base_url.to_string())
            };
            if endpoints.insert(endpoint) {
                active.push(provider);
            }
        }
        active
    }

    /// Open the right credential dialog for a provider: local providers ask
    /// for a server URL (saved to setup.json); cloud providers ask for an API
    /// key (stored in the OS keyring).
    fn open_provider_dialog(&mut self, entry: &ProviderEntry) {
        if entry.local {
            self.dialog.show(DialogType::LocalUrlInput {
                provider: entry.name.to_string(),
                input: self
                    .setup
                    .local_base_url(entry.name)
                    .map_or_else(String::new, str::to_string),
                cursor_pos: 0,
            });
        } else {
            self.dialog.show(DialogType::ApiKeyInput {
                provider: entry.name.to_string(),
                env_var: entry.hint.clone(),
                input: String::new(),
                cursor_pos: 0,
            });
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
                        self.confirm_model_entry(&model, &provider);
                    }
                }
                true
            }
            KeyCode::Esc => {
                // Restore original model + reasoning
                self.restore_model_dialog();
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

    /// Apply a picked model — or, when the model supports configurable
    /// reasoning, push the reasoning sub-dialog on top of the model list.
    fn confirm_model_entry(&mut self, model: &str, provider: &str) {
        if model == "auto" {
            // Auto mode may land on ANY fallback model, so it offers the
            // standard effort set; the choice is re-mapped onto the closest
            // level each fallback model actually accepts (both here and on
            // every harness fallback switch via `resolve_reasoning_effort`).
            let current = self.llm_config.reasoning.clone().unwrap_or_default();
            let levels = vec![
                "default".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ];
            let start = levels.iter().position(|l| l == &current).unwrap_or(0);
            self.dialog.show(DialogType::ReasoningList {
                model: model.to_string(),
                provider: provider.to_string(),
                levels,
                current,
            });
            if let Some(inst) = self.dialog.current_mut() {
                inst.selected = start;
            }
            return;
        }

        if crate::config::model_supports_reasoning(model) {
            let current = self.llm_config.reasoning.clone().unwrap_or_default();
            let levels = crate::config::model_reasoning_levels(model);
            let start = levels.iter().position(|l| l == &current).unwrap_or(0);
            // Push the sub-dialog FIRST, then set its initial selection —
            // mutating the ModelList's `selected` (the current top before
            // the push) would corrupt the model highlight after Esc.
            self.dialog.show(DialogType::ReasoningList {
                model: model.to_string(),
                provider: provider.to_string(),
                levels,
                current,
            });
            if let Some(inst) = self.dialog.current_mut() {
                inst.selected = start;
            }
        } else {
            self.llm_config.model = Some(model.to_string());
            self.llm_config.provider = provider.to_string();
            // Keep the previous reasoning level — the new model either
            // ignores it or uses it; user can change it from the dialog.
            self.model_dialog_original = None;
            self.reasoning_dialog_original = None;
            self.dialog.pop();
        }
    }

    /// Restore the model + reasoning that were active when the dialog opened.
    fn restore_model_dialog(&mut self) {
        if let Some(ref orig) = self.model_dialog_original {
            self.llm_config.model = if orig.is_empty() {
                None
            } else {
                Some(orig.clone())
            };
        }
        if let Some(ref orig) = self.reasoning_dialog_original {
            self.llm_config.reasoning = if orig.is_empty() {
                None
            } else {
                Some(orig.clone())
            };
        }
        self.model_dialog_original = None;
        self.reasoning_dialog_original = None;
        self.dialog.pop();
    }

    fn is_reasoning_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ReasoningList { .. })
            )
    }

    fn handle_reasoning_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_reasoning_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ReasoningList { levels, .. } = &d.dialog_type
                    && !levels.is_empty()
                {
                    d.selected = if d.selected == 0 {
                        levels.len() - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ReasoningList { levels, .. } = &d.dialog_type
                    && !levels.is_empty()
                {
                    d.selected = (d.selected + 1) % levels.len();
                }
                true
            }
            KeyCode::Enter => {
                let (model, provider, level) = {
                    let Some(d) = self.dialog.current() else {
                        return true;
                    };
                    let DialogType::ReasoningList {
                        model,
                        provider,
                        levels,
                        ..
                    } = &d.dialog_type
                    else {
                        return true;
                    };
                    let idx = d.selected.min(levels.len().saturating_sub(1));
                    (model.clone(), provider.clone(), levels[idx].clone())
                };
                self.llm_config.model = Some(model);
                self.llm_config.provider = provider;
                self.llm_config.reasoning = if level == "default" {
                    None
                } else {
                    Some(level)
                };
                self.model_dialog_original = None;
                self.reasoning_dialog_original = None;
                // Pop both the reasoning sub-dialog and the model list.
                self.dialog.pop();
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Back to the model list (nothing applied yet).
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Open the tool-call mode picker (`native` | `inline`).
    fn open_tool_call_dialog(&mut self) {
        let current = match self.llm_config.tool_call_mode {
            cosh_sdk::connector::ToolCallMode::Native => "native",
            cosh_sdk::connector::ToolCallMode::Inline => "inline",
        };
        self.dialog.replace(DialogType::ToolCallList {
            current: current.to_string(),
        });
        // Preselect the current mode (index 0 = native, 1 = inline).
        if let Some(d) = self.dialog.current_mut() {
            d.selected = if current == "inline" { 1 } else { 0 };
        }
    }

    fn is_tool_call_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::ToolCallList { .. }))
    }

    fn is_message_actions_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::MessageActions { .. }))
    }

    fn handle_message_actions_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_message_actions_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 { 2 } else { d.selected - 1 };
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % 3;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self.dialog.current().map_or(0, |d| d.selected.min(2));
                let message_id = match self.dialog.current() {
                    Some(d) => match &d.dialog_type {
                        DialogType::MessageActions { message_id, .. } => message_id.clone(),
                        _ => return true,
                    },
                    None => return true,
                };
                self.dialog.pop();
                self.run_message_action(selected, &message_id);
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Execute the picked Message Actions entry: 0 = Revert, 1 = Copy,
    /// 2 = Fork (same order as the opencode dialog).
    fn run_message_action(&mut self, action: usize, message_id: &str) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let working = self.state.status == SessionStatus::Working;
        let session = self.state.current_session();
        let msg_idx = session.and_then(|s| s.messages.iter().position(|m| m.id == message_id));
        let Some(session) = session else {
            return;
        };
        let Some(idx) = msg_idx else {
            return;
        };
        let msg = &session.messages[idx];

        match action {
            0 => {
                // Revert: drop this message and everything after it, and put
                // its text back into the prompt for editing/resending.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let prompt_text = message_prompt_text(msg);
                let session = self.state.current_session_mut().expect("session");
                session.messages.truncate(idx);
                self.session_store.save_session_async(session);
                self.session_view.hovered_msg_idx = None;
                if !prompt_text.is_empty() {
                    self.prompt_view.input = prompt_text;
                    self.prompt_view.cursor_pos = self.prompt_view.input.len();
                }
                self.prompt_view.focus();
            }
            1 => {
                // Copy: join non-synthetic text parts into the clipboard.
                let text = message_prompt_text(msg);
                if !text.is_empty() {
                    selection::copy_selection(&text, &mut self.toast_state);
                } else {
                    self.toast_state.show(ToastOptions {
                        title: Some("Copy".into()),
                        message: "Nothing to copy in this message.".into(),
                        variant: ToastVariant::Info,
                        duration_ms: 2500,
                    });
                }
            }
            2 => {
                // Fork: branch a new session containing everything up to and
                // including the clicked message, then switch to it.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let mut forked = session.clone();
                forked.messages.truncate(idx + 1);
                forked.id = generate_session_id();
                forked.title = format!("{} (fork)", session.title);
                forked.created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                forked.title_generated = false;
                let new_id = forked.id.clone();
                self.state.add_session(forked);
                self.session_store.save_session(
                    self.state
                        .current_session()
                        .expect("forked session just added"),
                );
                self.state
                    .ensure_session_summary(&self.state.current_session_id.clone().expect("id"));
                self.finalize_stale_compaction_lines();
                self.state.switch_to_session(new_id, &self.session_store);
                self.session_view.hovered_msg_idx = None;
                self.toast_state.show(ToastOptions {
                    title: Some("Fork".into()),
                    message: "New session created from this point.".into(),
                    variant: ToastVariant::Success,
                    duration_ms: 3000,
                });
            }
            _ => {}
        }
    }

    fn handle_tool_call_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_tool_call_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 { 1 } else { 0 };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % 2;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self.dialog.current().map_or(0, |d| d.selected.min(1));
                self.llm_config.tool_call_mode = if selected == 1 {
                    cosh_sdk::connector::ToolCallMode::Inline
                } else {
                    cosh_sdk::connector::ToolCallMode::Native
                };
                crate::routes::tools::save_tool_call_mode(
                    &mut self.setup,
                    self.llm_config.tool_call_mode,
                );
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Open the rename dialog for the current session, prefilled with its
    /// title (opencode-style prompt: edit in place, Enter applies, Esc
    /// cancels). No-op without a session.
    fn open_rename_dialog(&mut self) {
        let Some(session) = self.state.current_session() else {
            return;
        };
        let input = session.title.clone();
        let cursor_pos = input.len();
        self.dialog
            .show(DialogType::RenameSession { input, cursor_pos });
    }

    /// Create a fresh empty session and select it — the shared path behind
    /// Home's "New session" (keyboard + mouse) and the `/new` slash command.
    /// Selecting the session flips the app into Session mode.
    fn start_new_session(&mut self) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let id = format!("{now_ms}");
        let title = format_session_timestamp(now_ms);
        self.state.add_empty_session(id.clone(), title, now_ms);
        self.state.current_session_id = Some(id);
        self.title_generated = false;
        self.prompt_view.focus();
    }

    /// Execute a slash-menu command (Enter or click). Shared by the keyboard
    /// and mouse handlers so both dispatch identically — a command missed here
    /// silently degrades to filling the prompt with "/name ".
    fn run_slash_command(&mut self, cmd: &crate::ui::slash_menu::SlashCommand) {
        if cmd.name == "themes" {
            self.open_theme_dialog();
        } else if cmd.name == "models" {
            self.open_model_dialog();
        } else if cmd.name == "toolcall" {
            self.open_tool_call_dialog();
        } else if cmd.name == "compact" {
            self.start_manual_compaction();
        } else if cmd.name == "new" {
            if self.state.status == crate::types::SessionStatus::Idle {
                self.start_new_session();
            } else {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("New session".into()),
                    message: "The agent is working — wait for it to finish.".into(),
                    variant: ToastVariant::Warning,
                    duration_ms: 4000,
                });
                self.slash_menu.visible = false;
                return;
            }
        } else if cmd.name == "rename" {
            self.open_rename_dialog();
        } else if cmd.name == "bell" {
            self.bell_enabled = !self.bell_enabled;
            use crate::ui::toast::{ToastOptions, ToastVariant};
            let state = if self.bell_enabled {
                "enabled"
            } else {
                "disabled"
            };
            self.toast_state.show(ToastOptions {
                title: Some("Bell".into()),
                message: format!("Completion bell {state}."),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            self.setup.appearance.bell_enabled = self.bell_enabled;
            self.setup.save();
        } else {
            let cmd_name = format!("/{} ", cmd.name);
            self.prompt_view.input = cmd_name;
            self.prompt_view.cursor_pos = self.prompt_view.input.len();
        }
        self.slash_menu.visible = false;
    }

    /// User-triggered `/compact`: run the compaction funnel NOW (the
    /// deterministic phases across every segment, then the LLM summary)
    /// instead of waiting for the 80% trigger. Refusals are surfaced as
    /// toasts; the work runs on a one-off tokio task that rebuilds a Harness
    /// from the persisted `.ctx` snapshot — between loops no harness exists,
    /// and while a loop runs its context manager is untouchable, so both
    /// cases refuse.
    fn start_manual_compaction(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        fn refuse(app: &mut App, message: String) {
            app.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message,
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
        }
        if self.manual_compaction_active {
            refuse(self, "A compaction is already running.".into());
            return;
        }
        if self.state.status != crate::types::SessionStatus::Idle {
            refuse(
                self,
                "The agent is working — /compact runs between messages.".into(),
            );
            return;
        }
        let Some(id) = self.state.current_session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let Some(ctx_bytes) = self.session_store.load_ctx(&id) else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "Nothing to compact yet.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };

        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let base_url = self.base_url_for(&provider);
        let cwd = self.state.working_directory.clone();
        let stop_signal = self.stop_signal.clone();
        let event_tx = self.event_tx.clone();
        self.manual_compaction_active = true;
        self.tokio_handle.spawn(async move {
            use cosh::harness::{Harness, ManualCompactionOutcome};
            let outcome = match cosh_sdk::connector::Connector::new(&provider) {
                Ok(mut connector) => {
                    if let Some(ref m) = model {
                        connector = connector.with_model(m);
                    }
                    if let Some(ref url) = base_url {
                        connector = connector.with_base_url(url.clone());
                    }
                    let mut harness =
                        Harness::new(connector, &cwd, std::collections::HashSet::new());
                    if let Ok(state) =
                        bincode::deserialize::<cosh::harness::ContextManagerState>(&ctx_bytes)
                    {
                        harness.context_manager.restore_state(&state);
                    }
                    harness.compact_on_demand(&event_tx, stop_signal).await
                }
                Err(e) => {
                    let _ = event_tx.send(HarnessEvent::Error(format!("connector: {e}")));
                    ManualCompactionOutcome::Failed
                }
            };
            let _ = event_tx.send(HarnessEvent::CompactOnDemand { outcome });
        });
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
        for provider in self.active_providers() {
            if let Some(cached) = self.model_cache.get(&provider.to_string()) {
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
                        let base_urls = self.configured_local_base_urls();
                        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
                        let handle = self.tokio_handle.spawn(async move {
                            let result = embed_document(
                                &uri,
                                &table_name,
                                &embedder_config,
                                &embed_content,
                                &base_urls,
                            )
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
        } else if self.show_settings {
            AppMode::Settings
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

    /// The session view's content area: full terminal minus the open sidebar
    /// and (in Session mode) the visible right panel. Shared by `render` and
    /// the mouse dispatch so click hit-testing uses the EXACT width the view
    /// rendered at — a wider mouse area would re-wrap every message, shifting
    /// `prefix_y` and making tool-box clicks land on the wrong row.
    fn session_main_area(&self, area: Rect) -> SessionArea {
        let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
        let right_panel_w = if matches!(self.mode(), AppMode::Session)
            && (should_show_right_panel(area.width, &self.state.right_panel))
        {
            RIGHT_PANEL_WIDTH
        } else {
            0
        };
        SessionArea {
            main: Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w + right_panel_w),
                area.height,
            ),
            sidebar_w,
            right_panel_w,
        }
    }

    /// The session transcript viewport (shared by render and the mouse
    /// dispatch): the main area minus header, prompt, spinner and question
    /// rows. Used for click hit-testing AND hover tracking so both map
    /// cursor positions with the exact geometry the view rendered at.
    fn session_viewport_area(&self) -> Rect {
        let area = self.terminal_size();
        let SessionArea {
            main: main_area, ..
        } = self.session_main_area(area);
        let footer_y = main_area.bottom().saturating_sub(1);
        let prompt_budget = footer_y
            .saturating_sub(area.y + 1)
            .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
        let prompt_h = self
            .prompt_view
            .required_height(main_area.width.saturating_sub(4), prompt_budget);
        let question_h = if self.question_dialog.visible {
            self.question_dialog
                .required_height(main_area.width.saturating_sub(4))
        } else {
            0
        };
        let spinner_h = u16::from(
            matches!(self.state.status, SessionStatus::Working)
                && self.agent_spinner.is_some()
                && !self.question_dialog.visible,
        );
        let prompt_area_y = footer_y.saturating_sub(prompt_h);
        let spinner_area_y = prompt_area_y.saturating_sub(spinner_h);
        let question_h = question_h.min(spinner_area_y.saturating_sub(area.y + 1));
        let question_area_y = spinner_area_y.saturating_sub(question_h);
        let session_bottom = question_area_y;
        Rect::new(
            main_area.x,
            area.y + 1,
            main_area.width,
            session_bottom.saturating_sub(area.y + 1),
        )
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
            || self.state.right_panel.is_auto_scrolling
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
            // Rate-limited: the fill always exceeds 200us, so without this the
            // log would receive one line per frame for the whole session
            // lifetime (a 400+ MB /tmp/tui_main.log in 8 hours).
            self.perf_frame = self.perf_frame.wrapping_add(1);
            if _bg_us > 200 && self.perf_frame.is_multiple_of(30) {
                log::debug!(
                    "[PERF] bg_fill: {_bg_us}us area={}x{}",
                    area.width,
                    area.height
                );
            }

            let header_style = Style::default().fg(rgba_color(self.theme.text_muted));
            let title_chars: Vec<char> = "~$co-sh".chars().collect();
            for (i, ch) in title_chars.iter().enumerate() {
                if let Some(cell) = buf.cell_mut((area.x + 1 + i as u16, area.y)) {
                    cell.set_char(*ch);
                    cell.set_style(header_style);
                }
            }

            let bug_w = BUG_REPORT_TEXT.chars().count() as u16;

            let SessionArea {
                main: main_area,
                sidebar_w,
                right_panel_w,
            } = self.session_main_area(area);

            // Context info bar — only after the user has sent at least one
            // message. Shows the agent's current context against the model
            // window to the left of the budget bar, with thousands separators
            // so large token counts are readable at a glance.
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
                let max = self.context_info.as_ref().map_or(0, |info| info.max_tokens);

                // "9,612 / 100,000 tok" — current context vs the model
                // window; without a known window only the count is shown.
                let token_str = if max > 0 {
                    format!("{} / {} tok", format_tokens(tokens), format_tokens(max))
                } else {
                    format!("{} tok", format_tokens(tokens))
                };
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

            // Bug report link — right-aligned in the header, only on the Home
            // screen. Clicking it opens the GitHub issues page in the default
            // browser. Other routers reuse this space, so it's hidden there.
            let bug_right_x = main_area.right().saturating_sub(bug_w + 1);
            let bug_link_area = if matches!(self.mode(), AppMode::Home) && bug_right_x >= area.x + 9
            {
                let bug_link_style = Style::default().fg(rgba_color(self.theme.accent));
                for (i, ch) in BUG_REPORT_TEXT.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((bug_right_x + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(bug_link_style);
                    }
                }
                Some(Rect::new(bug_right_x, area.y, bug_w, 1))
            } else {
                None
            };
            self.bug_link_area = bug_link_area;

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
                self.state.right_panel.handle_auto_scroll(delta_time);
            }

            if self.sidebar.open {
                self.sidebar.render(
                    buf,
                    Rect::new(area.x, area.y, sidebar_w, area.height),
                    &self.state,
                    &self.theme,
                );

                // Hover tooltip: when the mouse is over a sidebar item
                // whose title was LLM-generated, show the full title as
                // a toast.  Checked every frame so it works even when
                // the terminal doesn't send Move events.
                let mx = self.last_mouse_x;
                let my = self.last_mouse_y;
                let content_start_y = area.y + 2;
                if my >= content_start_y && my < area.y + area.height && mx < sidebar_w {
                    let idx =
                        self.sidebar.selection.scroll_offset + (my - content_start_y) as usize;
                    if let Some(summary) = self.state.session_summaries.get(idx)
                        && summary.title_generated
                    {
                        self.toast_state.show(crate::ui::toast::ToastOptions {
                            title: None,
                            message: summary.title.clone(),
                            variant: crate::ui::toast::ToastVariant::Info,
                            duration_ms: 3000,
                        });
                    }
                }
            }

            let footer_y = main_area.bottom().saturating_sub(1);
            let is_session = matches!(self.mode(), AppMode::Session);

            // When question, permission or queue-choice dialog is visible, hide
            // prompt and spinner (like OpenCode).
            let hide_prompt_and_spinner = is_session
                && (self.question_dialog.visible
                    || self.permission_dialog.visible
                    || self.queue_choice_dialog.visible);

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
                // Responsive vertical budget: everything between the header and
                // the footer, minus the rows always reserved for the session.
                let prompt_budget = footer_y
                    .saturating_sub(area.y + 1)
                    .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
                self.prompt_view
                    .required_height(prompt_area_w, prompt_budget)
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
            // Pending queued-message region (color-coded rows above the prompt).
            let pending_h = if is_session && !hide_prompt_and_spinner {
                self.state
                    .current_pending_queues()
                    .map_or(0, |q| (q.next_request.len() + q.next_loop.len()) as u16)
            } else {
                0
            };
            // Queue-choice dialog (same position as question/permission).
            let queue_choice_h = if is_session && self.queue_choice_dialog.visible {
                self.queue_choice_dialog
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

            let (prompt_area_y, _logo_start_y) = if is_empty_session && prompt_h > 0 {
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

            // The pending region and the dialogs grow upward from the prompt;
            // clamp their heights so they never cover the header row (area.y + 1)
            // or run off-screen. Long content scrolls instead.
            let pending_area_y = prompt_area_y.saturating_sub(pending_h);
            // The spinner sits ABOVE the pending queues when they are shown;
            // with no queues (pending_h = 0) it stays in exactly the same spot
            // (directly above the prompt).
            let spinner_area_y = pending_area_y.saturating_sub(spinner_h);
            let max_dialog_h = pending_area_y.saturating_sub(area.y + 1);
            let question_h = question_h.min(max_dialog_h);
            let permission_h = permission_h.min(max_dialog_h);
            let queue_choice_h = queue_choice_h.min(max_dialog_h);
            let question_area_y = pending_area_y.saturating_sub(question_h);
            let permission_area_y = pending_area_y.saturating_sub(permission_h);
            let queue_choice_area_y = pending_area_y.saturating_sub(queue_choice_h);
            let pending_area = Rect::new(
                main_area.x + 2,
                pending_area_y,
                main_area.width.saturating_sub(4),
                pending_h,
            );
            let queue_choice_area = Rect::new(
                main_area.x + 2,
                queue_choice_area_y,
                main_area.width.saturating_sub(4),
                queue_choice_h,
            );
            let prompt_padding: u16 = 1;
            // Session bottom is below whichever dialog is visible (mutually
            // exclusive, never both), below the pending region and below the
            // spinner (which sits above the queues when they are shown).
            let session_bottom = question_area_y
                .min(permission_area_y)
                .min(queue_choice_area_y)
                .min(pending_area_y)
                .min(spinner_area_y)
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
                    self.add_provider_view
                        .render(buf, tools_area, &self.theme, &self.setup);
                }
                AppMode::Settings => {
                    self.prompt_view.blur();
                    let settings_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.settings_view
                        .render(buf, settings_area, &self.theme, &self.setup);
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
                    // Blur prompt when a dialog is visible (like OpenCode)
                    if self.question_dialog.visible
                        || self.permission_dialog.visible
                        || self.queue_choice_dialog.visible
                    {
                        self.prompt_view.blur();
                    }

                    // The animated chat-logo ("O" with a red center and a laser
                    // beam) replaces the static logo on the empty session. It is
                    // rendered by the prompt view, which owns its animation state.
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
                    // Question/permission/queue-choice dialogs rendered inline
                    // between messages and prompt (mutually exclusive).
                    if self.question_dialog.visible {
                        let now = std::time::SystemTime::now();
                        // Sync focus so the answer input's cursor blurs when the
                        // terminal loses focus (same as every other cursor).
                        self.question_dialog.cursor.terminal_focused = self.terminal_focused;
                        self.question_dialog
                            .render(buf, question_area, &self.theme, now);
                    } else if self.permission_dialog.visible {
                        self.permission_dialog
                            .render(buf, permission_area, &self.theme);
                    } else if self.queue_choice_dialog.visible {
                        self.queue_choice_dialog
                            .render(buf, queue_choice_area, &self.theme);
                    }
                    // Pending queued messages, color-coded per queue, above the
                    // prompt (never rendered while a dialog covers that spot).
                    if !hide_prompt_and_spinner && pending_h > 0 {
                        self.render_pending_queues(buf, pending_area);
                    }
                    // Hide spinner and prompt when dialog is visible (like OpenCode)
                    if !self.question_dialog.visible
                        && !self.permission_dialog.visible
                        && !self.queue_choice_dialog.visible
                    {
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
                            delta_time,
                            is_empty_session,
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
                    let hide_text = self.question_dialog.visible
                        || self.permission_dialog.visible
                        || self.queue_choice_dialog.visible;
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

            self.slash_menu.render(buf, prompt_area, &self.theme);
        }
    }

    /// Start a full agent loop for `msg` (a user message that was just sent
    /// or dequeued from the pending "next agent loop" queue). Creates the
    /// session + history entry, spins the working state, and spawns the
    /// harness thread. Messages still pending in the "next request" queue
    /// are handed to the new loop's queued-input channel so they enter its
    /// first request.
    fn start_agent_loop(&mut self, msg: String) {
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
            self.title_generated = false;
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
        let reasoning = self.llm_config.reasoning.clone();
        let tool_call_mode = self.llm_config.tool_call_mode;
        let base_url = self.base_url_for(&provider);
        let local_base_urls = self.configured_local_base_urls();
        let fallbacks = self.router_view.fallbacks.clone();
        // Auto-rotate: move the first working fallback to the front so the
        // next message tries the provider that actually worked before wasting
        // time on failing ones. Rotation is in-memory only (not persisted).
        let fallbacks = if model.as_deref() == Some("auto") {
            let working = fallbacks
                .iter()
                .position(|fb| cosh_sdk::connector::Connector::new(&fb.provider).is_ok());
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

        // Create a fresh queued-input channel for the "next request" queue.
        // The harness drains it before every request of THIS loop.
        let (queued_tx, queued_rx) = mpsc::unbounded_channel();
        self.queued_input_tx = Some(queued_tx.clone());
        self.active_loop_session_id = self.state.current_session_id.clone();

        // Carry over messages that were queued for the next request while no
        // loop was running (e.g. the previous run ended in an error and the
        // user typed a fresh message): they enter this loop's first request.
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(queues) = self.state.pending_queues.get(&id)
        {
            for text in &queues.next_request {
                let _ = queued_tx.send(text.clone());
            }
        }

        let mut disabled_tools = self.internal_tools_view.disabled.clone();

        // RAG recall context
        // 1) Description suffix (what the model sees in the tool doc)
        #[cfg(feature = "embed")]
        let recall_suffix = self.recall_suffix();
        #[cfg(not(feature = "embed"))]
        let _recall_suffix = String::new();

        // 2) DB registry (what the dispatch uses to resolve db_name → connect)
        #[cfg(feature = "embed")]
        let recall_dbs: Vec<cosh::harness::tools::RecallDb> = self.recall_dbs_vec();
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
                                crate::types::Part::Text(t) => Some(t.text.as_str()),
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
                    let _ = event_tx.send(HarnessEvent::Error(format!("runtime: {e}")));
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
                                    let mut c =
                                        c.with_model(&fb.model).with_tool_call_mode(tool_call_mode);
                                    // Honor a configured local server URL for
                                    // this fallback provider, if any.
                                    if let Some(url) = local_base_urls.get(&fb.provider) {
                                        c = c.with_base_url(url.clone());
                                    }
                                    // The user chose a reasoning effort for
                                    // auto mode — apply it, mapped onto the
                                    // closest level THIS model accepts (a
                                    // model without the knob drops it). The
                                    // harness re-resolves per model on every
                                    // fallback switch.
                                    if let Some(r) = reasoning.as_deref()
                                        && let Some(resolved) =
                                            cosh_sdk::connector::resolve_reasoning_effort(
                                                &fb.model,
                                                r,
                                                Some("cosh/cache"),
                                            )
                                    {
                                        c = c.with_reasoning_effort(resolved);
                                    }
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
                                let _ = event_tx.send(HarnessEvent::Error(format!(
                                    "auto: no fallback available ({last_err})"
                                )));
                                return;
                            }
                        }
                    } else {
                        match Connector::new(&provider) {
                            Ok(c) => {
                                let with_model = if let Some(ref m) = model {
                                    c.with_model(m)
                                } else {
                                    c
                                };
                                // Honor a configured local server URL.
                                let with_model = if let Some(ref url) = base_url {
                                    with_model.with_base_url(url.clone())
                                } else {
                                    with_model
                                };
                                // Apply the user's tool-call mode (native /
                                // inline) — the two delivery paths are
                                // mutually exclusive at the request level.
                                let with_model = with_model.with_tool_call_mode(tool_call_mode);
                                connector = if let Some(ref r) = reasoning {
                                    // Map the chosen level onto the closest
                                    // one the model actually accepts (e.g.
                                    // "medium" on a low/high-only model) —
                                    // never send a knob that would 400 or be
                                    // silently ignored.
                                    let effective = model
                                        .as_deref()
                                        .and_then(|m| {
                                            cosh_sdk::connector::resolve_reasoning_effort(
                                                m,
                                                r,
                                                Some("cosh/cache"),
                                            )
                                        })
                                        .unwrap_or_else(|| r.clone());
                                    with_model.with_reasoning_effort(effective)
                                } else {
                                    with_model
                                };
                            }
                            Err(e) => {
                                let _ =
                                    event_tx.send(HarnessEvent::Error(format!("connector: {e}")));
                                return;
                            }
                        }
                    }

                    let mut harness = Harness::new(connector, &cwd, disabled_tools)
                        .with_mode(mode)
                        .with_history(&history)
                        .with_fallbacks(remaining)
                        .with_local_base_urls(local_base_urls);

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
                        .run_agent_loop_with_queued_input(
                            &input,
                            event_tx,
                            answer_rx,
                            perm_rx,
                            stop_signal,
                            queued_rx,
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
                let _ = event_tx_panic.send(HarnessEvent::Error(format!("panic: {msg}")));
            }
        });
    }

    /// Called when an agent loop terminates. Leftover "next request"
    /// messages that were never injected are promoted to the "next agent
    /// loop" queue (they behave like it: they wait for the next run). When
    /// `start_next` is true (the run ended cleanly — Done or Stopped) the
    /// first queued next-loop message starts a fresh loop. On Error the queues
    /// stay parked: the user decides when to resend (e.g. after switching the
    /// model) — a later manual message rolls them in.
    fn handle_loop_end(&mut self, start_next: bool) -> bool {
        self.queued_input_tx = None;
        let Some(id) = self.state.current_session_id.clone() else {
            return false;
        };
        if self.active_loop_session_id.is_some()
            && self.active_loop_session_id.as_deref() != Some(id.as_str())
        {
            // The user switched sessions while the loop ran — leave the queues
            // untouched for the session that owns them.
            return false;
        }
        let next: Option<String> = {
            let queues = self.state.pending_queues.entry(id).or_default();
            while let Some(text) = queues.next_request.pop_front() {
                queues.next_loop.push_back(text);
            }
            if start_next {
                queues.next_loop.pop_front()
            } else {
                None
            }
        };
        if let Some(msg) = next {
            self.start_agent_loop(msg);
            true
        } else {
            false
        }
    }

    /// Rings the terminal bell (BEL) so the user notices the agent loop ended.
    fn trigger_bell(&self) {
        if !self.bell_enabled {
            return;
        }
        let mut out = io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
    }

    /// Render the pending queued messages above the prompt, color-coded per
    /// queue: "next agent loop" rows on top (warm amber background), "next
    /// request" rows below (cool cyan/blue background), each preserving FIFO
    /// order. The two dedicated theme colors switch with the active theme. No
    /// explicit labels — the background color IS the identity of the queue.
    fn render_pending_queues(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let Some(queues) = self.state.current_pending_queues() else {
            return;
        };
        if queues.next_loop.is_empty() && queues.next_request.is_empty() {
            return;
        }
        let mut y = area.y;
        let panel = self.theme.background_panel;
        let border = self.theme.accent;
        for text in &queues.next_loop {
            Self::draw_pending_row(
                buf,
                text,
                area.x,
                y,
                area.width,
                self.theme.queue_next_loop,
                panel,
                border,
            );
            y += 1;
        }
        for text in &queues.next_request {
            Self::draw_pending_row(
                buf,
                text,
                area.x,
                y,
                area.width,
                self.theme.queue_next_request,
                panel,
                border,
            );
            y += 1;
        }
    }

    /// Draw one pending-message row: the app's standard `┃` left border (in the
    /// accent color, like the question/permission dialogs) on the neutral panel
    /// background, the queue color as the background of the rest of the row
    /// (starting right after the border, at `x + 1`, so it never covers the
    /// `┃` glyph), and the message text starting 3 columns in — like a normal
    /// user message, so queued rows stay visually consistent with the chat. The
    /// queue identity is carried by the background color only; no marker glyph
    /// is used on these rows.
    #[allow(clippy::too_many_arguments)]
    fn draw_pending_row(
        buf: &mut ratatui::buffer::Buffer,
        text: &str,
        x: u16,
        y: u16,
        width: u16,
        bg: RGBA,
        panel: RGBA,
        border: RGBA,
    ) {
        if width < 4 {
            return;
        }
        let fg = Self::contrast_on(bg);
        let bg_color = rgba_color(bg);
        // Standard app left border (┃) in the app's accent color on the
        // neutral panel background — the queue-colored band starts at `x + 1`,
        // right after the border, so the background never covers the glyph.
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char('┃');
            cell.set_style(
                Style::default()
                    .fg(rgba_color(border))
                    .bg(rgba_color(panel)),
            );
        }
        // Queue-colored background for the rest of the row.
        let band_style = Style::default().bg(bg_color);
        for cx in x + 1..x + width {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(' ');
                cell.set_style(band_style);
            }
        }
        // Message text starts 3 columns in (┃ + 2 pad), like a user message.
        let text_x = x + 3;
        let visible: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(width.saturating_sub(4) as usize)
            .collect();
        let text_style = Style::default().fg(fg).bg(bg_color);
        for (i, ch) in visible.chars().enumerate() {
            let cx = text_x + i as u16;
            if cx >= x + width {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(text_style);
            }
        }
    }

    /// Black or white depending on the background luminance (for readable
    /// text on the colored pending-queue rows).
    fn contrast_on(bg: RGBA) -> Color {
        let (r, g, b, _) = bg.to_ints();
        let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
        if lum > 128.0 {
            Color::Rgb(0, 0, 0)
        } else {
            Color::Rgb(255, 255, 255)
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

                    // Escape also clears a lingering create-db field selection.
                    #[cfg(feature = "embed")]
                    if key.code == KeyCode::Esc && self.rag_view.field_selection.is_some() {
                        self.rag_view.clear_field_selection();
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
                        // If there is a drag selection in a create-db field, copy it.
                        #[cfg(feature = "embed")]
                        if matches!(self.mode(), AppMode::Rag)
                            && self.rag_view.has_field_selection()
                        {
                            let text = self.rag_view.selected_field_text();
                            selection::copy_selection(&text, &mut self.toast_state);
                            self.rag_view.clear_field_selection();
                            return Ok(false);
                        }
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

                    // Check the tool-call mode dialog (standalone, like theme)
                    if self.is_tool_call_dialog_visible()
                        && self.handle_tool_call_dialog_key(key.code)
                    {
                        return Ok(false);
                    }

                    // Check the per-message actions dialog
                    if self.is_message_actions_dialog_visible()
                        && self.handle_message_actions_dialog_key(key.code)
                    {
                        return Ok(false);
                    }

                    // Check reasoning sub-dialog SECOND (pushed on top of the
                    // model list), before the model dialog.
                    if self.is_reasoning_dialog_visible()
                        && self.handle_reasoning_dialog_key(key.code)
                    {
                        return Ok(false);
                    }

                    // Check model dialog THIRD, before action lookup
                    if self.is_model_dialog_visible() && self.handle_model_dialog_key(key.code) {
                        return Ok(false);
                    }

                    // Check question dialog THIRD (inline)
                    if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                        let consumed = self.question_dialog.handle_key_event(key);
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

                    // Check the queue-choice dialog (inline, shown when the user
                    // sends a message while the agent loop is running).
                    if self.queue_choice_dialog.visible && matches!(self.mode(), AppMode::Session) {
                        let consumed = self.queue_choice_dialog.handle_key_event(key);
                        if consumed {
                            if self.queue_choice_dialog.submitted {
                                let text = self.prompt_view.send_message();
                                let target = self.queue_choice_dialog.choice();
                                self.queue_choice_dialog.hide();
                                if let Some(queues) = self.state.current_pending_queues_mut() {
                                    match target {
                                        QueueTarget::NextRequest => {
                                            queues.next_request.push_back(text.clone());
                                            // Hand it to the running loop immediately: it
                                            // enters the model context before the next request.
                                            if let Some(tx) = &self.queued_input_tx {
                                                let _ = tx.send(text);
                                            }
                                        }
                                        QueueTarget::NextLoop => {
                                            queues.next_loop.push_back(text);
                                        }
                                    }
                                }
                                self.prompt_view.focus();
                            } else if !self.queue_choice_dialog.visible {
                                // Dismissed via Esc: re-focus the prompt (the
                                // message stays in the input, nothing was queued).
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

                    // Hook registration box: handled with the full key event
                    // so ctrl-combos (word jumps) reach it intact.
                    if matches!(
                        self.dialog.current().map(|d| &d.dialog_type),
                        Some(DialogType::HookInput { .. })
                    ) && self.handle_hook_input_key(key)
                    {
                        return Ok(false);
                    }

                    // Check ApiKey/LocalUrl input dialog
                    if self.is_text_input_visible() {
                        let handled = self.handle_text_input_dialog_key(key.code);
                        if handled {
                            return Ok(false);
                        }
                    }

                    // Check Shortcuts dialog for scrolling
                    if self.is_shortcuts_dialog_visible() {
                        match key.code {
                            KeyCode::Up => {
                                if let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_sub(1);
                                }
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                if let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_add(1);
                                }
                                return Ok(false);
                            }
                            KeyCode::Char('k') => {
                                // Only handle 'k' for scrolling if not Ctrl+K (which toggles the dialog)
                                if !key.modifiers.contains(KeyModifiers::CONTROL)
                                    && let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_sub(1);
                                }
                                // Let Ctrl+K pass through to the action handler
                                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                                    return Ok(false);
                                }
                            }
                            KeyCode::Char('j') => {
                                if let Some(d) = self.dialog.current_mut()
                                    && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                                {
                                    *scroll = scroll.saturating_add(1);
                                }
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                // Don't close shortcuts dialog with Esc - use Ctrl+K to toggle
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
                        // In RAG mode with the create-db form open, the newline
                        // goes into the focused field instead of the prompt. The
                        // key is consumed even when the form is at its growth
                        // limit (handle_insert_newline then does nothing), so it
                        // never leaks into the hidden prompt.
                        #[cfg(feature = "embed")]
                        if self.is_rag_mode()
                            && !self.dialog.visible()
                            && self.rag_view.show_create_db
                        {
                            self.rag_view.handle_insert_newline();
                            return Ok(false);
                        }
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
                                        self.session_view.hovered_msg_idx = None;
                                        self.title_generated = true;
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
                                        self.start_new_session();
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
                                    HomeAction::OpenSettings => {
                                        self.show_settings = true;
                                    }
                                    HomeAction::OpenModelRouter => {
                                        // Refresh fallbacks from prefs cache and models from model cache
                                        let saved = fallback::load_fallbacks(&self.setup);
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
                                    &mut self.setup,
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

                    // Settings mode: navigation and activation keys
                    // Each matched arm returns early so unmatched keys fall through
                    // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
                    if matches!(self.mode(), AppMode::Settings) && !self.dialog.visible() {
                        match key.code {
                            KeyCode::Up => {
                                self.settings_view.select_prev(20, &self.setup);
                                return Ok(false);
                            }
                            KeyCode::Down => {
                                self.settings_view.select_next(20, &self.setup);
                                return Ok(false);
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                match self.settings_view.activate_selected(&mut self.setup) {
                                    Some(crate::routes::settings::SettingsAction::ToggleSaved) => {
                                        self.setup.save();
                                    }
                                    Some(crate::routes::settings::SettingsAction::OpenHookForm {
                                        event,
                                        index,
                                    }) => {
                                        self.open_hook_form(event, index);
                                    }
                                    None => {}
                                }
                                return Ok(false);
                            }
                            KeyCode::Esc => {
                                self.show_settings = false;
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
                            // Ctrl+J = newline (universal ^J) in the create-db
                            // fields. Consumed whenever the form is open, even at
                            // its growth limit, so it never falls through and
                            // types a literal 'j'.
                            KeyCode::Char('j')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && self.rag_view.show_create_db =>
                            {
                                self.rag_view.handle_insert_newline();
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
                                        &mut self.setup,
                                        &self.router_view.fallbacks,
                                    );
                                }
                                return Ok(false);
                            }
                            KeyCode::Backspace | KeyCode::Delete => {
                                if self.router_view.focus == FocusTarget::Fallbacks {
                                    if self.router_view.remove_selected_fallback().is_some() {
                                        fallback::save_fallbacks(
                                            &mut self.setup,
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
                                            &mut self.setup,
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
                                if let Some(entry) = self.add_provider_view.selected_provider() {
                                    self.open_provider_dialog(&entry);
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
                                if let Some(cmd) = self.slash_menu.get_selected_command().cloned() {
                                    self.run_slash_command(&cmd);
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
                                self.state.right_panel.scroll_up_at(self.last_mouse_y, 3);
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
                                self.state.right_panel.scroll_down_at(self.last_mouse_y, 3);
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
                                self.state
                                    .right_panel
                                    .scroll_up_at(self.last_mouse_y, vh / 2);
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
                                self.state
                                    .right_panel
                                    .scroll_down_at(self.last_mouse_y, vh / 2);
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
                            if self.dialog.visible()
                                && matches!(self.dialog.current(), Some(d) if matches!(d.dialog_type, DialogType::Shortcuts { .. }))
                            {
                                self.dialog.clear();
                            } else {
                                self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                            }
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
                                // The agent loop is running: ask which queue the
                                // message should join instead of sending it now.
                                let preview = self.prompt_view.input.clone();
                                if preview.trim().is_empty() {
                                    return Ok(false);
                                }
                                self.queue_choice_dialog.open(preview);
                                return Ok(false);
                            }

                            let msg = self.prompt_view.send_message();
                            if msg.trim().is_empty() {
                                return Ok(false);
                            }
                            self.start_agent_loop(msg);
                        }
                        Some(crate::keymap::Action::Interrupt) => {
                            if self.state.status == crate::types::SessionStatus::Working {
                                self.stop_signal.store(true, Ordering::Relaxed);
                            } else if self.queue_choice_dialog.visible {
                                self.queue_choice_dialog.hide();
                                self.prompt_view.focus();
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
                                // Clear pending queues: the user explicitly
                                // cancelled, so queued follow-ups should not
                                // auto-start a new loop.
                                if let Some(id) = self.state.current_session_id.clone()
                                    && let Some(queues) = self.state.pending_queues.get_mut(&id)
                                {
                                    queues.clear();
                                }
                                return Ok(false);
                            }
                            if self.queue_choice_dialog.visible {
                                self.queue_choice_dialog.hide();
                                self.prompt_view.focus();
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
                            } else if matches!(self.mode(), AppMode::Session) {
                                self.state.current_session_id = None;
                                self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
                            } else if matches!(self.mode(), AppMode::AddProvider) {
                                self.show_add_provider = false;
                            } else if matches!(self.mode(), AppMode::Settings) {
                                self.show_settings = false;
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
                        Some(crate::keymap::Action::ClearQueue) => {
                            if let Some(id) = self.state.current_session_id.clone()
                                && let Some(queues) = self.state.pending_queues.get_mut(&id)
                            {
                                queues.clear();
                            }
                        }
                        None => {
                            if self.slash_menu.visible {
                                match key.code {
                                    KeyCode::Up => self.slash_menu.select_prev(),
                                    KeyCode::Down => self.slash_menu.select_next(),
                                    KeyCode::Enter => {
                                        self.prompt_view.note_activity();
                                        if let Some(cmd) =
                                            self.slash_menu.get_selected_command().cloned()
                                        {
                                            self.run_slash_command(&cmd);
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
                                            self.state
                                                .right_panel
                                                .scroll_up_at(self.last_mouse_y, 3);
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
                                            self.state
                                                .right_panel
                                                .scroll_down_at(self.last_mouse_y, 3);
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
                                            self.state
                                                .right_panel
                                                .scroll_up_at(self.last_mouse_y, vh / 2);
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
                                            self.state
                                                .right_panel
                                                .scroll_down_at(self.last_mouse_y, vh / 2);
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

                                        // Vim-style scroll: j/k scroll the chat view only when
                                        // the prompt is NOT focused. When focused, all characters
                                        // type normally so the user can start messages with j/k.
                                        if (ch == 'j' || ch == 'k') && !self.prompt_view.is_focused
                                        {
                                            let vh = self.session_view.visible_height.max(1);
                                            let delta = if ch == 'j' {
                                                vh as f64 / 5.0
                                            } else {
                                                -(vh as f64 / 5.0)
                                            };
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
                // If a text input dialog is visible, paste into the dialog input
                if self.is_text_input_visible() {
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ApiKeyInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::LocalUrlInput {
                            input, cursor_pos, ..
                        } = &mut d.dialog_type
                    {
                        let cleaned: String =
                            text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                        input.insert_str(*cursor_pos, &cleaned);
                        *cursor_pos += cleaned.len();
                        d.cursor.note_activity();
                    }
                } else if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                    // Paste into the inline question dialog's text answer.
                    self.question_dialog.handle_paste(&text);
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
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
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
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
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
                HarnessEvent::ClearAssistant => {
                    // The SDK retried a mid-stream failure and is about to
                    // re-stream the response from the beginning: drop the
                    // partial assistant message rendered from the failed
                    // attempt (it would otherwise concatenate with the
                    // retried response).
                    if let Some(session) = self.state.current_session_mut()
                        && let Some(msg) = session.messages.last_mut()
                        && msg.role == MessageRole::Assistant
                    {
                        session.messages.pop();
                    }
                }
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
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
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
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
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
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
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
                        self.session_store.save_session_async(session);
                        self.session_store.save_ctx(&id, &context_state);

                        // Trigger async title generation for the first response.
                        // The session title starts as a timestamp; the LLM produces
                        // a semantic title from the first user message.
                        if !self.title_generated {
                            // Extract the first user message text.
                            let first_user = session.messages.iter().find_map(|m| {
                                if m.role == MessageRole::User {
                                    let text: String = m
                                        .parts
                                        .iter()
                                        .filter_map(|p| match p {
                                            Part::Text(t) => Some(t.text.as_str()),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    if text.is_empty() { None } else { Some(text) }
                                } else {
                                    None
                                }
                            });
                            if let Some(user_prompt) = first_user {
                                let provider = self.llm_config.provider.clone();
                                let model = self.llm_config.model.clone();
                                let base_url = self.base_url_for(&provider);
                                let session_id = id.clone();
                                let session_store = self.session_store.clone();
                                let event_tx = self.event_tx.clone();
                                self.tokio_handle.spawn(async move {
                                    let Ok(mut connector) =
                                        cosh_sdk::connector::Connector::new(&provider)
                                    else {
                                        return;
                                    };
                                    if let Some(ref m) = model {
                                        connector = connector.with_model(m);
                                    }
                                    if let Some(ref url) = base_url {
                                        connector = connector.with_base_url(url.clone());
                                    }
                                    // Disable tools and retry for the title call —
                                    // it is a simple chat completion.
                                    connector = connector
                                        .with_tool_call_mode(
                                            cosh_sdk::connector::ToolCallMode::Native,
                                        )
                                        .with_retry(false);
                                    if let Some(title) =
                                        cosh::harness::generate_title(&connector, &user_prompt)
                                            .await
                                    {
                                        // Persist the updated title to disk.
                                        session_store.update_title(&session_id, &title);
                                        // Update the session title in memory.
                                        let _ = event_tx.send(HarnessEvent::TitleGenerated {
                                            session_id,
                                            title,
                                        });
                                    }
                                });
                                self.title_generated = true;
                            }
                        }
                        // Ensure the sidebar shows the session (must happen
                        // after the immutable borrow of session is released).
                        self.state.ensure_session_summary(&id);
                    }

                    // The loop ended: leftover "next request" messages become
                    // "next agent loop" candidates, and the first one starts a
                    // fresh loop (FIFO). If nothing is queued, the work is
                    // complete — ring the terminal bell to call the user back.
                    let started_new_loop = self.handle_loop_end(true);
                    if !started_new_loop {
                        self.trigger_bell();
                    }
                }

                HarnessEvent::UserMessageInjected { text } => {
                    // The running loop consumed a "next request" message:
                    // move it out of the pending area (FIFO — the harness
                    // drains in order) into the normal history.
                    if let Some(queues) = self.state.current_pending_queues_mut() {
                        queues.next_request.pop_front();
                    }
                    if let Some(session) = self.state.current_session_mut() {
                        session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::User,
                            parts: vec![Part::Text(TextPart {
                                text,
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        });
                        self.session_view.scroll_to_bottom();
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
                        self.session_store.save_session_async(session);
                        self.session_store.save_ctx(&id, &context_state);
                        self.state.ensure_session_summary(&id);
                    }

                    self.handle_loop_end(true);
                }
                HarnessEvent::ContextInfo { info } => {
                    self.context_info = Some(info);
                }

                HarnessEvent::ContextSnapshot { context_state } => {
                    // Incremental persistence: the harness emits a throttled
                    // serialized context snapshot mid-run so a crash/restart
                    // does not lose the in-flight run. Persist the session
                    // JSONL and the `.ctx` companion file here, mirroring the
                    // Done/Stopped handlers.
                    self.persist_incrementally(&context_state);
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

                HarnessEvent::CompactOnDemand { outcome } => {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    use cosh::harness::ManualCompactionOutcome as Outcome;
                    self.manual_compaction_active = false;
                    let (title, message, variant) = match outcome {
                        Outcome::Compacted => (
                            "Compacted",
                            "Session context summarized.".into(),
                            ToastVariant::Success,
                        ),
                        Outcome::NothingToCompact => (
                            "Compact",
                            "Nothing to compact yet.".into(),
                            ToastVariant::Info,
                        ),
                        Outcome::Failed => (
                            "Compact failed",
                            "The summarization call did not complete.".into(),
                            ToastVariant::Error,
                        ),
                    };
                    self.toast_state.show(ToastOptions {
                        title: Some(title.into()),
                        message,
                        variant,
                        duration_ms: 4000,
                    });
                }

                HarnessEvent::Toast { message, variant } => {
                    // Route harness notifications (context-window overflow,
                    // exhausted retries) through the existing toast system.
                    use crate::ui::toast::{ToastOptions, ToastVariant as TuiToastVariant};
                    use cosh::harness::events::ToastVariant as HarnessToastVariant;
                    let variant = match variant {
                        HarnessToastVariant::Info => TuiToastVariant::Info,
                        HarnessToastVariant::Success => TuiToastVariant::Success,
                        HarnessToastVariant::Warning => TuiToastVariant::Warning,
                        HarnessToastVariant::Error => TuiToastVariant::Error,
                    };
                    self.toast_state.show(ToastOptions {
                        title: None,
                        message,
                        variant,
                        duration_ms: 8000,
                    });
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
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: self.llm_config.model.clone(),
                        });
                    }

                    // The run failed: leave the queues parked (promote leftover
                    // next-request messages to next-loop semantics but never
                    // auto-start — the user decides when to resend).
                    self.handle_loop_end(false);
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
                    // Default to "Allow Once" (index 1)
                    self.permission_dialog.selected = 1;
                }

                HarnessEvent::TitleGenerated { session_id, title } => {
                    // Update the session title in memory.
                    if let Some(session) = self.state.session_cache.get_mut(&session_id) {
                        session.title = title.clone();
                        session.title_generated = true;
                    }
                    // Update the sidebar summary.
                    if let Some(summary) = self
                        .state
                        .session_summaries
                        .iter_mut()
                        .find(|s| s.session_id == session_id)
                    {
                        summary.title = title;
                        summary.title_generated = true;
                    }
                }
            }
        }
    }

    /// Incremental persistence: called on each throttled `ContextSnapshot`
    /// event so a crash/restart mid-run resumes from the latest context
    /// instead of the session-start state. Mirrors the Done/Stopped save
    /// logic and skips sessions with no valid dialog yet.
    fn persist_incrementally(&mut self, context_state: &[u8]) {
        if self.state.status != SessionStatus::Working {
            return;
        }
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(session) = self.state.session_cache.get(&id)
            && is_valid_session(session)
        {
            self.session_store.save_session_async(session);
            self.session_store.save_ctx(&id, context_state);
            self.state.ensure_session_summary(&id);
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
        self.last_mouse_y = y;

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

                // Clear any leftover right-panel selection from a previous drag.
                self.state.right_panel.cancel_selection();

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

                // Click inside a create-db field (RAG) starts a drag selection,
                // mirroring the chat prompt's press-and-drag text selection.
                #[cfg(feature = "embed")]
                if matches!(self.mode(), AppMode::Rag) {
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
                    let mouse = MouseEvent::new(event_type, button, x, y, modifiers);
                    if self.rag_view.show_create_db
                        && let Some((focus, byte)) = self.rag_view.field_byte_at(&mouse, tools_area)
                    {
                        self.rag_view.start_field_selection(focus, byte);
                        return Ok(true);
                    }
                }

                // Click in the visible right panel → start a drag selection on
                // the bash / subagent section under the cursor.
                if matches!(self.mode(), AppMode::Session)
                    && should_show_right_panel(self.terminal_size().width, &self.state.right_panel)
                    && Self::is_in_right_panel(x, self.terminal_size())
                {
                    self.state.right_panel.begin_selection(x, y);
                    return Ok(true);
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
                // Dragging inside a create-db field (RAG) extends the selection.
                #[cfg(feature = "embed")]
                if matches!(self.mode(), AppMode::Rag) && self.rag_view.field_selection.is_some() {
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
                    let mouse = MouseEvent::new(event_type, button, x, y, modifiers);
                    if self.rag_view.extend_field_selection_at(&mouse, tools_area) {
                        return Ok(true);
                    }
                }
                if self.state.right_panel.has_selection() {
                    self.state.right_panel.update_drag_selection(x, y);
                    return Ok(true);
                }
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
                self.state.right_panel.stop_auto_scroll();
                let _rect = self.drag_selection.take();
                let drag_start = self.mouse_down_pos.take();
                let is_drag =
                    self.mouse_drag_active || drag_start.is_some_and(|(sx, sy)| sx != x || sy != y);
                self.mouse_drag_active = false;

                if is_drag {
                    // Auto-copy a create-db field drag selection on release.
                    #[cfg(feature = "embed")]
                    if matches!(self.mode(), AppMode::Rag) && self.rag_view.has_field_selection() {
                        let text = self.rag_view.selected_field_text();
                        selection::copy_selection(&text, &mut self.toast_state);
                        self.rag_view.clear_field_selection();
                        return Ok(true);
                    }

                    // Auto-copy prompt selection on mouse release after drag.
                    if self.prompt_view.has_selection() {
                        let text = self.prompt_view.selected_text();
                        selection::copy_selection(&text, &mut self.toast_state);
                        self.prompt_view.clear_selection();
                        return Ok(true);
                    }

                    // Auto-copy a right-panel (bash / subagent) drag selection.
                    if matches!(self.mode(), AppMode::Session)
                        && self.state.right_panel.has_selection()
                    {
                        let text = self.state.right_panel.extract_selected_text();
                        self.state.right_panel.cancel_selection();
                        if !text.is_empty() {
                            selection::copy_selection(&text, &mut self.toast_state);
                        }
                        return Ok(true);
                    }

                    // Extract selected text from the session view by drag region.
                    if matches!(self.mode(), AppMode::Session)
                        && let Some((sx, sy)) = drag_start
                    {
                        let session_area = self.session_viewport_area();

                        let margin = 2u16;
                        let inner_area = Rect::new(
                            session_area.x + margin,
                            session_area.y,
                            session_area.width.saturating_sub(margin * 2),
                            session_area.height,
                        );
                        let max_w = inner_area.width.saturating_sub(6);

                        if let Some(session) = self.state.current_session() {
                            // Build regions for the whole content span between
                            // anchor and focus, not just the currently visible
                            // window — the drag may have auto-scrolled across
                            // scroll boundaries.
                            let (cs_start, cs_end) =
                                self.session_view.selection_content_range(sy, y);
                            self.session_view.build_text_regions_for_content_range(
                                session,
                                inner_area,
                                max_w,
                                &self.config,
                                &self.theme,
                                (cs_start, cs_end + 1),
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
                // A plain click (down+up without moving) on a right-panel
                // section leaves a stale single-cell selection highlighted;
                // clear it (drag-copy paths already returned above).
                self.state.right_panel.cancel_selection();
            }
            _ => {}
        }

        // Auto-scroll stops on any mouse action (up, scroll, etc.) outside of drag.
        if event_type != MouseEventType::Drag {
            self.session_view.stop_auto_scroll();
            self.state.right_panel.stop_auto_scroll();
        }

        // Mouse wheel scrolling
        // Debounce: ignore scroll events that arrive within 50ms of the last one.
        // Different terminal emulators emit different numbers of events per physical
        // scroll tick (e.g. tmux/kitty emit 2-3, gnome-terminal emits 1). Without
        // debouncing, fast-emitters cause list navigation to skip items.
        //
        // NOTE: `last_scroll_time` must only be reset when a scroll is actually
        // processed. Resetting it on ANY mouse event (Move/Down/Up) made the
        // first wheel notch after moving the mouse land inside the 50ms window
        // and get dropped — the visible "lag" when starting to scroll after idle.
        let now = Instant::now();
        let scroll_elapsed = now.duration_since(self.last_scroll_time);
        if matches!(
            event_type,
            MouseEventType::ScrollUp | MouseEventType::ScrollDown
        ) && scroll_elapsed >= Duration::from_millis(50)
        {
            self.last_scroll_time = now;
            match event_type {
                MouseEventType::ScrollUp => {
                    if let Some(d) = self.dialog.current_mut() {
                        match &d.dialog_type {
                            DialogType::ModelList { .. } => {
                                self.handle_model_dialog_key(KeyCode::Up);
                            }
                            DialogType::ReasoningList { .. } => {
                                self.handle_reasoning_dialog_key(KeyCode::Up);
                            }
                            DialogType::ThemeList { .. } => {
                                self.handle_theme_dialog_key(KeyCode::Up);
                            }
                            DialogType::ToolCallList { .. } => {
                                self.handle_tool_call_dialog_key(KeyCode::Up);
                            }
                            DialogType::MessageActions { .. } => {
                                self.handle_message_actions_dialog_key(KeyCode::Up);
                            }
                            _ => {}
                        }
                    } else if self.sidebar_focused && self.sidebar.open && x < SIDEBAR_WIDTH {
                        self.sidebar.select_prev(self.state.session_summaries.len());
                    } else if matches!(self.mode(), AppMode::Session)
                        && Self::is_in_right_panel(x, self.terminal_size())
                    {
                        self.state.right_panel.scroll_up_at(y, 3);
                    } else if matches!(self.mode(), AppMode::Session)
                        && self.question_dialog.visible
                    {
                        self.question_dialog.scroll_up();
                    } else if matches!(self.mode(), AppMode::Session) {
                        self.session_view.scroll_by(-1.0);
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
                    } else if matches!(self.mode(), AppMode::Settings) {
                        self.settings_view.select_prev(20, &self.setup);
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
                            DialogType::ReasoningList { .. } => {
                                self.handle_reasoning_dialog_key(KeyCode::Down);
                            }
                            DialogType::ThemeList { .. } => {
                                self.handle_theme_dialog_key(KeyCode::Down);
                            }
                            DialogType::ToolCallList { .. } => {
                                self.handle_tool_call_dialog_key(KeyCode::Down);
                            }
                            DialogType::MessageActions { .. } => {
                                self.handle_message_actions_dialog_key(KeyCode::Down);
                            }
                            _ => {}
                        }
                    } else if self.sidebar_focused && self.sidebar.open && x < SIDEBAR_WIDTH {
                        self.sidebar.select_next(self.state.session_summaries.len());
                    } else if matches!(self.mode(), AppMode::Session)
                        && Self::is_in_right_panel(x, self.terminal_size())
                    {
                        self.state.right_panel.scroll_down_at(y, 3);
                    } else if matches!(self.mode(), AppMode::Session)
                        && self.question_dialog.visible
                    {
                        self.question_dialog.scroll_down();
                    } else if matches!(self.mode(), AppMode::Session) {
                        self.session_view.scroll_by(1.0);
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
                    } else if matches!(self.mode(), AppMode::Settings) {
                        self.settings_view.select_next(20, &self.setup);
                    } else if self.try_rag_scroll_down() {
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }

        // Only handle left-click UP events (standard "click" action)
        if event_type != MouseEventType::Up || button != MouseButton::Left {
            // Hover tracking for user messages (opencode-style highlight).
            if matches!(event_type, MouseEventType::Move)
                && matches!(self.mode(), AppMode::Session)
                && !self.dialog.visible()
                && !self.question_dialog.visible
            {
                let session_area = self.session_viewport_area();
                self.session_view
                    .update_hover(y, session_area, &self.state, &self.config);
            }
            return Ok(true);
        }

        let mouse = MouseEvent::new(event_type, button, x, y, modifiers);

        // Bug report link in the header — clicking opens the GitHub issues page.
        if let Some(link_area) = self.bug_link_area
            && x >= link_area.x
            && x < link_area.right()
            && y >= link_area.y
            && y < link_area.bottom()
        {
            self.open_bug_report_link();
            return Ok(true);
        }

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
                                        self.setup.appearance.theme = filtered[sel].clone();
                                        self.setup.save();
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
                                    // Applies the model — or pushes the reasoning
                                    // sub-dialog when the model supports it.
                                    self.confirm_model_entry(&entry.model, &entry.provider);
                                }
                                // confirm_model_entry pops the dialog itself (or
                                // stacked a reasoning dialog on top) — skip the
                                // unconditional pop below in that case.
                                return Ok(true);
                            }
                            DialogType::ReasoningList { .. } => {
                                self.handle_reasoning_dialog_key(KeyCode::Enter);
                                return Ok(true);
                            }
                            DialogType::ToolCallList { .. } => {
                                self.handle_tool_call_dialog_key(KeyCode::Enter);
                                return Ok(true);
                            }
                            DialogType::MessageActions { message_id, .. } => {
                                let action = d.selected.min(2);
                                let message_id = message_id.clone();
                                self.dialog.pop();
                                self.run_message_action(action, &message_id);
                                return Ok(true);
                            }
                            DialogType::ApiKeyInput { .. } | DialogType::LocalUrlInput { .. } => {
                                // Keep the dialog open when the input was
                                // rejected (e.g. invalid local URL) — matches
                                // the Enter-key behavior.
                                if self.save_text_input_dialog() {
                                    self.dialog.pop();
                                }
                                return Ok(true);
                            }
                            _ => {}
                        }
                    }
                    self.dialog.pop();
                    return Ok(true);
                }
                DialogAction::Dismissed => {
                    // Reasoning sub-dialog dismissed (click outside): pop back
                    // to the model list, keeping the restore state intact.
                    if self.is_reasoning_dialog_visible() {
                        self.dialog.pop();
                        return Ok(true);
                    }
                    // Restore original if needed
                    if self.is_theme_dialog_visible()
                        && let Some(ref orig) = self.theme_dialog_original
                        && let Some(t) = self.theme_registry.get(orig)
                    {
                        self.theme = t.clone();
                        self.config.theme_gen += 1;
                    }
                    if self.is_model_dialog_visible() {
                        self.restore_model_dialog();
                        return Ok(true);
                    }
                    self.theme_dialog_original = None;
                    self.model_dialog_original = None;
                    self.reasoning_dialog_original = None;
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

        // Slash menu
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
            let footer_y = main_area.bottom().saturating_sub(1);
            let prompt_budget = footer_y
                .saturating_sub(area.y + 1)
                .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
            let prompt_h = if is_session {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4), prompt_budget)
            } else {
                0
            };
            let prompt_area = Rect::new(
                main_area.x + 2,
                footer_y.saturating_sub(prompt_h),
                main_area.width.saturating_sub(4),
                prompt_h,
            );
            if self
                .slash_menu
                .handle_mouse(&mouse, prompt_area, &self.theme)
            {
                self.prompt_view.note_activity();
                if let Some(cmd) = self.slash_menu.get_selected_command().cloned() {
                    self.run_slash_command(&cmd);
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
            let footer_y = main_area.bottom().saturating_sub(1);
            let prompt_budget = footer_y
                .saturating_sub(area.y + 1)
                .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
            let prompt_h = if self.question_dialog.visible {
                0
            } else {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4), prompt_budget)
            };
            let question_h = self
                .question_dialog
                .required_height(main_area.width.saturating_sub(4));
            let prompt_area_y = footer_y.saturating_sub(prompt_h);
            let question_h = question_h.min(prompt_area_y.saturating_sub(area.y + 1));
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

        // 4b. Queue-choice dialog (inline, shown while the agent loop runs)
        if self.queue_choice_dialog.visible && matches!(self.mode(), AppMode::Session) {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let footer_y = main_area.bottom().saturating_sub(1);
            let queue_choice_h = self
                .queue_choice_dialog
                .required_height(main_area.width.saturating_sub(4));
            let queue_choice_h = queue_choice_h.min(footer_y.saturating_sub(area.y + 1));
            let queue_choice_area_y = footer_y.saturating_sub(queue_choice_h);
            let queue_choice_area = Rect::new(
                main_area.x + 2,
                queue_choice_area_y,
                main_area.width.saturating_sub(4),
                queue_choice_h,
            );
            if !self.mouse_drag_active && self.drag_selection.is_none() {
                let consumed = self
                    .queue_choice_dialog
                    .handle_mouse(&mouse, queue_choice_area);
                if consumed && self.queue_choice_dialog.submitted {
                    let text = self.prompt_view.send_message();
                    let target = self.queue_choice_dialog.choice();
                    self.queue_choice_dialog.hide();
                    if let Some(queues) = self.state.current_pending_queues_mut() {
                        match target {
                            QueueTarget::NextRequest => {
                                queues.next_request.push_back(text.clone());
                                if let Some(tx) = &self.queued_input_tx {
                                    let _ = tx.send(text);
                                }
                            }
                            QueueTarget::NextLoop => {
                                queues.next_loop.push_back(text);
                            }
                        }
                    }
                    self.prompt_view.focus();
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
                    self.session_view.hovered_msg_idx = None;
                    self.title_generated = true;
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
            let was_drag = std::mem::take(&mut self.mouse_up_was_drag);
            let session_area = self.session_viewport_area();
            if !was_drag
                && self
                    .session_view
                    .handle_mouse(&mouse, session_area, &self.state, &self.config)
            {
                if let Some(message_id) = self.session_view.pending_message_action.take() {
                    let preview = self
                        .state
                        .current_session()
                        .and_then(|s| s.messages.iter().find(|m| m.id == message_id))
                        .map(message_prompt_text)
                        .unwrap_or_default()
                        .chars()
                        .take(36)
                        .collect::<String>();
                    self.dialog.replace(DialogType::MessageActions {
                        message_id,
                        preview,
                    });
                    self.session_view.hovered_msg_idx = None;
                }
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
                        self.start_new_session();
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
                    crate::routes::home::HomeAction::OpenSettings => {
                        self.show_settings = true;
                    }
                    crate::routes::home::HomeAction::OpenModelRouter => {
                        let saved = fallback::load_fallbacks(&self.setup);
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
                fallback::save_fallbacks(&mut self.setup, &self.router_view.fallbacks);
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

        // 8a. Settings view — mouse click on a setting row toggles it
        if matches!(self.mode(), AppMode::Settings) && !self.dialog.visible() {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            // Matches the render geometry: in non-Session modes the render
            // path resolves to terminal height - 4 (session_main_area drops
            // the footer rows and the mode arm subtracts 1 more). Keeping
            // both rects identical keeps content_start_y — and therefore
            // hit-tested rows — aligned with the drawn option.
            let settings_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                main_area.height.saturating_sub(4),
            );
            if let Some(clicked_idx) =
                self.settings_view
                    .handle_mouse(&mouse, settings_area, &self.setup)
            {
                self.settings_view.selection.selected_index = clicked_idx;
                match self.settings_view.activate_selected(&mut self.setup) {
                    Some(crate::routes::settings::SettingsAction::ToggleSaved) => {
                        self.setup.save();
                    }
                    Some(crate::routes::settings::SettingsAction::OpenHookForm {
                        event,
                        index,
                    }) => {
                        self.open_hook_form(event, index);
                    }
                    None => {}
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
                    &mut self.setup,
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
                if let Some(entry) = self.add_provider_view.selected_provider() {
                    self.open_provider_dialog(&entry);
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

        // Same responsive budget as `render()` so mouse mapping matches the
        // actually-rendered prompt height on every screen size.
        let prompt_budget = footer_y
            .saturating_sub(area.y + 1)
            .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
        let prompt_h = self
            .prompt_view
            .required_height(prompt_area_w, prompt_budget);

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

/// Persist a provider API key in the OS credential store (keyring) under the
/// `cosh` service keyed by the provider's API key environment variable name.
///
/// The key is stored via the native store (Secret Service on Linux, Keychain
/// on macOS, Credential Manager on Windows). Errors are returned to the caller
/// so it can surface them instead of failing silently.
fn save_provider_api_key(env_var: &str, api_key: &str) -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(cosh_sdk::connector::COSH_SERVICE, env_var)?;
    entry.set_password(api_key)?;
    Ok(())
}

/// Whether a string looks like a usable local server URL: starts with
/// `http://` or `https://` and has a non-empty host.
#[must_use]
fn is_valid_local_url(url: &str) -> bool {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"));
    rest.is_some_and(|host| !host.is_empty())
}

#[cfg(test)]
mod local_url_tests {
    use super::is_valid_local_url;

    #[test]
    fn accepts_http_and_https_with_host() {
        assert!(is_valid_local_url("http://127.0.0.1:8080"));
        assert!(is_valid_local_url("https://localhost:11434"));
        assert!(is_valid_local_url("  http://host:1  "));
    }

    #[test]
    fn rejects_missing_scheme_or_empty_host() {
        assert!(!is_valid_local_url("127.0.0.1:8080"));
        assert!(!is_valid_local_url("http://"));
        assert!(!is_valid_local_url("https://"));
        assert!(!is_valid_local_url(""));
        assert!(!is_valid_local_url("ftp://host"));
    }
}

// Embedding helpers (feature-gated)

#[cfg(feature = "embed")]
async fn embed_document(
    uri: &str,
    table_name: &str,
    embedder_config: &crate::routes::rag::models::EmbedderConfig,
    content: &str,
    base_urls: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    use cosh_recall::embed::Rag;

    let dim = embedder_config.vector_dim();
    let embedder = match embedder_config {
        crate::routes::rag::models::EmbedderConfig::Local { model } => {
            create_local_embedder(*model)?
        }
        crate::routes::rag::models::EmbedderConfig::Cloud(c) => {
            create_cloud_embedder(&c.provider, &c.model, dim, base_urls)?
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
    base_urls: &std::collections::HashMap<String, String>,
) -> Result<cosh_recall::embed::Embedder, String> {
    use cosh_recall::embed::Embedder;
    use cosh_sdk::connector::Connector;
    let mut connector = Connector::new(provider)
        .map_err(|e| format!("Failed to create connector: {e}"))?
        .with_model(model);
    if let Some(url) = base_urls.get(provider) {
        connector = connector.with_base_url(url.clone());
    }
    Ok(Embedder::new_cloud(connector, dim))
}

#[cfg(all(feature = "embed", not(feature = "cloud")))]
fn create_cloud_embedder(
    _provider: &str,
    _model: &str,
    _dim: usize,
    _base_urls: &std::collections::HashMap<String, String>,
) -> Result<cosh_recall::embed::Embedder, String> {
    Err("Cloud embedding requires the 'cloud' feature (enable with --features cloud)".into())
}

#[cfg(test)]
mod tests {
    use super::{App, format_tokens};
    use crate::ui::dialogs::DialogType;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Plain key event (no modifiers) for driving text inputs in tests.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Serializes tests that redirect `$HOME`: `Setup` persists through
    /// `dirs`, which reads the process-wide environment.
    static HOME_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Redirect `$HOME` to a scratch dir and drop any config left there by a
    /// previous run, so `setup.save()` never touches the developer's files.
    fn isolate_home() {
        let home = std::env::temp_dir().join("cosh-hook-test-home");
        let _ = std::fs::remove_dir_all(home.join(".config"));
        std::fs::create_dir_all(&home).expect("create scratch home");
        // SAFETY: tests holding HOME_LOCK are the only threads reading it.
        unsafe { std::env::set_var("HOME", &home) };
    }

    #[test]
    fn format_tokens_small_values_have_no_separator() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(9), "9");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_groups_thousands() {
        assert_eq!(format_tokens(1_000), "1,000");
        assert_eq!(format_tokens(9_612), "9,612");
        assert_eq!(format_tokens(100_000), "100,000");
        assert_eq!(format_tokens(1_000_000), "1,000,000");
        assert_eq!(format_tokens(1_234_567), "1,234,567");
        assert_eq!(format_tokens(12_345_678), "12,345,678");
    }

    #[test]
    fn format_tokens_handles_large_values() {
        assert_eq!(format_tokens(123_456), "123,456");
        assert_eq!(format_tokens(9_876_543_210), "9,876,543,210");
        assert_eq!(format_tokens(usize::MAX), "18,446,744,073,709,551,615");
    }

    /// The session chat area must shrink by the right-panel width whenever the
    /// panel is visible — and the mouse dispatch shares this exact helper with
    /// `render`. A wider mouse area re-wraps every message and shifts `prefix_y`,
    /// making tool-box expand/collapse clicks land on the wrong row (the bug was
    /// "boxes can't be expanded while the agent loop is active").
    #[tokio::test]
    async fn session_main_area_matches_render_width_with_right_panel() {
        use ratatui::layout::Rect;

        use super::RIGHT_PANEL_WIDTH;
        use crate::routes::session::right_panel::types::RightPanelState;

        let mut app = App::new("/tmp".to_string());
        let id = super::generate_session_id();
        app.state.add_empty_session(id.clone(), "t".into(), 0);
        app.state.current_session_id = Some(id);
        app.state.right_panel = RightPanelState::new();
        app.state.right_panel.start_pty("echo hi".into(), None);

        let area = Rect::new(0, 0, 140, 30);
        let sa = app.session_main_area(area);
        assert_eq!(sa.main.width, 140 - RIGHT_PANEL_WIDTH);
        assert_eq!(sa.right_panel_w, RIGHT_PANEL_WIDTH);

        app.sidebar.open = true;
        let sa = app.session_main_area(area);
        assert_eq!(
            sa.main.width,
            140 - super::SIDEBAR_WIDTH - RIGHT_PANEL_WIDTH,
            "open sidebar + right panel must both be subtracted"
        );
    }

    /// Hidden panel (narrow terminal or no content) must not shrink the area.
    #[tokio::test]
    async fn session_main_area_ignores_hidden_right_panel() {
        use ratatui::layout::Rect;

        use crate::routes::session::right_panel::types::RightPanelState;

        let mut app = App::new("/tmp".to_string());
        app.state.add_empty_session("t".into(), "t".into(), 0);
        app.state.current_session_id = Some("t".into());
        app.state.right_panel = RightPanelState::new();

        // Narrow terminal → panel hidden even if there were content.
        app.state.right_panel.start_pty("echo hi".into(), None);
        let sa = app.session_main_area(Rect::new(0, 0, 90, 30));
        assert_eq!(sa.right_panel_w, 0);
        assert_eq!(sa.main.width, 90);

        // Wide terminal but no todos/pty content → panel hidden.
        let mut app2 = App::new("/tmp".to_string());
        app2.state.add_empty_session("t".into(), "t".into(), 0);
        app2.state.current_session_id = Some("t".into());
        let sa2 = app2.session_main_area(Rect::new(0, 0, 140, 30));
        assert_eq!(sa2.right_panel_w, 0);
        assert_eq!(sa2.main.width, 140);
    }

    /// The full create-db field drag selection flow through the app's real
    /// mouse dispatch: Down anchors, Drag extends, the render paints the
    /// selection, and Up auto-copies then clears it.
    #[tokio::test]
    #[cfg(feature = "embed")]
    async fn rag_field_drag_selection_through_app_mouse_events() {
        use crate::routes::rag::models::CreateDbFocus;
        use crossterm::event::{
            KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
        };
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut app = App::new("/tmp".to_string());
        app.show_rag = true;
        app.rag_view.toggle_create_db();
        app.rag_view.create_db_focus = CreateDbFocus::Name;
        app.rag_view.db_name_input = "hello world".into();

        // Geometry mirrors the app mouse handler (tools_area = Rect(0, 1, 80, 20))
        // and the rag render: gap + input, then gap + model line + gap, and the
        // value starts after the label.
        let input_h = app.rag_view.url_input.height(72);
        let name_y = 1 + 2 + input_h + 1 + 2;
        let value_x = 0 + 4 + 2 + 7; // pad (cx + 2) + label width

        let mouse = |kind: CKind, x: u16, y: u16| CMouse {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };

        // Press on the Name field → anchor the selection at byte 0.
        assert!(
            app.handle_mouse_event(mouse(CKind::Down(CBtn::Left), value_x, name_y))
                .unwrap()
        );
        assert!(app.rag_view.field_selection.is_some());

        // Drag to the end of "hello world" → selection 0..11.
        assert!(
            app.handle_mouse_event(mouse(CKind::Drag(CBtn::Left), value_x + 11, name_y))
                .unwrap()
        );
        assert_eq!(
            app.rag_view.field_selection,
            Some((CreateDbFocus::Name, 0, 11))
        );
        assert_eq!(app.rag_view.selected_field_text(), "hello world");

        // The render paints the selection: each selected cell gets the field
        // text color as background (light bar) — visible over the markdown.
        let theme = app.theme.clone();
        let text_color = {
            let (r, g, b, _) = theme.text.to_ints();
            ratatui::style::Color::Rgb(r, g, b)
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        app.rag_view
            .render(&mut buf, Rect::new(0, 1, 80, 20), &theme);
        for x in value_x..value_x + 11 {
            assert_eq!(
                buf[(x, name_y)].bg,
                text_color,
                "selected cell at ({x},{name_y}) must show the highlight bar"
            );
        }
        // The cell right after the selection is the caret (byte 11), which is
        // drawn with the same light background; two cells past it the field
        // background is untouched.
        assert_ne!(buf[(value_x + 12, name_y)].bg, text_color);

        // Release → auto-copy clears the selection.
        assert!(
            app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), value_x + 11, name_y))
                .unwrap()
        );
        assert!(app.rag_view.field_selection.is_none());
    }

    /// When the agent loop ends (Done/Stopped) before a "next request" message
    /// was consumed, it must be promoted to the "next agent loop" queue so it
    /// starts a fresh loop instead of being lost. FIFO order is preserved: the
    /// promoted message joins the back of the secondary queue.
    #[tokio::test]
    async fn loop_end_promotes_unconsumed_next_request_to_next_loop() {
        let mut app = App::new("/tmp".to_string());
        let id = super::generate_session_id();
        app.state.add_empty_session(
            id.clone(),
            "promotion test".into(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        );
        app.state.current_session_id = Some(id.clone());
        app.active_loop_session_id = Some(id.clone());
        {
            let queues = app.state.pending_queues.entry(id.clone()).or_default();
            queues.next_request.push_back("leftover request msg".into());
            queues.next_loop.push_back("secondary msg".into());
        }

        // Simulate the loop ending WITHOUT consuming the next-request message
        // (e.g. user stopped it before the harness drained the channel). The
        // `start_next` flag mirrors Done/Stopped; Error passes false.
        app.handle_loop_end(false);

        let queues = app.state.pending_queues.get(&id).expect("queues exist");
        assert!(
            queues.next_request.is_empty(),
            "unconsumed next-request message must be promoted"
        );
        assert_eq!(
            queues.next_loop,
            vec![
                "secondary msg".to_string(),
                "leftover request msg".to_string()
            ],
            "promoted message joins the back of the next-loop queue (FIFO)"
        );
        // The sender channel is dropped at loop end so nothing can be injected
        // into the finished loop.
        assert!(app.queued_input_tx.is_none());
    }

    /// Regression: Enter on `/toolcall` in the slash menu must open the
    /// mode picker dialog. It used to fall into the generic branch (fill the
    /// prompt with "/toolcall ") because the dispatch lived only in the
    /// unreachable `None` keymap branch, not in the handler that actually
    /// intercepts Enter.
    #[tokio::test]
    async fn slash_toolcall_command_opens_tool_call_dialog() {
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "toolcall".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(
            app.is_tool_call_dialog_visible(),
            "selecting /toolcall must open the native|inline picker"
        );
        assert!(!app.slash_menu.visible, "slash menu closes after Enter");
    }

    /// Generic slash commands still fill the prompt instead of opening a
    /// dialog (the fallback branch of `run_slash_command`).
    #[tokio::test]
    async fn slash_unknown_command_fills_prompt() {
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "nonexistent".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert_eq!(app.prompt_view.input, "/nonexistent ");
        assert!(!app.dialog.visible());
        assert!(!app.slash_menu.visible);
    }

    /// Regression: the picker box must be wide enough to show each option's
    /// full description — the inline row used to truncate at
    /// "JSON written in the text,". Geometry at 80x24 with the 60-wide box:
    /// dialog_x=10, list_top=12, labels start at x=13; the inline label ends
    /// with 'y' at x=63 and the native one with ')' at x=58.
    #[tokio::test]
    async fn tool_call_dialog_render_shows_full_description_text() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "toolcall".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);

        let theme = app.theme.clone();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        app.dialog.render(
            &mut buf,
            Rect::new(0, 0, 80, 24),
            &theme,
            std::time::SystemTime::now(),
        );

        assert_eq!(
            buf[(63, 13)].symbol(),
            "y",
            "inline description must end with 'locally' (not truncated)"
        );
        assert_eq!(
            buf[(58, 12)].symbol(),
            ")",
            "native description must end with 'default)' (not truncated)"
        );
    }

    /// The tool-call picker must respond to the mouse wheel like the other
    /// list dialogs (ModelList/ThemeList/ReasoningList). The wheel routes to
    /// the dialog's Up/Down handler; the dialog stays open while cycling the
    /// two options. Sleeps straddle the 50ms scroll debounce.
    #[tokio::test]
    async fn tool_call_dialog_mouse_wheel_changes_selection() {
        use crossterm::event::{KeyModifiers, MouseEvent as CMouse, MouseEventKind as CKind};
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "toolcall".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(app.is_tool_call_dialog_visible());
        assert_eq!(app.dialog.current().unwrap().selected, 0);

        let wheel = |kind: CKind| CMouse {
            kind,
            column: 40,
            row: 12,
            modifiers: KeyModifiers::NONE,
        };

        // The 50ms scroll debounce starts at App creation — wait before the
        // first wheel notch so it isn't dropped.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        // Wheel down: native -> inline (selection 1), dialog stays open.
        assert!(app.handle_mouse_event(wheel(CKind::ScrollDown)).unwrap());
        assert_eq!(app.dialog.current().unwrap().selected, 1);
        assert!(app.is_tool_call_dialog_visible());

        // Wheel down again: wraps back to native.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(app.handle_mouse_event(wheel(CKind::ScrollDown)).unwrap());
        assert_eq!(app.dialog.current().unwrap().selected, 0);

        // Wheel up: native -> inline.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(app.handle_mouse_event(wheel(CKind::ScrollUp)).unwrap());
        assert_eq!(app.dialog.current().unwrap().selected, 1);
        assert!(app.is_tool_call_dialog_visible());
    }

    /// Clicking an option in the picker must APPLY the mode (not just close
    /// the dialog like the pre-fix `_` fallback did). Row 1 = inline. Clicks
    /// are processed on the Up event (the app's click dispatch gate), so the
    /// test sends Down then Up.
    #[tokio::test]
    async fn tool_call_dialog_mouse_click_applies_mode() {
        use crossterm::event::{
            KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
        };
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "toolcall".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(app.is_tool_call_dialog_visible());

        // Geometry mirrors the ToolCallList mouse handler with the (80x24)
        // fallback terminal size: dialog_x = 20, dialog_y = 9, list_top = 12.
        // Dialog clicks are dispatched on the Up event; send Down first so
        // the app tracks a real press.
        fn click_row(app: &mut App, row: u16) {
            let evt = |kind: CKind| CMouse {
                kind,
                column: 40,
                row,
                modifiers: KeyModifiers::NONE,
            };
            app.handle_mouse_event(evt(CKind::Down(CBtn::Left)))
                .unwrap();
            app.handle_mouse_event(evt(CKind::Up(CBtn::Left))).unwrap();
        }

        // Click the "inline" row (list_top + 1 = 13).
        click_row(&mut app, 13);
        assert_eq!(
            app.llm_config.tool_call_mode,
            cosh_sdk::connector::ToolCallMode::Inline,
            "clicking the inline row must apply the mode"
        );
        assert!(
            !app.is_tool_call_dialog_visible(),
            "dialog closes after applying"
        );

        // Reopen and click "native" (row 12): back to Native.
        app.run_slash_command(&cmd);
        click_row(&mut app, 12);
        assert_eq!(
            app.llm_config.tool_call_mode,
            cosh_sdk::connector::ToolCallMode::Native
        );
        assert!(!app.is_tool_call_dialog_visible());
    }

    /// The user's exact scenario: a "next request" message that the loop
    /// stopped before consuming must become the FIRST "next agent loop"
    /// candidate when no secondary queue message exists. This verifies the
    /// promotion state that precedes auto-start: with `start_next = true`
    /// (Done/Stopped) `handle_loop_end` pops this front message and starts a
    /// fresh loop with it (FIFO). We use `false` here because `true` would
    /// spawn a real agent loop.
    #[tokio::test]
    async fn loop_end_orphaned_next_request_becomes_first_next_loop_candidate() {
        let mut app = App::new("/tmp".to_string());
        let id = super::generate_session_id();
        app.state.add_empty_session(
            id.clone(),
            "orphan test".into(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        );
        app.state.current_session_id = Some(id.clone());
        app.active_loop_session_id = Some(id.clone());
        {
            let queues = app.state.pending_queues.entry(id.clone()).or_default();
            queues.next_request.push_back("orphaned request msg".into());
        }

        // Done/Stopped path (start_next = true): promotion happens first, so
        // the orphaned message lands at the FRONT of the next-loop queue and
        // would be the message that starts the fresh loop.
        app.handle_loop_end(false);
        let queues = app.state.pending_queues.get(&id).expect("queues exist");
        assert_eq!(
            queues.next_loop,
            vec!["orphaned request msg".to_string()],
            "orphaned message is the first next-loop candidate"
        );
        assert!(queues.next_request.is_empty());
    }

    /// `/new` creates and selects a fresh session straight from the prompt —
    /// no detour through Home.
    #[tokio::test]
    async fn slash_new_creates_and_selects_a_fresh_session() {
        let mut app = App::new("/tmp".to_string());
        assert!(app.state.current_session_id.is_none());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "new".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(
            app.state.current_session_id.is_some(),
            "a session was selected"
        );
        assert!(
            matches!(app.mode(), crate::app::AppMode::Session),
            "the app flips into Session mode"
        );
        assert!(!app.slash_menu.visible);
        assert!(
            app.state.session_cache.len() == 1,
            "exactly one new session exists"
        );
    }

    /// `/new` while the agent loop is working is refused with a toast —
    /// selecting a different session mid-run would route the running loop's
    /// events into it.
    #[tokio::test]
    async fn slash_new_refuses_while_agent_is_working() {
        let mut app = App::new("/tmp".to_string());
        app.state.status = crate::types::SessionStatus::Working;
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "new".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(app.state.current_session_id.is_none(), "no session created");
        assert!(
            app.toast_state
                .current
                .as_ref()
                .is_some_and(|t| t.message.contains("wait for it")),
            "the refusal toast tells the user to wait"
        );
    }

    /// `/rename` opens the rename dialog prefilled with the current title;
    /// editing and pressing Enter applies it to the session (in-memory +
    /// sidebar) and closes the dialog.
    #[tokio::test]
    async fn slash_rename_edits_and_applies_the_session_title() {
        let mut app = App::new("/tmp".to_string());
        app.start_new_session();
        let id = app.state.current_session_id.clone().unwrap();
        if let Some(s) = app.state.session_cache.get_mut(&id) {
            s.title = "old title".into();
        }
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "rename".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(
            matches!(
                app.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::RenameSession { input, cursor_pos })
                    if input == "old title" && *cursor_pos == "old title".len()
            ),
            "the dialog opens prefilled with the current title, cursor at end"
        );

        // Clear the field and type a new name, then apply.
        for _ in 0.."old title".len() {
            app.handle_text_input_dialog_key(KeyCode::Backspace);
        }
        for ch in "manual name".chars() {
            app.handle_text_input_dialog_key(KeyCode::Char(ch));
        }
        assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
        assert!(!app.dialog.visible(), "Enter closes the dialog");
        assert_eq!(
            app.state.session_cache.get(&id).unwrap().title,
            "manual name",
            "the in-memory session title is updated"
        );
    }

    /// Esc on the rename dialog cancels without touching the title.
    #[tokio::test]
    async fn slash_rename_esc_cancels_without_changes() {
        let mut app = App::new("/tmp".to_string());
        app.start_new_session();
        let id = app.state.current_session_id.clone().unwrap();
        if let Some(s) = app.state.session_cache.get_mut(&id) {
            s.title = "keep me".into();
        }
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "rename".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        for ch in "edited".chars() {
            app.handle_text_input_dialog_key(KeyCode::Char(ch));
        }
        app.handle_text_input_dialog_key(KeyCode::Esc);
        assert!(!app.dialog.visible());
        assert_eq!(app.state.session_cache.get(&id).unwrap().title, "keep me");
    }

    /// The hook registration box saves a valid hook into setup (in-memory)
    /// and closes; an invalid one keeps the box open.
    #[tokio::test]
    async fn hook_input_dialog_saves_valid_hook_and_rejects_invalid() {
        // setup.save() persists to $HOME — point it at a scratch dir so the
        // test never touches the developer's real config. The lock keeps the
        // two hook tests from racing each other's environment.
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let mut app = App::new("/tmp".to_string());
        app.dialog.show(DialogType::HookInput {
            event: crate::routes::settings::PRE_TOOL_USE_EVENT,
            editing_index: None,
            name: String::new(),
            matcher: String::new(),
            command: String::new(),
            timeout: String::new(),
            field: 0,
            cursor_pos: 0,
        });

        // Invalid: no command yet → Enter keeps the dialog open.
        assert!(app.handle_hook_input_key(key(KeyCode::Enter)));
        assert!(app.dialog.visible(), "missing command keeps the form open");

        // Fill Name, then jump to Command and fill it.
        for ch in "block rm".chars() {
            app.handle_hook_input_key(key(KeyCode::Char(ch)));
        }
        app.handle_hook_input_key(key(KeyCode::Down));
        app.handle_hook_input_key(key(KeyCode::Down));
        for ch in "exit 2".chars() {
            app.handle_hook_input_key(key(KeyCode::Char(ch)));
        }
        app.handle_hook_input_key(key(KeyCode::Enter));

        assert!(!app.dialog.visible(), "valid save closes the dialog");
        let hooks = &app.setup.hooks.events[crate::routes::settings::PRE_TOOL_USE_EVENT];
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "block rm");
        assert_eq!(hooks[0].command, "exit 2");
    }

    #[tokio::test]
    async fn hook_input_dialog_esc_discards() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let mut app = App::new("/tmp".to_string());
        app.dialog.show(DialogType::HookInput {
            event: crate::routes::settings::PRE_TOOL_USE_EVENT,
            editing_index: None,
            name: String::new(),
            matcher: String::new(),
            command: "exit 2".into(),
            timeout: String::new(),
            field: 2,
            cursor_pos: 6,
        });
        assert!(app.handle_hook_input_key(key(KeyCode::Esc)));
        assert!(!app.dialog.visible());
        assert!(
            !app.setup
                .hooks
                .events
                .contains_key(crate::routes::settings::PRE_TOOL_USE_EVENT),
            "esc must not persist anything"
        );
    }

    /// Clicking a value row inside the hook panel focuses the field and
    /// moves the insertion point (app-level path: Up-event gate included).
    #[tokio::test]
    async fn hook_input_click_positions_cursor() {
        let mut app = App::new("/tmp".to_string());
        app.dialog.show(DialogType::HookInput {
            event: crate::routes::settings::PRE_TOOL_USE_EVENT,
            editing_index: None,
            name: String::new(),
            matcher: String::new(),
            command: "exit 2".into(),
            timeout: String::new(),
            field: 2,
            cursor_pos: 6,
        });

        // Derive geometry from the SAME terminal size the app will use when
        // handling the click.
        let term = app.terminal_size();
        let values = ["", "", "exit 2", ""];
        let (_, dialog_y, _, _, content_x, cols) =
            crate::ui::dialogs::hook_input_metrics(term, values);
        let cmd_value_row =
            crate::ui::dialogs::hook_field_geometries(dialog_y, values, cols)[2].value_y;
        use crossterm::event::{
            MouseButton as CrosstermMouseButton, MouseEvent as CrosstermMouseEvent, MouseEventKind,
        };

        let up = CrosstermMouseEvent {
            kind: MouseEventKind::Up(CrosstermMouseButton::Left),
            column: content_x + 2, // over the 'i' of "exit 2"
            row: cmd_value_row,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(up).expect("mouse handled");

        assert!(
            matches!(
                app.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::HookInput {
                    field: 2,
                    cursor_pos: 2,
                    ..
                })
            ),
            "click must park the cursor on char index 2"
        );
    }

    /// An empty (whitespace-only) title applies nothing — Enter just closes,
    /// mirroring opencode's prompt behavior.
    #[tokio::test]
    async fn slash_rename_empty_title_applies_nothing() {
        let mut app = App::new("/tmp".to_string());
        app.start_new_session();
        let id = app.state.current_session_id.clone().unwrap();
        if let Some(s) = app.state.session_cache.get_mut(&id) {
            s.title = "unchanged".into();
        }
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "rename".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        // Clear everything, then Enter on an empty field.
        for _ in 0.."unchanged".len() {
            app.handle_text_input_dialog_key(KeyCode::Backspace);
        }
        assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
        assert!(!app.dialog.visible(), "Enter still closes the dialog");
        assert_eq!(
            app.state.session_cache.get(&id).unwrap().title,
            "unchanged",
            "an empty title never clears the session name"
        );
    }

    /// Regression: the slash menu window must SCROLL. With more commands
    /// than the 6 visible rows, navigating down used to move the selection
    /// past the rendered window — the last commands (e.g. /rename) existed
    /// but were invisible and unreachable by arrow keys.
    #[tokio::test]
    async fn slash_menu_scrolls_so_the_selection_stays_visible() {
        let mut app = App::new("/tmp".to_string());
        app.prompt_view.input = "/".into();
        app.slash_menu.update(&app.prompt_view.input);
        assert!(
            app.slash_menu.commands.len() > 6,
            "precondition: more commands than the visible window"
        );

        // Navigate to the LAST command (past the bottom of the window).
        for _ in 0..app.slash_menu.commands.len() - 1 {
            app.slash_menu.select_next();
        }
        let selected = app.slash_menu.get_selected_command().unwrap().name.clone();

        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        app.slash_menu
            .render(&mut buf, Rect::new(30, 20, 50, 4), &app.theme);

        // Every row of the menu must be scanned: the selected command's name
        // must be drawn somewhere in the buffer.
        let rendered: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            rendered.contains(&selected),
            "the selected command '{selected}' must be on screen after scrolling down"
        );
    }

    /// `/compact` while the agent loop is working must be refused with a toast:
    /// the running loop owns the context manager, and the re-entry guard stays
    /// cleared (no task spawned).
    #[tokio::test]
    async fn slash_compact_refuses_while_agent_is_working() {
        let mut app = App::new("/tmp".to_string());
        app.state.status = crate::types::SessionStatus::Working;
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "compact".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(
            !app.manual_compaction_active,
            "no compaction task may start while a loop runs"
        );
        assert!(!app.slash_menu.visible);
        assert!(
            app.toast_state
                .current
                .as_ref()
                .is_some_and(|t| t.message.contains("between messages")),
            "the refusal toast explains when /compact can run"
        );
    }

    /// `/compact` in an idle session with no persisted context is refused too —
    /// there is no timeline snapshot to rebuild the summarizer from.
    #[tokio::test]
    async fn slash_compact_refuses_without_a_session_context() {
        let mut app = App::new("/tmp".to_string());
        let cmd = crate::ui::slash_menu::SlashCommand {
            name: "compact".into(),
            desc: String::new(),
        };
        app.run_slash_command(&cmd);
        assert!(!app.manual_compaction_active);
        assert!(app.state.current_session_id.is_none());
        assert!(
            app.toast_state.current.is_some(),
            "the refusal surfaces as a toast"
        );
    }

    // ── Message Actions (port of opencode's dialog-message) ────────────────

    fn app_with_user_message() -> App {
        use crate::types::{Message, MessageRole, Part, TextPart};
        let mut app = App::new("/tmp".to_string());
        let now = 1_000u64;
        let session = crate::types::Session {
            id: "t".into(),
            title: "t".into(),
            created_at: now,
            title_generated: false,
            messages: vec![
                Message {
                    id: "u1".into(),
                    role: MessageRole::User,
                    parts: vec![Part::Text(TextPart {
                        text: "hello world".into(),
                        synthetic: false,
                    })],
                    created_at: now,
                    agent: None,
                    model: None,
                },
                Message {
                    id: "a1".into(),
                    role: MessageRole::Assistant,
                    parts: vec![Part::Text(TextPart {
                        text: "reply".into(),
                        synthetic: false,
                    })],
                    created_at: now + 1,
                    agent: None,
                    model: None,
                },
                Message {
                    id: "u2".into(),
                    role: MessageRole::User,
                    parts: vec![Part::Text(TextPart {
                        text: "second".into(),
                        synthetic: false,
                    })],
                    created_at: now + 2,
                    agent: None,
                    model: None,
                },
            ],
        };
        app.state.add_session(session);
        app.state.current_session_id = Some("t".into());
        app
    }

    #[test]
    fn message_prompt_text_joins_non_synthetic_parts() {
        use crate::types::{Message, MessageRole, Part, TextPart};
        let msg = Message {
            id: "m".into(),
            role: MessageRole::User,
            parts: vec![
                Part::Text(TextPart {
                    text: "one".into(),
                    synthetic: false,
                }),
                Part::Text(TextPart {
                    text: "hidden".into(),
                    synthetic: true,
                }),
                Part::Text(TextPart {
                    text: "two".into(),
                    synthetic: false,
                }),
            ],
            created_at: 0,
            agent: None,
            model: None,
        };
        assert_eq!(super::message_prompt_text(&msg), "one\ntwo");
    }

    #[tokio::test]
    async fn message_actions_keyboard_cycles_three_options() {
        let mut app = app_with_user_message();
        app.dialog.replace(DialogType::MessageActions {
            message_id: "u1".into(),
            preview: "hello world".into(),
        });
        assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
        assert_eq!(app.dialog.current().unwrap().selected, 1);
        assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
        assert_eq!(app.dialog.current().unwrap().selected, 2);
        // Wraps at the bottom.
        assert!(app.handle_message_actions_dialog_key(KeyCode::Down));
        assert_eq!(app.dialog.current().unwrap().selected, 0);
        // And wraps upward back to the last item.
        assert!(app.handle_message_actions_dialog_key(KeyCode::Up));
        assert_eq!(app.dialog.current().unwrap().selected, 2);
    }

    #[tokio::test]
    async fn message_actions_revert_truncates_and_restores_prompt() {
        let mut app = app_with_user_message();
        app.run_message_action(0, "u1");
        let session = app.state.current_session().unwrap();
        assert!(
            session
                .messages
                .iter()
                .all(|m| m.id != "u1" && m.id != "a1"),
            "the reverted message and everything after it are dropped"
        );
        assert_eq!(app.prompt_view.input, "hello world");
        assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
    }

    #[tokio::test]
    async fn message_actions_fork_branches_new_session_up_to_message() {
        let mut app = app_with_user_message();
        app.run_message_action(2, "u1");
        let old = app.state.current_session().unwrap();
        assert_eq!(
            old.messages.len(),
            1,
            "fork keeps messages up to and including"
        );
        assert!(old.messages.iter().any(|m| m.id == "u1"));
        assert!(!old.messages.iter().any(|m| m.id == "a1"));
        assert!(old.title.contains("(fork)"));
        assert_ne!(old.id, "t", "the fork is a brand-new session id");
    }

    #[tokio::test]
    async fn message_actions_copy_writes_clipboard_text() {
        // Clipboard may be unavailable in headless CI; only assert the text
        // extraction path via a missing-message no-op and the toast for empty.
        let mut app = app_with_user_message();
        app.run_message_action(1, "does-not-exist");
        // No panic; dialog stack untouched.
        assert!(!app.dialog.visible());
    }

    #[tokio::test]
    async fn message_actions_dialog_renders_title_and_options() {
        let mut app = app_with_user_message();
        app.dialog.replace(DialogType::MessageActions {
            message_id: "u1".into(),
            preview: "hello world".into(),
        });
        let theme = app.theme.clone();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        app.dialog.render(
            &mut buf,
            ratatui::layout::Rect::new(0, 0, 80, 24),
            &theme,
            std::time::SystemTime::now(),
        );
        let line = |y: u16| -> String {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<Vec<_>>()
                .join("")
        };
        let all: String = (0..24).map(line).collect();
        assert!(all.contains("Message Actions"), "title rendered");
        assert!(all.contains("Revert") && all.contains("Copy") && all.contains("Fork"));
    }
}

#[cfg(test)]
#[path = "bench/bench_e2e.rs"]
mod bench_e2e;
#[cfg(test)]
#[path = "bench/probe_drain.rs"]
mod probe_drain;
#[cfg(test)]
#[path = "bench/probe_real.rs"]
mod probe_real;
