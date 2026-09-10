use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self};

use ratatui::layout::Rect;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use cosh::harness::HarnessEvent;
use cosh_tui::core::lib::rgba::RGBA;

use crate::component::agent_spinner_bass::AgentSpinnerBass;
use crate::component::prompt::PromptView;
use crate::config::{LlmConfig, TuiConfig};
use crate::fallback;
use crate::keymap::KeyMap;
use crate::logo::LOGO_CHAT;
use crate::routes::add_provider::AddProviderView;
use crate::routes::home::HomeView;
use crate::routes::router::RouterView;
use crate::routes::session::SessionView;
use crate::routes::session::free_gateway_recommendation::FreeGatewayRecommendationDialog;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::queue_choice::QueueChoiceDialog;
use crate::routes::session::queue_choice::QueueTarget;
use crate::routes::session::right_panel::{RIGHT_PANEL_WIDTH, should_show_right_panel};
use crate::routes::session::sidebar::SidebarView;
use crate::routes::settings::SettingsView;
use crate::routes::tools::InternalToolsView;
use crate::session_store::SessionStore;
use crate::state::AppState;
use crate::theme::{Theme, ThemeRegistry};
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogState;
use crate::ui::toast::ToastState;

mod agent_loop;
mod commands;
mod compaction;
mod dialogs;
mod events;
mod gateway_recommendation;
mod keys;
mod mouse;
mod providers;
mod rag;
mod render;
mod terminal;

#[cfg(test)]
mod tests;

use terminal::{init_terminal, restore_terminal};

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

/// One-line, control-character-free preview of a text for the Message
/// Actions / Queue Actions boxes. The box renders the preview with
/// `draw_text_line`, which writes characters into the ratatui buffer
/// directly — a raw `\n` (multiline prompt) or any other control char in
/// the source text panics ratatui's `cell_width`, so they never reach it.
pub(crate) fn dialog_preview_text(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(36)
        .collect()
}

/// Zero the alpha of a theme's base background when transparent mode is on
/// (the `/background` toggle). RGB channels are preserved so luminance
/// derivations keep working and toggling off restores the exact registry
/// color.
fn apply_background_preference(mut t: Theme, transparent_background: bool) -> Theme {
    if transparent_background {
        let (r, g, b, _) = t.background.to_ints();
        t.background = RGBA::from_ints(r, g, b, 0);
    }
    t
}

const SIDEBAR_WIDTH: u16 = 22;
/// Minimum terminal width to show the left panel. Below this width, the sidebar
/// is auto-hidden to prevent layout conflicts with home/session content.
const MIN_WIDTH_FOR_LEFT_PANEL: u16 = 80;

/// What the left panel currently shows. Ctrl+U jumps straight to the usage
/// dashboard; Ctrl+B jumps straight back to the session history — the two
/// keys are NOT a toggle, so a user only ever needs to know the one that
/// shows what they want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum LeftPanelMode {
    /// The session history list (Ctrl+B).
    #[default]
    History,
    /// The usage dashboard (Ctrl+U).
    Dashboard,
}

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

/// Where an edited queued message came from, so it can return to its exact
/// slot ([`App::enqueue_pending_message`]). Bound to the owning session —
/// the hint is meaningless (and dropped) anywhere else.
#[derive(Debug)]
pub(super) struct EditRequeueHint {
    pub(super) session_id: String,
    pub(super) queue: QueueTarget,
    pub(super) index: usize,
}

