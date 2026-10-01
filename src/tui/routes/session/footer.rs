use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line_wide;

pub struct FooterView;

impl FooterView {
    pub fn render_with_mode(
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        hide_text: bool,
    ) {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        // When hide_text is true (e.g. question dialog is visible), just render the background
        if hide_text {
            return;
        }

        let muted = Style::default().fg(rgba_color(theme.text_muted));
        let accent = Style::default().fg(rgba_color(theme.accent));

        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        draw_text_line_wide(
            buf,
            version,
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            muted,
        );

        if state.current_session().is_some() {
            let dir = &state.working_directory;
            let dir_display = if dir.is_empty() { "~" } else { dir };

            // Display width, not byte length: right-anchored segments and
            // the draw limit must both count terminal cells (wide glyphs
            // occupy two).
            let disp_w = |s: &str| unicode_width::UnicodeWidthStr::width(s).max(1) as u16;

            let mut rx = area.right().saturating_sub(2);

            let dir_str = format!(" {dir_display}");
            rx = rx.saturating_sub(disp_w(&dir_str));
            draw_text_line_wide(buf, &dir_str, rx, area.y, disp_w(&dir_str), muted);

            // Current git branch (or detached SHA) beside the working
            // directory; hidden entirely outside a repository.
            if let Some(branch) = &state.git_branch {
                let branch_str = format!(" {branch}");
                rx = rx.saturating_sub(disp_w(&branch_str));
                draw_text_line_wide(buf, &branch_str, rx, area.y, disp_w(&branch_str), accent);
            }

            if state.permission_count > 0 {
                let s = format!("  perm {}", state.permission_count);
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line_wide(buf, &s, rx, area.y, disp_w(&s), muted);
            }

            if state.mcp_count > 0 || state.mcp_errors > 0 {
                let s = if state.mcp_errors > 0 {
                    format!("  mcp {}/{}", state.mcp_count, state.mcp_errors)
                } else {
                    format!("  mcp {}", state.mcp_count)
                };
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line_wide(buf, &s, rx, area.y, disp_w(&s), muted);
            }

            if !state.lsp_servers.is_empty() {
                let s = format!("  lsp {}", state.lsp_servers.len());
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line_wide(buf, &s, rx, area.y, disp_w(&s), muted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FooterView;
    use crate::state::AppState;
    use crate::theme::ThemeRegistry;
    use crate::types::Session;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn renders_version_without_connection_indicator() {
        let mut state = AppState::new();
        state.add_session(Session {
            id: "test-session".into(),
            title: "Test".into(),
            created_at: 0,
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            ctx_ids: Default::default(),
            messages: vec![],
        });
        state.current_session_id = Some("test-session".into());
        state.working_directory = "~/project".into();

        let theme = ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterView::render_with_mode(&mut buf, area, &state, &theme, false);

        let line: String = (area.x..area.right())
            .filter_map(|x| buf.cell((x, area.y)).map(|cell| cell.symbol()))
            .collect();
        assert!(line.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
        assert!(!line.contains('●'));
        assert!(!line.contains('○'));
    }
}
