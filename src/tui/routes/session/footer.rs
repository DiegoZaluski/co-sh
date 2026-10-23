use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line_wide;

/// Cell width of the ` → ` separator between the running and the new
/// version (space + arrow + space — all single-cell glyphs).
const UPDATE_ARROW_W: u16 = 3;

pub struct FooterView;

impl FooterView {
    /// Renders the footer row.
    ///
    /// Returns the click hitbox of the update button (`v0.1.2` in
    /// `v0.1.1 → v0.1.2`) when it was drawn this frame; `None` when no
    /// update is available, the text is hidden (dialogs) or the row is too
    /// narrow. The caller stores the rect and resolves the hover state for
    /// the NEXT frame — the same muted→text swap the header's
    /// `bug report` / `← esc` links use.
    pub fn render_with_mode(
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        hide_text: bool,
        update_version: Option<&str>,
        update_hover: bool,
    ) -> Option<Rect> {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        // When hide_text is true (e.g. question dialog is visible), just render the background
        if hide_text {
            return None;
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

        // Update hint `v0.1.1 → v0.1.2`, drawn right after the running
        // version when a newer release is available. Only the new version
        // is the clickable button; the arrow stays muted. Pure display
        // work — the hitbox handed back covers exactly the button glyphs.
        let button = update_version.and_then(|new_version| {
            // Release versions are plain ASCII digits/dots, so the char
            // count is the terminal-cell width.
            let cur_w = version.chars().count() as u16;
            let button_text = format!("v{new_version}");
            let button_w = button_text.chars().count() as u16;
            let button_x = area.x.saturating_add(1 + cur_w + UPDATE_ARROW_W);
            // Skip the whole hint when it would run past the footer row
            // (right-anchored segments live at the other end).
            if button_x + button_w > area.right() {
                return None;
            }
            draw_text_line_wide(
                buf,
                " \u{2192} ",
                area.x.saturating_add(1 + cur_w),
                area.y,
                area.width.saturating_sub(2),
                muted,
            );
            draw_text_line_wide(
                buf,
                &button_text,
                button_x,
                area.y,
                button_w,
                // At rest the button sits in the muted color; the caller
                // flips it to the text color while hovered (accent can't
                // mark the hover state: in the default theme accent == text,
                // so swapping between them is invisible).
                if update_hover {
                    Style::default().fg(rgba_color(theme.text))
                } else {
                    muted
                },
            );
            Some(Rect::new(button_x, area.y, button_w, 1))
        });

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

        button
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
            agents_panes: None,
            agents_mode_open: false,
            agents_hidden: Vec::new(),
            ctx_ids: Default::default(),
            main_agent_worktree: None,
            messages: vec![],
        });
        state.current_session_id = Some("test-session".into());
        state.working_directory = "~/project".into();

        let theme = ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterView::render_with_mode(&mut buf, area, &state, &theme, false, None, false);

        let line: String = (area.x..area.right())
            .filter_map(|x| buf.cell((x, area.y)).map(|cell| cell.symbol()))
            .collect();
        assert!(line.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
        assert!(!line.contains('●'));
        assert!(!line.contains('○'));
    }

    #[test]
    fn renders_update_hint_with_button_hitbox() {
        let mut state = AppState::new();
        state.working_directory = "~/project".into();

        let theme = ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        let hitbox = FooterView::render_with_mode(
            &mut buf,
            area,
            &state,
            &theme,
            false,
            Some("0.1.2"),
            true,
        );

        let line: String = (area.x..area.right())
            .filter_map(|x| buf.cell((x, area.y)).map(|cell| cell.symbol()))
            .collect();
        assert!(
            line.contains(concat!("v", env!("CARGO_PKG_VERSION"), " → v0.1.2")),
            "line: {line}"
        );

        // The hitbox covers exactly the `v0.1.2` glyphs.
        let hitbox = hitbox.expect("button hitbox drawn");
        assert_eq!(hitbox.width, 6);
        let button_text: String = (hitbox.x..hitbox.right())
            .filter_map(|x| buf.cell((x, hitbox.y)).map(|cell| cell.symbol()))
            .collect();
        assert_eq!(button_text, "v0.1.2");
    }

    #[test]
    fn hides_update_hint_when_text_hidden() {
        let mut state = AppState::new();
        state.working_directory = "~/project".into();

        let theme = ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        let hitbox = FooterView::render_with_mode(
            &mut buf,
            area,
            &state,
            &theme,
            true,
            Some("0.1.2"),
            false,
        );

        let line: String = (area.x..area.right())
            .filter_map(|x| buf.cell((x, area.y)).map(|cell| cell.symbol()))
            .collect();
        assert!(!line.contains("→"));
        assert!(hitbox.is_none());
    }

    #[test]
    fn skips_update_hint_when_row_too_narrow() {
        let mut state = AppState::new();
        state.working_directory = "~/project".into();

        let theme = ThemeRegistry::new().default_theme().clone();
        // Fits `v0.1.1` (padded) but not the full `v0.1.1 → v0.1.2` hint.
        let area = Rect::new(0, 0, 9, 1);
        let mut buf = Buffer::empty(area);

        let hitbox = FooterView::render_with_mode(
            &mut buf,
            area,
            &state,
            &theme,
            false,
            Some("0.1.2"),
            false,
        );

        let line: String = (area.x..area.right())
            .filter_map(|x| buf.cell((x, area.y)).map(|cell| cell.symbol()))
            .collect();
        assert!(!line.contains("→"), "line: {line}");
        assert!(hitbox.is_none());
    }
}
