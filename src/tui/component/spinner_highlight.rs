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

// Colour helpers

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

//  SpinnerPhase

/// Lifecycle phase of a [`HighlightSpinner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerPhase {
    /// Beam is actively sweeping (tool is running).
    Active,
    /// Beam is completing its current sweep, then will stop.
    Finishing,
    /// Spinner is stopped — all characters render with base colour only.
    Idle,
}

//  HighlightSpinner

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

    // Animation state
    /// Normalised beam position (`-0.3 … 1.3`). Values < 0 or > 1 mean the
    /// beam is partly off-screen, creating a natural entry/exit.
    beam_pos: f32,
    frame: u32,
    /// Fraction of the text width the beam travels per frame.
    speed: f32,

    // Colour
    highlight_color: RGBA,
    base_color: RGBA,

    // Beam shape
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

    // Lifecycle
    /// Current phase of the spinner.
    phase: SpinnerPhase,

    // Multi-message cycling
    /// All messages to cycle through.
    messages: Vec<String>,
    /// How many frames to display each message (one per message).
    durations: Vec<u32>,
    /// Index of the currently displayed message.
    current_idx: usize,
    /// Frames elapsed since the last message switch.
    msg_frame_count: u32,
}

#[allow(dead_code)]
impl HighlightSpinner {
    /// Create a new `HighlightSpinner` with the given text and colours.
    ///
    /// * `text` — the string to display (may contain any Unicode characters).
    /// * `highlight_color` — the colour at the centre of the glow beam.
    /// * `base_color` — the colour of characters far from the beam.
    pub fn new(text: &str, highlight_color: RGBA, base_color: RGBA) -> Self {
        // Filter control characters (e.g. `\n` in multiline tool commands):
        // writing them into buffer cells makes ratatui's buffer diff panic
        // ("control character passed to cell_width without filtering").
        // The filtered string is stored so `text()` stays consistent with
        // `width()`/`chars`.
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        let chars: Vec<char> = filtered.chars().collect();
        let char_count = chars.len();

        Self {
            text: filtered.clone(),
            char_count,
            chars,
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
            phase: SpinnerPhase::Active,
            messages: vec![filtered],
            durations: vec![u32::MAX],
            current_idx: 0,
            msg_frame_count: 0,
        }
    }

    // Builder setters

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

    // Multi-message cycling

    /// Replace the current message with a cycling set of messages.
    ///
    /// When multiple messages are provided, the spinner automatically
    /// advances to the next one after `durations` frames have elapsed.
    /// The cycle loops back to the first message after the last.
    ///
    /// * `texts` — the messages to cycle through (must not be empty).
    /// * `durations` — optional per-message display durations in **frames**.
    ///   If `None`, every message gets 400 frames (≈ 6.6 s at 60 fps).
    ///   Pass an array shorter than `texts` and the remaining messages
    ///   use the last given value.
    pub fn with_messages(&mut self, texts: &[&str], durations: Option<&[u32]>) -> &mut Self {
        if texts.is_empty() {
            return self;
        }
        self.messages = texts.iter().map(|s| s.to_string()).collect();

        // Build durations array, padding with the last value or a default.
        self.durations = match durations {
            Some(d) => {
                let mut v = d.to_vec();
                let last = v.last().copied().unwrap_or(400);
                while v.len() < self.messages.len() {
                    v.push(last);
                }
                v
            }
            None => vec![400; self.messages.len()],
        };

        self.current_idx = 0;
        self.msg_frame_count = 0;
        self.apply_current_message();
        self
    }

    /// Manually advance to the next message in the cycle (wraps around).
    ///
    /// Does nothing if there is only one message.
    pub fn advance_message(&mut self) {
        if self.messages.len() <= 1 {
            return;
        }
        self.current_idx = (self.current_idx + 1) % self.messages.len();
        self.msg_frame_count = 0;
        self.apply_current_message();
    }

    /// Change the text mid-animation (resets beam position).
    ///
    /// This also replaces the message list with a single entry so
    /// auto-cycling stops. Call [`with_messages`] again to re-enable it.
    pub fn set_text(&mut self, text: &str) {
        self.messages = vec![text.to_string()];
        self.durations = vec![u32::MAX];
        self.current_idx = 0;
        self.msg_frame_count = 0;
        self.apply_current_message();
    }

    /// Change the colours mid-animation.
    pub fn set_colors(&mut self, highlight: RGBA, base: RGBA) {
        self.highlight_color = highlight;
        self.base_color = base;
    }

    // Lifecycle control

    /// Signal the spinner to finish its current beam sweep and then
    /// stop at the base colour. Idempotent — safe to call multiple times.
    pub fn finish(&mut self) {
        if self.phase == SpinnerPhase::Active {
            self.phase = SpinnerPhase::Finishing;
        }
    }

    /// Whether the spinner is in the `Active` phase (beam sweeping).
    pub const fn is_active(&self) -> bool {
        matches!(self.phase, SpinnerPhase::Active)
    }

    /// Whether the spinner is in the `Idle` phase (stopped, no beam).
    pub const fn is_idle(&self) -> bool {
        matches!(self.phase, SpinnerPhase::Idle)
    }

