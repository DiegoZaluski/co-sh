use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};

static NEXT_TAB_SELECT_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct TabSelectOption {
    pub name: String,
    pub description: String,
}

pub struct TabSelectRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    options: Vec<TabSelectOption>,
    selected_index: usize,
    scroll_offset: usize,
    tab_width: u16,
    max_visible_tabs: usize,

    background_color: RGBA,
    text_color: RGBA,
    focused_background_color: RGBA,
    focused_text_color: RGBA,
    selected_background_color: RGBA,
    selected_text_color: RGBA,
    selected_description_color: RGBA,
    show_scroll_arrows: bool,
    show_description: bool,
    show_underline: bool,
    wrap_selection: bool,
}

impl TabSelectRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_TAB_SELECT_NUM.fetch_add(1, Ordering::Relaxed);
        Self {
            id: format!("tabselect-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            options: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            tab_width: 20,
            max_visible_tabs: 10,
            background_color: RGBA::from_ints(0, 0, 0, 0),
            text_color: RGBA::from_ints(255, 255, 255, 255),
            focused_background_color: RGBA::from_ints(26, 26, 26, 255),
            focused_text_color: RGBA::from_ints(255, 255, 255, 255),
            selected_background_color: RGBA::from_ints(51, 68, 85, 255),
            selected_text_color: RGBA::from_ints(255, 255, 0, 255),
            selected_description_color: RGBA::from_ints(204, 204, 204, 255),
            show_scroll_arrows: true,
            show_description: true,
            show_underline: true,
            wrap_selection: false,
        }
    }

    pub fn set_options(&mut self, options: Vec<TabSelectOption>) {
        self.options = options;
        self.selected_index = self
            .selected_index
            .min(self.options.len().saturating_sub(1));
        self.update_scroll_offset();
    }

    #[must_use]
    pub fn options(&self) -> &[TabSelectOption] {
        &self.options
    }

    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn set_selected_index(&mut self, index: usize) {
        if index < self.options.len() {
            self.selected_index = index;
            self.update_scroll_offset();
        }
    }

    #[must_use]
    pub fn selected_option(&self) -> Option<&TabSelectOption> {
        self.options.get(self.selected_index)
    }

    pub fn move_left(&mut self) {
        if self.options.is_empty() {
            return;
        }
        if self.selected_index > 0 {
            self.selected_index -= 1;
        } else if self.wrap_selection {
            self.selected_index = self.options.len() - 1;
        }
        self.update_scroll_offset();
    }

    pub fn move_right(&mut self) {
        if self.options.is_empty() {
            return;
        }
        if self.selected_index < self.options.len() - 1 {
            self.selected_index += 1;
        } else if self.wrap_selection {
            self.selected_index = 0;
        }
        self.update_scroll_offset();
    }

    pub const fn set_tab_width(&mut self, width: u16) {
        self.tab_width = width;
    }

    #[must_use]
    pub const fn tab_width(&self) -> u16 {
        self.tab_width
    }

    pub fn set_background_color(&mut self, color: Option<ColorInput>) {
        self.background_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("transparent".into())));
    }

    pub fn set_text_color(&mut self, color: Option<ColorInput>) {
        self.text_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("#FFFFFF".into())));
    }

    pub fn set_focused_background_color(&mut self, color: Option<ColorInput>) {
        self.focused_background_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("#1a1a1a".into())));
    }

    pub fn set_focused_text_color(&mut self, color: Option<ColorInput>) {
        self.focused_text_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("#FFFFFF".into())));
    }

    pub fn set_selected_background_color(&mut self, color: Option<ColorInput>) {
        self.selected_background_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("#334455".into())));
    }

    pub fn set_selected_text_color(&mut self, color: Option<ColorInput>) {
        self.selected_text_color =
            parse_color(color.unwrap_or_else(|| ColorInput::String("#FFFF00".into())));
    }

    pub const fn set_show_description(&mut self, show: bool) {
        self.show_description = show;
    }

    pub const fn set_show_underline(&mut self, show: bool) {
        self.show_underline = show;
    }

    pub const fn set_show_scroll_arrows(&mut self, show: bool) {
        self.show_scroll_arrows = show;
    }

    fn update_scroll_offset(&mut self) {
        self.scroll_offset = self.scroll_offset_for(self.max_visible_tabs);
    }

    fn scroll_offset_for(&self, max_visible: usize) -> usize {
        if self.options.is_empty() || max_visible == 0 {
            return 0;
        }
        let half_visible = max_visible.saturating_sub(1) / 2;
        let max_scroll = self.options.len().saturating_sub(max_visible);
        let target = self.selected_index.saturating_sub(half_visible);
        target.min(max_scroll)
    }

    const fn calculated_height(&self) -> u16 {
        let mut h: u16 = 1;
        if self.show_underline {
            h += 1;
        }
        if self.show_description {
            h += 1;
        }
        h
    }
}

