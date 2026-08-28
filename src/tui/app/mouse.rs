use std::io;
use std::time::{Duration, Instant};

use cosh_tui::core::types::MouseEvent;
use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};
use crossterm::event::{
    KeyCode, KeyModifiers, MouseButton as CrosstermMouseButton, MouseEvent as CrosstermMouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;

use super::{App, AppMode, MIN_PROMPT_RESERVE_ROWS, SIDEBAR_WIDTH, message_prompt_text};
use crate::fallback;
use crate::routes::router::FocusTarget;
use crate::routes::session::queue_choice::QueueTarget;
use crate::routes::session::right_panel::should_show_right_panel;
use crate::routes::session::sidebar::SidebarAction;
use crate::ui::dialogs::{DialogAction, DialogType};
use crate::util::selection;

impl App {
    /// Handle a crossterm mouse event by converting it to a cosh-tui `MouseEvent`
    /// and dispatching to the appropriate component based on current layout.
    #[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
    pub(super) fn handle_mouse_event(&mut self, evt: CrosstermMouseEvent) -> io::Result<bool> {
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
                    // Clicking back into the prompt hands keyboard control
                    // over from the right panel.
                    self.state.right_panel.panel_focus = None;
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
                    // Clicking anywhere else in the chat also releases the
                    // right-panel keyboard focus.
                    if !Self::is_in_right_panel(x, self.terminal_size()) {
                        self.state.right_panel.panel_focus = None;
                    }
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

                // Click in the visible right panel → focus that slot (bash
                // toggle / a specific agent queue) and start a drag
                // selection on the bash / subagent section under the cursor.
                if matches!(self.mode(), AppMode::Session)
                    && should_show_right_panel(self.terminal_size().width, &self.state.right_panel)
                    && Self::is_in_right_panel(x, self.terminal_size())
                {
                    self.state.right_panel.focus_at(y);
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
                            DialogType::QueueActions { .. } => {
                                self.handle_queue_actions_dialog_key(KeyCode::Up);
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
                            DialogType::QueueActions { .. } => {
                                self.handle_queue_actions_dialog_key(KeyCode::Down);
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
                // Hover tracking for the pending queued rows above the prompt.
                self.hovered_queue_row = match self.compute_pending_queues_area() {
                    Some(area)
                        if x >= area.x && x < area.right() && y >= area.y && y < area.bottom() =>
                    {
                        Some((y - area.y) as usize)
                    }
                    _ => None,
                };
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
                            DialogType::MessageActions { message_id, is_last_user_message, .. } => {
                                let max_options = if *is_last_user_message { 3 } else { 1 };
                                let selected = d.selected.min(max_options - 1);
                                let action = App::message_action_index(selected, *is_last_user_message);
                                let message_id = message_id.clone();
                                self.dialog.pop();
                                self.run_message_action(action, &message_id);
                                return Ok(true);
                            }
                            DialogType::QueueActions { queue, index, .. } => {
                                let action = d.selected.min(2);
                                let (queue, index) = (*queue, *index);
                                self.dialog.pop();
                                self.run_queue_action(action, queue, index);
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
                    // Edited message returning to its queue → original
                    // position; otherwise appended at the end (helper).
                    self.enqueue_pending_message(target, text);
                    self.prompt_view.focus();
                }
                return Ok(true);
            }
        }

        // 5b. Free-gateway recommendation dialog (inline)
        if self.free_gateway_dialog.visible && matches!(self.mode(), AppMode::Session) {
            let area = self.terminal_size();
            let sidebar_w = if self.sidebar.open { SIDEBAR_WIDTH } else { 0 };
            let main_area = Rect::new(
                area.x + sidebar_w,
                area.y,
                area.width.saturating_sub(sidebar_w),
                area.height,
            );
            let footer_y = main_area.bottom().saturating_sub(1);
            let _prompt_h = 0u16; // prompt is hidden when dialog is visible
            let rec_h = self
                .free_gateway_dialog
                .required_height(main_area.width.saturating_sub(4));
            let rec_h = rec_h.min(footer_y.saturating_sub(area.y + 1));
            let rec_area_y = footer_y.saturating_sub(rec_h);
            let rec_area = Rect::new(
                main_area.x + 2,
                rec_area_y,
                main_area.width.saturating_sub(4),
                rec_h,
            );
            if !self.mouse_drag_active && self.drag_selection.is_none() {
                let consumed = self
                    .free_gateway_dialog
                    .handle_mouse(&mouse, rec_area, &self.theme);
                if consumed {
                    if self.free_gateway_dialog.submitted {
                        self.commit_gateway_choice();
                    } else if !self.free_gateway_dialog.visible {
                        self.restore_pending_gateway_message();
                    }
                    return Ok(true);
                }
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
                    // Returning to a session restores the last model used there.
                    self.restore_current_session_model();
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

        // 6b. Pending queued-message rows (Queue Actions): clicking a row
        // opens the Edit/Delete/Copy box for that specific queued message
        // and grants the 5-second hold on the next effective message.
        if matches!(self.mode(), AppMode::Session)
            && !self.mouse_drag_active
            && self.drag_selection.is_none()
            && let Some(pending_area) = self.compute_pending_queues_area()
            && y >= pending_area.y
            && y < pending_area.bottom()
        {
            if let Some(queues) = self.state.current_pending_queues() {
                let row = (y - pending_area.y) as usize;
                let loop_len = queues.next_loop.len();
                let (queue, index, text) = if row < loop_len {
                    (QueueTarget::NextLoop, row, queues.next_loop[row].clone())
                } else {
                    let idx = row - loop_len;
                    match queues.next_request.get(idx) {
                        Some(text) => (QueueTarget::NextRequest, idx, text.clone()),
                        None => return Ok(true),
                    }
                };
                self.open_queue_actions_for(queue, index, &text);
            }
            return Ok(true);
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
                    let session = self.state.current_session();
                    let is_last_user_message = session
                        .and_then(|s| {
                            s.messages
                                .iter()
                                .rposition(|m| m.role == crate::types::MessageRole::User)
                        })
                        .zip(session.and_then(|s| s.messages.iter().position(|m| m.id == message_id)))
                        .map(|(last_user_idx, clicked_idx)| last_user_idx == clicked_idx)
                        .unwrap_or(false);

                    let preview = session
                        .and_then(|s| s.messages.iter().find(|m| m.id == message_id))
                        .map(message_prompt_text)
                        .unwrap_or_default()
                        .chars()
                        .take(36)
                        .collect::<String>();
                    self.dialog.replace(DialogType::MessageActions {
                        message_id,
                        preview,
                        is_last_user_message,
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
                    Some(crate::routes::settings::SettingsAction::ZenGatewayToggled) => {
                        // Keep the process-wide anonymous-tier flag in sync
                        // so every new connector honors the switch.
                        cosh_sdk::connector::set_zen_public_tier_enabled(
                            self.setup.zen_public_opt_in() == Some(true),
                        );
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
}
