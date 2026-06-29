use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_SELECT_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct SelectOption {
    pub name: String,
    pub description: String,
}

pub struct SelectRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    options: Vec<SelectOption>,
    selected_index: usize,
    scroll_offset: usize,
    max_visible_items: usize,

    background_color: RGBA,
    text_color: RGBA,
    focused_background_color: RGBA,
    focused_text_color: RGBA,
    selected_background_color: RGBA,
    selected_text_color: RGBA,
    description_color: RGBA,
    selected_description_color: RGBA,
    show_scroll_indicator: bool,
    wrap_selection: bool,
    show_description: bool,
    show_selection_indicator: bool,
    fast_scroll_step: usize,
}

impl SelectRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_SELECT_NUM.fetch_add(1, Ordering::Relaxed);
        SelectRenderable {
            id: format!("select-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            options: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            max_visible_items: 10,
            background_color: RGBA::from_ints(0, 0, 0, 0),
            text_color: RGBA::from_ints(255, 255, 255, 255),
            focused_background_color: RGBA::from_ints(26, 26, 26, 255),
            focused_text_color: RGBA::from_ints(255, 255, 255, 255),
            selected_background_color: RGBA::from_ints(51, 68, 85, 255),
            selected_text_color: RGBA::from_ints(255, 255, 0, 255),
            description_color: RGBA::from_ints(136, 136, 136, 255),
            selected_description_color: RGBA::from_ints(204, 204, 204, 255),
            show_scroll_indicator: false,
            wrap_selection: false,
            show_description: true,
            show_selection_indicator: true,
            fast_scroll_step: 5,
        }
    }

    pub fn set_options(&mut self, options: Vec<SelectOption>) {
        self.options = options;
        self.selected_index = self
            .selected_index
            .min(self.options.len().saturating_sub(1));
        self.update_scroll_offset();
    }

    #[must_use]
    pub fn options(&self) -> &[SelectOption] {
        &self.options
    }

    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn set_selected_index(&mut self, index: usize) {
        if index < self.options.len() {
            self.selected_index = index;
            self.update_scroll_offset();
        }
    }

    #[must_use]
    pub fn selected_option(&self) -> Option<&SelectOption> {
        self.options.get(self.selected_index)
    }

    pub fn move_up(&mut self, steps: usize) {
        if self.options.is_empty() {
            return;
        }
        if steps > self.selected_index {
            if self.wrap_selection {
                self.selected_index = self.options.len() - 1;
            } else {
                self.selected_index = 0;
            }
        } else {
            self.selected_index -= steps;
        }
        self.update_scroll_offset();
    }

    pub fn move_down(&mut self, steps: usize) {
        if self.options.is_empty() {
            return;
        }
        let new_index = self.selected_index.saturating_add(steps);
        if new_index >= self.options.len() {
            if self.wrap_selection {
                self.selected_index = 0;
            } else {
                self.selected_index = self.options.len() - 1;
            }
        } else {
            self.selected_index = new_index;
        }
        self.update_scroll_offset();
    }

    pub fn set_show_scroll_indicator(&mut self, show: bool) {
        self.show_scroll_indicator = show;
    }

    pub fn set_show_description(&mut self, show: bool) {
        self.show_description = show;
    }

    pub fn set_show_selection_indicator(&mut self, show: bool) {
        self.show_selection_indicator = show;
    }

    pub fn set_background_color(&mut self, color: Option<ColorInput>) {
        self.background_color =
            parse_color(color.unwrap_or(ColorInput::String("transparent".into())));
    }

    pub fn set_text_color(&mut self, color: Option<ColorInput>) {
        self.text_color = parse_color(color.unwrap_or(ColorInput::String("#FFFFFF".into())));
    }

    pub fn set_focused_background_color(&mut self, color: Option<ColorInput>) {
        self.focused_background_color =
            parse_color(color.unwrap_or(ColorInput::String("#1a1a1a".into())));
    }

    pub fn set_focused_text_color(&mut self, color: Option<ColorInput>) {
        self.focused_text_color =
            parse_color(color.unwrap_or(ColorInput::String("#FFFFFF".into())));
    }

    pub fn set_selected_background_color(&mut self, color: Option<ColorInput>) {
        self.selected_background_color =
            parse_color(color.unwrap_or(ColorInput::String("#334455".into())));
    }

    pub fn set_selected_text_color(&mut self, color: Option<ColorInput>) {
        self.selected_text_color =
            parse_color(color.unwrap_or(ColorInput::String("#FFFF00".into())));
    }

    fn lines_per_item(&self) -> u16 {
        if self.show_description { 2 } else { 1 }
    }

    fn update_scroll_offset(&mut self) {
        if self.options.is_empty() {
            self.scroll_offset = 0;
            return;
        }
        let half_visible = self.max_visible_items.saturating_sub(1) / 2;
        let max_scroll = self.options.len().saturating_sub(self.max_visible_items);
        let target = self.selected_index.saturating_sub(half_visible);
        self.scroll_offset = target.min(max_scroll);
    }
}

