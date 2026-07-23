//! **Luminous Sweep Spinner** — animated text highlight with a glowing beam.
//!
//! A "searchlight" glides across the text from left to right, smoothly
//! illuminating each character with a gaussian glow that fades back to the
//! base colour. The effect is built from three layered contributions:
//!
//! | Layer        | Width (σ) | Contribution | Purpose                         |
//! |--------------|-----------|-------------|----------------------------------|
//! | Primary beam | 0.08      | 1.0×        | Tight, bright core              |
//! | Soft glow    | 0.22      | 0.25×       | Wide, atmospheric halo          |
//! | Asymmetric   | trailing  | 1.8× wider  | Directional motion feel         |
//!
//! The beam also shimmers very subtly (peak intensity oscillates ±6 %) so the
//! light doesn't feel static or mechanical.
//!
//! # Integration
//!
//! ```ignore
//! use cosh_tui::core::lib::rgba::RGBA;
//! use spinner_highlight::HighlightSpinner;
//!
//! let mut spinner = HighlightSpinner::new(
//!     "Hello, world!",
//!     RGBA::from_ints(255, 107, 48, 255),   // highlight (orange)
//!     RGBA::from_ints(128, 128, 128, 255),   // base (grey)
//! );
//!
//! // Each frame:
//! spinner.advance();
//! spinner.render(buf, x, y);
//! ```

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

// ── Colour helpers ─────────────────────────────────────────────────────────

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Linear interpolation between two RGBA colours in RGB space.
fn lerp_color(a: RGBA, b: RGBA, t: f32) -> RGBA {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab, _) = a.to_ints();
    let (br, bg, bb, _) = b.to_ints();
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
    RGBA::from_ints(
        (f32::from(ar) + (f32::from(br) - f32::from(ar)) * t).round() as u8,
        (f32::from(ag) + (f32::from(bg) - f32::from(ag)) * t).round() as u8,
        (f32::from(ab) + (f32::from(bb) - f32::from(ab)) * t).round() as u8,
        255,
    )
}

/// Gaussian function centred at zero with standard deviation `sigma`.
/// Returns `1.0` at `x = 0` and decays to ≈ 0 beyond `3·sigma`.
fn gaussian(x: f32, sigma: f32) -> f32 {
    if sigma <= 0.0 {
        return 0.0;
    }
    (-(x * x) / (2.0 * sigma * sigma)).exp()
}

// ── HighlightSpinner ───────────────────────────────────────────────────────

/// An animated text-highlight spinner that sweeps a luminous beam across the
/// characters.
///
/// # Visual layers
///
/// 1. **Primary beam** — a tight gaussian (σ ≈ 0.08 of text width) that
///    provides the intense bright core.
/// 2. **Soft glow** — a wide gaussian (σ ≈ 0.22) rendered at 25 % opacity,
///    creating an atmospheric halo around the beam.
/// 3. **Asymmetric falloff** — the trailing edge of the beam is 1.8× wider
///    than the leading edge, giving the sweep a directional, "sweeping"
///    feel rather than a static blob sliding sideways.
/// 4. **Shimmer** — the combined intensity gently oscillates (±6 %) every
///    few frames so the light looks organic.
///
/// # Construction
///
/// | Constructor               | Highlight colour          | Base colour               |
/// |---------------------------|---------------------------|---------------------------|
/// | `HighlightSpinner::new`   | Explicit `RGBA`           | Explicit `RGBA`           |
pub struct HighlightSpinner {
    text: String,
    chars: Vec<char>,
    char_count: usize,

    // ── Animation state ─────────────────────────────────────────────────
    /// Normalised beam position (`-0.3 … 1.3`). Values < 0 or > 1 mean the
    /// beam is partly off-screen, creating a natural entry/exit.
    beam_pos: f32,
    frame: u32,
    /// Fraction of the text width the beam travels per frame.
    speed: f32,

    // ── Colour ──────────────────────────────────────────────────────────
    highlight_color: RGBA,
    base_color: RGBA,

    // ── Beam shape ──────────────────────────────────────────────────────
    /// Gaussian sigma for the leading edge of the primary beam.
    primary_sigma: f32,
    /// Gaussian sigma for the wide atmospheric glow.
    glow_sigma: f32,
    /// Scale factor applied to `primary_sigma` on the trailing side to make
    /// the beam asymmetric.
    trail_scale: f32,
    /// Amplitude of the subtle shimmer oscillation (0.0 = none).
    shimmer_amp: f32,
    /// Frequency of the shimmer oscillation (radians per frame).
    shimmer_freq: f32,
}

#[allow(dead_code)]
impl HighlightSpinner {
    /// Create a new `HighlightSpinner` with the given text and colours.
    ///
    /// * `text` — the string to display (may contain any Unicode characters).
    /// * `highlight_color` — the colour at the centre of the glow beam.
    /// * `base_color` — the colour of characters far from the beam.
    pub fn new(text: &str, highlight_color: RGBA, base_color: RGBA) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let char_count = chars.len();

