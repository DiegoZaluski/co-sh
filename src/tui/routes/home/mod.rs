use crate::logo::{LOGO, LOGO_WIDTH};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
pub mod banner;
pub mod footer;

pub use banner::{BannerAction, BannerContent, BannerView, UpdateStatus};

use cosh_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};

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

fn blend_color(fg: Color, bg: Color, amount: f64) -> Color {
    let (r1, g1, b1) = match fg {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let (r2, g2, b2) = match bg {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    Color::Rgb(
        (r1 as f64 * amount + r2 as f64 * (1.0 - amount)) as u8,
        (g1 as f64 * amount + g2 as f64 * (1.0 - amount)) as u8,
        (b1 as f64 * amount + b2 as f64 * (1.0 - amount)) as u8,
    )
}

fn render_logo(
    buf: &mut Buffer,
    area: Rect,
    cx: u16,
    logo_start_y: u16,
    frame: u64,
    primary: Color,
    bg: Color,
) {
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
                cell.set_style(Style::default().fg(blend_color(primary, bg, bri)));
            }
        }
    }
}

const TAGLINE: &str = "Terminal AI Agent";

#[cfg(feature = "embed")]
pub const MENU_ITEMS: &[&str] = &[
    "Start a new session",
    "RAG via file or URL",
    "Model Router",
    "Internal tools",
    "ADD provider",
    "Settings",
];

