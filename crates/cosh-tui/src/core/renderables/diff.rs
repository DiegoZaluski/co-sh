use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::RGBA;

static NEXT_DIFF_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq)]
enum DiffLineType {
    Context,
    Add,
    Remove,
    Header,
}

pub struct DiffRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    diff: String,
    added_bg: RGBA,
    removed_bg: RGBA,
    added_sign_color: RGBA,
    removed_sign_color: RGBA,
}

impl DiffRenderable {
    #[must_use]
    pub fn new(diff: Option<String>) -> Self {
        let num = NEXT_DIFF_NUM.fetch_add(1, Ordering::Relaxed);
        Self {
            id: format!("diff-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            diff: diff.unwrap_or_default(),
            added_bg: RGBA::from_ints(22, 55, 22, 255),
            removed_bg: RGBA::from_ints(55, 22, 22, 255),
            added_sign_color: RGBA::from_ints(100, 200, 100, 255),
            removed_sign_color: RGBA::from_ints(200, 100, 100, 255),
        }
    }

    pub fn set_diff(&mut self, value: String) {
        self.diff = value;
    }

    fn parse_line_type(line: &str) -> DiffLineType {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            DiffLineType::Header
        } else if line.starts_with('+') {
            DiffLineType::Add
        } else if line.starts_with('-') {
            DiffLineType::Remove
        } else {
            DiffLineType::Context
        }
    }

    fn line_style(&self, line_type: DiffLineType) -> (Style, Style) {
        match line_type {
            DiffLineType::Add => {
                let (r, g, b, _a) = self.added_bg.to_ints();
                let (sr, sg, sb, _sa) = self.added_sign_color.to_ints();
                (
                    Style::default()
                        .bg(Color::Rgb(r, g, b))
                        .fg(Color::Rgb(sr, sg, sb)),
                    Style::default().bg(Color::Rgb(r, g, b)),
                )
            }
            DiffLineType::Remove => {
                let (r, g, b, _a) = self.removed_bg.to_ints();
                let (sr, sg, sb, _sa) = self.removed_sign_color.to_ints();
                (
                    Style::default()
                        .bg(Color::Rgb(r, g, b))
                        .fg(Color::Rgb(sr, sg, sb)),
                    Style::default().bg(Color::Rgb(r, g, b)),
                )
            }
            DiffLineType::Header => (
                Style::default()
                    .fg(Color::Rgb(100, 150, 255))
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(Color::Rgb(100, 150, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            DiffLineType::Context => (Style::default(), Style::default()),
        }
    }
}

impl Renderable for DiffRenderable {
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
        let mut y = area.y;
        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        for line in self.diff.lines() {
            if y >= max_y {
                break;
            }
            let line_type = Self::parse_line_type(line);
            let (sign_style, content_style) = self.line_style(line_type);

            let mut x = area.x;

            // sign column
            let sign = if line_type == DiffLineType::Add {
                '+'
            } else if line_type == DiffLineType::Remove {
                '-'
            } else {
                ' '
            };
            if x < max_x {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(sign);
                    cell.set_style(sign_style);
                }
                x += 1;
            }

            // content
            let content = if line.is_empty() { line } else { &line[1..] };
            for ch in content.chars() {
                if x >= max_x {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(content_style);
                }
                x += 1;
            }

            y += 1;
        }
    }
}
