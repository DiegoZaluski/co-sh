use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::estimate_height;
use cosh_tui::core::types::MouseEvent;

use crate::component::wall;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;

// Content definitions

/// Content for a specific free gateway recommendation.
///
/// Each gateway defines its own constant of this type. The UI dialog is
/// completely generic — it only reads these fields.
pub struct GatewayRecommendationContent {
    /// Bold title displayed in the header (e.g. "Use the OpenCode Zen free gateway?").
    pub title: &'static str,
    /// Markdown text revealed progressively during the fake streaming phase.
    /// Rendered via `MarkdownRenderable` as it grows, simulating an agent response.
    pub stream_text: &'static str,
}

/// Placeholder for future free-gateway recommendations.
///
/// The dialog itself is generic — to recommend a new gateway, define a
/// `GatewayRecommendationContent` const here (title + markdown body) and
/// return it from `App::gateway_recommendation_content()`.
///
/// Example:
/// ```ignore
/// pub const EXAMPLE: GatewayRecommendationContent = GatewayRecommendationContent {
///     title: "Use the Example free gateway?",
///     stream_text: "Description shown while streaming…",
/// };
/// ```
pub const PLACEHOLDER: GatewayRecommendationContent = GatewayRecommendationContent {
    title: "",
    stream_text: "",
};

/// Characters revealed per second during the fake streaming phase.
const CHARS_PER_SECOND: f64 = 120.0;

/// Minimum height in rows (1 padding top + 1 padding bottom).
const MIN_HEIGHT: u16 = 2;

// Streaming phase

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Text is being revealed character by character.
    Streaming,
    /// Full text shown, waiting for user to pick Yes/No.
    WaitingForResponse,
}

// Dialog

/// Generic inline dialog that recommends a free gateway.
///
/// Displays a fake streaming agent message (markdown text revealed
/// progressively), followed by a Yes/No question styled like the
/// permission dialog. Rendered inline between chat messages and the
/// prompt, mutually exclusive with `PermissionDialog`, `QuestionDialog`,
/// and `QueueChoiceDialog`.
pub struct FreeGatewayRecommendationDialog {
    pub visible: bool,
    /// `0` = Yes, `1` = No.
    pub selected: usize,
    /// Set to `true` when the user presses Enter or clicks an option.
    pub submitted: bool,
    /// The content being displayed (varies per gateway).
    content: &'static GatewayRecommendationContent,
    /// Current streaming phase.
    phase: Phase,
    /// How many *characters* of `stream_text` have been revealed so far.
    /// Tracked as a char index (not byte offset) so slicing is always on a
    /// valid char boundary even with multi-byte UTF-8 like `—` or `–`.
    stream_chars: usize,
    /// Wall-clock timestamp of the last `advance_stream()` call.
    last_frame: Instant,
}

