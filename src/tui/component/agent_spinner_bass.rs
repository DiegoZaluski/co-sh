//! Animated agent spinner — bass equalizer.
//!
//! Renders a row of vertical bars whose heights rise and fall like a music
//! equalizer driven by a bass rhythm, followed by a label and an animated
//! ellipsis. Each bar is a Unicode block glyph (`▁` … `█`), so the whole
//! equalizer fits in a single terminal row and slots straight into the same
//! 1-row spinner area the previous agent spinner used.
//!
//! The animation is fully deterministic: two spinners started at the same
//! frame produce byte-identical output. Bars appear one-by-one during a
//! staggered birth animation, then pulse on two superposed travelling waves
//! (a slow "bass" wave plus a faster shimmer) with an independently detuned
//! wobble on top whose amplitude grows with the bar's height — so the bars
//! in the upper range dance out of sync while the floor stays calm. Each
//! bar also receives random updrafts: unpredictable, snappy surges upward
//! followed by gentle sinks, drawn from deterministic hash noise so the
//! animation stays fully reproducible. Each bar's tone is shaded darker as
//! it rises and lighter as it falls, so the colour breathes with the rhythm.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};

// ── Constants ──────────────────────────────────────────────────────────────

/// Number of equalizer bars in the row.
const NUM_BARS: usize = 8;

/// Block glyphs from shortest to tallest — one per height level.
const BLOCK_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Character shown for a bar that has not yet been born.
const BIRTH_CHAR: char = '▁';

/// Maximum birth delay in frames (~0.6 s at 30 fps).
const BIRTH_DELAY_MAX: u32 = 20;

/// Frames a newborn bar takes to ease up to its animated height.
const BIRTH_GROW_FRAMES: u32 = 12;

/// Simulated time advanced per animation frame. At 30 fps this makes the
/// slow wave complete a cycle in ~1.4 s — a relaxed "bass" pulse.
const TIME_STEP: f32 = 0.1;

/// Angular frequency of the slow bass wave (rad per simulated time unit).
const BASS_WAVE_FREQUENCY: f32 = 1.4;

/// Angular frequency of the faster shimmer wave riding on the bass wave.
const SHIMMER_WAVE_FREQUENCY: f32 = 2.6;

/// Phase shift between adjacent bars on the slow wave (travels left→right).
const BASS_WAVE_PHASE_STEP: f32 = 0.6;

/// Phase shift between adjacent bars on the fast wave (travels right→left).
const SHIMMER_WAVE_PHASE_STEP: f32 = 1.1;

/// Amplitude of the slow bass wave.
const BASS_AMPLITUDE: f32 = 0.35;

/// Amplitude of the faster shimmer wave riding on the bass wave.
const SHIMMER_AMPLITUDE: f32 = 0.15;

/// Base angular frequency of the per-bar wobble.
const WOBBLE_WAVE_FREQUENCY: f32 = 2.9;

/// Per-bar detune of the wobble frequency: neighbouring bars drift out of
/// phase over time instead of oscillating as one block.
const WOBBLE_FREQUENCY_STEP: f32 = 0.35;

/// Phase shift between adjacent bars on the wobble.
const WOBBLE_PHASE_STEP: f32 = 2.3;

/// Peak amplitude of the per-bar wobble, scaled by the bar's current height
/// so the upper range varies independently while the floor stays calm.
const WOBBLE_AMPLITUDE: f32 = 0.22;

/// Length of one random updraft segment, in frames (~0.5 s at 30 fps).
/// A new random lift target is drawn at every segment boundary.
const UPDRAFT_INTERVAL: u32 = 15;

/// Peak lift of a random updraft, scaled by the bar's current height so
/// surges grow with the rhythm while the floor stays calm.
const UPDRAFT_AMPLITUDE: f32 = 0.25;

/// Exponent of the snappy ease-out applied while a bar surges upward:
/// higher values concentrate the rise into the first frames of a segment.
const UPDRAFT_RISE_POWER: i32 = 8;

/// How strongly a bar's colour is lightened when it sits at the floor
/// (lerp toward white).
const LIGHT_SHADE_STRENGTH: f32 = 0.30;

/// How strongly a bar's colour is darkened when it stands at the top
/// (lerp toward black).
const DARK_SHADE_STRENGTH: f32 = 0.45;

