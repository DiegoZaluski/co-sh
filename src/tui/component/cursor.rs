use std::time::SystemTime;

use ratatui::buffer::Cell;
use ratatui::style::{Color, Style};

/// Represents the current visual state of a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorState {
    /// Cursor is in its visible phase (focused, actively typing, or blink-on).
    On,
    /// Cursor is in blink-off phase (focused, but blink timer says off).
    Off,
    /// Terminal is not focused — cursor should be static and dimmed.
    Blur,
}

/// A reusable cursor component with configurable focus/blur behavior and blink timing.
///
/// The cursor tracks input activity and blink timing, returning a [`CursorState`]
/// that callers feed to [`paint_cursor_cell`] — the ONE shared look: on On the
/// cell's colors are inverted so the glyph under the caret stays visible, and on
/// Off/Blur the glyph is dimmed. All cursor sites (chat prompt, search bars,
/// dialog inputs, the RAG URL field, pickers) render through it.
///
/// # Blur behavior
///
/// When [`terminal_focused`] is `false`, [`current_state`] always returns
/// [`CursorState::Blur`]. The `App` synchronizes `terminal_focused` to all
/// cursor-owning components via its `render` method, so every cursor (chat input,
/// search bar, dialog filters) respects the terminal focus state automatically.
#[derive(Debug, Clone)]
pub struct Cursor {
    pub last_input_at: SystemTime,
    pub blink_start: SystemTime,
    pub terminal_focused: bool,
}

impl Cursor {
    pub fn new() -> Self {
        Self {
            last_input_at: SystemTime::now(),
            blink_start: SystemTime::now(),
            terminal_focused: true,
        }
    }

    /// Call this on any input activity (typing, cursor movement) to reset
    /// the blink timer and keep the cursor steady for 500 ms.
    pub fn note_activity(&mut self) {
        self.last_input_at = SystemTime::now();
        self.blink_start = SystemTime::now();
    }

    /// Returns the current [`CursorState`] based on focus and blink timing.
    ///
    /// - If `!terminal_focused` → [`CursorState::Blur`]
    /// - If idle < 500 ms (steady after typing) → [`CursorState::On`]
    /// - Otherwise: 500 ms on, 500 ms off blink cycle
    pub fn current_state(&self, now: SystemTime) -> CursorState {
        if !self.terminal_focused {
            return CursorState::Blur;
        }

        let idle_ms = now
            .duration_since(self.last_input_at)
            .map_or(0, |d| d.as_millis());

        if idle_ms < 500 {
            // Steady (user just typed)
            return CursorState::On;
        }

        let elapsed_ms = now
            .duration_since(self.blink_start)
            .map_or(0, |d| d.as_millis() % 1000);

        if elapsed_ms < 500 {
            CursorState::On
        } else {
            CursorState::Off
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// Paint a single cursor cell in the chat prompt's "transparent" style:
/// the character already on the cell keeps its glyph and gets its colors
/// inverted (text color becomes the background, background becomes the
/// foreground), so the character under the cursor stays readable.
///
/// This is the ONE shared cursor look — every component that renders a
/// `Cursor` (search bars, dialog inputs, the RAG URL field, pickers) calls
/// this instead of stamping a `█`/`▒` block glyph that would erase the
/// character beneath it.
///
/// - `On`: color inversion — the underlying glyph shows through.
/// - `Off`/`Blur`: dimmed tint — same glyph, muted foreground.
pub fn paint_cursor_cell(
    cell: &mut Cell,
    state: CursorState,
    background: Color,
    text: Color,
    muted: Color,
) {
    match state {
        CursorState::On => {
            cell.set_style(Style::default().fg(background).bg(text));
        }
        CursorState::Off | CursorState::Blur => {
            cell.set_style(Style::default().fg(muted).bg(background));
        }
    }
}
