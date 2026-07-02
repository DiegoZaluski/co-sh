use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
use crate::routes::home::HomeView;
use crate::routes::session::SessionView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::permission::PermissionDialog;
use crate::routes::session::question::QuestionDialog;
use crate::routes::session::sidebar::SidebarView;
use crate::state::AppState;
use crate::theme::Theme;
use crate::ui::command_palette::CommandPalette;
use crate::ui::dialogs::DialogState;
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
    pub session_view: SessionView,
    pub prompt_view: PromptView,
    pub sidebar: SidebarView,
    pub dialog: DialogState,
    pub permission_dialog: PermissionDialog,
    pub question_dialog: QuestionDialog,
    pub keymap: KeyMap,
    pub config: TuiConfig,
    pub toast_state: ToastState,
    pub command_palette: CommandPalette,
    pub should_quit: bool,
    pub tokio_handle: Handle,
    pub event_tx: mpsc::UnboundedSender<HarnessEvent>,
    event_rx: mpsc::UnboundedReceiver<HarnessEvent>,
    llm_config: LlmConfig,
}

impl App {
    pub fn new() -> Self {
        let mut state = AppState::new();
        state.add_demo_data();

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        App {
            state,
            theme: Theme::dark(),
            session_view: SessionView::new(),
            prompt_view: PromptView::new(),
            sidebar: SidebarView::new(),
            dialog: DialogState::new(),
            permission_dialog: PermissionDialog::new(),
            question_dialog: QuestionDialog::new(),
            keymap: KeyMap::default_vim(),
            config: TuiConfig::default(),
            toast_state: ToastState::new(),
            command_palette: CommandPalette::new(),
            should_quit: false,
            tokio_handle: Handle::current(),
            event_tx,
            event_rx,
            llm_config: LlmConfig::from_env(),
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
                let area = frame.area();
                let buf = frame.buffer_mut();
                self.render(buf, area);
            })?;

            if self.handle_events()? {
                break;
            }

            self.poll_events();
        }

        restore_terminal()?;
        Ok(())
    }

    fn render(&mut self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
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
                HomeView::render(buf, session_area, &self.state, &self.theme);
            }
            AppMode::Session => {
                self.session_view.tool_state.advance_spinner();
                let unique_agents = self.state.unique_agents();
                let agent_colors = crate::types::AgentColors::from_theme(&self.theme);
                self.session_view
                    .render(buf, session_area, &self.state, &self.theme, &self.config);
                self.prompt_view.render(
                    buf,
                    prompt_area,
                    &self.state,
                    &self.theme,
                    &agent_colors,
                    &unique_agents,
                );
            }
        }

        FooterView::render(
            buf,
            Rect::new(main_area.x, footer_y, main_area.width, 1),
            &self.state,
            &self.theme,
        );
        self.toast_state.render(buf, area, &self.theme);
        self.dialog.render(buf, area, &self.theme);
        self.permission_dialog.render(buf, area, &self.theme);
        self.question_dialog.render(buf, area, &self.theme);
        self.command_palette.render(buf, area, &self.theme);
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

                    let action = self.keymap.lookup(key.code, key.modifiers).cloned();

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
                            if self.state.status == crate::types::SessionStatus::Working {
                                return Ok(false);
                            }

                            let msg = self.prompt_view.send_message();
                            if msg.is_empty() {
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

                            self.state.status = crate::types::SessionStatus::Working;

                            let event_tx = self.event_tx.clone();
                            let provider = self.llm_config.provider.clone();
                            let model = self.llm_config.model.clone();
                            let cwd = std::env::current_dir().map_or_else(
                                |_| ".".to_string(),
                                |p| p.to_string_lossy().to_string(),
                            );
                            let input = msg;

                            self.tokio_handle.spawn(async move {
                                use cosh::harness::Harness;
                                use cosh_sdk::connector::Connector;

                                let connector = match Connector::new(&provider) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        let _ = event_tx
                                            .send(HarnessEvent::Error(format!("connector: {e}")));
                                        return;
                                    }
                                };
                                let connector = if let Some(ref m) = model {
                                    connector.with_model(m)
                                } else {
                                    connector
                                };

                                let mut harness = Harness::new(connector, &cwd);
                                harness.run_agent_loop(&input, event_tx).await;
                            });
                        }
                        Some(crate::keymap::Action::Cancel | crate::keymap::Action::Interrupt) => {
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
                            self.prompt_view.history_up();
                        }
                        Some(crate::keymap::Action::HistoryDown) => {
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
                                    let pos = self.prompt_view.cursor_pos;
                                    if pos > 0 {
                                        self.prompt_view.input.remove(pos - 1);
                                        self.prompt_view.cursor_pos = pos - 1;
                                    }
                                }
                                KeyCode::Char(ch) => {
                                    let pos = self.prompt_view.cursor_pos;
                                    self.prompt_view.input.insert(pos, ch);
                                    self.prompt_view.cursor_pos = pos + 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
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
                        message: msg,
                        variant: ToastVariant::Error,
                        duration_ms: 5000,
                    });
                }
            }
        }
    }
}

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() -> io::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
}