/// Ellipsis animation speed: number of frames per ellipsis step.
/// At 30 fps, 8 → ~3.75 steps/sec, quick but not distracting.
const ELLIPSIS_ANIM_SPEED: u32 = 8;

/// Ellipsis animation frames.
const ELLIPSIS_FRAMES: &[&str] = &[".", "..", "...", ""];

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

/// SplitMix64-style hash collapsed to `[0.0, 1.0)`. Drives the random
/// updraft targets so surges are unpredictable per bar and segment while
/// staying fully deterministic (pure function of the seed, column and
/// segment index).
#[allow(clippy::cast_precision_loss)]
fn noise01(seed: u64, col: u64, seg: u64) -> f32 {
    let mut h = seed
        ^ col.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ seg.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    #[allow(clippy::cast_precision_loss)]
    let fraction = (h >> 11) as f32;
    fraction / (1u64 << 53) as f32
}

// ── AgentSpinnerBass ───────────────────────────────────────────────────────

/// An animated spinner shaped like a music equalizer: a row of vertical
/// bars rising and falling on a bass rhythm, followed by a label and an
/// animated ellipsis.
///
/// Features:
/// - **Birth animation**: bars appear one-by-one and ease up to full motion
/// - **Bass rhythm**: two superposed travelling waves plus an independently
///   detuned wobble that grows with height, so upper bars move out of sync
/// - **Random updrafts**: snappy, unpredictable surges upward and gentle
///   sinks, drawn from deterministic hash noise
/// - **Height-shaded colour**: darker as bars rise, lighter as they fall
/// - **Deterministic**: two identical spinners produce byte-identical frames
pub struct AgentSpinnerBass {
    /// Seed derived from the label; drives all deterministic randomness
    /// (birth stagger and updraft surges).
    seed: u64,
    /// Number of equalizer bars.
    bar_count: usize,
    /// The single base colour for every bar (theme primary). Bars differ
    /// only in shade — lighter near the floor, darker at the top — never
    /// in hue.
    bar_color_base: RGBA,
    /// Frame at which each bar is born during the birth animation.
    birth_steps: Vec<u32>,
    /// Total frames elapsed since the spinner was started / reset.
    frames_elapsed: u32,
    /// Current ellipsis frame index.
    ellipsis_step: u32,
    /// Whether the birth animation has finished.
    initialized: bool,
    /// Label text displayed after the equalizer.
    label: String,
    /// Colour used for the label and ellipsis.
    label_color: RGBA,
    /// Number of characters in the label.
    label_width: usize,
}

