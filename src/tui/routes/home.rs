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

const LOGO: &[&str] = &[
    r"        __                 ",
    r"  _____/ /_  ____  _____   ",
    r" / ___/ __ \/ __ \/ ___/   ",
    r"/ /__/ / / / /_/ (__  )    ",
    r"\___/_/ /_/\____/____/     ",
];

const TAGLINE: &str = "Terminal AI Agent";

const PLACEHOLDER_PROMPTS: &[&str] = &[
    "Write a Rust CLI tool that processes JSON files",
    "Explain how async/await works in Python",
    "Help me debug a memory leak in C",
    "Create a React component with TypeScript",
    "Optimize this SQL query",
];

pub struct HomeView;

impl HomeView {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        let cx = area.x + area.width / 2;

        let primary = rgba_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        let text = rgba_color(theme.text);

        let logo_start_y = area.y + 2;
        for (i, line) in LOGO.iter().enumerate() {
            let ly = logo_start_y + i as u16;
            let lx = cx.saturating_sub(line.len() as u16 / 2);
            draw_text_line(buf, line, lx, ly, area.width, Style::default().fg(primary));
        }

        let tagline_y = logo_start_y + LOGO.len() as u16 + 1;
        let tagline_x = cx.saturating_sub(TAGLINE.len() as u16 / 2);
        draw_text_line(buf, TAGLINE, tagline_x, tagline_y, area.width, Style::default().fg(muted));

        if state.sessions.is_empty() {
            let prompt_y = tagline_y + 3;
            let prompt_header = "Try one of these:";
            let ph_x = cx.saturating_sub(prompt_header.len() as u16 / 2);
            draw_text_line(buf, prompt_header, ph_x, prompt_y, area.width, Style::default().fg(muted));

            for (i, prompt) in PLACEHOLDER_PROMPTS.iter().enumerate() {
                let py = prompt_y + 2 + i as u16;
                if py >= area.bottom() { break; }
                let entry = format!("  \u{25b6}  {}", prompt);
                let ex = cx.saturating_sub(entry.len() as u16 / 2);
                draw_text_line(buf, &entry, ex, py, area.width, Style::default().fg(text));
            }
        } else {
            let recent_y = tagline_y + 2;
            let recent_label = "Recent Sessions";
            let rl_x = cx.saturating_sub(recent_label.len() as u16 / 2);
            draw_text_line(buf, recent_label, rl_x, recent_y, area.width, Style::default().fg(muted));

            for (i, session) in state.sessions.iter().enumerate() {
                let sy = recent_y + 2 + i as u16;
                if sy >= area.bottom() { break; }

                let is_active = Some(session.id.as_str()) == state.current_session_id.as_deref();
                let marker = if is_active { "\u{25b8}" } else { " " };
                let entry = format!(" {}  {} ({} msgs)", marker, session.title, session.messages.len());
                let entry_style = if is_active {
                    Style::default().fg(primary)
                } else {
                    Style::default().fg(text)
                };
                let ex = cx.saturating_sub(entry.len() as u16 / 2);
                draw_text_line(buf, &entry, ex, sy, area.width, entry_style);
            }
        }

        let key_hints = "n: new session  q: quit  ?: help";
        let hint_x = cx.saturating_sub(key_hints.len() as u16 / 2);
        draw_text_line(buf, key_hints, hint_x, area.bottom().saturating_sub(2), area.width, Style::default().fg(muted));
    }
}
