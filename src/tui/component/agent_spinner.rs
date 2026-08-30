//! Animated agent spinner — a Rust port of the Go `anim` package.
//!
//! Renders a row of scrambled characters with a theme-aware gradient,
//! followed by a label and an animated ellipsis. The spinner has a
//! staggered birth animation where columns appear one-by-one.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};

// ── Constants ──────────────────────────────────────────────────────────────

/// Number of scrambled characters in the cycling region.
const NUM_CYCLING_CHARS: usize = 8;

/// Maximum birth delay in frames (~0.6 s at 30 fps).
const BIRTH_DELAY_MAX: u32 = 20;

/// Ellipsis animation speed: number of frames per ellipsis step.
/// At 30 fps, 8 → ~3.75 steps/sec, quick but not distracting.
const ELLIPSIS_ANIM_SPEED: u32 = 8;

/// Cycle divider: cycling characters change every N frames.
/// At 30 fps, 1 = every frame → lively scramble (~30 changes/sec).
const CYCLE_STEP_DIVIDER: u32 = 1;

/// Number of distinct animation frames (the sequence loops).
const PRERENDERED_FRAMES: usize = 10;

/// Characters used in the scrambled animation.
const AVAILABLE_RUNES: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'A', 'B', 'C',
    'D', 'E', 'F', '~', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '+', '=',
];

/// Ellipsis animation frames.
const ELLIPSIS_FRAMES: &[&str] = &[".", "..", "...", ""];

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
/// theme-aware gradient, followed by a label and an animated ellipsis.
///
/// Features:
/// - **Birth animation**: columns appear one-by-one with a staggered entrance
/// - **Cycling characters**: scrambled runes with a smooth gradient
/// - **Label + ellipsis**: text label with a cycling `...` animation
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
    /// Current ellipsis frame index.
    ellipsis_step: u32,
    /// Whether the birth animation has finished.
    initialized: bool,
    /// Label text displayed after the cycling characters.
    label: String,
    /// Colour used for the label and ellipsis.
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
            ellipsis_step: 0,
            initialized: false,
            label: label.to_string(),
            label_color,
            label_width,
        }
    }

    /// Advance the animation by one frame (call each render cycle).
    /// Cycling characters advance only every `CYCLE_STEP_DIVIDER` frames
    /// for a comfortable visual pace (≈30 character changes/sec at 125 fps).
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

        // Advance ellipsis (only after initialization and if there's a label)
        if self.initialized && self.label_width > 0 {
            self.ellipsis_step =
                (self.ellipsis_step + 1) % (ELLIPSIS_ANIM_SPEED * ELLIPSIS_FRAMES.len() as u32);
        }
    }

    /// Reset the spinner to its initial state, restarting the birth animation.
    pub const fn reset(&mut self) {
        self.step = 0;
        self.frames_elapsed = 0;
        self.ellipsis_step = 0;
        self.initialized = false;
    }

    /// Change the label text (width is recomputed).
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.label_width = label.chars().count();
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

    /// Total width in terminal cells (cycling + gap + label + ellipsis).
    pub fn width(&self) -> usize {
        let mut w = self.cycling_char_width;
        if self.label_width > 0 {
            w += 1; // gap
            w += self.label_width;
            // Widest ellipsis frame
            w += ELLIPSIS_FRAMES.iter().map(|f| f.len()).max().unwrap_or(0);
        }
        w
    }

    /// Whether the birth animation is still playing.
    pub const fn is_initialized(&self) -> bool {
        self.initialized
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

        // 4. Animated ellipsis (only after birth animation is complete)
        if self.initialized && self.label_width > 0 {
            let ellipsis_idx =
                (self.ellipsis_step / ELLIPSIS_ANIM_SPEED) as usize % ELLIPSIS_FRAMES.len();
            let ellipsis_text = ELLIPSIS_FRAMES[ellipsis_idx];
            let ellipsis_x = x + self.cycling_char_width as u16 + 1 + self.label_width as u16;
            let ellipsis_style = Style::default().fg(rgba_color(self.label_color));
            for (i, ch) in ellipsis_text.chars().enumerate() {
                let cell_x = ellipsis_x + i as u16;
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(ellipsis_style);
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
        assert_eq!(spinner.gradient_colors[NUM_CYCLING_CHARS - 1], theme1.accent);
        assert_eq!(spinner.label_color, theme1.text_muted);

        // Update to theme2
        spinner.update_theme(&theme2);

        // Verify colors are now from theme2
        assert_eq!(spinner.gradient_colors[0], theme2.primary);
        assert_eq!(spinner.gradient_colors[NUM_CYCLING_CHARS - 1], theme2.accent);
        assert_eq!(spinner.label_color, theme2.text_muted);
    }
}
