use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::MouseEvent;

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

fn dim_color(color: Color, brightness: f64) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    Color::Rgb(
        (r as f64 * brightness) as u8,
        (g as f64 * brightness) as u8,
        (b as f64 * brightness) as u8,
    )
}

fn render_logo(buf: &mut Buffer, area: Rect, cx: u16, logo_start_y: u16, frame: u64, primary: Color) {
    let t = frame as f64 * 0.025;
    let center_x: f64 = 14.0;
    let center_y: f64 = 2.5;

    let max_dist = (center_x.powi(2) + center_y.powi(2)).sqrt();
    let pulse_radius = (t * 0.8).sin().abs() * max_dist;

    for (row, line) in LOGO.iter().enumerate() {
        let ly = logo_start_y + row as u16;
        let lx = cx.saturating_sub(LOGO_WIDTH as u16 / 2);
        let right = (lx + LOGO_WIDTH as u16).min(area.right());

        for (col, ch) in line.chars().enumerate() {
            let cx_pos = lx + col as u16;
            if cx_pos >= right || ch == ' ' {
                continue;
            }

            let dist = ((col as f64 - center_x).powi(2) + (row as f64 - center_y).powi(2)).sqrt();
            let ring = (dist - pulse_radius).abs();
            let brightness = (-ring * 0.6).exp();
            let bri = (0.15 + brightness * 0.85).min(1.0);

            if let Some(cell) = buf.cell_mut((cx_pos, ly)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(dim_color(primary, bri)));
            }
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

pub const MENU_ITEMS: &[&str] = &[
    "Start a New Session",
    "Browse Session History",
    "Keyboard Shortcuts",
    "Internal Tools",
    "ADD Provider",
];

#[derive(Clone, Copy, PartialEq)]
pub enum HomeAction {
    NewSession,
    ToggleSidebar,
    OpenShortcuts,
    OpenInternalTools,
    OpenAddProvider,
}

pub struct HomeView {
    pub selected_index: usize,
    pub frame: u64,
    pub anim_active: bool,
    pub anim_total_frames: u64,
}

impl HomeView {
    pub fn new() -> Self {
        HomeView {
            selected_index: 0,
            frame: 0,
            anim_active: true,
            anim_total_frames: 850,
        }
    }

    pub fn advance(&mut self) {
        if !self.anim_active {
            return;
        }
        self.frame += 1;
        if self.frame >= self.anim_total_frames {
            self.anim_active = false;
        }
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
            1 => HomeAction::ToggleSidebar,
            2 => HomeAction::OpenShortcuts,
            3 => HomeAction::OpenInternalTools,
            _ => HomeAction::OpenAddProvider,
        }
    }

    #[allow(clippy::unused_self)]
    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<HomeAction> {
        let cx = area.x + area.width / 2;
        let logo_start_y = area.y + 2;
        let tagline_y = logo_start_y + LOGO.len() as u16 + 1;
        let menu_y = tagline_y + 4;

        let max_entry_len = MENU_ITEMS
            .iter()
            .map(|p| format!("  {p}").len())
            .max()
            .unwrap_or(0);
        let menu_left = cx.saturating_sub((max_entry_len / 2) as u16);

        let my = mouse.y;
        let mx = mouse.x;

        for (i, _item) in MENU_ITEMS.iter().enumerate() {
            let item_y = menu_y + i as u16;
            if my == item_y && mx >= menu_left && mx < menu_left + max_entry_len as u16 {
                return Some(match i {
                    0 => HomeAction::NewSession,
                    1 => HomeAction::ToggleSidebar,
                    2 => HomeAction::OpenShortcuts,
                    3 => HomeAction::OpenInternalTools,
                    _ => HomeAction::OpenAddProvider,
                });
            }
        }

        None
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let cx = area.x + area.width / 2;

        self.advance();

        let primary = rgba_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        let text = rgba_color(theme.text);

        let logo_start_y = area.y + 2;
        render_logo(buf, area, cx, logo_start_y, self.frame, primary);

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

        // Always show menu
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
            let prefix = if is_selected { "🞴 " } else { "  " };
            let entry = format!("{prefix}{item}");
            let style = if is_selected {
                Style::default().fg(primary)
            } else {
                Style::default().fg(text)
            };
            draw_text_line(buf, &entry, menu_left, my, area.width, style);
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