pub struct App {
    pub state: AppState,
    pub theme: Theme,
    pub theme_registry: ThemeRegistry,
    /// `/background` toggle: when on, the active theme's base background is
    /// painted with the terminal default ([`ratatui::style::Color::Reset`])
    /// instead of the theme color. Persisted in `setup.json`.
    pub transparent_background: bool,
    pub session_view: SessionView,
    pub prompt_view: PromptView,
    pub sidebar: SidebarView,
    pub dialog: DialogState,
    pub permission_dialog: PermissionDialog,
    pub question_dialog: QuestionDialog,
    pub queue_choice_dialog: QueueChoiceDialog,
    pub free_gateway_dialog: FreeGatewayRecommendationDialog,
    pub home_view: HomeView,
    pub internal_tools_view: InternalToolsView,
    pub show_internal_tools: bool,
    pub add_provider_view: AddProviderView,
    pub show_add_provider: bool,
    pub settings_view: SettingsView,
    /// None: agent picker; Some(None): add summarizer; Some(Some(i)): edit.
    pub summarization_model_edit: Option<Option<usize>>,
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
    /// `pub(crate)`: the session route's deletion lifecycle
    /// (`routes::session::delete`) drops it when it stops a deleted
    /// session's loop.
    pub(crate) queued_input_tx: Option<mpsc::UnboundedSender<String>>,
    /// Session id that owns the currently running agent loop — guards the
    /// queue auto-start against mid-run session switches. `pub(crate)`: read
    /// by `routes::session::delete` to decide whether a deleted session
    /// owns the running loop.
    pub(crate) active_loop_session_id: Option<String>,
    /// Pending-queue row (render order: next-loop rows first, then
    /// next-request rows) currently under the mouse cursor — drives the
    /// opencode-style hover highlight above the prompt.
    pub(super) hovered_queue_row: Option<usize>,
    /// Until when the agent loop may not consume the next queued message.
    /// Set to `now + QUEUE_ACTIONS_GRACE` whenever a Queue Actions box is
    /// opened; while a Queue Actions box is open the hold applies regardless
    /// (queued indexes must stay stable until the action runs).
    pub(super) queue_actions_grace_until: Option<Instant>,
    /// Set when a loop ended cleanly with a next-loop message waiting but its
    /// start was deferred by the queue-actions hold. Consumed by
    /// [`App::pump_queued_messages`] once the hold expires. `pub(crate)`:
    /// reset by `routes::session::delete` when it stops a loop.
    pub(crate) queue_actions_deferred_start: bool,
    /// True while the head of `next_request` has been handed to the running
    /// loop's channel but not yet acknowledged via `UserMessageInjected`.
    /// The deque entry is only retired on acknowledgment so the message can
    /// never be lost if the loop ends before injecting it. `pub(crate)`:
    /// reset by `routes::session::delete` when it stops a loop.
    pub(crate) next_request_in_flight: bool,
    /// Origin of an edited queued message: the owning session, its queue and
    /// the original position. Re-submitting into the SAME queue re-inserts at
    /// that (tracked) position; choosing the other queue appends at the end
    /// and drops the hint. Invalid outside the owning session.
    pub(super) edit_requeue_hint: Option<EditRequeueHint>,
    /// Session that owned the queue when the Queue Actions box was opened —
    /// actions from a box left open across a session switch are rejected.
    active_queue_actions_session: Option<String>,
    llm_config: LlmConfig,
    /// Shared stop flag handed to the running agent loop. `pub(crate)`:
    /// raised by `routes::session::delete` when the loop's session is
    /// deleted (same mechanism as the ESC stop).
    pub(crate) stop_signal: Arc<AtomicBool>,
    /// Re-entry guard for the user-triggered `/compact`: true while the
    /// one-off compaction task runs (there is no loop status to read —
    /// between loops the app is Idle). Cleared by the CompactOnDemand event.
    manual_compaction_active: bool,
    /// The (session id, message id) opened by the CURRENT streaming attempt
    /// (the `BeginAssistant` → `ClearAssistant` window). A mid-stream reset
    /// may discard ONLY this message, and ONLY while its session is still on
    /// screen: the previous iteration's transcript (with its tool parts) must
    /// survive a provider retry, and a session switch must never make a
    /// late reset reach another session's messages. `pub(crate)`: cleared
    /// by `routes::session::delete` when it stops a deleted session's loop.
    pub(crate) stream_msg_id: Option<(String, String)>,
    terminal_focused: bool,
    pub(crate) agent_spinner_bass: Option<AgentSpinnerBass>,
    /// Latest context manager info for the budget bar (None if no data yet).
    context_info: Option<cosh::harness::ContextDisplayInfo>,
    /// What the left panel shows (history vs dashboard) and its dashboard
    /// period selection.
    left_panel: LeftPanelMode,
    usage_period: crate::usage::UsagePeriod,
    /// Every usage record loaded from disk (for period/provider aggregation).
    /// Kept in RAM and refreshed on new events so the dashboard never re-reads
    /// the JSONL per frame.
    usage_records: Vec<crate::usage::UsageRecord>,
    /// Global usage log persistence (append per request, read for aggregates).
    usage_store: crate::usage::UsageStore,
    /// Monotonic id for correlating an in-flight cost lookup with its record.
    usage_next_id: u64,
    /// Receiver for the async price-backfill task: (record id, cost USD).
    usage_cost_rx: tokio::sync::mpsc::UnboundedReceiver<(u64, f64)>,
    /// Sender half of [`Self::usage_cost_rx`], cloned into backfill tasks.
    usage_cost_tx: tokio::sync::mpsc::UnboundedSender<(u64, f64)>,
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
    /// Session persistence store (JSONL files on disk). `pub(crate)`: the
    /// session route's deletion lifecycle removes the session file from it.
    pub(crate) session_store: SessionStore,
    /// When set, the current Confirm dialog is asking about deleting a session.
    pending_delete_session_id: Option<String>,
    /// Message captured while the one-time Zen free-gateway prompt was open;
    /// replayed through `start_agent_loop` when the user opts in.
    pending_zen_message: Option<String>,
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
    /// Whether the animated chat-logo plays on the empty-session landing
    /// screen (`/anim` toggle). When off, the static LOGO_CHAT is drawn.
    pub(super) anim_enabled: bool,
}

