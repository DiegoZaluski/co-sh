use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::RGBA;
use crate::core::types::TextAttributes;

static NEXT_TEXT_RENDERABLE_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
}

#[derive(Debug, Clone)]
pub struct StyledText {
    pub chunks: Vec<TextChunk>,
}

#[must_use]
pub fn string_to_styled_text(content: &str) -> StyledText {
    StyledText {
        chunks: vec![TextChunk {
            text: content.to_string(),
            fg: None,
            bg: None,
            attributes: 0,
        }],
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WrapMode {
    None,
    Char,
    Word,
}

pub struct TextRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    // TextBufferRenderable properties
    default_fg: RGBA,
    default_bg: RGBA,
    default_attributes: u32,
    #[allow(dead_code)]
    selection_bg: Option<RGBA>,
    #[allow(dead_code)]
    selection_fg: Option<RGBA>,
    selectable: bool,
    wrap_mode: WrapMode,
    scroll_x: i32,
    scroll_y: i32,
    truncate: bool,
    #[allow(dead_code)]
    first_line_offset: i32,

    // Text-specific properties
    text: StyledText,
    has_manual_styled_text: bool,
}

impl TextRenderable {
    #[must_use]
    pub fn new(content: Option<StyledText>) -> Self {
        let num = NEXT_TEXT_RENDERABLE_NUM.fetch_add(1, Ordering::Relaxed);
        let text = content.unwrap_or_else(|| string_to_styled_text(""));

        TextRenderable {
            id: format!("text-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),

            default_fg: RGBA::from_ints(255, 255, 255, 255),
            default_bg: RGBA::from_ints(0, 0, 0, 0),
            default_attributes: 0,
            selection_bg: None,
            selection_fg: None,
            selectable: true,
            wrap_mode: WrapMode::Word,
            scroll_x: 0,
            scroll_y: 0,
            truncate: false,
            first_line_offset: 0,

            text,
            has_manual_styled_text: false,
        }
    }

    pub fn content(&self) -> &StyledText {
        &self.text
    }

    pub fn set_content(&mut self, value: StyledText) {
        self.has_manual_styled_text = true;
        self.text = value;
    }

    pub fn chunks(&self) -> &[TextChunk] {
        &self.text.chunks
    }

    pub fn fg(&self) -> RGBA {
        self.default_fg
    }

    pub fn set_fg(&mut self, value: RGBA) {
        if self.default_fg != value {
            self.default_fg = value;
        }
    }

    pub fn bg(&self) -> RGBA {
        self.default_bg
    }

    pub fn set_bg(&mut self, value: RGBA) {
        if self.default_bg != value {
            self.default_bg = value;
        }
    }

    pub fn attributes(&self) -> u32 {
        self.default_attributes
    }

    pub fn set_attributes(&mut self, value: u32) {
        self.default_attributes = value;
    }

    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap_mode
    }

    pub fn set_wrap_mode(&mut self, value: WrapMode) {
        self.wrap_mode = value;
    }

    pub fn scroll_y(&self) -> i32 {
        self.scroll_y
    }

    pub fn set_scroll_y(&mut self, value: i32) {
        self.scroll_y = value.max(0);
    }

    pub fn scroll_x(&self) -> i32 {
        self.scroll_x
    }

    pub fn set_scroll_x(&mut self, value: i32) {
        self.scroll_x = value.max(0);
    }

    pub fn truncate(&self) -> bool {
        self.truncate
    }

    pub fn set_truncate(&mut self, value: bool) {
        self.truncate = value;
    }

    pub fn set_selectable(&mut self, value: bool) {
        self.selectable = value;
    }

    pub fn clear(&mut self) {
        self.text = string_to_styled_text("");
        self.has_manual_styled_text = false;
        self.children.clear();
    }

    pub fn add_child_renderable(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }

    pub fn remove_child_renderable(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }
}

impl Renderable for TextRenderable {
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
        let x = area.x;
        let y = area.y;
        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        let mut current_line = y;
        let mut current_col = x;

        for chunk in &self.text.chunks {
            let fg_color = chunk
                .fg
                .unwrap_or(self.default_fg);
            let bg_color = chunk
                .bg
                .unwrap_or(self.default_bg);

            let (fr, fg, fb, fa) = fg_color.to_ints();
            let (br, bg, bb, ba) = bg_color.to_ints();

            let ratatui_fg = if fa == 0 {
                Color::Reset
            } else {
                Color::Rgb(fr, fg, fb)
            };
            let ratatui_bg = if ba == 0 {
                Color::Reset
            } else {
                Color::Rgb(br, bg, bb)
            };

            let attrs = chunk.attributes | self.default_attributes;
            let mut modifier = Modifier::empty();
            if attrs & TextAttributes::BOLD.bits() != 0 {
                modifier |= Modifier::BOLD;
            }
            if attrs & TextAttributes::DIM.bits() != 0 {
                modifier |= Modifier::DIM;
            }
            if attrs & TextAttributes::ITALIC.bits() != 0 {
                modifier |= Modifier::ITALIC;
            }
            if attrs & TextAttributes::UNDERLINE.bits() != 0 {
                modifier |= Modifier::UNDERLINED;
            }
            if attrs & TextAttributes::BLINK.bits() != 0 {
                modifier |= Modifier::SLOW_BLINK;
            }
            if attrs & TextAttributes::INVERSE.bits() != 0 {
                modifier |= Modifier::REVERSED;
            }
            if attrs & TextAttributes::HIDDEN.bits() != 0 {
                modifier |= Modifier::HIDDEN;
            }
            if attrs & TextAttributes::STRIKETHROUGH.bits() != 0 {
                modifier |= Modifier::CROSSED_OUT;
            }

            let style = Style::default()
                .fg(ratatui_fg)
                .bg(ratatui_bg)
                .add_modifier(modifier);

            for ch in chunk.text.chars() {
                if current_col >= max_x {
                    current_col = x;
                    current_line += 1;
                }
                if current_line >= max_y {
                    break;
                }

                if let Some(cell) = buf.cell_mut((current_col, current_line)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }

                current_col += 1;
            }
        }
    }
}
