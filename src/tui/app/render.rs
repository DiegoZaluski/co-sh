use std::time::Instant;

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{
    App, AppMode, BUG_REPORT_TEXT, EMPTY_SESSION_PROMPT_MIN_WIDTH, EMPTY_SESSION_PROMPT_RATIO,
    MIN_PROMPT_RESERVE_ROWS, SessionArea,
};
use crate::logo::LOGO_CHAT;
use crate::routes::home::footer::HomeFooterView;
use crate::routes::session::footer::FooterView;
use crate::routes::session::right_panel::render_right_panel;
use crate::theme::rgba_color;

/// Render a 10-character budget bar like `▓▓▓▓▓░░░░░` from a 0-100 percentage.
pub(super) fn render_budget_bar(pct: u8) -> String {
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
pub(super) fn format_tokens(n: usize) -> String {
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

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame<'_>, delta_time: f64) {
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

                // Session total cost — the bare "$" makes its meaning obvious,
                // so only the amount is painted (green). Hidden while the
                // model's price is still unknown (no guessed $0).
                let cost_str = self.session_cost().map(|c| format!("${c:.2}"));
                let cost_w = cost_str.as_ref().map_or(0, |s| s.chars().count()) as u16;
                let cost_gap = if cost_str.is_some() { gap } else { 0 };

                let display_w = (cost_w as usize
                    + cost_gap as usize
                    + (token_str.chars().count() + gap as usize + budget_str.chars().count()))
                    as u16;
                let right_x = main_area.right().saturating_sub(display_w + 1);

                // Session cost — the leftmost element of the header line.
                if let Some(cost) = &cost_str {
                    let cost_style = Style::default().fg(rgba_color(self.theme.success));
                    for (i, ch) in cost.chars().enumerate() {
                        if let Some(cell) = buf.cell_mut((right_x + i as u16, area.y)) {
                            cell.set_char(ch);
                            cell.set_style(cost_style);
                        }
                    }
                }

                // Token counter — to the right of the cost (or left of the
                // budget bar when no price is known yet).
                let token_x = right_x + cost_w + cost_gap;
                for (i, ch) in token_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((token_x + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(rgba_color(self.theme.text_muted)));
                    }
                }

                // Budget bar (with conditional color)
                let offset = cost_w + cost_gap + token_str.chars().count() as u16 + gap;
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

            if self.sidebar.open && area.width >= super::MIN_WIDTH_FOR_LEFT_PANEL {
                if matches!(self.left_panel, super::LeftPanelMode::Dashboard) {
                    // Usage dashboard: a self-contained panel (session usage
                    // + spend per provider/total for the selected period).
                    let data = self.dashboard_data();
                    crate::routes::session::dashboard::render(
                        buf,
                        Rect::new(area.x, area.y, sidebar_w, area.height),
                        &data,
                        &self.theme,
                    );
                } else {
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
                            && self.state.status == crate::types::SessionStatus::Idle
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
            }

            let footer_y = main_area.bottom().saturating_sub(1);
            let is_session = matches!(self.mode(), AppMode::Session);

            // When question, permission, queue-choice, or gateway recommendation
            // dialog is visible, hide prompt and spinner (like OpenCode).
            let hide_prompt_and_spinner = is_session
                && (self.question_dialog.visible
                    || self.permission_dialog.visible
                    || self.queue_choice_dialog.visible
                    || self.free_gateway_dialog.visible);

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
            // Free-gateway recommendation dialog (same position).
            let recommendation_h = if is_session && self.free_gateway_dialog.visible {
                self.free_gateway_dialog
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
            let recommendation_h = recommendation_h.min(max_dialog_h);
            let question_area_y = pending_area_y.saturating_sub(question_h);
            let permission_area_y = pending_area_y.saturating_sub(permission_h);
            let queue_choice_area_y = pending_area_y.saturating_sub(queue_choice_h);
            let recommendation_area_y = pending_area_y.saturating_sub(recommendation_h);
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
                .min(recommendation_area_y)
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
            let recommendation_area = Rect::new(
                main_area.x + 2,
                recommendation_area_y,
                main_area.width.saturating_sub(4),
                recommendation_h,
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
                        || self.free_gateway_dialog.visible
                    {
                        self.prompt_view.blur();
                    }

                    // Advance the fake streaming text in the recommendation dialog
                    if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog.advance_stream();
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
                    // Question/permission/queue-choice/recommendation dialogs
                    // rendered inline between messages and prompt (mutually exclusive).
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
                    } else if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog
                            .render(buf, recommendation_area, &self.theme);
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
                        let provider = self.llm_config.provider.as_str();
                        let model_name = self.llm_config.model.as_deref().unwrap_or("");
                        let reasoning = self.llm_config.reasoning.as_deref();
                        self.prompt_view.render(
                            buf,
                            prompt_area,
                            &self.state,
                            &self.theme,
                            &agent_colors,
                            &unique_agents,
                            std::time::SystemTime::now(),
                            provider,
                            model_name,
                            reasoning,
                            delta_time,
                            is_empty_session && self.anim_enabled,
                        );

                        // Static LOGO_CHAT replaces the animation when /anim is off,
                        // centered above the prompt input.
                        if is_empty_session && !self.anim_enabled {
                            for (i, line) in LOGO_CHAT.iter().enumerate() {
                                let line_w = line.chars().count() as u16;
                                let logo_x =
                                    prompt_area.x + prompt_area.width.saturating_sub(line_w) / 2;
                                buf.set_string(
                                    logo_x,
                                    logo_start_y + i as u16,
                                    line.trim_end(),
                                    Style::default()
                                        .fg(rgba_color(self.theme.primary))
                                        .bg(rgba_color(self.theme.background)),
                                );
                            }
                        }
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
                        || self.queue_choice_dialog.visible
                        || self.free_gateway_dialog.visible;
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
            // The slash menu renders first so modal dialogs (e.g. the Ctrl+C
            // "Quit cosh?" confirm) always paint on top of it — the menu is
            // inline chrome, the dialog is a modal overlay that owns the
            // whole input surface while visible (sovereign keys + mouse).
            self.slash_menu.render(buf, prompt_area, &self.theme);
            self.dialog.render(buf, area, &self.theme, now);
        }
    }

    /// Render the pending queued messages above the prompt, color-coded per
    /// queue: "next agent loop" rows on top (warm amber background), "next
    /// request" rows below (cool cyan/blue background), each preserving FIFO
    /// order. The two dedicated theme colors switch with the active theme. No
    /// explicit labels — the background color IS the identity of the queue.
    pub(super) fn render_pending_queues(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let Some(queues) = self.state.current_pending_queues() else {
            return;
        };
        if queues.next_loop.is_empty() && queues.next_request.is_empty() {
            return;
        }
        // The hovered row swaps its queue color for pure WHITE so the focus
        // clearly stands out against every theme surface (and against the
        // prompt box, which shares the element/panel colors). `contrast_on`
        // flips the row text to black for readability.
        let hover_bg = RGBA::from_hex("#FFFFFF");
        let hover = self.hovered_queue_row;
        let mut y = area.y;
        let mut row = 0usize;
        let panel = self.theme.background_panel;
        let border = self.theme.accent;
        for text in &queues.next_loop {
            let bg = if hover == Some(row) {
                hover_bg
            } else {
                self.theme.queue_next_loop
            };
            Self::draw_pending_row(buf, text, area.x, y, area.width, bg, panel, border);
            y += 1;
            row += 1;
        }
        for text in &queues.next_request {
            let bg = if hover == Some(row) {
                hover_bg
            } else {
                self.theme.queue_next_request
            };
            Self::draw_pending_row(buf, text, area.x, y, area.width, bg, panel, border);
            y += 1;
            row += 1;
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
}
