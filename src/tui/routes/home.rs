use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;

/// How many frames between each pixel reveal during logo animation
const ANIM_REVEAL_INTERVAL: u64 = 3;

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

fn build_pixels() -> Vec<(u16, u16)> {
    let mut pixels = Vec::new();
    for (row, line) in LOGO.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            if ch != ' ' {
                pixels.push((row as u16, col as u16));
            }
        }
    }
    pixels
}

fn shuffled_order(len: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let mut state = seed;
    for i in (1..len).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = (state >> 33) as usize % (i + 1);
        order.swap(i, j);
    }
    order
}

fn flicker_brightness(idx: usize, frame: u64) -> f64 {
    let h = idx as u64 * 374_761_393 + frame.wrapping_mul(668_265_263);
    let h = h.wrapping_mul(h.wrapping_add(12345));
    let r = (h >> 16) & 0xff;
    if r > 200 {
        0.0
    } else if r > 150 {
        0.08
    } else if r > 100 {
        0.2
    } else if r > 50 {
        0.4
    } else {
        0.6
    }
}

#[allow(clippy::too_many_arguments)]
fn render_logo_glitch(
    buf: &mut Buffer,
    area: Rect,
    cx: u16,
    logo_start_y: u16,
    frame: u64,
    primary: Color,
    active: bool,
    pixels: &[(u16, u16)],
    order: &[usize],
    revealed_count: usize,
) {
    let mut revealed_set = vec![false; pixels.len()];
    for &idx in &order[..revealed_count] {
        revealed_set[idx] = true;
    }

    for (row, line) in LOGO.iter().enumerate() {
        let ly = logo_start_y + row as u16;
        let lx = cx.saturating_sub(LOGO_WIDTH as u16 / 2);
        let right = (lx + LOGO_WIDTH as u16).min(area.right());

        for (col, ch) in line.chars().enumerate() {
            let cx_pos = lx + col as u16;
            if cx_pos >= right || ch == ' ' {
                continue;
            }

            let style = if active {
                let pixel_idx = pixels
                    .iter()
                    .position(|&(r, c)| r == row as u16 && c == col as u16);
                match pixel_idx {
                    Some(idx) if revealed_set[idx] => Style::default().fg(primary),
                    Some(idx) => {
                        let bri = flicker_brightness(idx, frame);
                        Style::default().fg(dim_color(primary, bri))
                    }
                    None => Style::default().fg(primary),
                }
            } else {
                Style::default().fg(primary)
            };

            if let Some(cell) = buf.cell_mut((cx_pos, ly)) {
                cell.set_char(ch);
                cell.set_style(style);
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
    pixels: Vec<(u16, u16)>,
    order: Vec<usize>,
    revealed_count: usize,
}

impl HomeView {
    pub fn new() -> Self {
        let pixels = build_pixels();
        let order = shuffled_order(pixels.len(), 42);
        HomeView {
            selected_index: 0,
            frame: 0,
            anim_active: true,
            revealed_count: 0,
            pixels,
            order,
        }
    }

    pub fn advance(&mut self) {
        if !self.anim_active {
            return;
        }
        self.frame += 1;
        if self.frame.is_multiple_of(ANIM_REVEAL_INTERVAL)
            && self.revealed_count < self.pixels.len()
        {
            self.revealed_count += 1;
        }
        if self.revealed_count >= self.pixels.len() {
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
        render_logo_glitch(
            buf,
            area,
            cx,
            logo_start_y,
            self.frame,
            primary,
            self.anim_active,
            &self.pixels,
            &self.order,
            self.revealed_count,
        );

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

#[allow(clippy::cast_sign_loss)]
fn dim_color(color: Color, brightness: f64) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    Color::Rgb(
        (f64::from(r) * brightness) as u8,
        (f64::from(g) * brightness) as u8,
        (f64::from(b) * brightness) as u8,
    )
}