        Self {
            text: text.to_string(),
            char_count,
            chars,
            // Start off-screen to the left so the beam enters naturally.
            beam_pos: -0.3,
            frame: 0,
            speed: 0.008,
            highlight_color,
            base_color,
            primary_sigma: 0.08,
            glow_sigma: 0.22,
            trail_scale: 2.0,
            shimmer_amp: 0.06,
            shimmer_freq: 0.12,
        }
    }

    // ── Builder setters ─────────────────────────────────────────────────

    /// Customise the beam travel speed (fraction of text width per frame).
    ///
    /// Default: `0.008` — one full sweep takes ≈ 200 frames.
    pub fn with_speed(&mut self, speed: f32) -> &mut Self {
        self.speed = speed;
        self
    }

    /// Customise the width of the primary beam gaussian.
    ///
    /// Smaller = tighter, brighter core. Default: `0.08`.
    pub fn with_primary_sigma(&mut self, sigma: f32) -> &mut Self {
        self.primary_sigma = sigma.max(0.01);
        self
    }

    /// Customise the width of the soft atmospheric glow.
    ///
    /// Default: `0.22`.
    pub fn with_glow_sigma(&mut self, sigma: f32) -> &mut Self {
        self.glow_sigma = sigma.max(0.01);
        self
    }

    /// Customise the trailing-edge asymmetry.
    ///
    /// `1.0` = symmetric; larger values make the trailing glow wider.
    /// Default: `2.0`.
    pub fn with_trail_scale(&mut self, scale: f32) -> &mut Self {
        self.trail_scale = scale.max(0.1);
        self
    }

    /// Set the shimmer amplitude (0.0 disables shimmer).
    ///
    /// Default: `0.06` (±6 % oscillation).
    pub fn with_shimmer_amp(&mut self, amp: f32) -> &mut Self {
        self.shimmer_amp = amp.max(0.0);
        self
    }

    /// Change the text mid-animation (resets beam position).
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.chars = text.chars().collect();
        self.char_count = self.chars.len();
        self.beam_pos = -0.3;
    }

    /// Change the colours mid-animation.
    pub fn set_colors(&mut self, highlight: RGBA, base: RGBA) {
        self.highlight_color = highlight;
        self.base_color = base;
    }

    // ── Animation ───────────────────────────────────────────────────────

    /// Advance the animation by one frame.
    ///
    /// Moves the beam `speed` units to the right and wraps back to
    /// off-screen-left when it passes beyond `1.3`.
    pub fn advance(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.beam_pos += self.speed;
        if self.beam_pos > 1.3 {
            self.beam_pos = -0.3;
        }
    }

    /// Reset the animation to its initial state.
    pub fn reset(&mut self) {
        self.beam_pos = -0.3;
        self.frame = 0;
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Total width in terminal cells (equal to the character count).
    pub fn width(&self) -> usize {
        self.char_count
    }

    /// The current text string.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the beam is currently visible over any characters.
    pub const fn is_beam_visible(&self) -> bool {
        self.beam_pos > -0.3 && self.beam_pos < 1.3
    }

    // ── Rendering ───────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)`.
    ///
    /// Each character is drawn with an individually interpolated colour
    /// based on its distance from the moving beam centre.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        if self.char_count == 0 {
            return;
        }

        let char_count_f = self.char_count as f32;
        let shimmer = 1.0
            + self.shimmer_amp
                * ((self.frame as f32) * self.shimmer_freq).sin();

        for (i, &ch) in self.chars.iter().enumerate() {
            // Normalised character position in [0.0, 1.0].
            let char_norm = if self.char_count > 1 {
                i as f32 / (char_count_f - 1.0)
            } else {
                0.5
            };

            // Signed distance: negative = character is behind the beam
            // (trailing side), positive = ahead (leading side).
            let dist = char_norm - self.beam_pos;
            let abs_dist = dist.abs();

            // Asymmetric beam: the trailing edge is wider so the beam
            // appears to be sweeping in one direction.
            let effective_sigma = if dist < 0.0 {
                self.primary_sigma * self.trail_scale
            } else {
                self.primary_sigma
            };

            // Primary beam contribution.
            let primary = gaussian(abs_dist, effective_sigma);

            // Wide atmospheric glow (always symmetric).
            let glow = gaussian(abs_dist, self.glow_sigma) * 0.25;

            // Combine and clamp.
            let intensity = (primary + glow).clamp(0.0, 1.0) * shimmer;

            // Interpolate colour.
            let color = lerp_color(self.base_color, self.highlight_color, intensity);

            let cell_x = x + i as u16;
            if let Some(cell) = buf.cell_mut((cell_x, y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(rgba_color(color)));
            }
        }
    }
}