impl Default for TabSelectRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for TabSelectRenderable {
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
        if area.width == 0 || area.height == 0 || self.options.is_empty() {
            return;
        }

        let max_visible = usize::from(area.width) / usize::from(self.tab_width);
        let max_visible = max_visible.max(1);
        let scroll_offset = self.scroll_offset_for(max_visible);
        let visible_end = (scroll_offset + max_visible).min(self.options.len());
        let content_y = area.y;

        // Render tab row
        for i in scroll_offset..visible_end {
            let option = &self.options[i];
            let is_selected = i == self.selected_index;
            let tab_x = area.x + ((i - scroll_offset) * usize::from(self.tab_width)) as u16;
            let actual_width = self.tab_width.min(area.right().saturating_sub(tab_x));

            let (bg_color, name_color) = if is_selected {
                (self.selected_background_color, self.selected_text_color)
            } else {
                (self.background_color, self.text_color)
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

            // Draw tab background
            for cx in tab_x..tab_x + actual_width {
                if let Some(cell) = buf.cell_mut((cx, content_y)) {
                    cell.set_style(bg_style);
                    cell.set_char(' ');
                }
            }

            // Draw tab name
            let max_name_width = actual_width.saturating_sub(2);
            let display_name = if option.name.len() > usize::from(max_name_width) {
                let mut s: String = option
                    .name
                    .chars()
                    .take(usize::from(max_name_width.saturating_sub(1)))
                    .collect();
                s.push('…');
                s
            } else {
                option.name.clone()
            };

            let mut cx = tab_x + 1;
            for ch in display_name.chars() {
                if cx >= tab_x + actual_width - 1 {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, content_y)) {
                    cell.set_char(ch);
                    cell.set_style(bg_style.patch(name_style));
                }
                cx += 1;
            }

            // Draw underline
            if is_selected && self.show_underline && area.height >= 2 {
                let underline_y = content_y + 1;
                for cx in tab_x..tab_x + actual_width {
                    if let Some(cell) = buf.cell_mut((cx, underline_y)) {
                        cell.set_char('▬');
                        cell.set_style(bg_style.patch(name_style));
                    }
                }
            }
        }

        // Draw scroll arrows
        if self.show_scroll_arrows && self.options.len() > max_visible {
            let (ar, ag, ab, _aa) = RGBA::from_ints(170, 170, 170, 255).to_ints();
            let arrow_style = Style::default().fg(Color::Rgb(ar, ag, ab));

            if scroll_offset > 0
                && let Some(cell) = buf.cell_mut((area.x, content_y))
            {
                cell.set_char('‹');
                cell.set_style(arrow_style);
            }
            if scroll_offset + max_visible < self.options.len() {
                let right_x = area.right().saturating_sub(1);
                if let Some(cell) = buf.cell_mut((right_x, content_y)) {
                    cell.set_char('›');
                    cell.set_style(arrow_style);
                }
            }
        }

        // Draw description
        if self.show_description && area.height >= u16::from(self.show_underline) + 2 {
            let desc_y = content_y + u16::from(self.show_underline) + 1;
            if let Some(selected) = self.selected_option() {
                let (dr, dg, db, da) = self.selected_description_color.to_ints();
                let desc_style = if da == 0 {
                    Style::default()
                } else {
                    Style::default().fg(Color::Rgb(dr, dg, db))
                };

                let max_desc = area.width.saturating_sub(2);
                let display_desc = if selected.description.len() > usize::from(max_desc) {
                    let mut s: String = selected
                        .description
                        .chars()
                        .take(usize::from(max_desc.saturating_sub(1)))
                        .collect();
                    s.push('…');
                    s
                } else {
                    selected.description.clone()
                };

                let mut cx = area.x + 1;
                for ch in display_desc.chars() {
                    if cx >= area.right() {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, desc_y)) {
                        cell.set_char(ch);
                        cell.set_style(desc_style);
                    }
                    cx += 1;
                }
            }
        }
    }
}
