//! Animated agent spinner — a Rust port of the Go `anim` package.
//!
//! Renders a row of scrambled characters with a theme-aware gradient,
//! followed by a label and an animated glyph reflecting the agent's current
//! activity. The spinner has a staggered birth animation where columns
//! appear one-by-one.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};
use crate::types::AgentActivity;

// ── Constants ──────────────────────────────────────────────────────────────

/// Number of scrambled characters in the cycling region.
const NUM_CYCLING_CHARS: usize = 8;

/// Maximum birth delay in frames (~0.6 s at 30 fps).
const BIRTH_DELAY_MAX: u32 = 20;

/// Status glyph animation speed: number of frames per glyph step.
/// At 30 fps, 8 → ~3.75 steps/sec, quick but not distracting.
const STATUS_ANIM_SPEED: u32 = 8;

/// Cycle divider: cycling characters change every N frames.
/// At 30 fps, 4 = ~7.5 changes/sec — a calm readable scramble instead of
/// a frantic flicker.
const CYCLE_STEP_DIVIDER: u32 = 4;

/// Number of distinct animation frames (the sequence loops).
const PRERENDERED_FRAMES: usize = 10;

/// Characters used in the scrambled animation: the Elder Futhark runes
/// plus solid block terminals in the same carved style.
const AVAILABLE_RUNES: &[char] = &[
    'ᚠ', 'ᚢ', 'ᚦ', 'ᚨ', 'ᚱ', 'ᚲ', 'ᚷ', 'ᚹ', 'ᚺ', 'ᚾ', 'ᛁ', 'ᛃ', 'ᛇ', 'ᛈ', 'ᛉ', 'ᛊ', 'ᛏ',
    'ᛒ', 'ᛖ', 'ᛗ', 'ᛚ', 'ᛜ', 'ᛞ', 'ᛟ', '▮', '▰', '◼',
];

/// Status glyph sequences: the animation shown after the status text. Each
/// agent activity continuously cycles its own sequence while active; the
/// runic scramble is unaffected. Switching activity restarts the new
/// sequence from its first glyph. Shared verbatim with the sibling
/// spinner components (`agent_spinner_bass`, `agent_spinner_opencode`).
pub const WORKING_FRAMES: &[&str] = &["·", "•", "●", "•", "·"];
pub const PONDERING_FRAMES: &[&str] = &["+", "×", "+", "·", "·"];
pub const SEARCHING_FRAMES: &[&str] = &["›", "»", "›"];
pub const RECALLING_FRAMES: &[&str] = &["↶", "↺", "↶"];

/// The glyph sequence owned by an agent activity.
pub const fn activity_frames(activity: AgentActivity) -> &'static [&'static str] {
    match activity {
        AgentActivity::Working => WORKING_FRAMES,
        AgentActivity::Pondering => PONDERING_FRAMES,
        AgentActivity::Searching => SEARCHING_FRAMES,
        AgentActivity::Recalling => RECALLING_FRAMES,
    }
}

/// Initial character shown during the birth animation.
const BIRTH_CHAR: char = '.';

// ── Colour helpers ─────────────────────────────────────────────────────────

/// Linear interpolation between two RGBA colours in RGB space.
#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn lerp_color(a: RGBA, b: RGBA, t: f32) -> RGBA {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab, _) = a.to_ints();
    let (br, bg, bb, _) = b.to_ints();
    RGBA::from_ints(
        (f32::from(ar) + (f32::from(br) - f32::from(ar)) * t).round() as u8,
        (f32::from(ag) + (f32::from(bg) - f32::from(ag)) * t).round() as u8,
        (f32::from(ab) + (f32::from(bb) - f32::from(ab)) * t).round() as u8,
        255,
    )
}

/// Generate a gradient ramp of `size` steps from colour `a` to colour `b`.
#[allow(clippy::cast_precision_loss)]
fn make_gradient_ramp(a: RGBA, b: RGBA, size: usize) -> Vec<RGBA> {
    if size == 0 {
        return vec![];
    }
    let mut ramp = Vec::with_capacity(size);
    for i in 0..size {
        let t = if size == 1 {
            0.0
        } else {
            i as f32 / (size - 1) as f32
        };
        ramp.push(lerp_color(a, b, t));
    }
    ramp
}

// ── Deterministic pseudo-random number generator (LCG) ────────────────────
// Avoids pulling in a full RNG dependency.

fn seeded_rng(seed: u64) -> impl Iterator<Item = u64> {
    let mut state = seed;
    std::iter::from_fn(move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        Some(state)
    })
}

// ── AgentSpinner ───────────────────────────────────────────────────────────

