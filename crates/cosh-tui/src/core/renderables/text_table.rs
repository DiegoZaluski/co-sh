use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::border::{BorderCharacters, BorderStyle, border_chars};
use crate::core::lib::styled_text::TextChunk;
use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_TABLE_NUM: AtomicU64 = AtomicU64::new(1);

pub type TextTableCellContent = Vec<TextChunk>;
pub type TextTableContent = Vec<Vec<TextTableCellContent>>;

pub struct TextTableRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    content: TextTableContent,
    column_widths: Vec<u16>,
    padding_x: u16,
    padding_y: u16,
    border: bool,
    border_style: BorderStyle,
    border_color: RGBA,
    default_fg: RGBA,
    default_bg: RGBA,
}

impl TextTableRenderable {
    #[must_use]
    pub fn new(content: Option<TextTableContent>) -> Self {
        let num = NEXT_TABLE_NUM.fetch_add(1, Ordering::Relaxed);
        let c = content.unwrap_or_default();
        Self {
            id: format!("table-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            column_widths: Vec::new(),
            content: c,
            padding_x: 1,
            padding_y: 0,
            border: true,
            border_style: BorderStyle::Single,
            border_color: RGBA::from_ints(136, 136, 136, 255),
            default_fg: RGBA::from_ints(255, 255, 255, 255),
            default_bg: RGBA::from_ints(0, 0, 0, 0),
        }
    }

    pub fn set_content(&mut self, value: TextTableContent) {
        self.content = value;
    }

    pub const fn set_border(&mut self, value: bool) {
        self.border = value;
    }

    pub const fn set_border_style(&mut self, value: BorderStyle) {
        self.border_style = value;
    }

    pub fn set_border_color(&mut self, value: ColorInput) {
        self.border_color = parse_color(value);
    }

    pub const fn set_padding_x(&mut self, value: u16) {
        self.padding_x = value;
    }

    pub fn set_default_fg(&mut self, value: ColorInput) {
        self.default_fg = parse_color(value);
    }

    pub fn set_default_bg(&mut self, value: ColorInput) {
        self.default_bg = parse_color(value);
    }

    const fn chars_for_style(&self) -> &'static BorderCharacters {
        border_chars(self.border_style)
    }

    fn max_col_widths(&self) -> Vec<u16> {
        let mut widths: Vec<u16> = Vec::new();
        for row in &self.content {
            for (col_idx, cell) in row.iter().enumerate() {
                let cell_width: u16 = cell
                    .iter()
                    .map(|c| c.text.chars().count() as u16)
                    .max()
                    .unwrap_or(0);
                while widths.len() <= col_idx {
                    widths.push(0);
                }
                widths[col_idx] = widths[col_idx].max(cell_width);
            }
        }
        widths
    }

    const fn row_count(&self) -> usize {
        self.content.len()
    }

    fn col_count(&self) -> usize {
        self.content.first().map_or(0, |r| r.len())
    }
}

impl Renderable for TextTableRenderable {
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

    #[allow(clippy::too_many_lines)]
    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let col_widths = self.max_col_widths();
        if col_widths.is_empty() || self.row_count() == 0 {
            return;
        }

        let col_count = col_widths.len();
        let row_count = self.row_count();
        let borders = self.border;
        let chars = self.chars_for_style();

        let (br, bg, bb, _ba) = self.border_color.to_ints();
        let bstyle = Style::default().fg(Color::Rgb(br, bg, bb));

        let cell_fg = self.default_fg;

        let cell_w_total: Vec<u16> = col_widths.iter().map(|&w| w + 2 * self.padding_x).collect();

        let col_start: Vec<u16> = {
            let mut x = area.x;
            let mut starts = Vec::with_capacity(col_count);
            for &cw in &cell_w_total {
                starts.push(x);
                x += cw;
                if borders {
                    x += 1;
                }
            }
            starts
        };

        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        // top border
        let mut y = area.y;
        if borders && y < max_y {
            for ci in 0..col_count {
                let sx = col_start[ci];
                let cw = cell_w_total[ci];
                let ex = sx + cw;
                let corner_l = if ci == 0 { chars.top_left } else { chars.top_t };
                for cx in sx..ex {
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(chars.horizontal);
                        cell.set_style(bstyle);
                    }
                }
                if let Some(cell) = buf.cell_mut((ex.min(max_x.saturating_sub(1)), y)) {
                    if ci + 1 < col_count {
                        cell.set_char(chars.top_t);
                    } else {
                        cell.set_char(chars.top_right);
                    }
                    cell.set_style(bstyle);
                }
                if let Some(cell) = buf.cell_mut((sx.saturating_sub(1), y)) {
                    cell.set_char(corner_l);
                    cell.set_style(bstyle);
                }
            }
            y += 1;
        }

        for ri in 0..row_count {
            if y >= max_y {
                break;
            }

            let row = &self.content[ri];

            // render content row(s)
            for py in 0..=self.padding_y {
                if y >= max_y {
                    break;
                }
                if borders && let Some(cell) = buf.cell_mut((col_start[0].saturating_sub(1), y)) {
                    cell.set_char(chars.vertical);
                    cell.set_style(bstyle);
                }
                for ci in 0..col_count {
                    let sx = col_start[ci];
                    let cw = cell_w_total[ci];
                    let ex = sx + cw;

                    if py == 0 {
                        let chunks = row.get(ci);
                        let mut cx = sx + self.padding_x;
                        if let Some(chunks) = chunks {
                            for chunk in chunks {
                                let (fr, fgr, fb, fa) = chunk.fg.unwrap_or(cell_fg).to_ints();
                                let st = Style::default().fg(if fa == 0 {
                                    Color::Reset
                                } else {
                                    Color::Rgb(fr, fgr, fb)
                                });
                                for ch in chunk.text.chars() {
                                    if cx >= ex {
                                        break;
                                    }
                                    if let Some(cell) = buf.cell_mut((cx, y)) {
                                        cell.set_char(ch);
                                        cell.set_style(st);
                                    }
                                    cx += 1;
                                }
                            }
                        }
                    }

                    if borders {
                        let ex_clamped = ex.min(max_x.saturating_sub(1));
                        if let Some(cell) = buf.cell_mut((ex_clamped, y)) {
                            cell.set_char(chars.vertical);
                            cell.set_style(bstyle);
                        }
                    }
                }
                y += 1;
            }

            // bottom border / separator
            if borders && y < max_y {
                for ci in 0..col_count {
                    let sx = col_start[ci];
                    let cw = cell_w_total[ci];
                    let ex = sx + cw;
                    let corner_l = if ci == 0 {
                        if ri + 1 == row_count {
                            chars.bottom_left
                        } else {
                            chars.left_t
                        }
                    } else if ri + 1 == row_count {
                        chars.bottom_t
                    } else {
                        chars.cross
                    };
                    for cx in sx..ex {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_char(chars.horizontal);
                            cell.set_style(bstyle);
                        }
                    }
                    if let Some(cell) = buf.cell_mut((ex.min(max_x.saturating_sub(1)), y)) {
                        let corner_r = if ci + 1 < col_count {
                            if ri + 1 == row_count {
                                chars.bottom_t
                            } else {
                                chars.right_t
                            }
                        } else if ri + 1 == row_count {
                            chars.bottom_right
                        } else {
                            chars.right_t
                        };
                        cell.set_char(corner_r);
                        cell.set_style(bstyle);
                    }
                    if let Some(cell) = buf.cell_mut((sx.saturating_sub(1), y)) {
                        cell.set_char(corner_l);
                        cell.set_style(bstyle);
                    }
                }
                y += 1;
            }
        }
    }
}
