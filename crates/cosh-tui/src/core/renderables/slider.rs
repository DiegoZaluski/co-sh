use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{parse_color, ColorInput, RGBA};

static NEXT_SLIDER_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliderOrientation {
    Horizontal,
    Vertical,
}

pub struct SliderRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    orientation: SliderOrientation,
    value: f64,
    min: f64,
    max: f64,
    view_port_size: f64,
    track_color: RGBA,
    thumb_color: RGBA,
}

impl SliderRenderable {
    #[must_use]
    pub fn new(orientation: SliderOrientation) -> Self {
        let num = NEXT_SLIDER_NUM.fetch_add(1, Ordering::Relaxed);
        SliderRenderable {
            id: format!("slider-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            orientation,
            value: 0.0,
            min: 0.0,
            max: 100.0,
            view_port_size: 10.0,
            track_color: RGBA::from_ints(37, 37, 39, 255),
            thumb_color: RGBA::from_ints(154, 158, 163, 255),
        }
    }

    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn set_value(&mut self, new_value: f64) {
        let clamped = new_value.clamp(self.min, self.max);
        if (clamped - self.value).abs() > f64::EPSILON {
            self.value = clamped;
        }
    }

    #[must_use]
    pub fn min(&self) -> f64 {
        self.min
    }

    pub fn set_min(&mut self, new_min: f64) {
        self.min = new_min;
        if self.value < new_min {
            self.value = new_min;
        }
    }

    #[must_use]
    pub fn max(&self) -> f64 {
        self.max
    }

    pub fn set_max(&mut self, new_max: f64) {
        self.max = new_max;
        if self.value > new_max {
            self.value = new_max;
        }
    }

    #[must_use]
    pub fn orientation(&self) -> SliderOrientation {
        self.orientation
    }

    pub fn set_orientation(&mut self, orientation: SliderOrientation) {
        self.orientation = orientation;
    }

    #[must_use]
    pub fn view_port_size(&self) -> f64 {
        self.view_port_size
    }

    pub fn set_view_port_size(&mut self, size: f64) {
        let range = self.max - self.min;
        self.view_port_size = size.clamp(0.01, if range > 0.0 { range } else { f64::MAX });
    }

    pub fn set_track_color(&mut self, color: Option<ColorInput>) {
        self.track_color = parse_color(color.unwrap_or(ColorInput::String("#252527".into())));
    }

    pub fn set_thumb_color(&mut self, color: Option<ColorInput>) {
        self.thumb_color = parse_color(color.unwrap_or(ColorInput::String("#9a9ea3".into())));
    }

    #[must_use]
    pub fn track_color(&self) -> RGBA {
        self.track_color
    }

    #[must_use]
    pub fn thumb_color(&self) -> RGBA {
        self.thumb_color
    }

    fn virtual_track_size(&self, render_size: u16) -> f64 {
        f64::from(render_size) * 2.0
    }

    fn virtual_thumb_size(&self, render_size: u16) -> f64 {
        let virtual_track = self.virtual_track_size(render_size);
        let range = self.max - self.min;
        if range <= 0.0 {
            return virtual_track;
        }
        let vp = self.view_port_size.max(1.0);
        let content_size = range + vp;
        if content_size <= vp {
            return virtual_track;
        }
        let ratio = vp / content_size;
        (virtual_track * ratio).max(1.0).min(virtual_track)
    }

    fn virtual_thumb_start(&self, render_size: u16) -> f64 {
        let virtual_track = self.virtual_track_size(render_size);
        let range = self.max - self.min;
        if range <= 0.0 {
            return 0.0;
        }
        let ratio = (self.value - self.min) / range;
        let thumb_size = self.virtual_thumb_size(render_size);
        (ratio * (virtual_track - thumb_size)).round()
    }
}

impl Renderable for SliderRenderable {
    fn id(&self) -> &str {
        &self.id
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

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        &self.children
    }

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let (tr, tg, tb, ta) = self.track_color.to_ints();
        let track_style = if ta == 0 {
            Style::default()
        } else {
            Style::default().bg(Color::Rgb(tr, tg, tb))
        };

