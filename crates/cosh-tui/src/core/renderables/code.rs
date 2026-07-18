use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};
use crate::core::syntax_style::SyntaxStyle;

static NEXT_CODE_RENDERABLE_NUM: AtomicU64 = AtomicU64::new(1);

pub struct CodeRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    content: String,
    filetype: String,
    syntax_style: Option<SyntaxStyle>,
    fg: Option<RGBA>,
    bg: Option<RGBA>,
    conceal: bool,
    wrap_mode: WrapMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    None,
    Char,
    Word,
}

impl CodeRenderable {
    #[must_use]
    pub fn new(content: Option<String>, filetype: Option<String>) -> Self {
        let num = NEXT_CODE_RENDERABLE_NUM.fetch_add(1, Ordering::Relaxed);
        Self {
            id: format!("code-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            content: content.unwrap_or_default(),
            filetype: filetype.unwrap_or_default(),
            syntax_style: None,
            fg: None,
            bg: None,
            conceal: true,
            wrap_mode: WrapMode::Word,
        }
    }

    pub fn set_content(&mut self, value: String) {
        self.content = value;
    }

    pub fn set_filetype(&mut self, value: String) {
        self.filetype = value;
    }

    pub fn set_syntax_style(&mut self, value: Option<SyntaxStyle>) {
        self.syntax_style = value;
    }

    pub fn set_fg(&mut self, value: Option<ColorInput>) {
        self.fg = value.map(parse_color);
    }

    pub fn set_bg(&mut self, value: Option<ColorInput>) {
        self.bg = value.map(parse_color);
    }

    pub const fn set_conceal(&mut self, value: bool) {
        self.conceal = value;
    }

    pub const fn set_wrap_mode(&mut self, value: WrapMode) {
        self.wrap_mode = value;
    }
}

impl Renderable for CodeRenderable {
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
        let fg = self.fg.unwrap_or(RGBA::from_ints(200, 200, 200, 255));
        let bg = self.bg.unwrap_or(RGBA::from_ints(30, 30, 30, 0));
        let (fr, fg_c, fb, fa) = fg.to_ints();
        let (br, bg_c, bb, ba) = bg.to_ints();

        let ratatui_fg = if fa == 0 {
            Color::Reset
        } else {
            Color::Rgb(fr, fg_c, fb)
        };
        let ratatui_bg = if ba == 0 {
            Color::Reset
        } else {
            Color::Rgb(br, bg_c, bb)
        };

        let style = Style::default()
            .fg(ratatui_fg)
            .bg(ratatui_bg)
            .add_modifier(Modifier::DIM);

        let mut y = area.y;
        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        for line in self.content.lines() {
            if y >= max_y {
                break;
            }
            let wrapped = crate::core::lib::unicode_util::word_wrap(line, area.width);
            for wl in &wrapped {
                if y >= max_y {
                    break;
                }
                let mut x = area.x;
                for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(wl) {
                    if x + w > max_x {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        if grapheme.len() == 1 {
                            if let Some(c) = grapheme.chars().next() {
                                cell.set_char(c);
                            }
                        } else {
                            cell.set_symbol(grapheme);
                        }
                        cell.set_style(style);
                    }
                    if w > 1 {
                        for dx in 1..w {
                            if let Some(next_cell) = buf.cell_mut((x + dx, y)) {
                                next_cell.set_diff_option(CellDiffOption::Skip);
                            }
                        }
                    }
                    x += w;
                }
                y += 1;
            }
        }
    }
}