/// An animated spinner that cycles through scrambled characters with a
/// theme-aware gradient, followed by a label and a per-status glyph animation.
///
/// Features:
/// - **Birth animation**: columns appear one-by-one with a staggered entrance
/// - **Cycling characters**: scrambled runes with a smooth gradient
/// - **Label + status glyph**: text label with its own cycling glyph sequence
///   per agent activity (`working`, `pondering`, `searching`, `recalling`)
/// - **Deterministic**: two identical spinners produce byte-identical frames
pub struct AgentSpinner {
    /// Number of cycling characters.
    cycling_char_width: usize,
    /// Pre-computed gradient colours for each column (cycling area).
    gradient_colors: Vec<RGBA>,
    /// Pre-computed characters for each (frame, column).
    cycling_chars: Vec<Vec<char>>,
    /// Frame at which each column appears during the birth animation.
    birth_steps: Vec<u32>,
    /// Current animation frame index.
    step: u32,
    /// Total frames elapsed since the spinner was started / reset.
    frames_elapsed: u32,
    /// Current status-glyph step (ticks at `STATUS_ANIM_SPEED`).
    status_step: u32,
    /// Glyph sequence of the current agent activity (see [`activity_frames`]):
    /// each status cycles its own animation.
    frames: &'static [&'static str],
    /// Whether the birth animation has finished.
    initialized: bool,
    /// Label text displayed after the cycling characters.
    label: String,
    /// Colour used for the label and status glyph.
    label_color: RGBA,
    /// Number of characters in the label.
    label_width: usize,
}