impl App {
    pub fn new(cwd: String) -> Self {
        let mut state = AppState::new();
        state.working_directory = cwd;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (answer_tx, _answer_rx) = mpsc::unbounded_channel();
        let (perm_tx, _perm_rx) = mpsc::unbounded_channel();
        let (usage_cost_tx, usage_cost_rx) = tokio::sync::mpsc::unbounded_channel();

        // Load session summaries derived from the immutable histories.
        // Full sessions are loaded lazily into the LRU cache on demand.
        let session_store = SessionStore::new().with_notify(event_tx.clone());
        state.session_summaries = session_store.list_sessions();

        let theme_registry = ThemeRegistry::new();
        let setup = crate::util::setup::Setup::load();

        // Load saved fallback chain from setup config
        let saved_fallbacks = fallback::load_fallbacks(&setup);

        // Load saved disabled tools from setup config
        let saved_disabled_tools = crate::routes::tools::load_disabled_tools(&setup);
        // Persisted tool-call mode overrides the env default for the session.
        let saved_tool_call_mode = crate::routes::tools::load_tool_call_mode(&setup);

        // Load saved theme from setup config, honoring the /background toggle:
        // when enabled the base background is swapped for a fully transparent
        // RGBA (alpha 0 → Color::Reset at draw time) so the terminal's own
        // background shows through.
        let transparent_background = setup.appearance.transparent_background;
        let theme = {
            let base = if setup.appearance.theme.is_empty() {
                theme_registry.default_theme().clone()
            } else {
                theme_registry
                    .get(&setup.appearance.theme)
                    .cloned()
                    .unwrap_or_else(|| theme_registry.default_theme().clone())
            };
            apply_background_preference(base, transparent_background)
        };

        let saved_bell = setup.appearance.bell_enabled;
        let saved_anim = setup.appearance.anim_enabled;

        // Restore the globally persisted model selection (the last one the
        // user picked) so new sessions start with it — a stored selection
        // wins over the env-var defaults.
        let mut llm_config = LlmConfig {
            tool_call_mode: saved_tool_call_mode,
            ..LlmConfig::from_env()
        };
        if let Some(model) = setup.persisted_model() {
            llm_config.model = Some(model.to_string());
            llm_config.provider = setup.model.provider.clone();
            llm_config.reasoning = setup.model.reasoning.clone();
        }

        // Propagate the persisted Zen free-gateway opt-in to every connector
        // built in this process (harness fallback chains, compaction, session
        // titles) without each construction site needing the setup file.
        cosh_sdk::connector::set_zen_public_tier_enabled(setup.zen_public_opt_in() == Some(true));

        // Propagate the persisted LSP switch to the process-wide flag the
        // harness consults when building language servers, and seed the app
        // state so the prompt footer reflects it even before the first agent
        // loop emits an LSP event.
        cosh::harness::lsp::set_lsp_enabled(setup.lsp);
        state.lsp_available = setup.lsp;

        let usage_store = crate::usage::UsageStore::new();

        // Boot-time refresh of the models.dev catalog: keeps token prices and
        // context windows current instead of aging with the first-ever cached
        // copy. Runs in the background so startup never blocks on the network;
        // a failed refresh keeps the existing cache (validated before it can
        // replace it). Stale data beats no data.
        tokio::runtime::Handle::current().spawn(async {
            if !cosh_sdk::connector::refresh_pricing_catalog(crate::usage::MODELS_DEV_CACHE_DIR)
                .await
            {
                log::info!("models.dev catalog refresh at boot failed; keeping cached copy");
            }
        });

        // Records loaded from disk get fresh unique runtime ids (see
        // UsageStore::load) so cost backfills can never enrich a stale record.
        let usage_records = usage_store.load();
        let usage_next_id = usage_records.len() as u64;

        Self {
            state,
            theme_registry,
            theme,
            transparent_background,
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
            summarization_model_edit: None,
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
            free_gateway_dialog: FreeGatewayRecommendationDialog::new(),
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
            pending_zen_message: None,
            title_generated: false,
            manual_compaction_active: false,
            stream_msg_id: None,
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
            hovered_queue_row: None,
            queue_actions_grace_until: None,
            queue_actions_deferred_start: false,
            next_request_in_flight: false,
            edit_requeue_hint: None,
            active_queue_actions_session: None,
            llm_config,
            stop_signal: Arc::new(AtomicBool::new(false)),
            terminal_focused: true,
            agent_spinner_bass: None,
            context_info: None,
            left_panel: LeftPanelMode::default(),
            usage_period: crate::usage::UsagePeriod::default(),
            usage_records,
            usage_store,
            usage_next_id,
            usage_cost_rx,
            usage_cost_tx,
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
            anim_enabled: saved_anim,
        }
    }

