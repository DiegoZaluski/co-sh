use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_ASCII_FONT_NUM: AtomicU64 = AtomicU64::new(1);

pub struct ASCIIFontRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    text: String,
    font: String,
    color: Vec<RGBA>,
    background_color: RGBA,
}

impl ASCIIFontRenderable {
    #[must_use]
    pub fn new(text: Option<String>, font: Option<String>, color: Option<ColorInput>) -> Self {
        let num = NEXT_ASCII_FONT_NUM.fetch_add(1, Ordering::Relaxed);
        let t = text.unwrap_or_default();
        let f = font.unwrap_or_else(|| "tiny".to_string());
        let c = match color {
            Some(ColorInput::RGBA(rgba)) => vec![rgba],
            Some(ColorInput::String(s)) => vec![parse_color(ColorInput::String(s))],
            None => vec![RGBA::from_ints(255, 255, 255, 255)],
        };

        Self {
            id: format!("asciifont-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            text: t,
            font: f,
            color: c,
            background_color: RGBA::from_ints(0, 0, 0, 0),
        }
    }

    pub fn set_text(&mut self, value: String) {
        self.text = value;
    }

    pub fn set_font(&mut self, value: String) {
        self.font = value;
    }

    pub fn set_color(&mut self, value: ColorInput) {
        self.color = vec![parse_color(value)];
    }

    pub fn set_background_color(&mut self, value: ColorInput) {
        self.background_color = parse_color(value);
    }
}

impl Renderable for ASCIIFontRenderable {
    fn id(&self) -> &str {
        &self.id
    }

    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }

    fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }

    fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        if let Some(pos) = self.children.iter().position(|c| c.id() == anchor_id) {
            self.children.insert(pos, child);
            Some(pos)
        } else {
            None
        }
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

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let fg = self
            .color
            .first()
            .copied()
            .unwrap_or(RGBA::from_ints(255, 255, 255, 255));
        let (fr, fg_c, fb, fa) = fg.to_ints();
        let (br, bg, bb, ba) = self.background_color.to_ints();

        let ratatui_fg = if fa == 0 {
            Color::Reset
        } else {
            Color::Rgb(fr, fg_c, fb)
        };
        let ratatui_bg = if ba == 0 {
            Color::Reset
        } else {
            Color::Rgb(br, bg, bb)
        };

        let style = Style::default().fg(ratatui_fg).bg(ratatui_bg);

        for (i, ch) in self.text.char_indices() {
            let x = area.x + i as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
    }
}
