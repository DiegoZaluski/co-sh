//! Animated agent spinner — the cosh classic braille spinner.
//!
//! Wraps the shared [`SPINNER_FRAMES`] braille table (`component/spinner.rs`)
//! — the `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` wheel every in-app busy indicator speaks — as a
//! `/spinner` style: a single spinning glyph in the theme's primary colour,
//! followed by the label and the per-activity status glyph animation.
//!
//! The original `SpinnerState` advances one frame every 2 ticks; this
//! wrapper keeps exactly that pacing on the shared spinner surface (label
//! swap per activity, mode colour, theme updates, deterministic render).

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::component::spinner::SPINNER_FRAMES;
use crate::theme::{Theme, rgba_color};

use super::agent_spinner::activity_frames;

// ── Constants ──────────────────────────────────────────────────────────────

/// Frames per braille step — `SpinnerState::advance` flips the frame every
/// 2 ticks.
const TICKS_PER_FRAME: u32 = 2;

/// Frames the shared spinner surface needs before it reports initialized
/// (the birth window every `/spinner` style observes).
const INIT_FRAMES: u32 = 20;

/// Status glyph animation speed: number of ticks per glyph step.
const STATUS_ANIM_SPEED: u32 = 8;

// ── AgentSpinnerClassic ────────────────────────────────────────────────────

/// An animated spinner cycling the classic braille wheel in the theme's
/// primary colour, followed by a label and a per-status glyph animation.
pub struct AgentSpinnerClassic {
    /// Animation ticks elapsed since start/reset.
    ticks_elapsed: u32,
    /// Current status-glyph step (ticks at `STATUS_ANIM_SPEED`).
    status_step: u32,
    /// Glyph sequence of the current agent activity (see
    /// [`activity_frames`]): each status cycles its own animation.
    frames: &'static [&'static str],
    /// Whether the birth window has elapsed.
    initialized: bool,
    /// Label text displayed after the spinner glyph.
    label: String,
    /// Number of characters in the label.
    label_width: usize,
    /// The spinner glyph colour (the active prompt mode colour).
    glyph_color: RGBA,
    /// Colour used for the label and status glyph.
    label_color: RGBA,
}

impl AgentSpinnerClassic {
    /// Create a new classic spinner with the given label, sourcing the
    /// glyph colour from `theme.primary` and the label colour from
    /// `theme.text_muted`.
    pub fn new(label: &str, theme: &Theme) -> Self {
        Self {
            ticks_elapsed: 0,
            status_step: 0,
            frames: activity_frames(crate::types::AgentActivity::Working),
            initialized: false,
            label: label.to_string(),
            label_width: label.chars().count(),
            glyph_color: theme.primary,
            label_color: theme.text_muted,
        }
    }

    /// Advance the animation by one 30 fps tick (the braille wheel flips
    /// every 2 ticks, exactly like `SpinnerState::advance`).
    pub fn advance(&mut self) {
        self.ticks_elapsed = self.ticks_elapsed.saturating_add(1);

        if !self.initialized && self.ticks_elapsed >= INIT_FRAMES {
            self.initialized = true;
        }

        // Advance the status glyphs (only after initialization and if
        // there's a label): cycle the CURRENT activity's sequence.
        if self.initialized && self.label_width > 0 {
            self.status_step =
                (self.status_step + 1) % (STATUS_ANIM_SPEED * self.frames.len() as u32);
        }
    }

    /// Reset the spinner to its initial state.
    pub const fn reset(&mut self) {
        self.ticks_elapsed = 0;
        self.status_step = 0;
        self.initialized = false;
    }