    // Internal helpers

    /// Apply the current message's text to the rendering state.
    fn apply_current_message(&mut self) {
        let text = &self.messages[self.current_idx];
        // Keep control characters out of the rendered char list and the
        // stored text (see `new`).
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text.clone_from(&filtered);
        self.chars = filtered.chars().collect();
        self.char_count = self.chars.len();
        self.beam_pos = -0.3;
    }

    // Animation

    /// Advance the animation by one frame, scaled by `delta_secs`.
    ///
    /// The beam moves at `speed * delta_secs * 30.0` so its visual
    /// speed stays consistent regardless of the actual frame rate.
    /// At the default 30 fps (`delta_secs ≈ 0.033`) the factor is ~1.0×.
    ///
    /// * `Active` — beam moves and wraps around.
    /// * `Finishing` — beam continues until it exits past `1.3`,
    ///   then transitions to `Idle`.
    /// * `Idle` — no-op (spinner is stopped).
    ///
    /// If multiple messages are configured, automatically advances to the
    /// next message when the current one's display duration expires.
    pub fn advance(&mut self, delta_secs: f64) {
        let step = self.speed * (delta_secs as f32 * 30.0);
        match self.phase {
            SpinnerPhase::Idle => {}
            SpinnerPhase::Finishing => {
                self.frame = self.frame.wrapping_add(1);
                self.beam_pos += step;
                if self.beam_pos > 1.3 {
                    self.phase = SpinnerPhase::Idle;
                    self.beam_pos = 2.0;
                }
            }
            SpinnerPhase::Active => {
                self.frame = self.frame.wrapping_add(1);
                self.beam_pos += step;
                if self.beam_pos > 1.3 {
                    self.beam_pos = -0.3;
                }

                // Multi-message cycling: check if it's time to advance.
                if self.messages.len() > 1 {
                    self.msg_frame_count += 1;
                    let current_duration = self.durations[self.current_idx];
                    if self.msg_frame_count >= current_duration {
                        self.advance_message();
                    }
                }
            }
        }
    }

    /// Set the beam position directly (useful to start already on the text).
    /// `0.0` = first character, `1.0` = last character.
    pub fn set_beam_pos(&mut self, pos: f32) {
        self.beam_pos = pos;
    }

    /// Reset the animation to its initial state.
    pub fn reset(&mut self) {
        self.beam_pos = -0.3;
        self.frame = 0;
    }

    // Queries

    /// Total width in terminal cells (equal to the character count).
    pub fn width(&self) -> usize {
        self.char_count
    }

    /// The current text string (the currently active message).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Total number of messages in the cycle.
    pub fn messages_count(&self) -> usize {
        self.messages.len()
    }

    /// Index of the currently displayed message.
    pub const fn current_index(&self) -> usize {
        self.current_idx
    }

    /// Duration (in frames) of the currently displayed message.
    pub fn current_duration(&self) -> u32 {
        self.durations[self.current_idx]
    }

    /// Whether the beam is currently visible over any characters.
    pub const fn is_beam_visible(&self) -> bool {
        self.beam_pos > -0.3 && self.beam_pos < 1.3
    }

    /// The current beam position.
    pub fn beam_pos(&self) -> f32 {
        self.beam_pos
    }

    /// The current phase.
    pub fn phase(&self) -> SpinnerPhase {
        self.phase
    }

    // Rendering

    /// Render the spinner into the buffer at position `(x, y)`.
    ///
    /// * `Active` / `Finishing` — draws each character with a colour
    ///   interpolated between `base_color` and `highlight_color` based
    ///   on the beam position.
    /// * `Idle` — draws all characters with `base_color` only (no beam).
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        if self.char_count == 0 {
            return;
        }

        // Idle: all characters at base colour, no beam calculation.
        if self.phase == SpinnerPhase::Idle {
            for (i, &ch) in self.chars.iter().enumerate() {
                let cell_x = x + i as u16;
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(rgba_color(self.base_color)));
                }
            }
            return;
        }

        // Active / Finishing: beam rendering.
        let char_count_f = self.char_count as f32;
        let shimmer = 1.0 + self.shimmer_amp * ((self.frame as f32) * self.shimmer_freq).sin();

        for (i, &ch) in self.chars.iter().enumerate() {
            let char_norm = if self.char_count > 1 {
                i as f32 / (char_count_f - 1.0)
            } else {
                0.5
            };

            let dist = char_norm - self.beam_pos;
            let abs_dist = dist.abs();

            let effective_sigma = if dist < 0.0 {
                self.primary_sigma * self.trail_scale
            } else {
                self.primary_sigma
            };

            let primary = gaussian(abs_dist, effective_sigma);
            let glow = gaussian(abs_dist, self.glow_sigma) * 0.25;
            let intensity = (primary + glow).clamp(0.0, 1.0) * shimmer;
            let color = lerp_color(self.base_color, self.highlight_color, intensity);

            let cell_x = x + i as u16;
            if let Some(cell) = buf.cell_mut((cell_x, y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(rgba_color(color)));
            }
        }
    }
}