impl Default for SelectRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for SelectRenderable {
    fn id(&self) -> &str {
        &self.id
    }

    fn add_child(&mut self, child: Box<dyn crate::core::renderable::Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }
    fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }
    fn insert_child_before(
        &mut self,
        child: Box<dyn crate::core::renderable::Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        let anchor_idx = self.children.iter().position(|c| c.id() == anchor_id)?;
        self.children.insert(anchor_idx, child);
        Some(anchor_idx)
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
        if area.width == 0 || area.height == 0 || self.options.is_empty() {
            return;
        }

        let max_visible = usize::from(area.height) / usize::from(self.lines_per_item());
        let max_visible = max_visible.max(1);

        let visible_range =
            self.scroll_offset..self.options.len().min(self.scroll_offset + max_visible);
        let mut y = area.y;

        for i in visible_range {
            let option = &self.options[i];
            let is_selected = i == self.selected_index;
            let item_remaining = area.bottom().saturating_sub(y);
            if item_remaining == 0 {
                break;
            }

            let (bg_color, name_color, desc_color) = if is_selected {
                (
                    self.selected_background_color,
                    self.selected_text_color,
                    self.selected_description_color,
                )
            } else {
                (
                    self.background_color,
                    self.text_color,
                    self.description_color,
                )
            };

            let (br, bg, bb, ba) = bg_color.to_ints();
            let bg_style = if ba == 0 {
                Style::default()
            } else {
                Style::default().bg(Color::Rgb(br, bg, bb))
            };

            let (nr, ng, nb, na) = name_color.to_ints();
            let name_style = if na == 0 {
                Style::default()
            } else {
                Style::default().fg(Color::Rgb(nr, ng, nb))
            };

            // Draw background for this item row
            for cx in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_style(bg_style);
                    cell.set_char(' ');
                }
            }

            // Draw indicator + name
            let indicator = if self.show_selection_indicator && is_selected {
                "▶ "
            } else if self.show_selection_indicator {
                "  "
            } else {
                ""
            };
            let name_text = format!("{}{}", indicator, option.name);
            let mut cx = area.x + 1;
            for ch in name_text.chars() {
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(ch);
                    cell.set_style(bg_style.patch(name_style));
                }
                cx += 1;
            }

            // Draw description on next line
            if self.show_description && item_remaining > 1 {
                let dy = y + 1;
                let (dr, dg, db, da) = desc_color.to_ints();
                let desc_style = if da == 0 {
                    Style::default()
                } else {
                    Style::default().fg(Color::Rgb(dr, dg, db))
                };
                for cx in area.x..area.right() {
                    if let Some(cell) = buf.cell_mut((cx, dy)) {
                        cell.set_style(bg_style);
                        cell.set_char(' ');
                    }
                }
                let mut cx = area.x + 1;
                for ch in option.description.chars() {
                    if cx >= area.right() {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, dy)) {
                        cell.set_char(ch);
                        cell.set_style(bg_style.patch(desc_style));
                    }
                    cx += 1;
                }
                y = dy + 1;
            } else {
                y += 1;
            }
        }
    }
}
