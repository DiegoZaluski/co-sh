use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters: writing them into cells makes ratatui's
        // buffer diff panic ("control character passed to cell_width without
        // filtering").
        if ch.is_control() {
            continue;
        }
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

pub struct SubagentFooterView;

impl SubagentFooterView {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        let Some(_session) = state.current_session() else {
            return;
        };

        let mut bg = BoxRenderable::new();
        bg.set_background_color(Some(theme.background_element.into()));
        bg.render_self(buf, area);

        let agents = state.unique_agents();
        let label = if agents.is_empty() {
            "no agents".to_string()
        } else {
            format!("agents: {}", agents.join(", "))
        };

        let style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(
            buf,
            &label,
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            style,
        );
    }
}
