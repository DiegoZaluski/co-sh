use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::border::{BorderCharacters, BorderSidesConfig, BorderStyle, border_chars};
use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_BOX_NUM: AtomicU64 = AtomicU64::new(1);

pub struct BoxRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    background_color: RGBA,
    border: BorderSidesConfig,
    border_style: BorderStyle,
    border_color: RGBA,
    focused_border_color: RGBA,
    custom_border_chars: Option<BorderCharacters>,
    should_fill: bool,
    title: Option<String>,
    title_color: Option<RGBA>,
    title_alignment: TitleAlignment,
    bottom_title: Option<String>,
    bottom_title_alignment: TitleAlignment,
    gap: Option<f32>,
    row_gap: Option<f32>,
    column_gap: Option<f32>,
    layout_node: Option<taffy::NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TitleAlignment {
    Left,
    Center,
    Right,
}

impl BoxRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_BOX_NUM.fetch_add(1, Ordering::Relaxed);
        BoxRenderable {
            id: format!("box-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),

            background_color: parse_color(ColorInput::String("transparent".into())),
            border: BorderSidesConfig::NONE,
            border_style: BorderStyle::Single,
            border_color: RGBA::from_ints(255, 255, 255, 255),
            focused_border_color: RGBA::from_ints(0, 170, 255, 255),
            custom_border_chars: None,
            should_fill: true,
            title: None,
            title_color: None,
            title_alignment: TitleAlignment::Left,
            bottom_title: None,
            bottom_title_alignment: TitleAlignment::Left,
            gap: None,
            row_gap: None,
            column_gap: None,
            layout_node: None,
        }
    }

    pub fn set_background_color(&mut self, color: Option<ColorInput>) {
        self.background_color =
            parse_color(color.unwrap_or(ColorInput::String("transparent".into())));
    }

    #[must_use]
    pub fn background_color(&self) -> RGBA {
        self.background_color
    }

    pub fn set_border(&mut self, enable: bool) {
        if enable {
            self.border = BorderSidesConfig::ALL;
        } else {
            self.border = BorderSidesConfig::NONE;
        }
    }

    #[must_use]
    pub fn border_sides(&self) -> &BorderSidesConfig {
        &self.border
    }

    pub fn set_border_style(&mut self, style: BorderStyle) {
        self.border_style = style;
        self.custom_border_chars = None;
        if !self.border.top {
            self.border = BorderSidesConfig::ALL;
        }
    }

    #[must_use]
    pub fn border_style(&self) -> BorderStyle {
        self.border_style
    }

    pub fn set_border_color(&mut self, color: Option<ColorInput>) {
        self.border_color = parse_color(color.unwrap_or(ColorInput::String("#FFFFFF".into())));
        if !self.border.top {
            self.border = BorderSidesConfig::ALL;
        }
    }

    #[must_use]
    pub fn border_color(&self) -> RGBA {
        self.border_color
    }

    pub fn set_focused_border_color(&mut self, color: Option<ColorInput>) {
        self.focused_border_color =
            parse_color(color.unwrap_or(ColorInput::String("#00AAFF".into())));
    }

    #[must_use]
    pub fn focused_border_color(&self) -> RGBA {
        self.focused_border_color
    }

    pub fn set_should_fill(&mut self, fill: bool) {
        self.should_fill = fill;
    }

    #[must_use]
    pub fn should_fill(&self) -> bool {
        self.should_fill
    }

    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn set_title_color(&mut self, color: Option<ColorInput>) {
        self.title_color = color.map(parse_color);
    }

    #[must_use]
    pub fn title_color(&self) -> Option<RGBA> {
        self.title_color
    }

    pub fn set_title_alignment(&mut self, align: TitleAlignment) {
        self.title_alignment = align;
    }

    #[must_use]
    pub fn title_alignment(&self) -> TitleAlignment {
        self.title_alignment
    }

    pub fn set_bottom_title(&mut self, title: Option<String>) {
        self.bottom_title = title;
    }

    #[must_use]
    pub fn bottom_title(&self) -> Option<&str> {
        self.bottom_title.as_deref()
    }

    pub fn set_bottom_title_alignment(&mut self, align: TitleAlignment) {
        self.bottom_title_alignment = align;
    }

    #[must_use]
    pub fn bottom_title_alignment(&self) -> TitleAlignment {
        self.bottom_title_alignment
    }

    pub fn set_gap(&mut self, value: Option<f32>) {
        self.gap = value;
    }

    #[must_use]
    pub fn gap(&self) -> Option<f32> {
        self.gap
    }

    pub fn set_row_gap(&mut self, value: Option<f32>) {
        self.row_gap = value;
    }

    #[must_use]
    pub fn row_gap(&self) -> Option<f32> {
        self.row_gap
    }

    pub fn set_column_gap(&mut self, value: Option<f32>) {
        self.column_gap = value;
    }

    #[must_use]
    pub fn column_gap(&self) -> Option<f32> {
        self.column_gap
    }

    pub fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        crate::core::renderable::adopt_child(self.num, &mut self.children, child)
    }

    pub fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }

    fn has_border(&self) -> bool {
        self.border.top || self.border.right || self.border.bottom || self.border.left
    }

    fn border_inset(&self) -> (u16, u16, u16, u16) {
        let top = u16::from(self.border.top);
        let bottom = u16::from(self.border.bottom);
        let left = u16::from(self.border.left);
        let right = u16::from(self.border.right);
        (top, right, bottom, left)
    }

    fn render_border(&self, buf: &mut Buffer, area: Rect, chars: &BorderCharacters) {
        let (r, g, b, a) = self.border_color.to_ints();
        let border_style = if a == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Rgb(r, g, b))
        };

        let max_x = area.right().saturating_sub(1);
        let max_y = area.bottom().saturating_sub(1);

        if self.border.top {
            for x in (area.x + 1)..max_x {
                if let Some(cell) = buf.cell_mut((x, area.y)) {
                    cell.set_char(chars.horizontal);
                    cell.set_style(border_style);
                }
            }
        }

        if self.border.bottom {
            for x in (area.x + 1)..max_x {
                if let Some(cell) = buf.cell_mut((x, max_y)) {
                    cell.set_char(chars.horizontal);
                    cell.set_style(border_style);
                }
            }
        }

        if self.border.left {
            for y in (area.y + 1)..max_y {
                if let Some(cell) = buf.cell_mut((area.x, y)) {
                    cell.set_char(chars.vertical);
                    cell.set_style(border_style);
                }
            }
        }

        if self.border.right {
            for y in (area.y + 1)..max_y {
                if let Some(cell) = buf.cell_mut((max_x, y)) {
                    cell.set_char(chars.vertical);
                    cell.set_style(border_style);
                }
            }
        }

        if self.border.top
            && self.border.left
            && let Some(cell) = buf.cell_mut((area.x, area.y))
        {
            cell.set_char(chars.top_left);
            cell.set_style(border_style);
        }
        if self.border.top
            && self.border.right
            && let Some(cell) = buf.cell_mut((max_x, area.y))
        {
            cell.set_char(chars.top_right);
            cell.set_style(border_style);
        }
        if self.border.bottom
            && self.border.left
            && let Some(cell) = buf.cell_mut((area.x, max_y))
        {
            cell.set_char(chars.bottom_left);
            cell.set_style(border_style);
        }
        if self.border.bottom
            && self.border.right
            && let Some(cell) = buf.cell_mut((max_x, max_y))
        {
            cell.set_char(chars.bottom_right);
            cell.set_style(border_style);
        }
    }

    fn render_background(&self, buf: &mut Buffer, area: Rect) {
        let (r, g, b, a) = self.background_color.to_ints();
        if a == 0 {
            return;
        }
        let bg_style = Style::default().bg(Color::Rgb(r, g, b));
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(bg_style);
                }
            }
        }
    }

    fn render_title(&self, buf: &mut Buffer, area: Rect) {
        let (_top_inset, _right_inset, _bottom_inset, left_inset) = self.border_inset();

        if let Some(ref title) = self.title
            && self.border.top
        {
            let (r, g, b, a) = self.title_color.unwrap_or(self.border_color).to_ints();
            let title_style = if a == 0 {
                Style::default()
            } else {
                Style::default().fg(Color::Rgb(r, g, b))
            };

            let available = area.width.saturating_sub(left_inset).saturating_sub(1);
            let y = area.y;
            let x_start = area.x + left_inset + 1;

            let display_text = if title.len() as u16 > available {
                &title[..available as usize]
            } else {
                title
            };

            let x = match self.title_alignment {
                TitleAlignment::Left => x_start,
                TitleAlignment::Center => {
                    let padding = available.saturating_sub(display_text.len() as u16) / 2;
                    x_start + padding
                }
                TitleAlignment::Right => {
                    x_start + available.saturating_sub(display_text.len() as u16)
                }
            };

            for (i, ch) in display_text.chars().enumerate() {
                if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
                    cell.set_char(ch);
                    cell.set_style(title_style);
                }
            }
        }

        if let Some(ref bottom_title) = self.bottom_title
            && self.border.bottom
        {
            let max_y = area.bottom().saturating_sub(1);
            let (r, g, b, a) = self.title_color.unwrap_or(self.border_color).to_ints();
            let title_style = if a == 0 {
                Style::default()
            } else {
                Style::default().fg(Color::Rgb(r, g, b))
            };

            let available = area.width.saturating_sub(left_inset).saturating_sub(1);
            let x_start = area.x + left_inset + 1;

            let display_text = if bottom_title.len() as u16 > available {
                &bottom_title[..available as usize]
            } else {
                bottom_title
            };

            let x = match self.bottom_title_alignment {
                TitleAlignment::Left => x_start,
                TitleAlignment::Center => {
                    let padding = available.saturating_sub(display_text.len() as u16) / 2;
                    x_start + padding
                }
                TitleAlignment::Right => {
                    x_start + available.saturating_sub(display_text.len() as u16)
                }
            };

            for (i, ch) in display_text.chars().enumerate() {
                if let Some(cell) = buf.cell_mut((x + i as u16, max_y)) {
                    cell.set_char(ch);
                    cell.set_style(title_style);
                }
            }
        }
    }
}

