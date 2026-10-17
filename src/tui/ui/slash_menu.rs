use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::component::spinner::SpinnerState;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line_compact;

/// Maximum number of command rows the menu shows at once. More commands than
/// this scroll inside the window ([`SlashMenu::visible_window`]).
const MAX_ROWS: usize = 6;

fn draw_bg_line(buf: &mut Buffer, x: u16, y: u16, width: u16, color: Color) {
    for cx in x..(x + width) {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            // Clear the cell too: the menu band overlays whatever the chat
            // rendered in those rows, so stale glyphs must not bleed through.
            cell.set_char(' ');
            cell.set_style(Style::default().bg(color));
        }
    }
}

fn selected_foreground_color(bg: RGBA, fallback: RGBA) -> Color {
    let (r, g, b, a) = bg.to_ints();
    let (r, g, b) = if a == 0 {
        let (fr, fg, fb, _) = fallback.to_ints();
        (fr, fg, fb)
    } else {
        (r, g, b)
    };

    let luminance = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
    if luminance > 0.5 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

#[derive(Clone)]
pub struct SlashCommand {
    pub name: String,
    pub desc: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashMenuMouseAction {
    None,
    Consumed,
    Execute,
}

/// OpenCode-faithful slash ("/") autocomplete menu.
/// Renders inline above the prompt, not as a modal dialog.
///
/// The command SOURCE is dynamic: the default cosh set, or the focused
/// `/agents` pane's own command list (the agent's exact spellings). While
/// an agent snapshot is being fetched the menu renders a spinner row
/// instead of commands — the probe takes seconds, so the user sees WHY the
/// list is still empty.
///
/// Which command source the slash menu is currently serving. The router
/// reads this — not the split's focus — so a picked command can never
/// disagree with the list the user saw: the menu's own source IS the
/// routing decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The cosh harness's own set (the main agent / no pane focused).
    Cosh,
    /// A focused agent pane's own list (already blocklist-filtered).
    Agent,
}

pub struct SlashMenu {
    pub visible: bool,
    pub query: String,
    pub selected: usize,
    pub commands: Vec<SlashCommand>,
    /// Which list `commands` currently holds (see [`Source`]).
    pub source: Source,
    /// True while an agent command snapshot is being fetched for the
    /// focused pane: the menu shows a spinner row, accepts navigation
    /// keys as a no-op, and refuses execution.
    pub loading: bool,
    /// Animation state for the loading row (advanced by the render tick
    /// while `loading` is set, same contract the ModelList spinner has).
    pub spinner: SpinnerState,
}

impl SlashMenu {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            selected: 0,
            commands: Self::cosh_commands(),
            source: Source::Cosh,
            loading: false,
            spinner: SpinnerState::new(),
        }
    }

    /// The cosh harness's own command set (the default source).
    pub fn cosh_commands() -> Vec<SlashCommand> {
        vec![
            SlashCommand {
                name: "models".into(),
                desc: "Manage models".into(),
            },
            SlashCommand {
                name: "agents".into(),
                desc: "Split the chat into agent panes".into(),
            },
            SlashCommand {
                name: "router".into(),
                desc: "Open the fallback router".into(),
            },
            SlashCommand {
                name: "providers".into(),
                desc: "Add an API provider".into(),
            },
            SlashCommand {
                name: "settings".into(),
                desc: "Open settings".into(),
            },
            SlashCommand {
                name: "tools".into(),
                desc: "Open internal tools".into(),
            },
            SlashCommand {
                name: "themes".into(),
                desc: "Change theme".into(),
            },
            SlashCommand {
                name: "background".into(),
                desc: "Toggle terminal background".into(),
            },
            SlashCommand {
                name: "bell".into(),
                desc: "Toggle completion bell".into(),
            },
            SlashCommand {
                name: "anim".into(),
                desc: "Toggle chat animation".into(),
            },
            SlashCommand {
                name: "spinner".into(),
                desc: "Set spinner animation".into(),
            },
            SlashCommand {
                name: "toolcall".into(),
                desc: "Set tool call mode".into(),
            },
            SlashCommand {
                name: "compact".into(),
                desc: "Compact context now".into(),
            },
            SlashCommand {
                name: "export".into(),
                desc: "Export transcript to Markdown".into(),
            },
            SlashCommand {
                name: "new".into(),
                desc: "Start a new session".into(),
            },
            SlashCommand {
                name: "rename".into(),
                desc: "Rename session".into(),
            },
            SlashCommand {
                name: "undo".into(),
                desc: "Undo last revert".into(),
            },
        ]
    }

    /// Swap the command source to an agent's own list (the pane's cached
    /// snapshot, blocklist-filtered by the app layer). Resets selection.
    pub fn set_agent_commands(&mut self, commands: Vec<SlashCommand>) {
        self.commands = commands;
        self.source = Source::Agent;
        self.loading = false;
        self.selected = 0;
    }

    /// Swap back to the cosh command set (unfocused / closed split).
    pub fn set_cosh_commands(&mut self) {
        self.commands = Self::cosh_commands();
        self.source = Source::Cosh;
        self.loading = false;
        self.selected = 0;
    }

    /// Flag the menu as fetching an agent's command snapshot: a spinner
    /// row renders until `set_agent_commands` / `set_cosh_commands` swaps
    /// the source in.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn check_state(input: &str) -> (bool, String) {
        if !input.starts_with('/') {
            return (false, String::new());
        }
        let after_slash = &input[1..];
        if after_slash.contains(' ') {
            return (false, String::new());
        }
        (true, after_slash.to_string())
    }

    pub fn update(&mut self, input: &str) {
        let (should_be_visible, query) = Self::check_state(input);
        self.visible = should_be_visible;
        self.query = query;
        if self.visible {
            self.selected = self.filtered_indices().first().copied().unwrap_or(0);
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.commands.len()).collect();
        }
        let needle = self.query.to_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn select_next(&mut self) {
        if self.loading {
            return;
        }
        let idxs = self.filtered_indices();
        if idxs.is_empty() {
            return;
        }
        let pos = idxs.iter().position(|&i| i == self.selected).unwrap_or(0);
        self.selected = idxs[(pos + 1) % idxs.len()];
    }

    pub fn select_prev(&mut self) {
        if self.loading {
            return;
        }
        let idxs = self.filtered_indices();
        if idxs.is_empty() {
            return;
        }
        let pos = idxs.iter().position(|&i| i == self.selected).unwrap_or(0);
        self.selected = if pos == 0 {
            idxs[idxs.len() - 1]
        } else {
            idxs[pos - 1]
        };
    }

    pub fn get_selected_command(&self) -> Option<&SlashCommand> {
        if self.loading {
            return None;
        }
        let idxs = self.filtered_indices();
        if idxs.is_empty() {
            return None;
        }
        let pos = idxs.iter().position(|&i| i == self.selected)?;
        Some(&self.commands[idxs[pos]])
    }

    /// The slice of filtered indices shown in the menu's MAX_ROWS window,
    /// scrolled so the selected item is always on screen. Returns
    /// `(start, len)` into the filtered list — the same computation for the
    /// renderer and for mouse hit-testing, so a click always lands on the
    /// row that is drawn.
    fn visible_window(&self, idxs: &[usize]) -> (usize, usize) {
        let len = MAX_ROWS.min(idxs.len());
        if len == 0 {
            return (0, 0);
        }
        let pos = idxs.iter().position(|&i| i == self.selected).unwrap_or(0);
        // Keep the selection inside the window (same rule as the model list
        // dialog): scroll only once it falls past the last visible row.
        let start = if pos >= len { pos - len + 1 } else { 0 };
        (start, len)
    }

    /// Handle mouse input using the same selected index as keyboard navigation.
    /// `prompt_area` is the same area passed to `render()`.
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        prompt_area: Rect,
        _theme: &Theme,
    ) -> SlashMenuMouseAction {
        if !self.visible {
            return SlashMenuMouseAction::None;
        }
        let idxs = self.filtered_indices();
        let (start, visible_rows) = self.visible_window(&idxs);
        // An empty result still reserves one row for the "No matching items"
        // placeholder.
        let max_rows = if idxs.is_empty() { 1 } else { visible_rows };
        let menu_y_start = prompt_area.y.saturating_sub(max_rows as u16);
        let menu_width = prompt_area.width;

        let x = mouse.x;
        let y = mouse.y;

        // Check if the pointer is within the menu bounds.
        if x < prompt_area.x || x >= prompt_area.x + menu_width {
            return SlashMenuMouseAction::None;
        }
        if y < menu_y_start || y >= menu_y_start + max_rows as u16 {
            return SlashMenuMouseAction::None;
        }

        let row = (y - menu_y_start) as usize;
        if !matches!(
            mouse.event_type,
            MouseEventType::ScrollUp | MouseEventType::ScrollDown
        ) && row < visible_rows
        {
            self.selected = idxs[start + row];
        }

        match mouse.event_type {
            MouseEventType::ScrollUp => {
                self.select_prev();
                SlashMenuMouseAction::Consumed
            }
            MouseEventType::ScrollDown => {
                self.select_next();
                SlashMenuMouseAction::Consumed
            }
            MouseEventType::Move | MouseEventType::Drag | MouseEventType::Down => {
                SlashMenuMouseAction::Consumed
            }
            MouseEventType::Up if mouse.button == MouseButton::Left && !idxs.is_empty() => {
                SlashMenuMouseAction::Execute
            }
            MouseEventType::Up => SlashMenuMouseAction::Consumed,
        }
    }

    /// Render autocomplete menu inline above prompt (like `OpenCode`).
    /// `prompt_area`: where the prompt is rendered.
    /// Renders just above the prompt with scrollable items.
    /// `mode_color` is the current harness mode's color (the same one the
    /// prompt's rails and caps use): the menu's side borders and the
    /// selected-row fill follow it so the menu reads as part of the prompt.
    pub fn render(&self, buf: &mut Buffer, prompt_area: Rect, theme: &Theme, mode_color: RGBA) {
        if !self.visible {
            return;
        }

        // Loading state: one spinner row instead of commands (an agent
        // snapshot probe is in flight — the list is empty BECAUSE of that,
        // so the placeholder below would lie about it).
        if self.loading {
            let row_y = prompt_area.y.saturating_sub(1);
            let menu_width = prompt_area.width;
            let content_x = prompt_area.x + 1;
            let content_width = menu_width.saturating_sub(2);
            draw_bg_line(buf, content_x, row_y, content_width, rgba_color(theme.background_element));
            if let Some(cell) = buf.cell_mut((prompt_area.x, row_y)) {
                cell.set_char('│');
                cell.set_style(
                    Style::default()
                        .fg(rgba_color(mode_color))
                        .bg(rgba_color(theme.background)),
                );
            }
            if menu_width > 1
                && let Some(cell) = buf.cell_mut((prompt_area.x + menu_width - 1, row_y))
            {
                cell.set_char('│');
                cell.set_style(
                    Style::default()
                        .fg(rgba_color(mode_color))
                        .bg(rgba_color(theme.background)),
                );
            }
            let spinner_char = self.spinner.current_char();
            // The spinner alone carries the message: "loading" needs no
            // words after it — the frame is self-explanatory.
            let text = spinner_char.to_string();
            draw_text_line_compact(
                buf,
                &text,
                content_x + 1,
                row_y,
                content_width.saturating_sub(2),
                Style::default().fg(rgba_color(theme.text_muted)),
            );
            return;
        }

        let idxs = self.filtered_indices();
        let (start, visible_rows) = self.visible_window(&idxs);
        let max_rows = if idxs.is_empty() { 1 } else { visible_rows };

        // Render just above the prompt input area
        let menu_y_start = prompt_area.y.saturating_sub(max_rows as u16);
        let menu_width = prompt_area.width;
        // Keep the menu's normal rows on the same fill as the prompt box.
        let row_fill = rgba_color(theme.background_element);
        let border_fg = rgba_color(mode_color);
        let command_column_width = idxs
            .iter()
            .map(|&i| format!("/{}", self.commands[i].name).chars().count())
            .max()
            .unwrap_or(0);

        // Draw each command option or no-results placeholder
        for row in 0..max_rows {
            let row_y = menu_y_start + row as u16;
            let is_no_results = idxs.is_empty();
            let (row_bg, cmd_text, desc_text, cmd_style, desc_style) = if is_no_results {
                (
                    row_fill,
                    String::new(),
                    "No matching items".to_string(),
                    Style::default(),
                    Style::default().fg(rgba_color(theme.text_muted)),
                )
            } else {
                let i = idxs[start + row];
                let cmd = &self.commands[i];
                let is_selected = i == self.selected;
                let row_bg = if is_selected {
                    rgba_color(mode_color)
                } else {
                    row_fill
                };
                let cmd_text = format!("/{}", cmd.name);
                let desc_text = format!(" {}", cmd.desc);
                let selected_fg = selected_foreground_color(mode_color, theme.background);
                let cmd_style = if is_selected {
                    Style::default().fg(selected_fg)
                } else {
                    Style::default().fg(rgba_color(theme.text))
                };
                let desc_style = if is_selected {
                    Style::default().fg(selected_fg)
                } else {
                    Style::default().fg(rgba_color(theme.text_muted))
                };
                (row_bg, cmd_text, desc_text, cmd_style, desc_style)
            };

            // content area leaves 1 cell gap to the left and right borders (match OpenCode)
            let content_x = prompt_area.x + 1;
            let content_width = menu_width.saturating_sub(2);
            draw_bg_line(buf, content_x, row_y, content_width, row_bg);

            // Draw left/right border lines with base background so the menu has a visible gap
            if let Some(cell) = buf.cell_mut((prompt_area.x, row_y)) {
                cell.set_char('│');
                cell.set_style(
                    Style::default()
                        .fg(border_fg)
                        .bg(rgba_color(theme.background)),
                );
            }
            if menu_width > 1
                && let Some(cell) = buf.cell_mut((prompt_area.x + menu_width - 1, row_y))
            {
                cell.set_char('│');
                cell.set_style(
                    Style::default()
                        .fg(border_fg)
                        .bg(rgba_color(theme.background)),
                );
            }

            let text_x = content_x + 1; // internal padding like OpenCode's paddingLeft
            let text_width = content_width.saturating_sub(2);
            if idxs.is_empty() {
                draw_text_line_compact(buf, &desc_text, text_x, row_y, text_width, desc_style);
            } else {
                draw_text_line_compact(buf, &cmd_text, text_x, row_y, text_width, cmd_style);
                let desc_x = text_x + command_column_width as u16;
                draw_text_line_compact(
                    buf,
                    &desc_text,
                    desc_x,
                    row_y,
                    text_width.saturating_sub(command_column_width as u16),
                    desc_style,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn mouse(event_type: MouseEventType, x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            event_type,
            MouseButton::Left,
            x,
            y,
            cosh_tui::core::types::MouseModifiers::none(),
        )
    }

    #[test]
    fn hover_moves_the_shared_selection() {
        let mut menu = SlashMenu::new();
        menu.update("/");
        let prompt_area = Rect::new(10, 10, 40, 4);
        let theme = ThemeRegistry::new().default_theme().clone();

        let action = menu.handle_mouse(&mouse(MouseEventType::Move, 20, 6), prompt_area, &theme);

        assert_eq!(action, SlashMenuMouseAction::Consumed);
        assert_eq!(menu.selected, 2);
    }

    #[test]
    fn wheel_navigation_uses_the_keyboard_selection_state() {
        let mut menu = SlashMenu::new();
        menu.update("/");
        let prompt_area = Rect::new(10, 10, 40, 4);
        let theme = ThemeRegistry::new().default_theme().clone();

        let down = menu.handle_mouse(
            &mouse(MouseEventType::ScrollDown, 20, 6),
            prompt_area,
            &theme,
        );
        assert_eq!(down, SlashMenuMouseAction::Consumed);
        assert_eq!(menu.selected, 1);

        let up = menu.handle_mouse(&mouse(MouseEventType::ScrollUp, 20, 6), prompt_area, &theme);
        assert_eq!(up, SlashMenuMouseAction::Consumed);
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn click_on_an_option_executes_the_hovered_selection() {
        let mut menu = SlashMenu::new();
        menu.update("/");
        let prompt_area = Rect::new(10, 10, 40, 4);
        let theme = ThemeRegistry::new().default_theme().clone();

        let action = menu.handle_mouse(&mouse(MouseEventType::Up, 20, 7), prompt_area, &theme);

        assert_eq!(action, SlashMenuMouseAction::Execute);
        assert_eq!(menu.selected, 3);
    }
}
