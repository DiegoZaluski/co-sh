//! Home banner: a horizontal rectangle centered below the menu.
//!
//! Two kinds of announcements share this slot:
//!
//! 1. Feature announcements (future — add a [`BannerContent`] variant and
//!    extend [`BannerView::render`]; the layout and hit-testing are shared).
//! 2. Release updates (implemented): when a newer version is published on
//!    GitHub the banner shows `Vx.y.z Now available`, a changelog link and
//!    an "Update version" button that runs the in-app update pipeline.
//!
//! The banner background matches the left panel (`theme.background_panel`)
//! so it reads as part of the same chrome as the rest of the interface.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};

use super::draw_text_line;

/// Content currently shown in the banner. `None` hides the banner entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerContent {
    /// A new release is available for download.
    ReleaseUpdate {
        /// Version without the `v` prefix (e.g. `0.1.1`).
        version: String,
        /// Raw git tag as published on GitHub (e.g. `v0.1.1`).
        tag: String,
    },
    // Future: `FeatureNews { ... }` — the "announce new features" slot.
    // Adding a variant only requires extending `render` below; positioning,
    // background and hit-testing are shared.
}

/// Lifecycle of the in-app update pipeline (drives the button's look).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateStatus {
    /// Update available; the button is clickable.
    #[default]
    Idle,
    /// The update pipeline is running; the button is disabled.
    Updating,
    /// The update installed; the user must reopen cosh to apply it.
    Installed,
    /// The update pipeline failed; the button becomes a retry.
    Failed,
}

/// What a click on the banner means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    /// The "Update version" button was clicked.
    StartUpdate,
    /// The changelog link was clicked.
    OpenChangelog,
}

/// Rounded-corner border glyphs matching the soft boxes used across the TUI.
const CORNER_TOP_LEFT: char = '\u{256d}';
const CORNER_TOP_RIGHT: char = '\u{256e}';
const CORNER_BOTTOM_LEFT: char = '\u{2570}';
const CORNER_BOTTOM_RIGHT: char = '\u{256f}';
const BORDER_HORIZONTAL: char = '\u{2500}';
const BORDER_VERTICAL: char = '\u{2502}';

/// Total height, derived from the banner's OWN internal items (never from
/// the surrounding layout): top border with the embedded title (1) + one
/// row per content item — changelog link, update button (2) + bottom
/// border (1). Adding a future content row means bumping the item count
/// here and extending `render` — every consumer reads [`Self::height`].
pub const BANNER_HEIGHT: u16 = 2 + 2;
const INNER_PADDING: u16 = 2;

pub struct BannerView {
    /// What the banner announces (`None` = hidden).
    pub content: Option<BannerContent>,
    /// Update pipeline lifecycle (only meaningful with `ReleaseUpdate`).
    pub status: UpdateStatus,
    /// Button hitbox from the last render (`None` when not clickable).
    button_area: Option<Rect>,
    /// Changelog-link hitbox from the last render.
    link_area: Option<Rect>,
}

impl BannerView {
    pub const fn new() -> Self {
        Self {
            content: None,
            status: UpdateStatus::Idle,
            button_area: None,
            link_area: None,
        }
    }

    /// Display width of a string in terminal cells: for these fixed BMP
    /// strings (arrow, ellipsis, em-dash, check) the char count matches what
    /// `draw_text_line` writes, while byte `len()` would overestimate.
    fn text_width(s: &str) -> u16 {
        s.chars().count() as u16
    }

    /// Rendered height in rows — derived from the internal items (see
    /// [`BANNER_HEIGHT`]), used by callers to anchor the banner bottom-up.
    pub const fn height(&self) -> u16 {
        BANNER_HEIGHT
    }

    /// Drop the click hitboxes (used when the banner is hidden because the
    /// terminal is too short): a banner that is not on screen must never
    /// receive clicks from hitboxes left over by a previous frame.
    pub fn clear_hitboxes(&mut self) {
        self.button_area = None;
        self.link_area = None;
    }

    /// Hit-test a click against the last-rendered hitboxes.
    pub fn handle_click(&self, x: u16, y: u16) -> Option<BannerAction> {
        let inside = |area: Option<Rect>| -> bool {
            area.is_some_and(|a| x >= a.x && x < a.right() && y >= a.y && y < a.bottom())
        };
        if matches!(self.status, UpdateStatus::Idle | UpdateStatus::Failed)
            && inside(self.button_area)
        {
            return Some(BannerAction::StartUpdate);
        }
        if inside(self.link_area) {
            return Some(BannerAction::OpenChangelog);
        }
        None
    }

