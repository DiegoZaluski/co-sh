use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::types::MouseEvent;

use super::super::super::theme::Theme;
use crate::component::wall;
use crate::theme::rgba_color;

/// Which pending queue a message typed during a running agent loop joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTarget {
    /// The message enters the next LLM request of the CURRENT loop.
    NextRequest,
    /// The message waits for the current loop to end and starts a new one.
    NextLoop,
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
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

/// Option rows of the dialog, parallel to [`QueueTarget`].
const OPTION_LABELS: [&str; 2] = ["Next request", "Next agent loop"];
const OPTION_DESCRIPTIONS: [&str; 2] = [
    "enters the next request of the current loop",
    "starts a new loop when the current one ends",
];

/// Inline dialog that asks the user which queue a message typed while the
/// agent loop is running should join. Rendered exactly like the question
/// dialog (left border, panel background, footer hints) so the two share a
/// visual language.
///
/// The typed message stays in the prompt input until confirmed; the dialog
/// only carries a one-line preview of it. `Esc` dismisses without losing the
/// message.
pub struct QueueChoiceDialog {
    pub visible: bool,
    /// 0 = NextRequest, 1 = NextLoop.
    pub selected: usize,
    /// Set when the user presses Enter on the selected option. The App reads
    /// it, consumes the message via `PromptView::send_message`, then calls
    /// [`Self::hide`].
    pub submitted: bool,
    /// One-line preview of the message being queued (the prompt keeps the
    /// authoritative text until confirmation).
    preview: String,
}