#[cfg(not(feature = "embed"))]
pub const MENU_ITEMS: &[&str] = &[
    "Start a new session",
    "Model Router",
    "Internal tools",
    "ADD provider",
    "Settings",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HomeAction {
    NewSession,
    ToggleSidebar,
    OpenShortcuts,
    #[cfg(feature = "embed")]
    OpenRag,
    OpenModelRouter,
    OpenInternalTools,
    OpenAddProvider,
    OpenSettings,
    /// A click landed on the announcement banner below the menu.
    Banner(BannerAction),
}

pub struct HomeView {
    pub selected_index: usize,
    pub frame: u64,
    pub anim_active: bool,
    pub anim_total_frames: u64,
    /// Announcement banner rendered below the menu (release updates now;
    /// feature news can be added later via [`BannerContent`]).
    pub banner: BannerView,
}

impl HomeView {
    pub const fn new() -> Self {
        Self {
            selected_index: 0,
            frame: 0,
            anim_active: true,
            anim_total_frames: 850,
            banner: BannerView::new(),
        }
    }

    pub const fn advance(&mut self) {
        if !self.anim_active {
            return;
        }
        self.frame += 1;
        if self.frame >= self.anim_total_frames {
            self.anim_active = false;
        }
    }

    pub const fn select_next(&mut self) {
        self.selected_index = (self.selected_index + 1) % MENU_ITEMS.len();
    }

    pub const fn select_prev(&mut self) {
        self.selected_index = if self.selected_index == 0 {
            MENU_ITEMS.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    pub const fn selected_action(&self) -> HomeAction {
        match self.selected_index {
            0 => HomeAction::NewSession,
            #[cfg(feature = "embed")]
            1 => HomeAction::OpenRag,
            #[cfg(feature = "embed")]
            2 => HomeAction::OpenModelRouter,
            #[cfg(feature = "embed")]
            3 => HomeAction::OpenInternalTools,
            #[cfg(feature = "embed")]
            4 => HomeAction::OpenAddProvider,
            #[cfg(feature = "embed")]
            5 => HomeAction::OpenSettings,
            #[cfg(not(feature = "embed"))]
            1 => HomeAction::OpenModelRouter,
            #[cfg(not(feature = "embed"))]
            2 => HomeAction::OpenInternalTools,
            #[cfg(not(feature = "embed"))]
            3 => HomeAction::OpenAddProvider,
            #[cfg(not(feature = "embed"))]
            4 => HomeAction::OpenSettings,
            _ => HomeAction::OpenAddProvider,
        }
    }

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

        // Banner (below the menu) takes precedence over the menu rows.
        if let Some(banner_action) = self.banner.handle_click(mx, my) {
            return Some(HomeAction::Banner(banner_action));
        }

        for (i, _item) in MENU_ITEMS.iter().enumerate() {
            let item_y = menu_y + i as u16;
            if my == item_y && mx >= menu_left && mx < menu_left + max_entry_len as u16 {
                return Some(match i {
                    0 => HomeAction::NewSession,
                    #[cfg(feature = "embed")]
                    1 => HomeAction::OpenRag,
                    #[cfg(feature = "embed")]
                    2 => HomeAction::OpenModelRouter,
                    #[cfg(feature = "embed")]
                    3 => HomeAction::OpenInternalTools,
                    #[cfg(feature = "embed")]
                    4 => HomeAction::OpenAddProvider,
                    #[cfg(feature = "embed")]
                    5 => HomeAction::OpenSettings,
                    #[cfg(not(feature = "embed"))]
                    1 => HomeAction::OpenModelRouter,
                    #[cfg(not(feature = "embed"))]
                    2 => HomeAction::OpenInternalTools,
                    #[cfg(not(feature = "embed"))]
                    3 => HomeAction::OpenAddProvider,
                    #[cfg(not(feature = "embed"))]
                    4 => HomeAction::OpenSettings,
                    _ => HomeAction::OpenAddProvider,
                });
            }
        }

        None
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, animation_ticks: u32) {
        let cx = area.x + area.width / 2;

        for _ in 0..animation_ticks {
            self.advance();
        }

        let primary = rgba_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        let text = rgba_color(theme.text);

        let bg_color = rgba_color(theme.background);
        let logo_start_y = area.y + 2;
        render_logo(buf, area, cx, logo_start_y, self.frame, primary, bg_color);

        let tagline_y = logo_start_y + LOGO.len() as u16 + 1;
        let tagline_x = cx.saturating_sub(TAGLINE.len() as u16 / 2);
        draw_text_line(
            buf,
            TAGLINE,
            tagline_x,
            tagline_y,
            area.width,
            Style::default().fg(text),
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

        // Announcement banner (release updates): a horizontal rectangle
        // anchored BOTTOM-UP — it sits just above the footer with a 2-line
        // gap, not right below the menu. Its height comes from the banner's
        // own internal items (BannerView::height), never hardcoded here.
        const SIDEBAR_RESERVE: u16 = 22;
        const BANNER_FOOTER_GAP: u16 = 2;
        let key_hints = "show session history ctrl+B | show keyboard shortcuts ctrl+K";
        let hints_visible = (key_hints.len() as u16) + SIDEBAR_RESERVE <= area.width;
        let hints_y = area.bottom().saturating_sub(2);
        // First row BELOW the menu: neither the footer nor the banner may
        // ever encroach on it. The menu always wins on short terminals.
        let menu_bottom = menu_y + MENU_ITEMS.len() as u16;
        // Shrink-height policy — HIDE, never overlap. As the terminal gets
        // shorter: the footer (key hints) is hidden first once the menu would
        // reach its row, then the banner once the gap between the menu and
        // the bottom can no longer fit it.
        let hints_fits = hints_visible && hints_y >= menu_bottom;
        // Anchor from the hints row (or terminal bottom when the hints are
        // hidden): banner bottom edge = anchor - gap, then subtract the
        // banner's own height to get its top row.
        let anchor_y = if hints_fits { hints_y } else { area.bottom() };
        let banner_top = anchor_y.saturating_sub(BANNER_FOOTER_GAP + self.banner.height());
        let banner_fits = banner_top >= menu_bottom && banner_top >= area.y;
        if banner_fits {
            self.banner.render(buf, area, theme, banner_top, anchor_y);
        } else {
            // Hidden: drop the click hitboxes from the previous frame so a
            // stale on-screen button can never receive clicks.
            self.banner.clear_hitboxes();
        }

        // Hide the hints once the terminal is too narrow to also fit the left
        // panel: expanding the sidebar narrows the usable width by
        // SIDEBAR_WIDTH (22), so only show them while they'd still fit in that
        // worst case. They are ALSO hidden when the terminal is too short and
        // the menu would reach their row (shrink-height policy above).
        if hints_fits {
            let hint_x = cx.saturating_sub(key_hints.len() as u16 / 2);
            draw_text_line(
                buf,
                key_hints,
                hint_x,
                hints_y,
                area.width,
                Style::default().fg(muted),
            );
        }
    }
}