impl AgentSpinner {
    /// Create a new spinner with the given label, sourcing colours from
    /// `theme.primary` / `theme.accent` for the gradient and
    /// `theme.text_muted` for the label.
    pub fn new(label: &str, theme: &Theme) -> Self {
        let gradient_a = theme.primary;
        let gradient_b = theme.accent;
        let label_color = theme.text_muted;

        let cycling_char_width = NUM_CYCLING_CHARS;
        let label_width = label.chars().count();

        // ── Gradient colours ───────────────────────────────────────────
        let gradient_colors = make_gradient_ramp(gradient_a, gradient_b, cycling_char_width);

        // ── Pre-compute cycling characters for each frame ──────────────
        // Seed the RNG off the label so output is deterministic per label.
        let seed: u64 = label.bytes().fold(0u64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u64::from(b))
        });
        let mut rng = seeded_rng(seed);

        let mut cycling_chars = Vec::with_capacity(PRERENDERED_FRAMES);
        let rune_count = AVAILABLE_RUNES.len();
        for _ in 0..PRERENDERED_FRAMES {
            let frame: Vec<char> = (0..cycling_char_width)
                .map(|_| {
                    let idx = (rng.next().unwrap() as usize) % rune_count;
                    AVAILABLE_RUNES[idx]
                })
                .collect();
            cycling_chars.push(frame);
        }

        // ── Pre-compute birth steps (staggered entrance) ───────────────
        let mut birth_rng = seeded_rng(seed.wrapping_add(42));
        let birth_steps: Vec<u32> = (0..cycling_char_width)
            .map(|_| (birth_rng.next().unwrap() % u64::from(BIRTH_DELAY_MAX)) as u32)
            .collect();

        Self {
            cycling_char_width,
            gradient_colors,
            cycling_chars,
            birth_steps,
            step: 0,
            frames_elapsed: 0,
            status_step: 0,
            // Spun up on the default (working) sequence; `set_activity`
            // switches — and restarts — the sequence as the loop's activity
            // changes.
            frames: activity_frames(AgentActivity::Working),
            initialized: false,
            label: label.to_string(),
            label_color,
            label_width,
        }
    }

    /// Advance the animation by one frame (call each render cycle).
    /// Cycling characters advance only every `CYCLE_STEP_DIVIDER` frames
    /// for a comfortable visual pace (≈7.5 character changes/sec at 30 fps).
    pub fn advance(&mut self) {
        let frame = self.frames_elapsed;
        self.frames_elapsed = self.frames_elapsed.saturating_add(1);

        // Only advance cycling character step every N frames
        if frame.is_multiple_of(CYCLE_STEP_DIVIDER) {
            self.step = (self.step + 1) % PRERENDERED_FRAMES as u32;
        }

        // Birth animation finishes once we've passed the maximum delay
        if !self.initialized && self.frames_elapsed >= BIRTH_DELAY_MAX {
            self.initialized = true;
        }

        // Advance the status glyphs (only after initialization and if there's
        // a label): cycle the CURRENT activity's sequence.
        if self.initialized && self.label_width > 0 {
            self.status_step =
                (self.status_step + 1) % (STATUS_ANIM_SPEED * self.frames.len() as u32);
        }
    }

    /// Reset the spinner to its initial state, restarting the birth animation.
    /// The label and glyph sequence are preserved — they describe the loop's
    /// activity, which survives a reset; construct a fresh spinner for a new
    /// activity (as `start_agent_loop` does).
    pub const fn reset(&mut self) {
        self.step = 0;
        self.frames_elapsed = 0;
        self.status_step = 0;
        self.initialized = false;
    }

    /// Change the label text (width is recomputed).
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.label_width = label.chars().count();
    }

    /// Switch the label to the given agent activity and start that status's
    /// own glyph animation from its first frame. The runic scramble
    /// (seed, birth stagger, frame clock) is untouched; a repeated activity
    /// is a cheap no-op that keeps the current glyph cycle running.
    pub fn set_activity(&mut self, activity: AgentActivity) {
        if self.label != activity.label() {
            self.set_label(activity.label());
            // Immediately switch to the new status's own glyph sequence,
            // restarting it from the first glyph so the change reads at once.
            self.frames = activity_frames(activity);
            self.status_step = 0;
        }
    }

    /// Update the rune colour for the active prompt mode without resetting
    /// the animation state. The caller resolves the mode colour through the
    /// prompt's shared mode-colour mapping; the whole scramble row takes it
    /// as a solid tone.
    pub fn set_mode_color(&mut self, color: RGBA) {
        self.gradient_colors =
            make_gradient_ramp(color, color, self.cycling_char_width);
    }

    /// Update the theme colours (gradient and label colour) without
    /// resetting the animation state. This allows the spinner to
    /// immediately reflect theme changes while a loop is active.
    pub fn update_theme(&mut self, theme: &Theme) {
        let gradient_a = theme.primary;
        let gradient_b = theme.accent;
        self.label_color = theme.text_muted;
        self.gradient_colors = make_gradient_ramp(gradient_a, gradient_b, self.cycling_char_width);
    }

    /// Total width in terminal cells (cycling + gap + label + status glyph).
    pub fn width(&self) -> usize {
        let mut w = self.cycling_char_width;
        if self.label_width > 0 {
            w += 1; // gap
            w += self.label_width;
            // Widest glyph of the current status's sequence
            w += self
                .frames
                .iter()
                .map(|f| f.chars().count())
                .max()
                .unwrap_or(0);
        }
        w
    }

    /// Whether the birth animation is still playing.
    pub const fn is_initialized(&self) -> bool {
        self.initialized
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

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)`.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        let step = self.step as usize % PRERENDERED_FRAMES;
        let frames = self.frames_elapsed;

        // 1. Cycling characters (scrambled runes with gradient)
        for col in 0..self.cycling_char_width {
            let cell_x = x + col as u16;
            let color = self
                .gradient_colors
                .get(col)
                .copied()
                .unwrap_or_else(|| RGBA::from_ints(255, 255, 255, 255));
            let style = Style::default().fg(rgba_color(color));

            if !self.initialized && col < self.birth_steps.len() && frames < self.birth_steps[col] {
                // Birth phase — show the initial character
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(BIRTH_CHAR);
                    cell.set_style(style);
                }
            } else if step < self.cycling_chars.len() && col < self.cycling_chars[step].len() {
                // Normal phase — show the cycling scrambled char
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(self.cycling_chars[step][col]);
                    cell.set_style(style);
                }
            }
        }

        // 2. Gap between cycling area and label
        if self.label_width > 0 {
            let gap_x = x + self.cycling_char_width as u16;
            if let Some(cell) = buf.cell_mut((gap_x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }

        // 3. Label text
        if self.label_width > 0 {
            let label_x = x + self.cycling_char_width as u16 + 1;
            let label_style = Style::default().fg(rgba_color(self.label_color));
            for (i, ch) in self.label.chars().enumerate() {
                let cell_x = label_x + i as u16;
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(label_style);
                }
            }
        }

        // 4. Status glyph animation (only after birth animation is complete).
        // The slot is always padded to the widest frame of the CURRENT
        // sequence: a narrower frame must clear stale cells instead of
        // ghosting them.
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
                .saturating_add(self.cycling_char_width as u16)
                .saturating_add(1)
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

    #[test]
    fn update_theme_changes_gradient_and_label_color() {
        let registry = crate::theme::ThemeRegistry::new();
        let mut theme1 = registry.get("opencode").cloned().unwrap();
        theme1.primary = RGBA::from_hex("#FF0000");
        theme1.accent = RGBA::from_hex("#0000FF");
        theme1.text_muted = RGBA::from_hex("#00FF00");

        let mut theme2 = registry.get("opencode").cloned().unwrap();
        theme2.primary = RGBA::from_hex("#00FF00");
        theme2.accent = RGBA::from_hex("#FF00FF");
        theme2.text_muted = RGBA::from_hex("#FFFF00");

        let mut spinner = AgentSpinner::new("Test", &theme1);

        // Verify initial colors are from theme1
        assert_eq!(spinner.gradient_colors[0], theme1.primary);
        assert_eq!(
            spinner.gradient_colors[NUM_CYCLING_CHARS - 1],
            theme1.accent
        );
        assert_eq!(spinner.label_color, theme1.text_muted);

        // Update to theme2
        spinner.update_theme(&theme2);

        // Verify colors are now from theme2
        assert_eq!(spinner.gradient_colors[0], theme2.primary);
        assert_eq!(
            spinner.gradient_colors[NUM_CYCLING_CHARS - 1],
            theme2.accent
        );
        assert_eq!(spinner.label_color, theme2.text_muted);
    }
}
