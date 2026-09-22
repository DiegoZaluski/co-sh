use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::left_panel::layout::SessionListLayout;
use crate::session_store::SessionSummary;
use crate::theme::{Theme, rgba_color};
use crate::types::SessionStatus;
use crate::util::list_selection::ListSelection;
use ratatui::crossterm::event::KeyCode;

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters (session titles derive from user message
        // text, which can contain `\n`/`\t`): writing them into cells makes
        // ratatui's buffer diff panic ("control character passed to
        // cell_width without filtering").
        if ch.is_control() {
            continue;
        }
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Action returned by the left panel after a mouse click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsAction {
    /// Switch to the given session.
    SwitchTo(String),
    /// Request deletion of the given session.
    RequestDelete(String),
    /// Request deletion of every session of the current working directory.
    RequestDeleteAll,
    /// No action.
    None,
}

pub struct SessionsView {
    pub open: bool,
    /// Shared list-selection state: selected_index + scroll_offset.
    pub selection: ListSelection,
}

impl SessionsView {
    pub const fn new() -> Self {
        Self {
            open: false,
            selection: ListSelection::new(),
        }
    }

    /// Delegate to `selection.select_prev`.
    pub fn select_prev(&mut self, total: usize) {
        self.selection.select_prev(total);
    }

    /// Delegate to `selection.select_next`.
    pub fn select_next(&mut self, total: usize) {
        self.selection.select_next(total);
    }

    /// Delegate to `selection.select_first`.
    pub fn select_first(&mut self, total: usize) {
        self.selection.select_first(total);
    }

    /// Delegate to `selection.select_last`.
    pub fn select_last(&mut self, total: usize) {
        self.selection.select_last(total);
    }

    /// Handle a mouse click on the sessions list. Returns an action to perform.
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        summaries: &[SessionSummary],
        status: &SessionStatus,
    ) -> SessionsAction {
        if !self.open {
            return SessionsAction::None;
        }
        let my = mouse.y;
        let mx = mouse.x;

        if mx < area.x || mx >= area.right() {
            return SessionsAction::None;
        }

        // Footer button first: it lives on its own reserved row, where the
        // item hit-test below can never reach.
        if my == SessionListLayout::footer_row(area)
            && SessionListLayout::footer_span(area).is_some_and(|span| span.contains(&mx))
        {
            return SessionsAction::RequestDeleteAll;
        }

        // Compute which item was clicked, accounting for scroll offset —
        // None outside the list rows (header, separator, footer, past
        // bottom).
        let Some(clicked_idx) =
            SessionListLayout::item_index_at(area, self.selection.scroll_offset, my)
        else {
            return SessionsAction::None;
        };
        let Some(summary) = summaries.get(clicked_idx) else {
            return SessionsAction::None;
        };

        // Update selection to clicked item
        let total = summaries.len();
        if clicked_idx < total && *status == SessionStatus::Idle {
            self.selection.selected_index = clicked_idx;
        }

        // 🗑 hit-test: exactly the columns the glyph occupies — never a
        // half-row region.
        let trash_hit = SessionListLayout::trash_span(area, &summary.title)
            .is_some_and(|span| span.contains(&mx));

        if trash_hit {
            return SessionsAction::RequestDelete(summary.session_id.clone());
        }

        // Switch hit-test: only the visible title text enters the session.
        // The span is the string clamped to what the renderer actually
        // displays — the string's own length is irrelevant once the panel
        // truncates it. Clicks on the row outside the string (selection
        // prefix, gap, the pad past the glyph) do nothing.
        let title_hit = SessionListLayout::title_span(area, &summary.title)
            .is_some_and(|span| span.contains(&mx));

        if title_hit {
            return SessionsAction::SwitchTo(summary.session_id.clone());
        }

        // On the row, but not on the title and not on the glyph.
        SessionsAction::None
    }

    /// Handle a key press on the sessions list. Returns an action to perform.
    pub fn handle_key(&mut self, key: KeyCode, summaries: &[SessionSummary]) -> SessionsAction {
        if !self.open {
            return SessionsAction::None;
        }
        match key {
            KeyCode::Enter => {
                let idx = self.selection.selected_index;
                let Some(summary) = summaries.get(idx) else {
                    return SessionsAction::None;
                };
                SessionsAction::SwitchTo(summary.session_id.clone())
            }
            _ => SessionsAction::None,
        }
    }

    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        summaries: &[SessionSummary],
        theme: &Theme,
    ) {
        if !self.open {
            return;
        }

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_panel.into()));
        bg_box.render_self(buf, area);

        let header_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(
            buf,
            " Sessions",
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            header_style,
        );

        let separator_style = Style::default().fg(rgba_color(theme.border));
        if let Some(cell) = buf.cell_mut((area.x + 1, area.y + 1)) {
            cell.set_char('\u{2500}');
            cell.set_style(separator_style);
        }

        // ── Clamp and scroll ──
        let content_start_y = SessionListLayout::content_start_y(area);
        let content_end_y = SessionListLayout::content_end_y(area);
        let visible_count = content_end_y.saturating_sub(content_start_y) as usize;
        let total_items = summaries.len();

        self.selection.set_visible_count(visible_count);
        self.selection.clamp(total_items);

        // ── Styles ──
        let primary_color = rgba_color(theme.primary);
        let text_color = rgba_color(theme.text);
        let mute_fg = rgba_color(theme.text_muted);

        let selected_fg = Style::default().fg(primary_color);
        let normal_fg = Style::default().fg(text_color);

        // Layout: left_pad + prefix + text + gap + 🗑 + clear + right_pad —
        // the full column budget lives in SessionListLayout.
        let left_pad = SessionListLayout::LEFT_PAD;
        let prefix_w = SessionListLayout::PREFIX_W;
        let text_x = area.x + left_pad + prefix_w;
        let max_text_w = SessionListLayout::max_text_w(area);

        for (i, summary) in summaries
            .iter()
            .enumerate()
            .skip(self.selection.scroll_offset)
        {
            let local_idx = i - self.selection.scroll_offset;
            let y = content_start_y + local_idx as u16;
            if y >= content_end_y {
                break;
            }

            let is_selected = i == self.selection.selected_index;
            let style = if is_selected { selected_fg } else { normal_fg };
            let px = area.x + left_pad;

            // Prefix indicator — drawn manually to avoid draw_text_line breaking wide emoji
            if is_selected {
                // 🞴 emoji at px (takes 2 cols — don't touch px+1)
                if let Some(cell) = buf.cell_mut((px, y)) {
                    cell.set_char('\u{1F7B4}');
                    cell.set_style(style);
                }
            } else {
                // 2 spaces for alignment
                if let Some(cell) = buf.cell_mut((px, y)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
                if let Some(cell) = buf.cell_mut((px + 1, y)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }

            // Text
            let label = &summary.title;
            draw_text_line(buf, label, text_x, y, max_text_w, style);

            // 🗑 + cleared cell — one span drives both writes and the
            // hit-test, so a visible glyph is always clickable and vice
            // versa.
            if let Some(span) = SessionListLayout::trash_span(area, label) {
                if let Some(cell) = buf.cell_mut((span.start, y)) {
                    cell.set_char('\u{1F5D1}');
                    cell.set_style(Style::default().fg(mute_fg));
                }
                // Clear the cell after the wide emoji
                if let Some(cell) = buf.cell_mut((span.end - 1, y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default());
                }
            }
        }

        // ── Footer: delete all ──
        // Deletes every session of the current working directory (the
        // store's sessions dir is cwd-scoped by construction) — never other
        // directories' sessions. The whole panel-wide row is painted red
        // with white text on the label; only the label span is clickable.
        if let Some(span) = SessionListLayout::footer_span(area) {
            let footer_y = SessionListLayout::footer_row(area);
            let red_bg = Style::default().bg(rgba_color(theme.error));
            // Paint the full row red so list leftovers (a scrolled row
            // previously drawn there) never bleed into the button.
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, footer_y)) {
                    cell.set_char(' ');
                    cell.set_style(red_bg);
                }
            }
            draw_text_line(
                buf,
                SessionListLayout::DELETE_ALL_TEXT,
                span.start,
                footer_y,
                span.end - span.start,
                // set_style replaces the whole style: the label keeps the
                // red background under its white glyphs.
                Style::default()
                    .fg(rgba_color(theme.text))
                    .bg(rgba_color(theme.error)),
            );
        }
    }
}
