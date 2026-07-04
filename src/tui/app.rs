use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use cosh::harness::HarnessEvent;

use crate::component::prompt::PromptView;
use crate::config::{LlmConfig, TuiConfig};
use crate::keymap::KeyMap;
use crate::routes::home::{HomeAction, HomeView};
use crate::routes::session::SessionView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::sidebar::SidebarView;
use crate::state::AppState;
use crate::theme::{Theme, ThemeRegistry};
use crate::ui::command_palette::CommandPalette;
use crate::ui::dialogs::{DialogState, DialogType};
use crate::ui::toast::ToastState;

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

const SIDEBAR_WIDTH: u16 = 24;
const FOOTER_HEIGHT: u16 = 1;

enum AppMode {
    Home,
    Session,
}

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
    pub keymap: KeyMap,
    pub config: TuiConfig,
    pub toast_state: ToastState,
    pub command_palette: CommandPalette,
    pub slash_menu: crate::ui::slash_menu::SlashMenu,
    pub should_quit: bool,
    pub tokio_handle: Handle,
    pub event_tx: mpsc::UnboundedSender<HarnessEvent>,
    event_rx: mpsc::UnboundedReceiver<HarnessEvent>,
    llm_config: LlmConfig,
    stop_signal: Arc<AtomicBool>,
    terminal_focused: bool,
    /// Stores the theme name that was active when the theme dialog opened (for cancel/restore)
    theme_dialog_original: Option<String>,
    /// Stores the model that was active when the model dialog opened (for cancel/restore)
    model_dialog_original: Option<String>,
}