    /// Welcome toast, shown only the first time the TUI runs: a marker
    /// file in the local data dir (`~/.local/share/cosh/welcome`) records
    /// that it has already been displayed.
    pub fn show_welcome_toast(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        use directories::BaseDirs;
        let marker = BaseDirs::new().map(|d| d.data_local_dir().join("cosh/welcome"));
        let Some(marker) = marker.filter(|m| !m.exists()) else {
            return;
        };
        let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
        let _ = std::fs::write(&marker, b"");
        self.toast_state.show(ToastOptions {
            title: Some("cosh".to_string()),
            message: "Welcome!".to_string(),
            variant: ToastVariant::Info,
            duration_ms: 5000,
        });
    }

    /// Apply the `/background` preference to a theme: when the toggle is on,
    /// zero the alpha of the base background (keeping its RGB channels so
    /// luminance-based derivations keep working). At draw time the shared
    /// [`crate::theme::rgba_color`] maps alpha 0 to the terminal default.
    pub(in crate::app) fn themed(&self, t: &Theme) -> Theme {
        apply_background_preference(t.clone(), self.transparent_background)
    }

    /// Install a new active theme (honoring the background preference) and
    /// bump the render-cache generation so cached cells repaint. Every
    /// `self.theme` mutation must go through here — including previews and
    /// cancels in the theme dialog — or the toggle would silently drop.
    pub(in crate::app) fn set_theme(&mut self, t: &Theme) {
        self.theme = self.themed(t);
        self.config.theme_gen += 1;
        // Update the active spinner's colours to reflect the new theme
        if let Some(spinner) = &mut self.agent_spinner_bass {
            spinner.update_theme(&self.theme);
        }
    }

    // RAG helper methods (cfg-gated at method level, always compiles)
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

            // Tick time-based UI with the REAL frame delta. Toast
            // auto-dismiss must track wall-clock time, not input events:
            // during the agent loop `handle_events()` is only reached when
            // input events are pending, so a fixed 50 ms tick there let
            // toasts outlive their programmed duration while streaming
            // (no input -> no ticks). Ticking on the measured delta keeps
            // the lifetime exact at any frame rate.
            self.toast_state.tick(delta.as_millis() as u64);

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
            self.pump_queued_messages();
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
        let sidebar_w = if self.sidebar.open && area.width >= MIN_WIDTH_FOR_LEFT_PANEL {
            self.left_panel_width()
        } else {
            0
        };
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

    /// Width of the left panel. The usage dashboard shares the session
    /// sidebar's width, so geometry is consistent regardless of view. Shared
    /// by render and mouse dispatch for exact click hit-testing.
    fn left_panel_width(&self) -> u16 {
        SIDEBAR_WIDTH
    }