impl AgentSpinnerBass {
    /// Create a new spinner with the given label, sourcing the bar colour
    /// from `theme.primary` and the label colour from `theme.text_muted`.
    pub fn new(label: &str, theme: &Theme) -> Self {
        let bar_count = NUM_BARS;
        let label_width = label.chars().count();

        let bar_color_base = theme.primary;
        let label_color = theme.text_muted;

        // Staggered birth: seed the delays off the label so the entrance is
        // deterministic per label.
        let seed: u64 = label.bytes().fold(0u64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u64::from(b))
        });
        let mut rng = seeded_rng(seed.wrapping_add(42));
        let birth_steps: Vec<u32> = (0..bar_count)
            .map(|_| (rng.next().unwrap() % u64::from(BIRTH_DELAY_MAX)) as u32)
            .collect();

        Self {
            seed,
            bar_count,
            bar_color_base,
            birth_steps,
            frames_elapsed: 0,
            ellipsis_step: 0,
            initialized: false,
            label: label.to_string(),
            label_color,
            label_width,
        }
    }

    /// Advance the animation by one frame. Call once per rendered frame.
    pub fn advance(&mut self) {
        self.frames_elapsed = self.frames_elapsed.saturating_add(1);

        // Birth animation finishes once every bar has been born and grown.
        if !self.initialized
            && self.frames_elapsed >= BIRTH_DELAY_MAX + BIRTH_GROW_FRAMES
        {
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
        self.frames_elapsed = 0;
        self.ellipsis_step = 0;
        self.initialized = false;
    }

    /// Change the label text (width is recomputed).
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.label_width = label.chars().count();
    }

    /// Update the theme colours (bar colour and label colour) without
    /// resetting the animation state. This allows the spinner to
    /// immediately reflect theme changes while a loop is active.
    pub fn update_theme(&mut self, theme: &Theme) {
        self.label_color = theme.text_muted;
        self.bar_color_base = theme.primary;
    }

    /// Total width in terminal cells (bars + gap + label + ellipsis).
    pub fn width(&self) -> usize {
        let mut w = self.bar_count;
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

    // ── Waveform ────────────────────────────────────────────────────────

    /// Normalized height of bar `col` at frame `frame` before the
    /// independent wobble, in `[0.0, 1.0]`.
    ///
    /// Two travelling waves are superposed: a slow bass wave (large
    /// amplitude, moving left→right) and a faster shimmer wave (small
    /// amplitude, moving right→left). The sum stays in `[0, 1]` and is
    /// deterministic in `frame`, so identical spinners render identically.
    fn bar_base_height(&self, col: usize, frame: u32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let t = frame as f32 * TIME_STEP;
        #[allow(clippy::cast_precision_loss)]
        let i = col as f32;
        let bass = BASS_AMPLITUDE * (t * BASS_WAVE_FREQUENCY - i * BASS_WAVE_PHASE_STEP).sin();
        let shimmer =
            SHIMMER_AMPLITUDE * (t * SHIMMER_WAVE_FREQUENCY + i * SHIMMER_WAVE_PHASE_STEP).sin();
        (0.5 + bass + shimmer).clamp(0.0, 1.0)
    }

    /// Random updraft lift target for bar `col` in updraft segment `seg`, in
    /// `[0.0, UPDRAFT_AMPLITUDE]`. Pure hash noise: unpredictable per bar
    /// and segment, yet reproducible.
    fn updraft_target(&self, col: usize, seg: u32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let lift = UPDRAFT_AMPLITUDE * noise01(self.seed, col as u64, u64::from(seg));
        lift
    }

    /// Random updraft lift for bar `col` at frame `frame`, in
    /// `[0.0, UPDRAFT_AMPLITUDE]`.
    ///
    /// The lift follows a piecewise curve between random targets refreshed
    /// every `UPDRAFT_INTERVAL` frames, phase-shifted per bar so segments
    /// never align across the row. Rising is deliberately snappy — most of
    /// the surge happens in the first frames of a segment — while sinking
    /// is a gentle quadratic ease, so the upward moves read as random
    /// bursts rather than another smooth wave.
    fn updraft(&self, col: usize, frame: u32) -> f32 {
        let interval = UPDRAFT_INTERVAL;
        let phase = (noise01(self.seed, col as u64, u64::MAX) * interval as f32) as u32;
        let shifted = frame + phase;
        let seg = shifted / interval;
        #[allow(clippy::cast_precision_loss)]
        let t = (shifted % interval) as f32 / interval as f32;
        let from = self.updraft_target(col, seg);
        let to = self.updraft_target(col, seg + 1);
        let p = if to >= from {
            // Snappy surge upward.
            1.0 - (1.0 - t).powi(UPDRAFT_RISE_POWER)
        } else {
            // Gentle sink: linger high, then ease down.
            t * t
        };
        from + (to - from) * p
    }

    /// Normalized height of bar `col` at frame `frame`, in `[0.0, 1.0]`.
    ///
    /// On top of the shared travelling waves each bar carries an
    /// independently detuned wobble whose amplitude grows with the bar's
    /// height, so the bars in the upper range rise and fall out of sync
    /// while the floor stays calm. A random updraft is added on top: snappy,
    /// unpredictable surges upward (also scaled by height) so upward moves
    /// never settle into a fixed rhythm.
    fn bar_height(&self, col: usize, frame: u32) -> f32 {
        let base = self.bar_base_height(col, frame);
        #[allow(clippy::cast_precision_loss)]
        let t = frame as f32 * TIME_STEP;
        #[allow(clippy::cast_precision_loss)]
        let i = col as f32;
        let wobble = WOBBLE_AMPLITUDE
            * base
            * (t * (WOBBLE_WAVE_FREQUENCY + WOBBLE_FREQUENCY_STEP * i) + i * WOBBLE_PHASE_STEP)
                .sin();
        let updraft = base * self.updraft(col, frame);
        (base + wobble + updraft).clamp(0.0, 1.0)
    }

    /// Height actually displayed for bar `col` at frame `frame`, in
    /// `[0.0, 1.0]`: zero before the bar is born, eased up from zero to the
    /// animated height while it grows in.
    fn rendered_height(&self, col: usize, frame: u32) -> f32 {
        let height = self.bar_height(col, frame);
        let birth = self.birth_steps.get(col).copied().unwrap_or(0);
        if frame < birth {
            return 0.0;
        }
        let grow = frame.saturating_sub(birth);
        if grow < BIRTH_GROW_FRAMES {
            #[allow(clippy::cast_precision_loss)]
            let ease = grow as f32 / BIRTH_GROW_FRAMES as f32;
            // Ease-out cubic: fast rise, gentle settle.
            let progress = 1.0 - (1.0 - ease).powi(3);
            return height * progress;
        }
        height
    }

    /// Block glyph for bar `col` at frame `frame`, including the birth
    /// animation: a bar not yet born sits at the floor character, a newborn
    /// bar eases up from the floor to its animated height.
    fn bar_char(&self, col: usize, frame: u32) -> char {
        #[allow(clippy::cast_precision_loss)]
        let level = {
            let levels = (BLOCK_CHARS.len() - 1) as f32;
            (self.rendered_height(col, frame) * levels).round() as usize
        };
        BLOCK_CHARS[level.min(BLOCK_CHARS.len() - 1)]
    }

    /// Colour for bar `col` at frame `frame`: the single theme colour
    /// shaded by the bar's current height — a lighter tone near the floor
    /// and a darker tone at the top, so the tone tracks the rhythm. There
    /// is deliberately no per-column gradient: two bars at the same height
    /// always share the exact same colour.
    fn bar_color(&self, col: usize, frame: u32) -> RGBA {
        let base = self.bar_color_base;
        let height = self.rendered_height(col, frame);
        let light = lerp_color(base, RGBA::from_ints(255, 255, 255, 255), LIGHT_SHADE_STRENGTH);
        let dark = lerp_color(base, RGBA::from_ints(0, 0, 0, 255), DARK_SHADE_STRENGTH);
        lerp_color(light, dark, height)
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)`.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        let frame = self.frames_elapsed;

        // 1. Equalizer bars (block glyphs shaded by their current height)
        for col in 0..self.bar_count {
            let cell_x = x + col as u16;
            let style = Style::default().fg(rgba_color(self.bar_color(col, frame)));

            if let Some(cell) = buf.cell_mut((cell_x, y)) {
                cell.set_char(self.bar_char(col, frame));
                cell.set_style(style);
            }
        }

        // 2. Gap between equalizer and label
        if self.label_width > 0 {
            let gap_x = x + self.bar_count as u16;
            if let Some(cell) = buf.cell_mut((gap_x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }

        // 3. Label text
        if self.label_width > 0 {
            let label_x = x + self.bar_count as u16 + 1;
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
            let ellipsis_x = x + self.bar_count as u16 + 1 + self.label_width as u16;
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
    fn update_theme_changes_bar_color_and_label_color() {
        let registry = crate::theme::ThemeRegistry::new();
        let mut theme1 = registry.get("opencode").cloned().unwrap();
        theme1.primary = RGBA::from_hex("#FF0000");
        // The accent colour is deliberately different: the component must
        // ignore it now that the per-column gradient is gone.
        theme1.accent = RGBA::from_hex("#0000FF");
        theme1.text_muted = RGBA::from_hex("#00FF00");

        let mut theme2 = registry.get("opencode").cloned().unwrap();
        theme2.primary = RGBA::from_hex("#00FF00");
        theme2.accent = RGBA::from_hex("#FF00FF");
        theme2.text_muted = RGBA::from_hex("#FFFF00");

        let mut spinner = AgentSpinnerBass::new("Test", &theme1);

        // Verify initial colors are from theme1
        assert_eq!(spinner.bar_color_base, theme1.primary);
        assert_eq!(spinner.label_color, theme1.text_muted);

        // Update to theme2
        spinner.update_theme(&theme2);

        // Verify colors are now from theme2
        assert_eq!(spinner.bar_color_base, theme2.primary);
        assert_eq!(spinner.label_color, theme2.text_muted);
    }

    #[test]
    fn bars_share_one_colour_shaded_only_by_height() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        // No per-column gradient: whenever two bars display the same
        // height, they must render in the same colour, regardless of their
        // column. A ±1 per-channel slack absorbs float rounding at the
        // colour byte boundary; a real gradient would differ far more.
        let mut checked = 0;
        let max_channel_diff = |a: RGBA, b: RGBA| -> i32 {
            let (ar, ag, ab, _) = a.to_ints();
            let (br, bg, bb, _) = b.to_ints();
            (i32::from(ar) - i32::from(br))
                .abs()
                .max((i32::from(ag) - i32::from(bg)).abs())
                .max((i32::from(ab) - i32::from(bb)).abs())
        };
        for frame in (BIRTH_DELAY_MAX + BIRTH_GROW_FRAMES)..600u32 {
            for a in 0..NUM_BARS {
                for b in (a + 1)..NUM_BARS {
                    let ha = spinner.rendered_height(a, frame);
                    let hb = spinner.rendered_height(b, frame);
                    if (ha - hb).abs() < 1e-3 {
                        checked += 1;
                        assert!(
                            max_channel_diff(
                                spinner.bar_color(a, frame),
                                spinner.bar_color(b, frame)
                            ) <= 1,
                            "bars {a} and {b} differ in colour at equal height"
                        );
                    }
                }
            }
        }
        assert!(checked > 0, "no equal-height pairs found to compare");
    }

    #[test]
    fn identical_spinners_produce_identical_frames() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();

        let mut a = AgentSpinnerBass::new("Working", &theme);
        let mut b = AgentSpinnerBass::new("Working", &theme);
        for _ in 0..120 {
            a.advance();
            b.advance();
            for col in 0..NUM_BARS {
                assert_eq!(
                    a.bar_char(col, a.frames_elapsed),
                    b.bar_char(col, b.frames_elapsed)
                );
            }
        }
    }

    #[test]
    fn bar_heights_stay_in_range_and_move() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        let mut moved = false;
        for frame in [0u32, 30, 60, 90, 150, 300] {
            for col in 0..NUM_BARS {
                let h = spinner.bar_height(col, frame);
                assert!(
                    (0.0..=1.0).contains(&h),
                    "bar {col} height {h} out of range at frame {frame}"
                );
            }
            if spinner.bar_char(0, frame) != spinner.bar_char(0, frame + 15) {
                moved = true;
            }
        }
        assert!(moved, "bars never changed height");
    }

    #[test]
    fn bars_stay_at_floor_until_birth_then_rise() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        for (col, &birth) in spinner.birth_steps.iter().enumerate() {
            assert_eq!(
                spinner.bar_char(col, birth.saturating_sub(1)),
                BIRTH_CHAR,
                "bar {col} not at floor before its birth step"
            );
            let grown = spinner.bar_char(col, birth + BIRTH_GROW_FRAMES + 1);
            assert!(BLOCK_CHARS.contains(&grown), "bar {col} glyph {grown} invalid");
        }
    }

    #[test]
    fn tall_bars_wobble_more_than_low_ones() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        // Wobble and updraft both scale with the bar's base height, so a
        // bar near the floor barely deviates from the shared wave while a
        // tall bar deviates visibly — i.e. the upper group moves
        // independently.
        let max_low_deviation = (WOBBLE_AMPLITUDE + UPDRAFT_AMPLITUDE) * 0.25;
        let mut saw_big_deviation = false;
        for frame in 0..600u32 {
            for col in 0..NUM_BARS {
                let base = spinner.bar_base_height(col, frame);
                let deviation = (spinner.bar_height(col, frame) - base).abs();
                if base < 0.25 {
                    assert!(
                        deviation <= max_low_deviation + 1e-4,
                        "low bar {col} wobbled by {deviation} at frame {frame}"
                    );
                } else if base > 0.8 && deviation > 0.5 * WOBBLE_AMPLITUDE {
                    saw_big_deviation = true;
                }
            }
        }
        assert!(
            saw_big_deviation,
            "tall bars never wobbled independently of the shared wave"
        );
    }

    #[test]
    fn updrafts_lift_randomly_and_surged_snappily() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        let mut saw_lift = false;
        let mut saw_calm = false;
        let mut saw_snap = false;
        let mut saw_bars_diverge = false;
        for frame in 0..900u32 {
            for col in 0..NUM_BARS {
                let lift = spinner.updraft(col, frame);
                assert!(
                    (0.0..=UPDRAFT_AMPLITUDE + 1e-4).contains(&lift),
                    "updraft lift {lift} out of range at frame {frame}"
                );
                if lift > 0.15 {
                    saw_lift = true;
                }
                if lift < 0.01 {
                    saw_calm = true;
                }
                // Snappy rise: a strong surge lands within two frames.
                if spinner.updraft(col, frame + 2) - lift > 0.08 {
                    saw_snap = true;
                }
                if col + 1 < NUM_BARS
                    && (spinner.updraft(col, frame) - spinner.updraft(col + 1, frame)).abs() > 0.1
                {
                    saw_bars_diverge = true;
                }
            }
        }
        assert!(saw_lift, "updrafts never lifted meaningfully");
        assert!(saw_calm, "updrafts were never calm — they read as a wave");
        assert!(saw_snap, "updrafts never surged snappily upward");
        assert!(
            saw_bars_diverge,
            "neighbouring updrafts never diverged — segments aligned"
        );
    }

    #[test]
    fn adjacent_bars_do_not_move_in_lockstep() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        // With per-bar detuning the two bars must disagree about their
        // direction of motion a meaningful share of the time.
        let mut same = 0u32;
        let mut opposite = 0u32;
        for frame in 40..500u32 {
            #[allow(clippy::cast_precision_loss)]
            let d0 = f64::from(spinner.bar_height(0, frame))
                - f64::from(spinner.bar_height(0, frame - 1));
            #[allow(clippy::cast_precision_loss)]
            let d1 = f64::from(spinner.bar_height(1, frame))
                - f64::from(spinner.bar_height(1, frame - 1));
            if d0.abs() < 1e-4 || d1.abs() < 1e-4 {
                continue;
            }
            if d0.signum() == d1.signum() {
                same += 1;
            } else {
                opposite += 1;
            }
        }
        assert!(opposite > 0, "bars never disagreed on direction");
        #[allow(clippy::cast_precision_loss)]
        let opposite_share = opposite as f32 / (opposite + same) as f32;
        assert!(
            opposite_share > 0.2,
            "bars move nearly in lockstep: {same} same vs {opposite} opposite"
        );
    }

    #[test]
    fn bar_color_darkens_as_it_rises() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();
        let spinner = AgentSpinnerBass::new("Working", &theme);

        let luminance = |c: RGBA| -> f32 {
            let (r, g, b, _) = c.to_ints();
            #[allow(clippy::cast_precision_loss)]
            let lum = 0.212_6 * f32::from(r)
                + 0.715_2 * f32::from(g)
                + 0.072_2 * f32::from(b);
            lum
        };

        // Compare each bar when it sits near the floor vs when it stands
        // tall: the tall frame must render in a darker tone.
        let mut comparisons = 0;
        for col in 0..NUM_BARS {
            let mut low = None;
            let mut high = None;
            for frame in (BIRTH_DELAY_MAX + BIRTH_GROW_FRAMES)..600u32 {
                let h = spinner.rendered_height(col, frame);
                if h < 0.15 {
                    low = Some(frame);
                }
                if h > 0.85 {
                    high = Some(frame);
                }
            }
            if let (Some(lo), Some(hi)) = (low, high) {
                comparisons += 1;
                let lum_low = luminance(spinner.bar_color(col, lo));
                let lum_high = luminance(spinner.bar_color(col, hi));
                assert!(
                    lum_high < lum_low,
                    "bar {col} not darker when tall ({lum_high} vs {lum_low})"
                );
            }
        }
        assert!(comparisons > 0, "no bar ever reached both extremes");
    }

    #[test]
    fn width_matches_agent_spinner_layout() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();

        // Bars + gap + "Working" (7) + widest ellipsis (3)
        let spinner = AgentSpinnerBass::new("Working", &theme);
        assert_eq!(spinner.width(), NUM_BARS + 1 + 7 + 3);

        // Empty label: bars only
        let spinner = AgentSpinnerBass::new("", &theme);
        assert_eq!(spinner.width(), NUM_BARS);
    }
}