    /// Renders the banner centered horizontally in `area`, starting at row
    /// `y`, never extending below `bottom_limit` (e.g. the key-hints row).
    /// No-op when there is no content or the terminal is too small.
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        y: u16,
        bottom_limit: u16,
    ) {
        // Fresh hitboxes every frame: stale areas from a previous layout
        // must never receive clicks.
        self.button_area = None;
        self.link_area = None;

        let Some(BannerContent::ReleaseUpdate { version, .. }) = self.content.clone() else {
            return;
        };
        if area.width < 10 || area.height == 0 || y < area.y || y >= bottom_limit {
            return;
        }

        let primary = rgba_color(theme.primary);
        let muted = rgba_color(theme.text_muted);
        // Same white the router menu font uses — title, box border and the
        // changelog link all share it.
        let white = rgba_color(theme.text);
        let success_fg = rgba_color(theme.success);

        // 🎉 (party popper) — the customary "something just shipped" emoji
        // for release titles.
        let headline = format!("\u{1f389} V{version} Now available");
        let link_text = "Read the changelog \u{2197}";
        let button_label = "Update";

        // Width fits the widest row plus inner padding and borders; the
        // banner shrinks on narrow terminals and hides when even the title
        // would not fit. Widths are in terminal CELLS (char count), not
        // bytes — the link contains multi-byte BMP glyphs.
        let title_w = Self::text_width(&headline) + 1; // +1: 🎉 renders 2 cells
        let button_w = Self::text_width(button_label);
        let mut banner_w =
            title_w.max(Self::text_width(link_text)).max(button_w) + INNER_PADDING * 2 + 2;
        if banner_w > area.width.saturating_sub(2) {
            banner_w = area.width.saturating_sub(2);
        }
        if banner_w < title_w + 6 {
            return;
        }

        let banner_h = BANNER_HEIGHT.min(bottom_limit.saturating_sub(y));
        if banner_h < BANNER_HEIGHT {
            return;
        }
        let rect = Rect::new(area.x + (area.width - banner_w) / 2, y, banner_w, banner_h);

        let border_style = Style::default().fg(white);
        let right = rect.right().saturating_sub(1);
        let bottom = rect.bottom().saturating_sub(1);

        // Top border with the title embedded: `╭─V0.1.1 Now available──╮`.
        // The row is filled with ─ first, then the title is written over it,
        // so the closing run of ─ before ╮ happens automatically.
        for xx in (rect.x + 1)..right {
            if let Some(cell) = buf.cell_mut((xx, rect.y)) {
                cell.set_char(BORDER_HORIZONTAL);
                cell.set_style(border_style);
            }
        }
        let title_x = rect.x + 2;
        // The 🎉 is a 2-cell-wide glyph while `draw_text_line` places one
        // char per cell — draw the emoji explicitly and shift the rest of
        // the title 2 cells right so it lines up with the border dashes.
        if let Some(cell) = buf.cell_mut((title_x, rect.y)) {
            cell.set_char('\u{1f389}');
            cell.set_style(border_style);
        }
        let rest_x = title_x + 2;
        let rest: String = headline.chars().skip(1).collect(); // skip the emoji
        draw_text_line(
            buf,
            &rest,
            rest_x,
            rect.y,
            right.saturating_sub(rest_x),
            border_style,
        );

        // Remaining border: verticals, bottom row and the 4 corners.
        for yy in (rect.y + 1)..bottom {
            for xx in [rect.x, right] {
                if let Some(cell) = buf.cell_mut((xx, yy)) {
                    cell.set_char(BORDER_VERTICAL);
                    cell.set_style(border_style);
                }
            }
        }
        for xx in (rect.x + 1)..right {
            if let Some(cell) = buf.cell_mut((xx, bottom)) {
                cell.set_char(BORDER_HORIZONTAL);
                cell.set_style(border_style);
            }
        }
        for (xx, yy, ch) in [
            (rect.x, rect.y, CORNER_TOP_LEFT),
            (right, rect.y, CORNER_TOP_RIGHT),
            (rect.x, bottom, CORNER_BOTTOM_LEFT),
            (right, bottom, CORNER_BOTTOM_RIGHT),
        ] {
            if let Some(cell) = buf.cell_mut((xx, yy)) {
                cell.set_char(ch);
                cell.set_style(border_style);
            }
        }

        // Inner content: left-aligned (never centered) with inner padding.
        let content_x = rect.x + 1 + INNER_PADDING;
        let content_w = banner_w.saturating_sub(2 + INNER_PADDING);

        // Row 1: changelog link — or, once installed, the "restart" note.
        let link_y = rect.y + 1;
        if self.status == UpdateStatus::Installed {
            let note = "\u{2713} Update installed \u{2014} restart cosh to apply";
            draw_text_line(
                buf,
                note,
                content_x,
                link_y,
                content_w,
                Style::default().fg(success_fg),
            );
        } else {
            let link_w = Self::text_width(link_text);
            self.link_area = Some(Rect::new(content_x, link_y, link_w, 1));
            draw_text_line(
                buf,
                link_text,
                content_x,
                link_y,
                content_w,
                // Same white as the border/title (user request): the link
                // no longer uses the markdown-link color.
                Style::default().fg(white),
            );
        }

        // Row 2: the update button — colored text only (no fill; proper
        // personalization comes later), left-aligned with the link.
        let button_y = rect.y + 2;
        match self.status {
            UpdateStatus::Idle | UpdateStatus::Failed => {
                self.button_area = Some(Rect::new(content_x, button_y, button_w, 1));
                draw_text_line(
                    buf,
                    button_label,
                    content_x,
                    button_y,
                    content_w,
                    Style::default().fg(primary),
                );
            }
            UpdateStatus::Updating => {
                let label = "Updating\u{2026}";
                draw_text_line(
                    buf,
                    label,
                    content_x,
                    button_y,
                    content_w,
                    Style::default().fg(muted),
                );
            }
            UpdateStatus::Installed => {}
        }
    }
}
