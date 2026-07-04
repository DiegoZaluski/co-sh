use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;

use crate::state::AppState;
use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

const LOGO_WIDTH: usize = 28;

const LOGO: &[&str] = &[
    "                            ",
    " █████ ██████ █████ ██      ",
    " ██    ██  ██ ██    ██████ ",
    " ██    ██  ██    ██ ██  ██  ",
    " █████ ██████ █████ ██  ██ ",
    "                            ",
];

const TAGLINE: &str = "Terminal AI Agent";

pub const MENU_ITEMS: &[&str] = &["Start a New Session", "Browse Session History"];

#[derive(Clone, Copy, PartialEq)]
pub enum HomeAction {
    NewSession,
    ToggleSidebar,
}

pub struct HomeView {
    pub selected_index: usize,
}

impl HomeView {
    pub fn new() -> Self {
        HomeView { selected_index: 0 }
    }

    pub fn select_next(&mut self) {
        self.selected_index = (self.selected_index + 1) % MENU_ITEMS.len();
    }

    pub fn select_prev(&mut self) {
        self.selected_index = if self.selected_index == 0 {
            MENU_ITEMS.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    pub fn selected_action(&self) -> HomeAction {
        match self.selected_index {
            0 => HomeAction::NewSession,
            _ => HomeAction::ToggleSidebar,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        let cx = area.x + area.width / 2;

        let primary = rgba_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        let text = rgba_color(theme.text);

        let logo_start_y = area.y + 2;
        for (i, line) in LOGO.iter().enumerate() {
            let ly = logo_start_y + i as u16;
            let lx = cx.saturating_sub(LOGO_WIDTH as u16 / 2);
            draw_text_line(buf, line, lx, ly, area.width, Style::default().fg(primary));
        }

        let tagline_y = logo_start_y + LOGO.len() as u16 + 1;
        let tagline_x = cx.saturating_sub(TAGLINE.len() as u16 / 2);
        draw_text_line(
            buf,
            TAGLINE,
            tagline_x,
            tagline_y,
            area.width,
            Style::default().fg(muted),
        );

        if state.sessions.is_empty() {
            let menu_y = tagline_y + 4;

            let max_entry_len = MENU_ITEMS
                .iter()
                .map(|p| format!("  {p}").len())
                .max()
                .unwrap_or(0);
            let menu_left = cx.saturating_sub((max_entry_len / 2) as u16);

            for (i, item) in MENU_ITEMS.iter().enumerate() {
                let my = menu_y + i as u16;
                if my >= area.bottom() {
                    break;
                }
                let is_selected = i == self.selected_index;
                let prefix = if is_selected { "> " } else { "  " };
                let entry = format!("{prefix}{item}");
                let style = if is_selected {
                    Style::default().fg(primary)
                } else {
                    Style::default().fg(text)
                };
                draw_text_line(buf, &entry, menu_left, my, area.width, style);
            }
        } else {
            let recent_y = tagline_y + 2;
            let recent_label = "Recent Sessions";
            let rl_x = cx.saturating_sub(recent_label.len() as u16 / 2);
            draw_text_line(
                buf,
                recent_label,
                rl_x,
                recent_y,
                area.width,
                Style::default().fg(muted),
            );

            for (i, session) in state.sessions.iter().enumerate() {
                let sy = recent_y + 2 + i as u16;
                if sy >= area.bottom() {
                    break;
                }

                let is_active =
                    Some(session.id.as_str()) == state.current_session_id.as_deref();
                let marker = if is_active { "\u{25b8}" } else { " " };
                let entry = format!(
                    " {}  {} ({} msgs)",
                    marker,
                    session.title,
                    session.messages.len()
                );
                let entry_style = if is_active {
                    Style::default().fg(primary)
                } else {
                    Style::default().fg(text)
                };
                let ex = cx.saturating_sub(entry.len() as u16 / 2);
                draw_text_line(buf, &entry, ex, sy, area.width, entry_style);
            }
        }

        let key_hints = "\u{2191}\u{2193} navigate  enter select  q quit";
        let hint_x = cx.saturating_sub(key_hints.len() as u16 / 2);
        draw_text_line(
            buf,
            key_hints,
            hint_x,
            area.bottom().saturating_sub(2),
            area.width,
            Style::default().fg(muted),
        );
    }
}
