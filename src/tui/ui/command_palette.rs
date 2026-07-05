use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub struct CommandItem {
    pub key: String,
    pub description: String,
}

pub struct CommandPalette {
    pub visible: bool,
    pub filter: String,
    pub selected: usize,
    pub commands: Vec<CommandItem>,
}

impl CommandPalette {
    pub fn new() -> Self {
        CommandPalette {
            visible: false,
            filter: String::new(),
            selected: 0,
            commands: vec![
                CommandItem {
                    key: "Ctrl+B".into(),
                    description: "Toggle sidebar".into(),
                },
                CommandItem {
                    key: "Ctrl+C".into(),
                    description: "Toggle conceal mode".into(),
                },
                CommandItem {
                    key: "Ctrl+T".into(),
                    description: "Toggle thinking mode".into(),
                },
                CommandItem {
                    key: "Ctrl+D".into(),
                    description: "Toggle tool details".into(),
                },
                CommandItem {
                    key: "Ctrl+G".into(),
                    description: "Toggle generic tool output".into(),
                },
                CommandItem {
                    key: "Ctrl+Y".into(),
                    description: "Toggle timestamps".into(),
                },
                CommandItem {
                    key: "?".into(),
                    description: "Toggle help".into(),
                },
                CommandItem {
                    key: "Esc".into(),
                    description: "Cancel / Interrupt".into(),
                },
                CommandItem {
                    key: "q".into(),
                    description: "Quit".into(),
                },
            ],
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.filter.clear();
            self.selected = 0;
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.commands.len()).collect();
        }
        let lower = self.filter.to_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                cmd.key.to_lowercase().contains(&lower)
                    || cmd.description.to_lowercase().contains(&lower)
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn select_next(&mut self) {
        let indices = self.filtered_indices();
        if indices.is_empty() {
            return;
        }
        let cur = indices
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        self.selected = indices[(cur + 1) % indices.len()];
    }

    pub fn select_prev(&mut self) {
        let indices = self.filtered_indices();
        if indices.is_empty() {
            return;
        }
        let cur = indices
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        self.selected = if cur == 0 {
            indices[indices.len() - 1]
        } else {
            indices[cur - 1]
        };
    }

    pub fn push_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.selected = self.filtered_indices().first().copied().unwrap_or(0);
    }

    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.selected = self.filtered_indices().first().copied().unwrap_or(0);
    }

    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect, _theme: &Theme) -> bool {
        if !self.visible {
            return false;
        }
        let palette_w = 50.min(area.width.saturating_sub(8));
        let palette_h = (self.commands.len() as u16).min(area.height.saturating_sub(4));
        let palette_x = area.x + (area.width - palette_w) / 2;
        let palette_y = area.y + (area.height - palette_h) / 2;

        let x = mouse.x;
        let y = mouse.y;

        // Check if click is within palette area
        if x < palette_x || x >= palette_x + palette_w || y < palette_y || y >= palette_y + palette_h {
            // Click outside -> close
            self.visible = false;
            return true;
        }

        // Click on filter row
        if y == palette_y + 2 {
            return true;
        }

        // Click on an item
        let indices = self.filtered_indices();
        let start_y = palette_y + 4;
        let max_rows = palette_h.saturating_sub(5);
        if y >= start_y && y < start_y + max_rows {
            let row = (y - start_y) as usize;
            if row < indices.len() {
                self.selected = indices[row];
                self.visible = false;
            }
        }

        true
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let palette_w = 50.min(area.width.saturating_sub(8));
        let palette_h = (self.commands.len() as u16).min(area.height.saturating_sub(4));
        let palette_x = area.x + (area.width - palette_w) / 2;
        let palette_y = area.y + (area.height - palette_h) / 2;

        let palette_area = Rect::new(palette_x, palette_y, palette_w, palette_h);

        let mut bg = BoxRenderable::new();
        bg.set_background_color(Some(theme.background_element.into()));
        bg.set_border_color(Some(theme.border_active.into()));
        bg.render_self(buf, palette_area);

        let header = "Command Palette";
        let header_style = Style::default().fg(rgba_color(theme.text));
        draw_text_line(
            buf,
            header,
            palette_x + 2,
            palette_y + 1,
            palette_w.saturating_sub(4),
            header_style,
        );

        let filter_str = if self.filter.is_empty() {
            "Type to filter..."
        } else {
            &self.filter
        };
        let filter_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(
            buf,
            filter_str,
            palette_x + 2,
            palette_y + 2,
            palette_w.saturating_sub(4),
            filter_style,
        );

        let indices = self.filtered_indices();
        let start_y = palette_y + 4;
        let max_rows = palette_h.saturating_sub(5) as usize;

        for (row, &idx) in indices.iter().enumerate().take(max_rows) {
            let ry = start_y + row as u16;
            if ry >= palette_area.bottom() {
                break;
            }

            let cmd = &self.commands[idx];
            let is_sel = idx == self.selected;
            let prefix = if is_sel { "\u{25b8} " } else { "  " };
            let entry = format!("{}{}  {}", prefix, cmd.key, cmd.description);

            let style = if is_sel {
                Style::default().fg(rgba_color(theme.primary))
            } else {
                Style::default().fg(rgba_color(theme.text))
            };
            draw_text_line(
                buf,
                &entry,
                palette_x + 2,
                ry,
                palette_w.saturating_sub(4),
                style,
            );
        }
    }
}