    /// Change the label text (width is recomputed).
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.label_width = label.chars().count();
    }

    /// Switch the label to the given agent activity and start that status's
    /// own glyph animation from its first frame. The wheel phase is
    /// untouched; a repeated activity is a cheap no-op.
    pub fn set_activity(&mut self, activity: crate::types::AgentActivity) {
        if self.label != activity.label() {
            self.set_label(activity.label());
            self.frames = activity_frames(activity);
            self.status_step = 0;
        }
    }

    /// Update the wheel colour for the active prompt mode without resetting
    /// the animation state.
    pub fn set_mode_color(&mut self, color: RGBA) {
        self.glyph_color = color;
    }

    /// Update the theme colours (wheel and label colour) without resetting
    /// the animation state.
    pub fn update_theme(&mut self, theme: &Theme) {
        self.glyph_color = theme.primary;
        self.label_color = theme.text_muted;
    }

    /// Total width in terminal cells (glyph + gap + label + status glyph).
    pub fn width(&self) -> usize {
        let mut w = 1; // the braille glyph
        if self.label_width > 0 {
            w += 1; // gap
            w += self.label_width;
            w += self
                .frames
                .iter()
                .map(|f| f.chars().count())
                .max()
                .unwrap_or(0);
        }
        w
    }

    /// Whether the birth window has elapsed.
    pub const fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Current braille frame index (one step every `TICKS_PER_FRAME`).
    pub const fn wheel_index(&self) -> usize {
        (self.ticks_elapsed / TICKS_PER_FRAME) as usize % SPINNER_FRAMES.len()
    }

    /// Test-only view of the current label (the field is private).
    #[cfg(test)]
    pub(crate) fn label_for_test(&self) -> &str {
        &self.label
    }

    #[cfg(test)]
    pub(crate) fn status_frame(&self) -> &'static str {
        let idx = (self.status_step / STATUS_ANIM_SPEED) as usize % self.frames.len();
        self.frames[idx]
    }

    #[cfg(test)]
    pub(crate) fn status_x_for_test(&self) -> u16 {
        1 + 1 + self.label_width as u16
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)`.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        // 1. The braille wheel, in the prompt-mode colour.
        let ch = SPINNER_FRAMES[self.wheel_index()];
        let wheel_style = Style::default().fg(rgba_color(self.glyph_color));
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(wheel_style);
        }

        // 2. Gap between wheel and label.
        if self.label_width > 0 {
            let gap_x = x.saturating_add(1);
            if let Some(cell) = buf.cell_mut((gap_x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }

        // 3. Label text.
        if self.label_width > 0 {
            let label_x = x.saturating_add(2);
            let label_style = Style::default().fg(rgba_color(self.label_color));
            for (i, ch) in self.label.chars().enumerate() {
                let cell_x = label_x.saturating_add(i as u16);
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(label_style);
                }
            }
        }

        // 4. Status glyph animation (only after the birth window). The slot
        // is always padded to the widest frame of the CURRENT sequence: a
        // narrower frame must clear stale cells instead of ghosting them.
        if self.initialized && self.label_width > 0 {
            let glyph_idx = (self.status_step / STATUS_ANIM_SPEED) as usize % self.frames.len();
            let glyph_text = self.frames[glyph_idx];
            let glyph_max = self
                .frames
                .iter()
                .map(|f| f.chars().count())
                .max()
                .unwrap_or(0);
            let glyph_x = x
                .saturating_add(2)
                .saturating_add(self.label_width as u16);
            let glyph_style = Style::default().fg(rgba_color(self.label_color));
            let mut written = 0usize;
            for (i, ch) in glyph_text.chars().enumerate() {
                let cell_x = glyph_x.saturating_add(i as u16);
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(glyph_style);
                }
                written += 1;
            }
            for i in written..glyph_max {
                let cell_x = glyph_x.saturating_add(i as u16);
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        let registry = crate::theme::ThemeRegistry::new();
        registry.get("opencode").cloned().unwrap()
    }

    #[test]
    fn wheel_flips_every_two_ticks_like_spinner_state() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerClassic::new("Working", &theme);
        // Tick 1: still on frame 0 (tick_counter < 2).
        spinner.advance();
        assert_eq!(spinner.wheel_index(), 0);
        // Tick 2: SpinnerState advances its frame.
        spinner.advance();
        assert_eq!(spinner.wheel_index(), 1);
        // The wrapper cycles the exact shared frame table.
        for _ in 0..(TICKS_PER_FRAME * SPINNER_FRAMES.len() as u32) {
            spinner.advance();
        }
        assert_eq!(spinner.wheel_index(), (SPINNER_FRAMES.len() + 1) % SPINNER_FRAMES.len());
    }

    #[test]
    fn identical_spinners_produce_identical_frames() {
        let theme = test_theme();
        let mut a = AgentSpinnerClassic::new("Working", &theme);
        let mut b = AgentSpinnerClassic::new("Working", &theme);
        for _ in 0..120 {
            a.advance();
            b.advance();
            assert_eq!(a.ticks_elapsed, b.ticks_elapsed);
            assert_eq!(a.wheel_index(), b.wheel_index());
        }
    }

    #[test]
    fn set_activity_swaps_label_without_disturbing_the_wheel() {
        use crate::types::AgentActivity;
        let theme = test_theme();

        let mut spinner = AgentSpinnerClassic::new(AgentActivity::Working.label(), &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let ticks_before = spinner.ticks_elapsed;

        spinner.set_activity(AgentActivity::for_tool("web_search"));
        assert_eq!(spinner.label, "searching");
        assert!(spinner.width() > 1);

        // Re-applying the SAME activity is a no-op; the wheel clock is
        // never touched by a status change.
        spinner.set_activity(AgentActivity::Searching);
        assert_eq!(spinner.ticks_elapsed, ticks_before);

        spinner.set_activity(AgentActivity::Working);
        assert_eq!(spinner.label, "working");
        assert_eq!(spinner.ticks_elapsed, ticks_before);
    }

    #[test]
    fn status_glyph_renders_after_label_and_follows_activity() {
        use crate::types::AgentActivity;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = test_theme();
        let mut spinner = AgentSpinnerClassic::new(AgentActivity::Working.label(), &theme);
        for _ in 0..(INIT_FRAMES + 10) {
            spinner.advance();
        }

        spinner.set_activity(AgentActivity::Searching);
        assert_eq!(spinner.status_frame(), super::super::agent_spinner::SEARCHING_FRAMES[0]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
        spinner.render(&mut buf, 0, 0);
        let ex = spinner.status_x_for_test();
        assert_eq!(buf[(ex, 0)].symbol(), super::super::agent_spinner::SEARCHING_FRAMES[0]);
    }

    #[test]
    fn narrow_terminal_clips_without_panic() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = test_theme();
        let mut spinner = AgentSpinnerClassic::new("Working", &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        spinner.render(&mut buf, 0, 0);
    }

    #[test]
    fn width_matches_spinner_layout() {
        let theme = test_theme();

        let spinner = AgentSpinnerClassic::new("Working", &theme);
        assert_eq!(spinner.width(), 1 + 1 + 7 + 1);

        let spinner = AgentSpinnerClassic::new("", &theme);
        assert_eq!(spinner.width(), 1);
    }
}
