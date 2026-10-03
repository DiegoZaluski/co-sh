use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use cosh_tui::core::lib::border::BorderSidesConfig;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::styled_text::string_to_styled_text;
use cosh_tui::core::lib::unicode_util;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::types::TextAttributes;

use crate::theme::{Theme, rgba_color};

#[derive(Debug, Clone)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastVariant {
    const fn theme_color(&self, theme: &Theme) -> RGBA {
        match self {
            Self::Info => theme.info,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Error => theme.error,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToastOptions {
    pub title: Option<String>,
    pub message: String,
    pub variant: ToastVariant,
    pub duration_ms: u64,
}

impl Default for ToastOptions {
    fn default() -> Self {
        Self {
            title: None,
            message: String::new(),
            variant: ToastVariant::Info,
            duration_ms: 5000,
        }
    }
}

pub struct ToastState {
    pub current: Option<ToastOptions>,
    pub elapsed: u64,
    /// Copy payload of the current toast (`Some` → the ⧉ button is drawn
    /// and clickable). Set only by [`Self::show_copyable`]; cleared when the
    /// slot changes hands or the toast is dismissed.
    copy_text: Option<String>,
    /// Hitbox of the ⧉ button from the last render (`None` when not drawn).
    /// Cleared whenever the box is not rendered (stale-hitbox guard, same
    /// contract as the header/footer link areas).
    copy_button_area: Option<Rect>,
    /// Whether the current/last toast is a REAL (event-driven)
    /// toast, so per-frame hover tooltips must not take it over. Cleared in
    /// `tick()` at the exact moment the toast is dismissed, so suppression
    /// and visibility always end together — no wall-clock/frame-tick drift.
    real_toast_showing: bool,
}

impl ToastState {
    pub const fn new() -> Self {
        Self {
            current: None,
            elapsed: 0,
            copy_text: None,
            copy_button_area: None,
            real_toast_showing: false,
        }
    }

    /// Shared slot assignment for both toast kinds.
    fn set_current(&mut self, options: ToastOptions) {
        self.current = Some(options);
        self.elapsed = 0;
        // A fresh toast never inherits the previous one's copy affordance:
        // the ⧉ button belongs to the toast that asked for it.
        self.copy_text = None;
        self.copy_button_area = None;
    }

    pub fn show(&mut self, options: ToastOptions) {
        // A real toast reserves the slot for its whole display window: while
        // it shows, per-frame hover tooltips must not take it over.
        self.real_toast_showing = true;
        self.set_current(options);
    }

    /// Show a real toast carrying a copy payload: the ⧉ button in its top
    /// row copies `copy_text` to the system clipboard when clicked (for
    /// long strings — install commands, URLs — that are impractical to
    /// retype). Everything else behaves exactly like [`Self::show`].
    pub fn show_copyable(&mut self, options: ToastOptions, copy_text: String) {
        self.show(options);
        self.copy_text = Some(copy_text);
    }

    /// Hit-test a click against the ⧉ button's last-rendered hitbox.
    /// Returns the toast's copy payload on a hit, `None` otherwise.
    pub fn copy_button_hit(&self, x: u16, y: u16) -> Option<String> {
        let area = self.copy_button_area?;
        let inside = x >= area.x && x < area.right() && y >= area.y && y < area.bottom();
        inside.then(|| self.copy_text.clone()).flatten()
    }

    /// Show a low-priority hover tooltip.
    ///
    /// The tooltip re-fires every frame while the pointer rests on an item,
    /// so without arbitration it instantly overwrites any real (event-driven)
    /// toast shown by an action under that same pointer — e.g. clicking a
    /// session locked by another process. While a real toast is showing, the
    /// tooltip is dropped; once the slot is free the tooltip shows as usual.
    pub fn show_tooltip(&mut self, options: ToastOptions) {
        if self.real_toast_showing {
            return;
        }
        // Deliberately NOT via `show`: a tooltip must not reserve the slot
        // against later tooltips (hovering a second item right after the
        // first must still show its own tooltip).
        self.set_current(options);
    }

    pub fn tick(&mut self, dt: u64) {
        if let Some(ref toast) = self.current {
            self.elapsed += dt;
            if self.elapsed >= toast.duration_ms {
                self.current = None;
                self.elapsed = 0;
                // The slot frees exactly when the toast leaves the screen.
                self.real_toast_showing = false;
                // A toast that is gone can no longer own a clickable ⧉: a
                // stale hitbox must never survive the dismissal.
                self.copy_button_area = None;
            }
        }
    }

    /// Render the toast. `mouse` (current pointer position, when tracked)
    /// drives the ⧉ copy button's hover highlight; the button's hitbox is
    /// re-registered every frame and cleared whenever the box is not drawn.
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        mouse: Option<(u16, u16)>,
    ) {
        // Stale-hitbox guard FIRST: every path that returns without drawing
        // the box must leave no clickable ⧉ behind (same contract as the
        // header/footer link areas).
        self.copy_button_area = None;
        let Some(toast) = self.current.clone() else { return };
        let copy_text = self.copy_text.clone();

        // Width hugs the widest content line (plus horizontal padding). The
        // old fixed 60-column cap is gone: the clamp now follows the SCREEN,
        // so a long copyable payload — the manual install command — stays
        // INLINE on one row whenever the terminal is wide enough to hold it,
        // and the box simply grows wider as needed. Wrapping is only the
        // fallback for terminals too narrow to hold the line.
        let padding_x = 2u16;
        let max_w = area.width.saturating_sub(6);
        // A copyable toast reserves one cell + one gap at the title row's
        // right edge for the ⧉ button, so a long title can never run under it.
        let button_reserve: usize = if copy_text.is_some() { 2 } else { 0 };
        let title_w = toast
            .title
            .as_deref()
            .map(|t| unicode_util::str_display_width(t) + button_reserve)
            .unwrap_or(0);

        // Long messages (e.g. the manual install command) are word-wrapped
        // over MULTIPLE rows: a message wider than the cap must never be
        // clipped into an unreadable first fragment. Lines are computed at
        // the inner width so every wrapped line fits whatever box width the
        // hug produces; the box height grows to hold them all.
        let wrap_w = max_w.saturating_sub(padding_x * 2).max(8) as usize;
        let msg_lines = wrap_message(&toast.message, wrap_w);
        let msg_longest = msg_lines
            .iter()
            .map(|l| unicode_util::str_display_width(l))
            .max()
            .unwrap_or(0);
        let content_w = title_w.max(msg_longest);
        let toast_w = content_w
            .saturating_add((padding_x * 2) as usize)
            .min(max_w as usize) as u16;
        let toast_x = area.right().saturating_sub(toast_w + 2);
        let toast_y = area.y + 2;

        // One row per wrapped message line, plus the title row (or the
        // blank centering row when there is no title), plus the BOTTOM
        // PADDING row — part of the original toast design (the old fixed
        // `inner_h = 3` was title + message + 1 blank row), never dropped
        // when the box grows. The box never grows past the screen bottom:
        // extra lines are dropped instead.
        const BOTTOM_PAD_ROWS: u16 = 1;
        let title_rows: u16 = 1;
        let max_rows = area
            .bottom()
            .saturating_sub(toast_y)
            .saturating_sub(1) as usize;
        let shown_lines: Vec<String> = msg_lines
            .into_iter()
            .take(
                max_rows
                    .saturating_sub(title_rows as usize)
                    .saturating_sub(BOTTOM_PAD_ROWS as usize),
            )
            .collect();
        let inner_h = title_rows + shown_lines.len() as u16 + BOTTOM_PAD_ROWS;

        let border_color = toast.variant.theme_color(theme);
        let mut bg = BoxRenderable::new();
        bg.set_background_color(Some(theme.background_panel.into()));
        bg.set_border_color(Some(border_color.into()));
        bg.set_border_sides(BorderSidesConfig {
            left: true,
            right: true,
            top: false,
            bottom: false,
        });

        let toast_area = Rect::new(toast_x, toast_y, toast_w, inner_h);
        bg.render_self(buf, toast_area);

        let inner_w = toast_w.saturating_sub(padding_x * 2);

        let mut y = toast_y;
        if let Some(ref title) = toast.title {
            let mut t = TextRenderable::new(Some(string_to_styled_text(title)));
            t.set_fg(theme.text);
            t.set_bg(theme.background_panel);
            t.set_attributes(TextAttributes::BOLD.bits());
            let title_area = Rect::new(toast_x + padding_x, y, inner_w, 1);
            t.render_self(buf, title_area);
            y += 1;
        } else {
            // No title: skip one line so the message appears centered.
            y += 1;
        }
        // ⧉ copy button — top row, right-aligned inside the padding. Drawn
        // only for copyable toasts; hovered it lights up (muted → text),
        // the same affordance the header/footer links use.
        if copy_text.is_some() {
            let button_x = toast_x + toast_w - padding_x - 1;
            let hovered = mouse.is_some_and(|(mx, my)| {
                mx >= button_x && mx < button_x + 1 && my >= toast_y && my < toast_y + 1
            });
            let fg = if hovered { theme.text } else { theme.text_muted };
            if let Some(cell) = buf.cell_mut((button_x, toast_y)) {
                cell.set_char('\u{29c9}');
                // BOLD on the button only (user request): a clickable glyph
                // must stand out from the surrounding text even before any
                // hover, where color alone would be the only cue.
                cell.set_style(
                    ratatui::style::Style::default()
                        .fg(rgba_color(fg))
                        .bg(rgba_color(theme.background_panel))
                        .add_modifier(ratatui::style::Modifier::BOLD),
                );
            }
            self.copy_button_area = Some(Rect::new(button_x, toast_y, 1, 1));
        }

        // Message — each pre-wrapped line gets its own 1-row renderable, so
        // the TextRenderable's internal wrap can never clip content: every
        // character the wrap produced is drawn on its own dedicated row.
        for line in &shown_lines {
            let mut msg = TextRenderable::new(Some(string_to_styled_text(line)));
            msg.set_fg(theme.text);
            msg.set_bg(theme.background_panel);
            let msg_area = Rect::new(toast_x + padding_x, y, inner_w, 1);
            msg.render_self(buf, msg_area);
            y += 1;
        }
    }
}

/// Word-wrap `text` into lines of at most `max_cols` display columns
/// (unicode-aware via the crate's shared width util). Words longer than a
/// whole line are hard-split; newlines in the source force a break. Shared
/// by the toast renderer so long copy payloads (install commands) stay
/// fully readable instead of being clipped to the box's first row.
fn wrap_message(text: &str, max_cols: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;

    let push_line = |lines: &mut Vec<String>, line: &mut String| {
        lines.push(std::mem::take(line));
    };

    for paragraph in text.split('\n') {
        for word in paragraph.split(' ') {
            if word.is_empty() {
                continue;
            }
            let w = unicode_util::str_display_width(word);
            if w > max_cols {
                // Hard-split an oversized word (e.g. an unbroken URL).
                if !line.is_empty() {
                    push_line(&mut lines, &mut line);
                }
                let mut chunk = String::new();
                let mut chunk_w = 0usize;
                for (grapheme, gw) in unicode_util::graphemes_with_width(word) {
                    let gw = gw as usize;
                    if chunk_w + gw > max_cols {
                        lines.push(std::mem::take(&mut chunk));
                        chunk_w = 0;
                    }
                    chunk.push_str(grapheme);
                    chunk_w += gw;
                }
                line = chunk;
                line_w = chunk_w;
                continue;
            }
            // Overflow check must NOT include a separator computed from the
            // pre-push state: when the line is about to be pushed it is
            // empty afterwards, so no separator will actually be written.
            if line_w + usize::from(!line.is_empty()) + w > max_cols {
                push_line(&mut lines, &mut line);
                line_w = 0;
            }
            // Separator decided on the POST-push state (the same word can
            // trigger the push above and then start the fresh line).
            let sep_w = usize::from(!line.is_empty());
            if sep_w == 1 {
                line.push(' ');
            }
            line.push_str(word);
            line_w += sep_w + w;
        }
        // A source newline forces a line break (the doc contract): each
        // paragraph ends its own wrapped line, never joined by spaces.
        if !line.is_empty() {
            push_line(&mut lines, &mut line);
            line_w = 0;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

impl Default for ToastState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(duration_ms: u64) -> ToastOptions {
        ToastOptions {
            title: Some("t".to_string()),
            message: "m".to_string(),
            variant: ToastVariant::Info,
            duration_ms,
        }
    }

    /// Regression: during the agent loop frames can be far apart (streaming
    /// render throttling, blocking draws) and `handle_events()` may not run
    /// at all between them. The toast must dismiss once the CUMULATIVE real
    /// deltas reach `duration_ms`, no matter how sparse the ticks are.
    #[test]
    fn dismisses_after_cumulative_real_deltas() {
        let mut state = ToastState::new();
        state.show(toast(5_000));

        // Streaming-like frames: ~30 fps with occasional multi-hundred-ms
        // draw stalls. Each tick stays below the duration, so the toast
        // must remain visible after every one of them.
        for dt in [33, 34, 300, 33, 34, 200, 33] {
            state.tick(dt);
            assert!(
                state.current.is_some(),
                "toast must outlive a {dt} ms tick below its duration"
            );
        }

        // 667 ms elapsed so far — crossing the threshold dismisses it.
        state.tick(4_333);
        assert!(state.current.is_none(), "toast must dismiss at duration");
        assert_eq!(state.elapsed, 0, "elapsed resets on dismissal");
    }

    /// A new toast replaces the previous one and its lifetime restarts
    /// from zero (never inherits the previous toast's elapsed time).
    #[test]
    fn show_resets_elapsed() {
        let mut state = ToastState::new();
        state.show(toast(1_000));
        state.tick(900);

        state.show(toast(1_000));
        assert_eq!(state.elapsed, 0);

        state.tick(999);
        assert!(state.current.is_some(), "fresh toast must outlive 999 ms");
        state.tick(1);
        assert!(state.current.is_none());
    }

    /// Ticking with no active toast is a no-op (keeps elapsed at zero).
    #[test]
    fn tick_without_toast_is_noop() {
        let mut state = ToastState::new();
        state.tick(10_000);
        assert!(state.current.is_none());
        assert_eq!(state.elapsed, 0);
    }

    fn tooltip_options(message: &str) -> ToastOptions {
        ToastOptions {
            title: None,
            message: message.to_string(),
            variant: ToastVariant::Info,
            duration_ms: 3_000,
        }
    }

    /// A real (event-driven) toast — e.g. "session open in another cosh
    /// process" after a refused click — must survive the per-frame tooltip
    /// re-fire while the pointer rests on the same row.
    #[test]
    fn tooltip_cannot_overwrite_a_showing_real_toast() {
        let mut state = ToastState::new();
        let mut blocked = toast(4_000);
        blocked.message = "blocked".to_string();
        state.show(blocked);

        state.show_tooltip(tooltip_options("session 3"));
        assert_eq!(
            state.current.as_ref().unwrap().message,
            "blocked",
            "the per-frame tooltip must not take the slot of a showing real toast"
        );
    }

    /// Once the real toast expires, the tooltip slot frees up: hovering the
    /// same row shows the title tooltip again.
    #[test]
    fn tooltip_returns_after_the_real_toast_expires() {
        let mut state = ToastState::new();
        let mut blocked = toast(50);
        blocked.message = "blocked".to_string();
        state.show(blocked);

        state.show_tooltip(tooltip_options("hover title"));
        assert_ne!(state.current.as_ref().unwrap().message, "hover title");

        // Tick the real toast past its duration: the slot must free exactly
        // when the toast is dismissed.
        state.tick(51);
        assert!(state.current.is_none(), "the real toast must be gone");
        state.show_tooltip(tooltip_options("hover title"));
        assert_eq!(
            state.current.as_ref().unwrap().message,
            "hover title",
            "the tooltip must return once the real toast is dismissed"
        );
    }

    /// Tooltips never suppress each other: hovering a second item right
    /// after the first swaps the tooltip. And a real toast still outranks a
    /// showing tooltip.
    #[test]
    fn tooltips_do_not_suppress_each_other_but_real_toasts_win() {
        let mut state = ToastState::new();
        state.show_tooltip(tooltip_options("a"));
        state.show_tooltip(tooltip_options("b"));
        assert_eq!(state.current.as_ref().unwrap().message, "b");

        state.show(toast(4_000));
        assert_eq!(state.current.as_ref().unwrap().message, "m");
    }

    /// Before any real toast has been shown, tooltips behave exactly as
    /// before (no suppression state to trip over).
    #[test]
    fn tooltip_shows_without_prior_real_toast() {
        let mut state = ToastState::new();
        state.show_tooltip(tooltip_options("first"));
        assert_eq!(state.current.as_ref().unwrap().message, "first");
    }

    /// The toast box hugs its content instead of using the old hardcoded
    /// 60-column width: the text column position reveals the box geometry
    /// (text x = toast_x + padding, toast_x = area.right() - (w + margin)).
    #[test]
    fn width_fits_content() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 10);

        // Short message: content_w = 1, padding 2*2, right margin 2 →
        // toast_w = 5, box starts at column 73, text at 75.
        let mut state = ToastState::new();
        state.show(toast(5_000)); // title "t", message "m"
        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);
        let msg_col = (0..area.width)
            .find(|&x| buf[(x, 3)].symbol() == "m")
            .expect("message rendered");
        assert_eq!(msg_col, 75, "short toast must hug its content");

        // A message wider than the SCREEN still wraps (the clamp can only
        // grow to the available width): on an 80-col terminal the 100-char
        // message exceeds max_w = 74, so it hard-splits. Box hugs the cap:
        // toast_x = 80 - 74 - 2 = 4, text at 6.
        let mut long = toast(5_000);
        long.message = "x".repeat(100);
        let mut state = ToastState::new();
        state.show(long);
        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);
        let msg_col = (0..area.width)
            .find(|&x| buf[(x, 3)].symbol() == "x")
            .expect("long message rendered");
        assert_eq!(msg_col, 6, "screen-wide clamp: box hugs max_w on 80 cols");
    }

    /// On a terminal wide enough to hold the whole manual install command,
    /// the message must stay INLINE on a single row — no wrapping. The box
    /// simply grows wider (the clamp follows the screen, there is no fixed
    /// 60-column cap anymore).
    #[test]
    fn long_command_stays_inline_on_wide_screens() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 130, 12);
        let mut state = ToastState::new();
        let mut cmd = toast(5_000);
        cmd.message =
            "curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/co-sh/v0.1.2/download.sh | COSH_VERSION=v0.1.2 bash"
                .into();
        state.show(cmd);

        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);

        // Every row of the screen as a string.
        let rows: Vec<String> = (0..area.height)
            .map(|yy| {
                (0..area.width)
                    .map(|xx| buf[(xx, yy)].symbol())
                    .collect::<String>()
            })
            .collect();

        // The row holding the tail (`bash`) must ALSO hold the head
        // (`curl`) and the middle (`COSH_VERSION=…`): one contiguous line,
        // the whole command inline.
        let tail_row = rows
            .iter()
            .find(|r| r.contains("bash"))
            .expect("command tail must be rendered");
        assert!(
            tail_row.contains("curl -fsSL") && tail_row.contains("COSH_VERSION=v0.1.2"),
            "the whole command must sit on ONE row on a wide screen: {tail_row:?}"
        );
        // And the box must have exactly one message row (title + 1 line).
        let msg_rows = rows
            .iter()
            .filter(|r| r.contains("curl -fsSL"))
            .count();
        assert_eq!(msg_rows, 1, "the command must not be split across rows");
    }

    /// Whether any cell of `buf` holds the ⧉ glyph.
    fn has_copy_glyph(buf: &Buffer, area: Rect) -> bool {
        (0..area.width).any(|x| (0..area.height).any(|y| buf[(x, y)].symbol() == "⧉"))
    }

    /// The (x, y) of the ⧉ glyph, if drawn.
    fn find_copy_glyph(buf: &Buffer, area: Rect) -> Option<(u16, u16)> {
        (0..area.width)
            .find_map(|x| (0..area.height).find(|&y| buf[(x, y)].symbol() == "⧉").map(|y| (x, y)))
    }

    /// A copyable toast draws the ⧉ button in its top row (title row for a
    /// titled toast, first content row otherwise) and registers a hitbox
    /// covering exactly that cell. The hit-test returns the payload inside
    /// the hitbox and `None` outside it.
    #[test]
    fn copy_button_draws_and_hits() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 10);
        let mut state = ToastState::new();
        state.show_copyable(
            toast(5_000),
            "curl -fsSL https://example.com/install.sh | bash".into(),
        );

        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);

        let hitbox = state.copy_button_hit(0, 0); // warm the field existence check
        assert!(hitbox.is_none(), "no hit outside the button");
        // Locate the ⧉ glyph: it must be inside the toast box (x > toast_x).
        let (bx, by) = find_copy_glyph(&buf, area).expect("copy button glyph rendered");
        // The glyph row must be the toast's top row (y == area.y + 2).
        assert_eq!(by, area.y + 2, "⧉ sits on the toast's top row");

        let payload = state
            .copy_button_hit(bx, by)
            .expect("clicking the ⧉ cell returns the copy payload");
        assert!(payload.starts_with("curl -fsSL"), "payload is the command");
        // One cell right of the button: no hit.
        assert!(state.copy_button_hit(bx + 1, by).is_none());
        assert!(state.copy_button_hit(bx, by + 1).is_none());
    }

    /// A plain (non-copyable) toast never draws ⧉ and never registers a
    /// hitbox — and a NEW plain toast after a copyable one must not inherit
    /// the button (the copy affordance belongs to the toast that asked).
    #[test]
    fn plain_toasts_have_no_copy_button() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);

        let mut state = ToastState::new();
        state.show(toast(5_000));
        state.render(&mut buf, area, &theme, None);
        assert!(state.copy_button_hit(0, 0).is_none());
        assert!(
            !has_copy_glyph(&buf, area),
            "plain toast must not draw the copy button"
        );

        // Copyable first, then a plain replacement: the ⧉ must be gone.
        state.show_copyable(toast(5_000), "payload".into());
        state.render(&mut buf, area, &theme, None);
        assert!(
            has_copy_glyph(&buf, area),
            "copyable toast draws the button (sanity for the replacement check)"
        );
        state.show(toast(5_000));
        state.render(&mut buf, area, &theme, None);
        assert!(
            !has_copy_glyph(&buf, area),
            "replacement plain toast must not keep the copy button"
        );
    }

    /// When the toast is dismissed (tick past duration), the ⧉ hitbox is
    /// cleared — a toast that is gone can never accept a click.
    #[test]
    fn dismissal_clears_the_copy_hitbox() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 10);
        let mut state = ToastState::new();
        state.show_copyable(toast(100), "payload".into());
        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);

        state.tick(101);
        assert!(state.current.is_none());
        assert!(state.copy_button_hit(0, 0).is_none());
        // And a fresh render with no toast re-registers nothing.
        state.render(&mut buf, area, &theme, None);
        assert!(state.copy_button_hit(0, 0).is_none());
    }

    /// Regression: a message wider than the terminal (e.g. the manual
    /// install command, ~110 columns, on an 80-col screen) must NEVER lose
    /// characters to clipping — it wraps onto multiple rows instead. The
    /// box's interior must therefore contain exactly the message's
    /// characters (spaces aside), and the wrap must actually occupy
    /// multiple rows.
    #[test]
    fn narrow_screen_wraps_without_losing_characters() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 12);
        let mut state = ToastState::new();
        let message =
            "curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/co-sh/v0.1.2/download.sh | COSH_VERSION=v0.1.2 bash"
                .to_string();
        let mut cmd = toast(5_000);
        cmd.message = message.clone();
        state.show(cmd);

        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme, None);

        // Locate the box borders: columns holding `│` on several rows.
        let border_cols: Vec<u16> = (0..area.width)
            .filter(|&xx| {
                (0..area.height)
                    .filter(|&yy| buf[(xx, yy)].symbol() == "│")
                    .count()
                    >= 2
            })
            .collect();
        let (left, right) = (
            *border_cols.first().expect("left border column"),
            *border_cols.last().expect("right border column"),
        );

        // Interior text of the MESSAGE rows only: the title row (the toast's
        // top row, area.y + 2) holds the toast TITLE — its characters must
        // not pollute the conservation check against the message.
        let msg_start_y = area.y + 3;
        let mut rendered = String::new();
        for yy in msg_start_y..area.height {
            for xx in (left + 1)..right {
                rendered.push_str(buf[(xx, yy)].symbol());
            }
        }
        // Character conservation (the old bug dropped everything past the
        // first clipped row): the box must hold exactly the message's
        // non-space characters — no more, no less.
        let mut got: Vec<char> = rendered.chars().filter(|c| !c.is_whitespace()).collect();
        let mut want: Vec<char> = message.chars().filter(|c| !c.is_whitespace()).collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "wrap must conserve every message character");

        // And the wrap must actually span multiple rows (title row + >= 2
        // message rows) — the command cannot fit one 80-col row.
        let rows_with_content = (0..area.height)
            .filter(|&yy| {
                (0..area.width)
                    .any(|xx| {
                        let c = buf[(xx, yy)].symbol();
                        !c.trim().is_empty() && c != "│"
                    })
            })
            .count();
        assert!(
            rows_with_content >= 3,
            "a too-wide command must wrap over several rows, got {rows_with_content}"
        );
    }

    /// Unit coverage for `wrap_message`: word wrapping respects the column
    /// budget, an unbroken URL longer than the budget is hard-split, and a
    /// single short line is passed through untouched.
    #[test]
    fn wrap_message_respects_the_column_budget() {
        // Word wrap: no line exceeds the budget.
        let lines = wrap_message("aa bb cc dd ee ff gg", 5);
        assert_eq!(lines, vec!["aa bb", "cc dd", "ee ff", "gg"]);

        // Hard split of an oversized token.
        let lines = wrap_message("aaaaaaaaaaaa", 5);
        assert_eq!(lines, vec!["aaaaa", "aaaaa", "aa"]);

        // Short input passes through as one line.
        assert_eq!(wrap_message("short", 60), vec!["short"]);

        // A source newline forces a line break (doc contract).
        assert_eq!(wrap_message("line one\nline two", 60), vec!["line one", "line two"]);
    }
}