        match self.orientation {
            SliderOrientation::Horizontal => {
                self.render_horizontal(buf, area, track_style);
            }
            SliderOrientation::Vertical => {
                self.render_vertical(buf, area, track_style);
            }
        }
    }
}

impl SliderRenderable {
    fn render_horizontal(&self, buf: &mut Buffer, area: Rect, track_style: Style) {
        let (fr, fg, fb, fa) = self.thumb_color.to_ints();
        let thumb_style = if fa == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Rgb(fr, fg, fb)).bg(Color::Rgb(fr, fg, fb))
        };

        // Fill track background
        for x in area.x..area.right() {
            for y in area.y..area.bottom() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(track_style);
                    cell.set_char(' ');
                }
            }
        }

        let render_size = area.width;
        if render_size == 0 {
            return;
        }

        let virtual_thumb_size = self.virtual_thumb_size(render_size);
        let virtual_thumb_start = self.virtual_thumb_start(render_size);
        let virtual_thumb_end = virtual_thumb_start + virtual_thumb_size;

        let start_cell = (virtual_thumb_start / 2.0).floor() as i32;
        let end_cell = (virtual_thumb_end / 2.0).ceil() as i32 - 1;
        let start_cell = start_cell.max(0);
        let end_cell = end_cell.min(area.width as i32 - 1);

        for real_x in start_cell..=end_cell {
            let virtual_cell_start = f64::from(real_x) * 2.0;
            let virtual_cell_end = virtual_cell_start + 2.0;

            let thumb_in_cell_start = virtual_thumb_start.max(virtual_cell_start);
            let thumb_in_cell_end = virtual_thumb_end.min(virtual_cell_end);
            let coverage = thumb_in_cell_end - thumb_in_cell_start;

            let ch = if coverage >= 2.0 {
                '█'
            } else {
                let is_left_half = (thumb_in_cell_start - virtual_cell_start).abs() < f64::EPSILON;
                if is_left_half {
                    '▌'
                } else {
                    '▐'
                }
            };

            let x = area.x + real_x as u16;
            for y in area.y..area.bottom() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(thumb_style);
                }
            }
        }
    }

    fn render_vertical(&self, buf: &mut Buffer, area: Rect, track_style: Style) {
        let (fr, fg, fb, fa) = self.thumb_color.to_ints();
        let thumb_style = if fa == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Rgb(fr, fg, fb)).bg(Color::Rgb(fr, fg, fb))
        };

        // Fill track background
        for x in area.x..area.right() {
            for y in area.y..area.bottom() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(track_style);
                    cell.set_char(' ');
                }
            }
        }

        let render_size = area.height;
        if render_size == 0 {
            return;
        }

        let virtual_thumb_size = self.virtual_thumb_size(render_size);
        let virtual_thumb_start = self.virtual_thumb_start(render_size);
        let virtual_thumb_end = virtual_thumb_start + virtual_thumb_size;

        let start_cell = (virtual_thumb_start / 2.0).floor() as i32;
        let end_cell = (virtual_thumb_end / 2.0).ceil() as i32 - 1;
        let start_cell = start_cell.max(0);
        let end_cell = end_cell.min(area.height as i32 - 1);

        for real_y in start_cell..=end_cell {
            let virtual_cell_start = f64::from(real_y) * 2.0;
            let virtual_cell_end = virtual_cell_start + 2.0;

            let thumb_in_cell_start = virtual_thumb_start.max(virtual_cell_start);
            let thumb_in_cell_end = virtual_thumb_end.min(virtual_cell_end);
            let coverage = thumb_in_cell_end - thumb_in_cell_start;

            let ch = if coverage >= 2.0 {
                '█'
            } else if coverage > 0.0 {
                let virtual_pos_in_cell = thumb_in_cell_start - virtual_cell_start;
                if virtual_pos_in_cell < 1.0 {
                    '▀'
                } else {
                    '▄'
                }
            } else {
                ' '
            };

            let y = area.y + real_y as u16;
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(thumb_style);
                }
            }
        }
    }
}