    /// Assemble the snapshot the usage dashboard renders. The session block
    /// comes from the live per-session records (which gain costs as backfills
    /// land); the spend-by-period block aggregates the full on-disk log.
    fn dashboard_data(&self) -> crate::routes::session::dashboard::DashboardData {
        use crate::usage::summarize;
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let session_records = self.session_records();
        let session_tokens = crate::usage::total_tokens(&session_records);
        let session_cost = Self::sum_cost(&session_records);
        // The session has requests whose spend could not be resolved at all
        // (no reported cost, no catalog price) — the dashboard warns in yellow.
        let session_unpriced = session_records.iter().any(|r| r.cost_usd.is_none());

        let period = summarize(&self.usage_records, self.usage_period, now_ms);

        crate::routes::session::dashboard::DashboardData {
            session_tokens,
            session_cost,
            session_unpriced,
            period,
            period_enum: self.usage_period,
        }
    }

    /// The usage records belonging to the current session.
    fn session_records(&self) -> Vec<crate::usage::UsageRecord> {
        let Some(sid) = self.state.current_session_id.as_deref() else {
            return Vec::new();
        };
        self.usage_records
            .iter()
            .filter(|r| r.session_id == sid)
            .cloned()
            .collect()
    }

    /// Total real cost (USD) of the current session's recorded API usage.
    ///
    /// `None` when no record resolved a price yet (no provider-reported cost
    /// and no catalog estimate) — a guessed `$0` is never reported.
    fn session_cost(&self) -> Option<f64> {
        let records = self.session_records();
        Self::sum_cost(&records)
    }

    /// Sum the resolved costs of `records`; `None` when none resolved.
    fn sum_cost(records: &[crate::usage::UsageRecord]) -> Option<f64> {
        let mut total = 0.0;
        let mut has = false;
        for r in records {
            if let Some(c) = r.cost_usd {
                total += c;
                has = true;
            }
        }
        has.then_some(total)
    }

    /// Resolve a request's recorded cost as `(value, reported)`:
    ///   - the provider-reported REAL cost (`usage.cost`) is the source of
    ///     truth and is used verbatim — never re-priced locally;
    ///   - otherwise the models.dev price-table estimate, when a price is
    ///     known (`reported` stays `None` so the record is marked estimated);
    ///   - otherwise `(None, None)` — the record stays UNPRICED and the
    ///     dashboard warns instead of guessing.
    fn resolve_recorded_cost(
        reported_cost: Option<f64>,
        usage: &cosh_sdk::connector::TokenUsage,
        pricing: Option<cosh_sdk::connector::Pricing>,
    ) -> (Option<f64>, Option<f64>) {
        match reported_cost {
            Some(rc) => (Some(rc), Some(rc)),
            None => (pricing.map(|p| usage.cost(&p)), None),
        }
    }

    /// Persist one real API request's usage for the current session.
    ///
    /// Cost resolution order (source of truth first):
    ///   1. The REAL cost the provider reported in its usage object
    ///      (`usage.cost` — OpenRouter, Vercel AI Gateway, OpenCode Zen):
    ///      recorded verbatim, NEVER touched by local pricing.
    ///   2. The OFFLINE models.dev catalog estimate (real tokens × official
    ///      price) when the provider does not report a cost. If the model's
    ///      price is not cached yet, a background task fetches the catalog
    ///      and reports the estimate back on `usage_cost_rx` — a guessed
    ///      `$0` is never recorded.
    ///   3. `None`: no reported cost and no price known. The record stays
    ///      unpriced and the dashboard shows a yellow warning instead of
    ///      silently understating spend.
    fn record_usage(
        &mut self,
        usage: cosh_sdk::connector::TokenUsage,
        provider: &str,
        model: &str,
        reported_cost: Option<f64>,
    ) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let session_id = self.state.current_session_id.clone().unwrap_or_default();
        let id = self.usage_next_id;
        self.usage_next_id = self.usage_next_id.wrapping_add(1);

        let (cost, reported) = Self::resolve_recorded_cost(
            reported_cost,
            &usage,
            cosh_sdk::connector::model_pricing(model, crate::usage::MODELS_DEV_CACHE_DIR),
        );

        let record = crate::usage::UsageRecord {
            id,
            ts,
            session_id,
            provider: provider.to_owned(),
            model: model.to_owned(),
            usage,
            cost_usd: cost,
            reported_cost_usd: reported,
        };

        self.usage_records.push(record.clone());
        self.usage_store.append(&record);

