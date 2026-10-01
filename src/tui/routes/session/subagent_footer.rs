use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;

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