impl FreeGatewayRecommendationDialog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            submitted: false,
            content: &PLACEHOLDER, // placeholder, overwritten by `show()`
            phase: Phase::Streaming,
            stream_chars: 0,
            last_frame: Instant::now(),
        }
    }

    /// Open the dialog for a specific recommendation.
    pub fn show(&mut self, content: &'static GatewayRecommendationContent) {
        self.content = content;
        self.visible = true;
        self.submitted = false;
        self.selected = 0;
        self.phase = Phase::Streaming;
        self.stream_chars = 0;
        self.last_frame = Instant::now();
    }

    /// Hide and reset the dialog.
    pub fn hide(&mut self) {
        self.visible = false;
        self.submitted = false;
    }

    /// Whether the streaming text is fully revealed.
    pub fn is_streaming_complete(&self) -> bool {
        self.phase == Phase::WaitingForResponse
    }

    /// Advance the streaming text by the elapsed time since the last call.
    /// Returns `true` if the offset changed (caller should redraw).
    pub fn advance_stream(&mut self) -> bool {
        if !self.visible || self.phase != Phase::Streaming {
            return false;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;

        let total = self.content.stream_text.chars().count();
        let chars_to_add = (elapsed * CHARS_PER_SECOND) as usize;
        let new_offset = (self.stream_chars + chars_to_add).min(total);

        if new_offset != self.stream_chars {
            self.stream_chars = new_offset;
            if self.stream_chars >= total {
                self.phase = Phase::WaitingForResponse;
            }
            true
        } else {
            false
        }
    }

    /// The revealed portion of the stream text (for rendering).
    /// Uses char indexing so the slice always lands on a valid UTF-8 boundary.
    fn revealed_text(&self) -> &str {
        // Find the byte offset for the stream_chars-th character.
        let byte_offset = self
            .content
            .stream_text
            .char_indices()
            .nth(self.stream_chars)
            .map_or(self.content.stream_text.len(), |(i, _)| i);
        &self.content.stream_text[..byte_offset]
    }

    // Height calculation

    /// Estimated height of the stream text portion at the given width.
    fn stream_text_height(&self, inner_w: u16) -> u16 {
        let text = self.revealed_text();
        if text.is_empty() {
            return 1;
        }
        estimate_height(text, inner_w).max(1)
    }

    /// Total required height for the dialog, including padding, text,
    /// question, options, and footer. 0 when hidden.
    pub fn required_height(&self, max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        let inner_w = max_width.saturating_sub(5); // pad(3) + 2

        let text_h = self.stream_text_height(inner_w);

        if self.phase == Phase::Streaming {
            // 1 top padding + text + cursor row + 1 bottom padding
            MIN_HEIGHT + text_h + 1
        } else {
            // 1 top padding + text + 1 gap + question(1) + options(1) + 1 bottom padding
            MIN_HEIGHT + text_h + 1 + 1 + 1
        }
    }

    // Keyboard input

    /// Handle a key event. Returns `true` if consumed.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }

        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // During streaming, skip navigation keys — only Esc is accepted.
        if self.phase == Phase::Streaming {
            return matches!(code, KeyCode::Esc);
        }

        // WaitingForResponse phase
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.selected = 0;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.selected = 1;
                true
            }
            KeyCode::Char('1') => {
                self.selected = 0;
                self.submitted = true;
                true
            }
            KeyCode::Char('2') => {
                self.selected = 1;
                self.submitted = true;
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

    // Mouse input

    /// Handle a mouse click. Returns `true` if consumed.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect, _theme: &Theme) -> bool {
        if !self.visible {
            return false;
        }

        let x = mouse.x;
        let y_click = mouse.y;
        let height = self.required_height(area.width).min(area.height);

        // Outside dialog bounds → not consumed (caller dismisses)
        if x < area.x || x >= area.x + area.width || y_click < area.y || y_click >= area.y + height
        {
            return false;
        }

        // During streaming, click anywhere inside dismisses
        if self.phase == Phase::Streaming {
            self.visible = false;
            return true;
        }

        // WaitingForResponse: hit-test the Yes/No options and footer
        let inner_x = area.x + 3;
        let _inner_w = area.width.saturating_sub(5);
        let footer_y = area.y + height.saturating_sub(2);

        // The question and options are at fixed positions relative to footer
        let question_y = footer_y.saturating_sub(2);
        let options_y = footer_y.saturating_sub(1);

        // Click on options row
        if y_click == options_y {
            // "Yes" at inner_x
            if x >= inner_x && x < inner_x + 3 {
                self.selected = 0;
                self.submitted = true;
                return true;
            }
            // "No" after gap
            let no_x = inner_x + 9; // "Yes   " (6) + "◉ " (2) + offset → simplified
            if x >= no_x && x < no_x + 2 {
                self.selected = 1;
                self.submitted = true;
                return true;
            }
        }

        // Click on footer: "enter" or "esc"
        if y_click == footer_y {
            if x >= inner_x && x < inner_x + 5 {
                // "enter"
                self.submitted = true;
                return true;
            }
            let esc_x = inner_x + 5 + 7; // "enter" + " confirm" (8)
            if x >= esc_x && x < esc_x + 3 {
                // "esc"
                self.visible = false;
                return true;
            }
        }

        // Click on question row — no action
        if y_click == question_y {
            return true;
        }

        true
    }

    // Rendering

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let height = self.required_height(area.width).min(area.height);
        let inner_area = Rect::new(area.x, area.y, area.width, height);

        wall::render(buf, inner_area, theme.accent, theme.background_panel);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let footer_y = area.y + height.saturating_sub(2);

        // Streaming text (markdown)
        let text_h = self.stream_text_height(inner_w);
        let text_y = area.y + 1;

        let content = self.revealed_text();
        if !content.is_empty() {
            let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(
                content.to_string(),
            ));
            md.set_fg(Some(cosh_tui::core::lib::rgba::ColorInput::RGBA(
                theme.text,
            )));
            md.set_bg(Some(cosh_tui::core::lib::rgba::ColorInput::RGBA(
                theme.background_panel,
            )));
            crate::util::markdown::apply_theme(&mut md, theme);
            let text_area = Rect::new(inner_x, text_y, inner_w, text_h);
            md.render_self(buf, text_area);
        }

        if self.phase == Phase::Streaming {
            // Blinking cursor below the text while streaming
            let cursor_y = text_y + text_h;
            if cursor_y < footer_y {
                let now = std::time::SystemTime::now();
                let blink = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    % 1000
                    < 500; // toggle every 500ms
                if blink {
                    let style = Style::default().fg(rgba_color(theme.text_muted));
                    draw_text_line(buf, "\u{2588}", inner_x, cursor_y, 1, style);
                }
            }
        } else {
            // Question + options
            let question_y = footer_y.saturating_sub(2);
            let options_y = footer_y.saturating_sub(1);

            // Question text
            let q_style = Style::default().fg(rgba_color(theme.text));
            draw_text_line(
                buf,
                self.content.title,
                inner_x,
                question_y,
                inner_w,
                q_style,
            );

            // Options: "Yes   No" (matching permission.rs style)
            let yes_selected = self.selected == 0;
            let no_selected = self.selected == 1;

            // Yes
            let yes_indicator = if yes_selected { "\u{1F7B4} " } else { "  " };
            let yes_style = if yes_selected {
                Style::default().fg(rgba_color(theme.success))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, yes_indicator, inner_x, options_y, 2, yes_style);
            let yes_label_style = if yes_selected {
                Style::default().fg(rgba_color(theme.success))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, "Yes", inner_x + 2, options_y, 3, yes_label_style);

            // No
            let no_x = inner_x + 9; // indicator(2) + "Yes"(3) + gap(4)
            let no_indicator = if no_selected { "\u{1F7B4} " } else { "  " };
            let no_style = if no_selected {
                Style::default().fg(rgba_color(theme.error))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, no_indicator, no_x, options_y, 2, no_style);
            let no_label_style = if no_selected {
                Style::default().fg(rgba_color(theme.error))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, "No", no_x + 2, options_y, 2, no_label_style);

            // Footer
            let bg = rgba_color(theme.background_panel);
            let key_fg = rgba_color(theme.text);
            let desc_fg = rgba_color(theme.text_muted);

            draw_text_line(
                buf,
                "enter",
                inner_x,
                footer_y,
                inner_w,
                Style::default().fg(key_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " confirm",
                inner_x + 5,
                footer_y,
                inner_w.saturating_sub(5),
                Style::default().fg(desc_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " esc",
                inner_x + 14,
                footer_y,
                inner_w.saturating_sub(14),
                Style::default().fg(key_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " dismiss",
                inner_x + 18,
                footer_y,
                inner_w.saturating_sub(18),
                Style::default().fg(desc_fg).bg(bg),
            );
        }
    }
}

// Helpers

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample content for exercising the generic dialog (the shipped
    /// `PLACEHOLDER` is intentionally empty).
    const SAMPLE: GatewayRecommendationContent = GatewayRecommendationContent {
        title: "Use the sample free gateway?",
        stream_text: "Sample recommendation body used to exercise the generic dialog. \
            It spans several sentences so streaming advances over multiple frames.",
    };

    #[test]
    fn hidden_dialog_has_zero_height() {
        let d = FreeGatewayRecommendationDialog::new();
        assert_eq!(d.required_height(80), 0);
    }

    #[test]
    fn streaming_height_includes_cursor() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        assert!(d.visible);
        let h = d.required_height(80);
        assert!(h >= MIN_HEIGHT + 2); // text(1+) + cursor(1) + padding(2)
    }

    #[test]
    fn response_height_includes_options_and_footer() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        // Force streaming complete
        d.phase = Phase::WaitingForResponse;
        d.stream_chars = d.content.stream_text.chars().count();
        let h = d.required_height(80);
        // text + gap + question + options + padding
        assert!(h > MIN_HEIGHT + 1 + 1);
    }

    #[test]
    fn advance_stream_progresses_offset() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        assert_eq!(d.stream_chars, 0);
        // Simulate some time passing
        d.last_frame = Instant::now() - std::time::Duration::from_millis(500);
        let changed = d.advance_stream();
        assert!(changed);
        assert!(d.stream_chars > 0);
    }

    #[test]
    fn advance_stream_completes_after_enough_time() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        let total = d.content.stream_text.chars().count();
        // Simulate enough time for all characters
        let secs = total as f64 / CHARS_PER_SECOND + 0.1;
        d.last_frame = Instant::now() - std::time::Duration::from_secs_f64(secs);
        d.advance_stream();
        assert_eq!(d.phase, Phase::WaitingForResponse);
        assert_eq!(d.stream_chars, total);
    }

    #[test]
    fn enter_submits_and_esc_dismisses() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.phase = Phase::WaitingForResponse;
        d.stream_chars = d.content.stream_text.chars().count();

        assert!(!d.submitted);
        d.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(d.submitted);

        let mut d2 = FreeGatewayRecommendationDialog::new();
        d2.show(&SAMPLE);
        d2.phase = Phase::WaitingForResponse;
        d2.stream_chars = d2.content.stream_text.chars().count();
        d2.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!d2.visible);
    }

    #[test]
    fn hidden_dialog_consumes_nothing() {
        let mut d = FreeGatewayRecommendationDialog::new();
        assert!(!d.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }
}
