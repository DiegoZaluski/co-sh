use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub fn render_markdown(buf: &mut Buffer, area: Rect, content: &str, fg: RGBA, bg: RGBA) {
    if content.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let mut md = MarkdownRenderable::new(Some(content.to_string()));
    md.set_fg(Some(ColorInput::RGBA(fg)));
    md.set_bg(Some(ColorInput::RGBA(bg)));
    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(255, 200, 0, 255))));
    md.render_self(buf, area);
}