impl QueueChoiceDialog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            submitted: false,
            preview: String::new(),
        }
    }

    /// Open the dialog for the given message preview.
    pub fn open(&mut self, preview: impl Into<String>) {
        self.preview = preview.into();
        self.selected = 0;
        self.submitted = false;
        self.visible = true;
    }

    /// Close the dialog (after submission or dismissal).
    pub fn hide(&mut self) {
        self.visible = false;
        self.submitted = false;
    }

    /// The queue selected by the user (only meaningful after submit).
    #[must_use]
    pub const fn choice(&self) -> QueueTarget {
        if self.selected == 0 {
            QueueTarget::NextRequest
        } else {
            QueueTarget::NextLoop
        }
    }

    /// Handle a key event. Returns true if consumed.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }
        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = 0;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = 1;
                true
            }
            // Number keys select directly (like the question dialog).
            KeyCode::Char('1') => {
                self.selected = 0;
                true
            }
            KeyCode::Char('2') => {
                self.selected = 1;
                true
            }
            KeyCode::Enter => {
                self.submitted = true;
                true
            }
            KeyCode::Esc => {
                self.visible = false;
                true
            }
            KeyCode::Char(_) if ctrl => true,
            _ => false,
        }
    }

    /// Required height in rows (0 when hidden).
    #[must_use]
    pub fn required_height(&self, _max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        // padding_vertical (5) + question row + preview row + 2 option rows
        // + footer (1).
        9
    }

    /// Handle a mouse click on the dialog. `area` is the area passed to render.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.visible {
            return false;
        }
        let x = mouse.x;
        let y_click = mouse.y;
        let height = self.required_height(area.width).min(area.height);
        if x < area.x || x >= area.x + area.width || y_click < area.y || y_click >= area.y + height
        {
            return false;
        }

        let inner_x = area.x + 3;
        // Layout mirrors `render`: question at area.y+1, preview at +2,
        // options at +4 and +5, footer at area.y + height - 2.
        let option_y = area.y + 4;
        for (i, _) in OPTION_LABELS.iter().enumerate() {
            if y_click == option_y + i as u16 {
                self.selected = i;
                self.submitted = true;
                return true;
            }
        }
        // Footer hints: "enter confirm" spans [inner_x, inner_x+13) and
        // "esc dismiss" spans [inner_x+13, inner_x+21) — must mirror `render`.
        let footer_y = area.y + height.saturating_sub(2);
        if y_click == footer_y {
            if x >= inner_x && x < inner_x + 13 {
                self.submitted = true;
                return true;
            }
            if x >= inner_x + 13 && x < inner_x + 21 {
                self.visible = false;
                return true;
            }
        }
        true
    }

    /// Render the dialog inline inside the given area.
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }
        let height = self.required_height(area.width).min(area.height);
        let inner_area = Rect::new(area.x, area.y, area.width, height);
        let footer_y = inner_area.bottom().saturating_sub(2);

        wall::render(buf, inner_area, theme.accent, theme.background_panel);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let bg = rgba_color(theme.background_panel);

        // Question row.
        draw_text_line(
            buf,
            "When to send this message?",
            inner_x,
            area.y + 1,
            inner_w,
            Style::default().fg(rgba_color(theme.text)).bg(bg),
        );

        // Message preview row (muted, truncated with an ellipsis).
        let preview = Self::truncate(&self.preview, inner_w);
        draw_text_line(
            buf,
            &preview,
            inner_x,
            area.y + 2,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );

        // Option rows.
        let option_y = area.y + 4;
        for (i, label) in OPTION_LABELS.iter().enumerate() {
            let ry = option_y + i as u16;
            if ry >= footer_y {
                break;
            }
            let is_selected = i == self.selected;
            // Marker glyph matching the permission dialog's selected option
            // (the "asterisk" marker), with a blank slot for unselected rows.
            let indicator = if is_selected { "\u{1F7B4} " } else { "  " };
            let indicator_color = if is_selected {
                theme.accent
            } else {
                theme.text_muted
            };
            // Highlight the active row across the full inner width.
            if is_selected {
                let bg_color = rgba_color(theme.background_element);
                for cx in inner_x..inner_x + inner_w {
                    if let Some(cell) = buf.cell_mut((cx, ry)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_color));
                    }
                }
            }
            draw_text_line(
                buf,
                indicator,
                inner_x,
                ry,
                inner_w,
                Style::default()
                    .fg(rgba_color(indicator_color))
                    .bg(if is_selected {
                        rgba_color(theme.background_element)
                    } else {
                        bg
                    }),
            );
            let text_fg = if is_selected {
                theme.secondary
            } else {
                theme.text
            };
            let opt_w = inner_w.saturating_sub(3);
            let combined = Self::truncate(&format!("{label} · {}", OPTION_DESCRIPTIONS[i]), opt_w);
            draw_text_line(
                buf,
                &combined,
                inner_x + 3,
                ry,
                opt_w,
                Style::default().fg(rgba_color(text_fg)).bg(if is_selected {
                    rgba_color(theme.background_element)
                } else {
                    bg
                }),
            );
        }

        // Footer (keyboard hints).
        draw_text_line(
            buf,
            "enter",
            inner_x,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text)).bg(bg),
        );
        draw_text_line(
            buf,
            " confirm",
            inner_x + 5,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
        draw_text_line(
            buf,
            " esc",
            inner_x + 13,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
        draw_text_line(
            buf,
            " dismiss",
            inner_x + 17,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
    }

    /// Truncate `text` to at most `width` display columns, appending `…` when
    /// cut (leaves room for the ellipsis).
    fn truncate(text: &str, width: u16) -> String {
        let width = usize::from(width).max(1);
        let mut out: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(width)
            .collect();
        if text.chars().filter(|c| !c.is_control()).count() > width {
            out.pop();
            out.push('…');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{QueueChoiceDialog, QueueTarget};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn open_resets_selection_and_shows_preview() {
        let mut d = QueueChoiceDialog::new();
        assert!(!d.visible);
        assert_eq!(d.required_height(60), 0);
        d.open("fix the test");
        assert!(d.visible);
        assert_eq!(d.selected, 0);
        assert_eq!(d.choice(), QueueTarget::NextRequest);
    }

    #[test]
    fn navigation_toggles_between_the_two_queues() {
        let mut d = QueueChoiceDialog::new();
        d.open("msg");
        d.handle_key_event(key(KeyCode::Down));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        d.handle_key_event(key(KeyCode::Char('k')));
        assert_eq!(d.choice(), QueueTarget::NextRequest);
        d.handle_key_event(key(KeyCode::Char('2')));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        d.handle_key_event(key(KeyCode::Char('1')));
        assert_eq!(d.choice(), QueueTarget::NextRequest);
    }

    #[test]
    fn enter_submits_and_esc_dismisses() {
        let mut d = QueueChoiceDialog::new();
        d.open("msg");
        assert!(!d.submitted);
        d.handle_key_event(key(KeyCode::Enter));
        assert!(d.submitted);
        assert!(d.visible);

        d.open("msg");
        d.handle_key_event(key(KeyCode::Esc));
        assert!(!d.visible);
        assert!(!d.submitted);
    }

    #[test]
    fn hidden_dialog_consumes_nothing_and_has_zero_height() {
        let mut d = QueueChoiceDialog::new();
        assert!(!d.handle_key_event(key(KeyCode::Enter)));
        assert_eq!(d.required_height(80), 0);
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(QueueChoiceDialog::truncate("hello", 40), "hello");
        assert_eq!(QueueChoiceDialog::truncate("hello world", 8), "hello w…");
        // Control characters are dropped (ratatui buffer safety).
        assert_eq!(QueueChoiceDialog::truncate("a\u{1b}[31mb", 40), "a[31mb");
    }
}