        // No reported cost and no price yet → fetch the catalog in the
        // background and report the estimate back (keeps the UI thread off
        // the network). The backfill is an ESTIMATE (reported stays None).
        if reported.is_none() && cost.is_none() && !model.is_empty() {
            let tx = self.usage_cost_tx.clone();
            let model = model.to_owned();
            self.tokio_handle.spawn(async move {
                if let Some(pricing) =
                    cosh_sdk::connector::lookup_pricing(&model, crate::usage::MODELS_DEV_CACHE_DIR)
                        .await
                {
                    let _ = tx.send((id, usage.cost(&pricing)));
                }
            });
        }
    }

    /// Drain resolved cost backfills from the background pricing task and
    /// enrich the matching records (memory + disk) so dollar totals settle.
    fn pump_usage_costs(&mut self) {
        let mut changed = false;
        while let Ok((id, cost)) = self.usage_cost_rx.try_recv() {
            if let Some(r) = self.usage_records.iter_mut().find(|r| r.id == id)
                && r.cost_usd.replace(cost) != Some(cost)
            {
                changed = true;
            }
        }
        if changed {
            // Rewrite the on-disk log with the enriched costs. Backfills are
            // rare, so this never happens on the hot path.
            let records = self.usage_records.clone();
            self.usage_store.rewrite(&records);
        }
    }

    /// Ctrl+U: always open the left panel showing the usage dashboard. Not a
    /// toggle — pressing it again keeps showing the dashboard, so the user
    /// never has to know a matching "other" shortcut.
    fn show_dashboard(&mut self) {
        self.left_panel = LeftPanelMode::Dashboard;
        self.sidebar.open = true;
    }

    /// Ctrl+S: always open the left panel showing the session history. Also
    /// not a toggle — it just points the panel at the session list.
    fn show_session_history(&mut self) {
        self.left_panel = LeftPanelMode::History;
        self.sidebar.open = true;
    }

    /// Tab/Shift+Tab on the dashboard: advance (or go back) through the
    /// period selector — day → week → month → year → total.
    fn cycle_usage_period(&mut self, forward: bool) {
        self.usage_period = if forward {
            self.usage_period.next()
        } else {
            self.usage_period.prev()
        };
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
                && self.agent_spinner_bass.is_some()
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
        let sidebar_w = if self.sidebar.open && area.width >= MIN_WIDTH_FOR_LEFT_PANEL {
            self.left_panel_width()
        } else {
            0
        };

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

    /// Compute the pending-queued-messages strip rectangle (same calculation
    /// as in `render()`): the rows above the prompt, growing upward from it.
    /// Shared by hover tracking and click hit-testing so both map cursor
    /// positions onto the exact rows that were drawn.
    pub(super) fn compute_pending_queues_area(&self) -> Option<Rect> {
        if !matches!(self.mode(), AppMode::Session) {
            return None;
        }
        // When an inline dialog covers the prompt region, the pending rows
        // are not rendered.
        if self.question_dialog.visible
            || self.permission_dialog.visible
            || self.queue_choice_dialog.visible
        {
            return None;
        }
        let pending_h = self
            .state
            .current_pending_queues()
            .map_or(0, |q| q.queued_count() as u16);
        if pending_h == 0 {
            return None;
        }
        let area = self.terminal_size();
        let SessionArea {
            main: main_area, ..
        } = self.session_main_area(area);
        let footer_y = main_area.bottom().saturating_sub(1);

        let is_empty_session = self
            .state
            .current_session()
            .is_none_or(|s| s.messages.is_empty());
        let full_w = main_area.width.saturating_sub(4);
        let prompt_area_w = if is_empty_session {
            std::cmp::max(
                EMPTY_SESSION_PROMPT_MIN_WIDTH,
                (main_area.width as f64 * EMPTY_SESSION_PROMPT_RATIO) as u16,
            )
            .min(full_w)
        } else {
            full_w
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

        let pending_area_y = prompt_area_y.saturating_sub(pending_h);
        Some(Rect::new(
            main_area.x + 2,
            pending_area_y,
            main_area.width.saturating_sub(4),
            pending_h,
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

// Embedding helpers (feature-gated)

#[cfg(test)]
#[path = "../bench/bench_e2e.rs"]
mod bench_e2e;
#[cfg(test)]
#[path = "../bench/probe_drain.rs"]
mod probe_drain;
#[cfg(test)]
#[path = "../bench/probe_real.rs"]
mod probe_real;
#[cfg(test)]
#[path = "../bench/stress_rebuild.rs"]
mod stress_rebuild;
#[cfg(test)]
#[path = "../bench/stress_streaming.rs"]
mod stress_streaming;