impl Default for BoxRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl BoxRenderable {
    /// Build a taffy `Style` from the current Box properties (border + gaps).
    #[must_use]
    pub fn to_taffy_style(&self) -> taffy::Style {
        let border = crate::core::layout::border_rect(
            self.border.top,
            self.border.right,
            self.border.bottom,
            self.border.left,
        );

        let mut style = crate::core::layout::default_box_style(border);

        if let Some(gap) = self.gap {
            style.gap = taffy::Size {
                width: taffy::LengthPercentage::length(gap),
                height: taffy::LengthPercentage::length(gap),
            };
        } else {
            let row = self.row_gap.unwrap_or(0.0);
            let col = self.column_gap.unwrap_or(0.0);
            if row > 0.0 || col > 0.0 {
                style.gap = taffy::Size {
                    width: taffy::LengthPercentage::length(col),
                    height: taffy::LengthPercentage::length(row),
                };
            }
        }

        style
    }
}

impl Renderable for BoxRenderable {
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

    fn layout_node(&self) -> Option<taffy::NodeId> {
        self.layout_node
    }

    fn set_layout_node(&mut self, node: Option<taffy::NodeId>) {
        self.layout_node = node;
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

    fn children_mut(&mut self) -> &mut [Box<dyn Renderable>] {
        &mut self.children
    }

    fn build_style(&self) -> Option<taffy::Style> {
        Some(self.to_taffy_style())
    }

    fn apply_layout(&mut self, _layout: &taffy::Layout) {}

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let has_border = self.has_border();
        let has_visible_fill = self.should_fill && self.background_color.a() > 0.0;

        if !has_border && !has_visible_fill {
            return;
        }

        if has_visible_fill {
            self.render_background(buf, area);
        }

        if has_border {
            let chars = self
                .custom_border_chars
                .as_ref()
                .unwrap_or_else(|| border_chars(self.border_style));
            self.render_border(buf, area, chars);
            self.render_title(buf, area);
        }
    }
}