impl App {
    pub fn new() -> Self {
        let state = AppState::new();

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let theme_registry = ThemeRegistry::new();
        let theme = theme_registry.default_theme().clone();

        App {
            state,
            theme_registry,
            theme,
            session_view: SessionView::new(),
            home_view: HomeView::new(),
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
            should_quit: false,
            tokio_handle: Handle::current(),
            event_tx,
            event_rx,
            llm_config: LlmConfig::from_env(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            terminal_focused: true,
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
        let mut themes: Vec<String> = self.theme_registry.names().into_iter().map(|s| s.to_string()).collect();
        themes.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

        let current = self.theme_registry
            .names()
            .iter()
            .find(|&&name| self.theme_registry.get(name).map_or(false, |t| t == &self.theme))
            .map(|&s| s.to_string())
            .unwrap_or_else(|| "opencode".to_string());

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
        use cosh_sdk::connector::known_providers;

        let current = self.llm_config.model.clone().unwrap_or_default();

        // Store the current model so we can restore on cancel
        self.model_dialog_original = Some(current.clone());

        let dialog_tx = self.event_tx.clone();

        // Show a loading state first
        self.dialog.replace(DialogType::ModelList {
            models: vec![],
            current: current.clone(),
            filter: String::new(),
        });

        // Fetch models from all known providers asynchronously
        let providers_to_check: Vec<&str> = known_providers().collect();
        let dialog_tx_clone = dialog_tx.clone();

        self.tokio_handle.spawn(async move {
            let mut all_models: Vec<ModelEntry> = Vec::new();

            for provider in providers_to_check {
                if let Ok(connector) = Connector::new(provider) {
                    match connector.list_models().await {
                        Ok(output) => {
                            for model_info in output.models() {
                                all_models.push(ModelEntry {
                                    provider: provider.to_string(),
                                    model: model_info.id().to_string(),
                                });
                            }
                        }
                        Err(_) => {
                            // Skip providers that fail to load models
                            continue;
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

    fn is_theme_dialog_visible(&self) -> bool {
        self.dialog.visible() && matches!(
            self.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::ThemeList { .. })
        )
    }

    fn is_model_dialog_visible(&self) -> bool {
        self.dialog.visible() && matches!(
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
                if !filtered.is_empty() {
                    if let Some(d) = self.dialog.current_mut() {
                        d.selected = if d.selected == 0 {
                            filtered.len() - 1
                        } else {
                            d.selected.saturating_sub(1)
                        };
                        // Preview theme on move
                        self.apply_filtered_theme_preview();
                    }
                }
                true
            }
            KeyCode::Down => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty() {
                    if let Some(d) = self.dialog.current_mut() {
                        d.selected = (d.selected + 1).min(filtered.len() - 1);
                        // Preview theme on move
                        self.apply_filtered_theme_preview();
                    }
                }
                true
            }
            KeyCode::Enter => {
                let filtered = self.theme_dialog_filtered();
                if !filtered.is_empty() {
                    let name = filtered[self.dialog.current().map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)))].to_string();
                    if let Some(t) = self.theme_registry.get(&name) {
                        self.theme = t.clone();
                    }
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Restore original theme
                if let Some(ref orig) = self.theme_dialog_original {
                    if let Some(t) = self.theme_registry.get(orig) {
                        self.theme = t.clone();
                    }
                }
                self.theme_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                let is_empty = {
                    let Some(d) = self.dialog.current_mut() else { return true; };
                    let DialogType::ThemeList { filter, .. } = &mut d.dialog_type else { return true; };
                    filter.pop();
                    d.selected = 0;
                    d.last_filter_at = std::time::SystemTime::now();
                    d.blink_start = std::time::SystemTime::now();
                    filter.is_empty()
                };
                if is_empty {
                    // Restore original theme when filter becomes empty (matches opencode)
                    if let Some(ref orig) = self.theme_dialog_original {
                        if let Some(t) = self.theme_registry.get(orig) {
                            self.theme = t.clone();
                        }
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
                    themes.iter().filter(|t| t.to_lowercase().contains(&lower)).cloned().collect()
                }
            } else {
                Vec::new()
            }
        })
    }

    /// Preview the currently selected theme from the filtered list
    fn apply_filtered_theme_preview(&mut self) {
        let filtered = self.theme_dialog_filtered();
        let sel = self.dialog.current().map_or(0, |d| d.selected.min(filtered.len().saturating_sub(1)));
        if sel < filtered.len() {
            if let Some(t) = self.theme_registry.get(&filtered[sel]) {
                self.theme = t.clone();
            }
        }
    }

    /// Add a character to the theme filter and reset selection
    fn theme_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ThemeList { filter, .. } = &mut d.dialog_type {
                filter.push(ch);
            }
            d.selected = 0;
            d.last_filter_at = std::time::SystemTime::now();
            d.blink_start = std::time::SystemTime::now();
        }
        self.apply_filtered_theme_preview();
    }

    fn handle_model_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_model_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Get the models from the dialog
                if let Some(d) = self.dialog.current() {
                    if let DialogType::ModelList { models, filter, .. } = &d.dialog_type {
                        // Build filtered indices
                        let filtered_indices: Vec<usize> = if filter.is_empty() {
                            (0..models.len()).collect()
                        } else {
                            let lower = filter.to_lowercase();
                            models.iter().enumerate()
                                .filter(|(_, m)| m.model.to_lowercase().contains(&lower))
                                .map(|(i, _)| i)
                                .collect()
                        };
                        
                        if !filtered_indices.is_empty() {
                            if let Some(d_mut) = self.dialog.current_mut() {
                                let current_pos = filtered_indices.iter().position(|&i| i == d_mut.selected);
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
                    }
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current() {
                    if let DialogType::ModelList { models, filter, .. } = &d.dialog_type {
                        // Build filtered indices
                        let filtered_indices: Vec<usize> = if filter.is_empty() {
                            (0..models.len()).collect()
                        } else {
                            let lower = filter.to_lowercase();
                            models.iter().enumerate()
                                .filter(|(_, m)| m.model.to_lowercase().contains(&lower))
                                .map(|(i, _)| i)
                                .collect()
                        };
                        
                        if !filtered_indices.is_empty() {
                            if let Some(d_mut) = self.dialog.current_mut() {
                                let current_pos = filtered_indices.iter().position(|&i| i == d_mut.selected);
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
                    }
                }
                true
            }
            KeyCode::Enter => {
                if let Some(d) = self.dialog.current() {
                    if let DialogType::ModelList { models, .. } = &d.dialog_type {
                        if !models.is_empty() {
                            let selected_idx = d.selected.min(models.len().saturating_sub(1));
                            let selected_entry = &models[selected_idx];
                            self.llm_config.model = Some(selected_entry.model.clone());
                            self.llm_config.provider = selected_entry.provider.clone();
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
                    self.llm_config.model = if orig.is_empty() { None } else { Some(orig.clone()) };
                }
                self.model_dialog_original = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                let _is_empty = {
                    let Some(d) = self.dialog.current_mut() else { return true; };
                    let DialogType::ModelList { filter, .. } = &mut d.dialog_type else { return true; };
                    filter.pop();
                    d.selected = 0;
                    d.last_filter_at = std::time::SystemTime::now();
                    d.blink_start = std::time::SystemTime::now();
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
            d.last_filter_at = std::time::SystemTime::now();
            d.blink_start = std::time::SystemTime::now();
        }
    }

    fn mode(&self) -> AppMode {
        if self.state.current_session().is_some() {
            AppMode::Session
        } else {
            AppMode::Home
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        let mut terminal = init_terminal()?;

        while !self.should_quit {
            terminal.draw(|frame| {
                self.render(frame);
            })?;

            if self.handle_events()? {
                break;
            }

            self.poll_events();
        }

        restore_terminal()?;
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
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
            let prompt_h = if matches!(self.mode(), AppMode::Session) {
                self.prompt_view
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            let prompt_area_y = footer_y.saturating_sub(prompt_h);
            let prompt_area = Rect::new(
                main_area.x + 2,
                prompt_area_y,
                main_area.width.saturating_sub(4),
                prompt_h,
            );
            let session_bottom = prompt_area_y;
            let session_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                session_bottom.saturating_sub(area.y + 1),
            );

            match self.mode() {
                AppMode::Home => {
                    self.prompt_view.blur();
                    self.home_view.render(buf, session_area, &self.state, &self.theme);
                }
                AppMode::Session => {
                    self.prompt_view.focus();
                    self.prompt_view.terminal_focused = self.terminal_focused;
                    self.session_view.tool_state.advance_spinner();
                    let unique_agents = self.state.unique_agents();
                    let agent_colors = crate::types::AgentColors::from_theme(&self.theme);
                    self.session_view.render(
                        buf,
                        session_area,
                        &self.state,
                        &self.theme,
                        &self.config,
                    );
                    self.prompt_view.render(
                        buf,
                        prompt_area,
                        &self.state,
                        &self.theme,
                        &agent_colors,
                        &unique_agents,
                        std::time::SystemTime::now(),
                    );
                }
            }

            FooterView::render(
                buf,
                Rect::new(main_area.x, footer_y, main_area.width, 1),
                &self.state,
                &self.theme,
            );
            let now = std::time::SystemTime::now();
            self.toast_state.render(buf, area, &self.theme);
            self.dialog.render(buf, area, &self.theme, now);
            self.permission_dialog.render(buf, area, &self.theme);
            self.question_dialog.render(buf, area, &self.theme);
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
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.should_quit = true;
                        return Ok(false);
                    }

                    // Check theme dialog FIRST, before action lookup
                    if self.is_theme_dialog_visible() {
                        if self.handle_theme_dialog_key(key.code) {
                            return Ok(false);
                        }
                    }

                    // Check model dialog SECOND, before action lookup
                    if self.is_model_dialog_visible() {
                        if self.handle_model_dialog_key(key.code) {
                            return Ok(false);
                        }
                    }

                    let action = self.keymap.lookup(key.code, key.modifiers).cloned();

                    // Home mode: navigation keys
                    if matches!(self.mode(), AppMode::Home) {
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
                                        self.state.sessions.push(crate::types::Session {
                                            id: format!("session-{}", self.state.sessions.len()),
                                            title: "New Session".to_string(),
                                            messages: vec![],
                                        });
                                        self.state.current_session_id =
                                            Some(self.state.sessions.last().unwrap().id.clone());
                                    }
                                    HomeAction::ToggleSidebar => {
                                        self.sidebar.open = !self.sidebar.open;
                                    }
                                }
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
                                self.prompt_view.note_activity();
                                if !self.prompt_view.input.is_empty() {
                                    self.prompt_view.input.pop();
                                    self.prompt_view.cursor_pos =
                                        self.prompt_view.cursor_pos.saturating_sub(1);
                                    self.slash_menu.update(&self.prompt_view.input);
                                }
                            }
                            KeyCode::Char(ch) => {
                                self.prompt_view.note_activity();
                                self.prompt_view.input.push(ch);
                                self.prompt_view.cursor_pos += 1;
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
                        Some(
                            crate::keymap::Action::ToggleHelp
                            | crate::keymap::Action::NextSession
                            | crate::keymap::Action::PrevSession
                            | crate::keymap::Action::FocusInput
                            | crate::keymap::Action::Quit,
                        ) => {
                            // TBD: help overlay
                        }
                        Some(
                            crate::keymap::Action::SendMessage | crate::keymap::Action::Confirm,
                        ) => {
                            self.prompt_view.note_activity();
                            if self.state.status == crate::types::SessionStatus::Working {
                                return Ok(false);
                            }

                            let msg = self.prompt_view.send_message();
                            if msg.trim().is_empty() {
                                return Ok(false);
                            }

                            if self.state.current_session_id.is_none() {
                                let id = format!("session-{}", self.state.sessions.len());
                                let title: String = msg.chars().take(40).collect();
                                self.state.sessions.push(crate::types::Session {
                                    id: id.clone(),
                                    title,
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

                            let event_tx = self.event_tx.clone();
                            let provider = self.llm_config.provider.clone();
                            let model = self.llm_config.model.clone();
                            let input = msg;

                            std::thread::spawn(move || {
                                use cosh_sdk::connector::Connector;
                                use std::panic::AssertUnwindSafe;
                                use tokio::runtime::Builder;

                                let event_tx_panic = event_tx.clone();

                                let rt = match Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt,
                                    Err(e) => {
                                        let _ = event_tx_panic.send(HarnessEvent::Error(
                                            format!("runtime: {e}"),
                                        ));
                                        return;
                                    }
                                };

                                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                    rt.block_on(async {
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

                                        // Use NON-STREAMING chat instead of streaming
                                        match connector.chat_with_system(&input, "").await {
                                            Ok(output) => {
                                                let _ = event_tx.send(HarnessEvent::Token {
                                                    text: output.message().to_string(),
                                                });
                                                let _ = event_tx.send(HarnessEvent::Done);
                                            }
                                            Err(e) => {
                                                let _ = event_tx.send(HarnessEvent::Error(
                                                    format!("chat error: {e}"),
                                                ));
                                            }
                                        }
                                    })
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
                        Some(crate::keymap::Action::Interrupt) => {
                            if self.state.status == crate::types::SessionStatus::Working {
                                self.stop_signal.store(true, Ordering::Relaxed);
                            } else if self.question_dialog.visible {
                                self.question_dialog.visible = false;
                            } else if self.permission_dialog.visible {
                                self.permission_dialog.visible = false;
                            } else if self.dialog.visible() {
                                self.dialog.pop();
                            }
                        }
                        Some(crate::keymap::Action::Cancel) => {
                            if self.question_dialog.visible {
                                self.question_dialog.visible = false;
                            } else if self.permission_dialog.visible {
                                self.permission_dialog.visible = false;
                            } else if self.dialog.visible() {
                                self.dialog.pop();
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
                        Some(crate::keymap::Action::NextAgent) => {
                            let agents = self.state.unique_agents();
                            self.prompt_view.next_agent(agents.len().max(1));
                        }
                        Some(crate::keymap::Action::PrevAgent) => {
                            let agents = self.state.unique_agents();
                            self.prompt_view.prev_agent(agents.len().max(1));
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
                                        self.prompt_view.note_activity();
                                        if !self.prompt_view.input.is_empty() {
                                            self.prompt_view.input.pop();
                                            self.prompt_view.cursor_pos =
                                                self.prompt_view.cursor_pos.saturating_sub(1);
                                            self.slash_menu.update(&self.prompt_view.input);
                                        }
                                    }
                                    KeyCode::Char(ch) => {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.input.push(ch);
                                        self.prompt_view.cursor_pos += 1;
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

                            match key.code {
                                KeyCode::Up => {
                                    self.session_view.scroll_y =
                                        (self.session_view.scroll_y - 3).max(0);
                                }
                                KeyCode::Down => {
                                    self.session_view.scroll_y =
                                        (self.session_view.scroll_y + 3).max(0);
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
                                    self.prompt_view.note_activity();
                                    let pos = self.prompt_view.cursor_pos;
                                    if pos > 0 {
                                        self.prompt_view.input.remove(pos - 1);
                                        self.prompt_view.cursor_pos = pos - 1;
                                    }
                                }
                                KeyCode::Char(ch) => {
                                    self.prompt_view.note_activity();
                                    // Insert character normally
                                    let pos = self.prompt_view.cursor_pos;
                                    self.prompt_view.input.insert(pos, ch);
                                    self.prompt_view.cursor_pos = pos + 1;

                                    // Check if "/" menu should open
                                    self.slash_menu.update(&self.prompt_view.input);
                                }
                                _ => {}
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
            _ => {}
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
                }

                HarnessEvent::Stopped => {
                    self.state.status = SessionStatus::Idle;
                    self.toast_state.show(ToastOptions {
                        title: Some("Interrupted".into()),
                        message: "Agent loop was stopped.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 3000,
                    });
                }

                HarnessEvent::Error(msg) => {
                    self.state.status = SessionStatus::Retry {
                        message: msg.clone(),
                        action: None,
                    };
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
                    // Update the dialog with the loaded models
                    if let Some(d) = self.dialog.current_mut() {
                        if let DialogType::ModelList { models: dialog_models, current: dialog_current, .. } = &mut d.dialog_type {
                            *dialog_models = models;
                            *dialog_current = current;
                        }
                    }
                }
            }
        }
    }
}

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableFocusChange,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::event::DisableFocusChange,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    stdout.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}
