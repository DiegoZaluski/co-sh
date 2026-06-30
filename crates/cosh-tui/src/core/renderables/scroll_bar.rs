use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_SCROLL_BAR_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollBarOrientation {
    Vertical,
    Horizontal,
}

pub struct ScrollBarRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    orientation: ScrollBarOrientation,
    scroll_size: f64,
    scroll_position: f64,
    viewport_size: f64,
    show_arrows: bool,
    manual_visibility: bool,

    track_color: RGBA,
    thumb_color: RGBA,
    arrow_color: RGBA,
}

impl ScrollBarRenderable {
    #[must_use]
    pub fn new(orientation: ScrollBarOrientation) -> Self {
        let num = NEXT_SCROLL_BAR_NUM.fetch_add(1, Ordering::Relaxed);
        ScrollBarRenderable {
            id: format!("scrollbar-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            orientation,
            scroll_size: 0.0,
            scroll_position: 0.0,
            viewport_size: 10.0,
            show_arrows: false,
            manual_visibility: false,
            track_color: RGBA::from_ints(37, 37, 39, 255),
            thumb_color: RGBA::from_ints(154, 158, 163, 255),
            arrow_color: RGBA::from_ints(154, 158, 163, 255),
        }
    }

    #[must_use]
    pub fn scroll_position(&self) -> f64 {
        self.scroll_position
    }

    pub fn set_scroll_position(&mut self, value: f64) {
        let max_pos = (self.scroll_size - self.viewport_size).max(0.0);
        self.scroll_position = value.clamp(0.0, max_pos);
    }

    #[must_use]
    pub fn scroll_size(&self) -> f64 {
        self.scroll_size
    }

    pub fn set_scroll_size(&mut self, value: f64) {
        self.scroll_size = value;
        self.set_scroll_position(self.scroll_position);
    }

    #[must_use]
    pub fn viewport_size(&self) -> f64 {
        self.viewport_size
    }

    pub fn set_viewport_size(&mut self, value: f64) {
        self.viewport_size = value.max(1.0);
        self.set_scroll_position(self.scroll_position);
    }

    pub fn set_show_arrows(&mut self, show: bool) {
        self.show_arrows = show;
    }

    pub fn scroll_by(&mut self, delta: f64, relative: bool) {
        let step = if relative {
            delta * self.viewport_size
        } else {
            delta
        };
        self.set_scroll_position(self.scroll_position + step);
    }

    pub fn set_track_color(&mut self, color: Option<ColorInput>) {
        self.track_color = parse_color(color.unwrap_or(ColorInput::String("#252527".into())));
    }

    pub fn set_thumb_color(&mut self, color: Option<ColorInput>) {
        self.thumb_color = parse_color(color.unwrap_or(ColorInput::String("#9a9ea3".into())));
    }

    fn ratio(&self) -> f64 {
        let range = self.scroll_size - self.viewport_size;
        if range <= 0.0 {
            return 0.0;
        }
        (self.scroll_position / range).clamp(0.0, 1.0)
    }

    #[allow(clippy::cast_sign_loss)]
    fn thumb_size(&self, render_size: u16) -> u16 {
        let rs = f64::from(render_size);
        if self.scroll_size <= self.viewport_size {
            return render_size;
        }
        let r = self.viewport_size / self.scroll_size;
        (rs * r).round().clamp(1.0, rs) as u16
    }

    #[allow(clippy::cast_sign_loss)]
    fn thumb_start(&self, render_size: u16) -> u16 {
        let rs = f64::from(render_size);
        let ts = f64::from(self.thumb_size(render_size));
        let available = rs - ts;
        if available <= 0.0 {
            return 0;
        }
        (self.ratio() * available).round() as u16
    }
}

impl Renderable for ScrollBarRenderable {
    fn id(&self) -> &str {
        &self.id
    }

    fn add_child(&mut self, child: Box<dyn crate::core::renderable::Renderable>) -> usize {
        crate::core::renderable::adopt_child(self.num, &mut self.children, child)
    }
    fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }
    fn insert_child_before(
        &mut self,
        child: Box<dyn crate::core::renderable::Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        crate::core::renderable::adopt_child_before(self.num, &mut self.children, child, anchor_id)
    }

