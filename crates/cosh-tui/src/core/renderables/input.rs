use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_INPUT_NUM: AtomicU64 = AtomicU64::new(1);

pub struct InputRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    value: String,
    placeholder: String,
    max_length: usize,
    min_length: usize,
    cursor_offset: usize,
    text_color: RGBA,
    background_color: RGBA,
}

impl InputRenderable {
    #[must_use]
    pub fn new(value: Option<String>) -> Self {
        let num = NEXT_INPUT_NUM.fetch_add(1, Ordering::Relaxed);
        InputRenderable {
            id: format!("input-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            value: value.unwrap_or_default(),
            placeholder: String::new(),
            max_length: 1000,
            min_length: 0,
            cursor_offset: 0,
            text_color: RGBA::from_ints(255, 255, 255, 255),
            background_color: RGBA::from_ints(0, 0, 0, 0),
        }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: &str) {
        let sanitized = value.replace(['\n', '\r'], "");
        let truncated: String = sanitized.chars().take(self.max_length).collect();
        self.value = truncated;
        self.cursor_offset = self.value.len();
    }

    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn set_placeholder(&mut self, placeholder: String) {
        self.placeholder = placeholder;
    }

    #[must_use]
    pub fn max_length(&self) -> usize {
        self.max_length
    }

    pub fn set_max_length(&mut self, max: usize) {
        self.max_length = max;
        if self.value.len() > max {
            self.value = self.value.chars().take(max).collect();
            self.cursor_offset = self.cursor_offset.min(self.value.len());
        }
    }

    #[must_use]
    pub fn min_length(&self) -> usize {
        self.min_length
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn set_min_length(&mut self, min: usize) {
        assert!(
            min <= self.max_length,
            "InputRenderable: min_length ({min}) cannot be greater than max_length ({})",
            self.max_length
        );
        self.min_length = min;
    }

    #[must_use]
    pub fn cursor_offset(&self) -> usize {
        self.cursor_offset
    }

    pub fn insert_text(&mut self, text: &str) {
        let sanitized = text.replace(['\n', '\r'], "");
        if sanitized.is_empty() {
            return;
        }
        let remaining = self.max_length - self.value.len();
        if remaining == 0 {
            return;
        }
        let to_insert: String = sanitized.chars().take(remaining).collect();
        self.value.insert_str(self.cursor_offset, &to_insert);
        self.cursor_offset += to_insert.len();
    }

    pub fn delete_char_backward(&mut self) -> bool {
        if self.cursor_offset == 0 || self.value.is_empty() {
            return false;
        }
        let bytes = self.cursor_offset;
        if !self.value.is_char_boundary(bytes) {
            return false;
        }
        let prev = self.value.floor_char_boundary(bytes - 1);
        self.value.drain(prev..bytes);
        self.cursor_offset = prev;
        true
    }

    pub fn delete_char(&mut self) -> bool {
        if self.cursor_offset >= self.value.len() {
            return false;
        }
        let bytes = self.cursor_offset;
        let next = self
            .value
            .floor_char_boundary((bytes + 1).min(self.value.len()));
        self.value.drain(bytes..next);
        true
    }

    pub fn set_text_color(&mut self, color: Option<ColorInput>) {
        self.text_color = parse_color(color.unwrap_or(ColorInput::String("#FFFFFF".into())));
    }

    pub fn set_background_color(&mut self, color: Option<ColorInput>) {
        self.background_color =
            parse_color(color.unwrap_or(ColorInput::String("transparent".into())));
    }
}

impl Renderable for InputRenderable {
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

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let display = if self.value.is_empty() && !self.placeholder.is_empty() {
            &self.placeholder
        } else {
            &self.value
        };

        let (tr, tg, tb, ta) = self.text_color.to_ints();
        let fg = if ta == 0 {
            Color::Reset
        } else {
            Color::Rgb(tr, tg, tb)
        };
        let style = Style::default().fg(fg);

        let max_x = area.right();
        let mut x = area.x;

        for ch in display.chars() {
            if x >= max_x {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            x += 1;
        }
    }
}