    fn num(&self) -> u64 {
        self.num
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focusable(&self) -> bool {
        self.focusable
    }

    fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    fn parent_num(&self) -> Option<u64> {
        self.parent_num
    }

    fn set_parent_num(&mut self, parent_num: Option<u64>) {
        self.parent_num = parent_num;
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        &self.children
    }

    #[allow(clippy::too_many_lines)]
    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let (ttr, ttg, ttb, tta) = self.track_color.to_ints();
        let track_style = if tta == 0 {
            Style::default()
        } else {
            Style::default().bg(Color::Rgb(ttr, ttg, ttb))
        };

        let (tr, tg, tb, ta) = self.thumb_color.to_ints();
        let thumb_fg = if ta == 0 {
            Color::Reset
        } else {
            Color::Rgb(tr, tg, tb)
        };
        let thumb_style = Style::default().fg(thumb_fg).bg(thumb_fg);

        match self.orientation {
            ScrollBarOrientation::Vertical => {
                // Fill track
                for y in area.y..area.bottom() {
                    if let Some(cell) = buf.cell_mut((area.x, y)) {
                        cell.set_style(track_style);
                        cell.set_char(' ');
                    }
                }

                let render_size = area.height;
                let thumb_sz = self.thumb_size(render_size);
                let thumb_start = self.thumb_start(render_size);

                // Draw thumb
                for y in 0..thumb_sz {
                    let ty = area.y + thumb_start + y;
                    if ty >= area.bottom() {
                        break;
                    }
                    let ch = if thumb_sz <= 1 {
                        '■'
                    } else if y == 0 {
                        '▲'
                    } else if y == thumb_sz - 1 {
                        '▼'
                    } else {
                        '█'
                    };
                    if let Some(cell) = buf.cell_mut((area.x, ty)) {
                        cell.set_char(ch);
                        cell.set_style(thumb_style);
                    }
                }

                // Draw arrows
                if self.show_arrows {
                    let (ar, ag, ab, aa) = self.arrow_color.to_ints();
                    let arrow_style = if aa == 0 {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Rgb(ar, ag, ab))
                    };
                    if let Some(cell) = buf.cell_mut((area.x, area.y)) {
                        cell.set_char('▲');
                        cell.set_style(arrow_style.patch(track_style));
                    }
                    if let Some(cell) = buf.cell_mut((area.x, area.bottom() - 1)) {
                        cell.set_char('▼');
                        cell.set_style(arrow_style.patch(track_style));
                    }
                }
            }
            ScrollBarOrientation::Horizontal => {
                // Fill track
                for x in area.x..area.right() {
                    if let Some(cell) = buf.cell_mut((x, area.y)) {
                        cell.set_style(track_style);
                        cell.set_char(' ');
                    }
                }

                let render_size = area.width;
                let thumb_sz = self.thumb_size(render_size);
                let thumb_start = self.thumb_start(render_size);

                // Draw thumb
                for x in 0..thumb_sz {
                    let tx = area.x + thumb_start + x;
                    if tx >= area.right() {
                        break;
                    }
                    let ch = if thumb_sz <= 1 {
                        '■'
                    } else if x == 0 {
                        '◄'
                    } else if x == thumb_sz - 1 {
                        '►'
                    } else {
                        '█'
                    };
                    if let Some(cell) = buf.cell_mut((tx, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(thumb_style);
                    }
                }

                // Draw arrows
                if self.show_arrows {
                    let (ar, ag, ab, aa) = self.arrow_color.to_ints();
                    let arrow_style = if aa == 0 {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Rgb(ar, ag, ab))
                    };
                    if let Some(cell) = buf.cell_mut((area.x, area.y)) {
                        cell.set_char('◄');
                        cell.set_style(arrow_style.patch(track_style));
                    }
                    if let Some(cell) = buf.cell_mut((area.right() - 1, area.y)) {
                        cell.set_char('►');
                        cell.set_style(arrow_style.patch(track_style));
                    }
                }
            }
        }
    }
}
