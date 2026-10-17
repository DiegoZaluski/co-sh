//! Animated agent spinner — a direct port of the opencode TUI scanner.
//!
//! This is a faithful port of `packages/tui/src/ui/spinner.ts`
//! from `anomalyco/opencode` in its **blocks** style — the exact
//! configuration the prompt uses (`createFrames({ style: "blocks",
//! inactiveFactor: 0.6, minAlpha: 0.3 })` driven at `interval={40}`):
//!
//! - **Bidirectional scan**: forward across `width` cells (one frame per
//!   cell), hold `holdEnd` (9) frames at the right edge, backward across
//!   `width − 1` frames, hold `holdStart` (30) frames at the left edge —
//!   a 54-frame cycle, identical to the original.
//! - **Blocks glyphs**: active cells render `■`, inactive cells `⬝`,
//!   exactly the original's blocks character pair.
//! - **Alpha-falloff trail** (`deriveTrailColors`): the lead at alpha 1.0,
//!   one bloom step at alpha 0.9 with a 1.15× brightness kick, then an
//!   exponential `0.65^(i−1)` decay — composited over the theme background
//!   because a terminal has no real alpha.
//! - **Global fade** (`createKnightRiderTrail`): inactive dots fade
//!   linearly down to `minAlpha` (0.3) across each hold and back up across
//!   each sweep, scaled by the `inactiveFactor` (0.6) base alpha.
//! - **Timing**: the original ticks at 40 ms/frame (25 fps); cosh drives
//!   `advance()` per 30 fps animation tick, so the scan frame index is
//!   `ticks × 5 / 6` — the exact same wall-clock pacing.
//!
//! Everything around the scanner row (label, per-activity status glyphs,
//! mode colour, deterministic render) follows the shared agent-spinner
//! surface so the component slots into the same 1-row spinner area.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};

use super::agent_spinner::activity_frames;

// ── Constants (verbatim from spinner.ts / prompt usage) ────────────────────

/// Scanner row width (`options.width ?? 8`).
const WIDTH: i32 = 8;

/// Frames the lead rests at the left edge (`options.holdStart ?? 30`).
const HOLD_START: u32 = 30;

/// Frames the lead rests at the right edge (`options.holdEnd ?? 9`).
const HOLD_END: u32 = 9;

/// Trail gradient steps (`deriveTrailColors` default `steps = 6`).
const TRAIL_STEPS: usize = 6;

/// Alpha of the inactive dots (`options.inactiveFactor = 0.6` at the call
/// site, overriding the 0.2 default).
const INACTIVE_FACTOR: f32 = 0.6;

/// Minimum alpha the fading bottoms out at (`options.minAlpha = 0.3`).
const MIN_ALPHA: f32 = 0.3;

/// Forward sweep: one frame per cell (`forwardFrames = totalChars`).
const SWEEP_FORWARD: u32 = WIDTH as u32;

/// Backward sweep: `totalChars - 1` frames.
const SWEEP_BACK: u32 = WIDTH as u32 - 1;

/// Total frames in one full scan cycle:
/// `width + holdEnd + (width - 1) + holdStart` (spinner.ts comment).
const TOTAL_FRAMES: u32 = SWEEP_FORWARD + HOLD_END + SWEEP_BACK + HOLD_START;

/// cosh advances one animation tick per 30 fps frame (`render.rs`), while
/// the original ticks at `interval={40}` (25 fps). The scan frame index is
/// therefore `ticks × 5/6` — exact integer math: tick 6 = 200 ms =
/// scan frame 5.
const TICKS_PER_SCAN_FRAME: u32 = 5;

/// Glyph for a cell inside the trail band (spinner.ts blocks style).
const ACTIVE_CHAR: char = '■';

/// Glyph for an inactive cell (spinner.ts blocks style).
const INACTIVE_CHAR: char = '⬝';

/// Frames the shared spinner surface needs before it reports initialized
/// (the birth window every `/spinner` style observes).
const INIT_FRAMES: u32 = 20;

/// Status glyph animation speed: number of ticks per glyph step.
const STATUS_ANIM_SPEED: u32 = 8;

// ── Scanner state (port of getScannerState, bidirectional) ─────────────────

/// One frame's scanner phase, exactly the fields
/// `getScannerState` returns for `direction: "bidirectional"`.
struct ScannerState {
    active_position: i32,
    is_holding: bool,
    hold_progress: i32,
    hold_total: i32,
    movement_progress: i32,
    movement_total: i32,
    is_moving_forward: bool,
}

/// `getScannerState(frameIndex, totalChars, options)` — bidirectional arm.
fn scanner_state(frame_index: u32) -> ScannerState {
    let frame = frame_index % TOTAL_FRAMES;
    if frame < SWEEP_FORWARD {
        // Moving forward
        ScannerState {
            active_position: frame as i32,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame as i32,
            movement_total: SWEEP_FORWARD as i32,
            is_moving_forward: true,
        }
    } else if frame < SWEEP_FORWARD + HOLD_END {
        // Holding at end
        ScannerState {
            active_position: WIDTH - 1,
            is_holding: true,
            hold_progress: (frame - SWEEP_FORWARD) as i32,
            hold_total: HOLD_END as i32,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: true,
        }
    } else if frame < SWEEP_FORWARD + HOLD_END + SWEEP_BACK {
        // Moving backward
        let backward_index = (frame - SWEEP_FORWARD - HOLD_END) as i32;
        ScannerState {
            active_position: WIDTH - 2 - backward_index,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: backward_index,
            movement_total: SWEEP_BACK as i32,
            is_moving_forward: false,
        }
    } else {
        // Holding at start
        ScannerState {
            active_position: 0,
            is_holding: true,
            hold_progress: (frame - SWEEP_FORWARD - HOLD_END - SWEEP_BACK) as i32,
            hold_total: HOLD_START as i32,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: false,
        }
    }
}

/// `calculateColorIndex` — directional distance (the trail runs behind the
/// movement direction); during a hold the index is shifted out by the hold
/// progress so the band slides off the row exactly like the original.
fn color_index(frame_index: u32, char_index: i32) -> i32 {
    let state = scanner_state(frame_index);

    let directional_distance = if state.is_moving_forward {
        state.active_position - char_index
    } else {
        char_index - state.active_position
    };

    if state.is_holding {
        return directional_distance + state.hold_progress;
    }

    if directional_distance > 0 && directional_distance < TRAIL_STEPS as i32 {
        return directional_distance;
    }

    if directional_distance == 0 {
        return 0;
    }

    -1
}

/// The global fade factor `createKnightRiderTrail` applies to the inactive
/// dots: down to `minAlpha` across a hold, up from `minAlpha` across a
/// sweep (`enableFading` defaults to true in the original).
#[allow(clippy::cast_precision_loss)]
fn fade_factor(state: &ScannerState) -> f32 {
    if state.is_holding && state.hold_total > 0 {
        let progress = (state.hold_progress as f32 / state.hold_total as f32).min(1.0);
        (1.0 - progress * (1.0 - MIN_ALPHA)).max(MIN_ALPHA)
    } else if !state.is_holding && state.movement_total > 0 {
        let progress = (state.movement_progress as f32
            / 1.0f32.max(state.movement_total as f32 - 1.0))
        .min(1.0);
        MIN_ALPHA + progress * (1.0 - MIN_ALPHA)
    } else {
        1.0
    }
}

// ── Colour helpers ─────────────────────────────────────────────────────────

/// `deriveTrailColors(brightColor, 6)`: the lead at full strength, one
/// bloom step (0.9 alpha, 1.15× brightness), then exponential `0.65^(i−1)`
/// alpha decay. Returns `(rgb, alpha)` pairs — the alpha is applied at
/// composite time over the theme background, since a terminal cell has no
/// real alpha channel.
#[allow(clippy::cast_precision_loss)]
fn trail_colors(base: RGBA) -> Vec<(RGBA, f32)> {
    let (r, g, b, _) = base.to_ints();
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);

    (0..TRAIL_STEPS)
        .map(|i| {
            let (alpha, brightness_factor): (f32, f32) = if i == 0 {
                (1.0, 1.0)
            } else if i == 1 {
                (0.9, 1.15)
            } else {
                (0.65f32.powi(i as i32 - 1), 1.0)
            };
            let rr = (r * brightness_factor).min(1.0);
            let gg = (g * brightness_factor).min(1.0);
            let bb = (b * brightness_factor).min(1.0);
            (RGBA::from_values(rr, gg, bb, 1.0), alpha)
        })
        .collect()
}

/// Alpha-composite `fg` over `bg`: the terminal twin of the RGBA alpha the
/// original renderer applies per glyph.
#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn composite(bg: RGBA, fg: RGBA, alpha: f32) -> RGBA {
    let alpha = alpha.clamp(0.0, 1.0);
    let (br, bgc, bb, _) = bg.to_ints();
    let (fr, fgc, fb, _) = fg.to_ints();
    let mix = |b: u8, f: u8| -> u8 {
        (f32::from(b) * (1.0 - alpha) + f32::from(f) * alpha).round() as u8
    };
    RGBA::from_ints(mix(br, fr), mix(bgc, fgc), mix(bb, fb), 255)
}

// ── AgentSpinnerOpencode ───────────────────────────────────────────────────

/// An animated spinner shaped like the opencode Knight-Rider scanner: a
/// bright `■` lead with an alpha-fading trail sweeping the row back and
/// forth, followed by a label and a per-status glyph animation.
///
/// A direct port of `anomalyco/opencode`'s `ui/spinner.ts` blocks style
/// (`inactiveFactor: 0.6`, `minAlpha: 0.3`, 40 ms frames) dressed in the
/// shared agent-spinner surface (label, status glyphs, mode colour,
/// deterministic render).
pub struct AgentSpinnerOpencode {
    /// Animation ticks elapsed (30 fps); the scan frame index is
    /// `ticks × 5 / 6` — the original's 40 ms pacing.
    ticks_elapsed: u32,
    /// Current status-glyph step (ticks at `STATUS_ANIM_SPEED`).
    status_step: u32,
    /// Glyph sequence of the current agent activity (see
    /// [`activity_frames`]): each status cycles its own animation.
    frames: &'static [&'static str],
    /// Whether the birth window has elapsed.
    initialized: bool,
    /// Label text displayed after the scanner row.
    label: String,
    /// The scanner's lead colour (the active prompt mode colour).
    scan_color: RGBA,
    /// Background the alpha-faded glyphs composite over.
    background: RGBA,
    /// Colour used for the label and status glyph.
    label_color: RGBA,
    /// Number of characters in the label.
    label_width: usize,
}

impl AgentSpinnerOpencode {
    /// Create a new scanner spinner with the given label, sourcing the
    /// lead colour from `theme.primary`, the composite background from
    /// `theme.background` and the label colour from `theme.text_muted`.
    pub fn new(label: &str, theme: &Theme) -> Self {
        Self {
            ticks_elapsed: 0,
            status_step: 0,
            frames: activity_frames(crate::types::AgentActivity::Working),
            initialized: false,
            label: label.to_string(),
            scan_color: theme.primary,
            background: theme.background,
            label_color: theme.text_muted,
            label_width: label.chars().count(),
        }
    }

    /// Advance the animation by one 30 fps tick. The scan clock runs at the
    /// original's 40 ms frame interval (`ticks × 5 / 6`), the status glyphs
    /// at the shared surface's pace.
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
    /// own glyph animation from its first frame. The scan cycle is
    /// untouched; a repeated activity is a cheap no-op.
    pub fn set_activity(&mut self, activity: crate::types::AgentActivity) {
        if self.label != activity.label() {
            self.set_label(activity.label());
            self.frames = activity_frames(activity);
            self.status_step = 0;
        }
    }

    /// Update the scan colour for the active prompt mode without resetting
    /// the animation state.
    pub fn set_mode_color(&mut self, color: RGBA) {
        self.scan_color = color;
    }

    /// Update the theme colours (scan, composite background and label
    /// colour) without resetting the animation state.
    pub fn update_theme(&mut self, theme: &Theme) {
        self.scan_color = theme.primary;
        self.background = theme.background;
        self.label_color = theme.text_muted;
    }

    /// Total width in terminal cells (cells + gap + label + status glyph).
    pub fn width(&self) -> usize {
        let mut w = WIDTH as usize;
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

    /// Current scan frame index at the original's 40 ms pacing: ticks are
    /// 1000/30 ms apart, so `frames = ticks × (1000/30) / 40 = ticks × 5/6`
    /// (exact integer math — tick 6 = 200 ms = frame 5).
    pub const fn scan_frame(&self) -> u32 {
        self.ticks_elapsed / 6 * TICKS_PER_SCAN_FRAME
            + (self.ticks_elapsed % 6) * TICKS_PER_SCAN_FRAME / 6
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
        WIDTH as u16 + 1 + self.label_width as u16
    }

    // ── Cell painting (the ported per-glyph logic) ──────────────────────

    /// Colour and glyph for one scanner cell at `frame`: the trail band's
    /// composited colour when the cell is active, otherwise the scan colour
    /// at `inactiveFactor × fadeFactor` alpha (the original's
    /// `defaultRgba.a = baseInactiveAlpha * fadeFactor`).
    #[allow(clippy::cast_precision_loss)]
    fn cell(&self, frame: u32, char_index: i32) -> (char, RGBA) {
        let colors = trail_colors(self.scan_color);
        let state = scanner_state(frame);
        let index = color_index(frame, char_index);

        if index >= 0 && index < TRAIL_STEPS as i32 {
            let (rgb, alpha) = colors[index as usize];
            (ACTIVE_CHAR, composite(self.background, rgb, alpha))
        } else {
            // deriveInactiveColor: same RGB, alpha = inactiveFactor, then
            // scaled by the global fade factor.
            let alpha = INACTIVE_FACTOR * fade_factor(&state);
            (INACTIVE_CHAR, composite(self.background, self.scan_color, alpha))
        }
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)`.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        let frame = self.scan_frame();

        // 1. Scanner cells: the lead plus its trail, everything else the
        //    faded inactive block.
        for col in 0..WIDTH {
            let cell_x = x.saturating_add(col as u16);
            let (ch, color) = self.cell(frame, col);
            let style = Style::default().fg(rgba_color(color));
            if let Some(cell) = buf.cell_mut((cell_x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        // 2. Gap between scanner and label.
        if self.label_width > 0 {
            let gap_x = x.saturating_add(WIDTH as u16);
            if let Some(cell) = buf.cell_mut((gap_x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }

        // 3. Label text.
        if self.label_width > 0 {
            let label_x = x.saturating_add(WIDTH as u16).saturating_add(1);
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
                .saturating_add(WIDTH as u16)
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

    fn test_theme() -> Theme {
        let registry = crate::theme::ThemeRegistry::new();
        registry.get("opencode").cloned().unwrap()
    }

    // ── getScannerState (bidirectional) ────────────────────────────────

    #[test]
    fn scanner_state_matches_the_bidirectional_phases() {
        // Forward: activePosition = frameIndex, one frame per cell.
        for f in 0..SWEEP_FORWARD {
            let s = scanner_state(f);
            assert_eq!(s.active_position, f as i32);
            assert!(!s.is_holding);
            assert!(s.is_moving_forward);
            assert_eq!(s.movement_total, SWEEP_FORWARD as i32);
        }
        // Hold end: pinned at the last cell, progress counting up.
        for i in 0..HOLD_END {
            let s = scanner_state(SWEEP_FORWARD + i);
            assert!(s.is_holding, "frame {}", SWEEP_FORWARD + i);
            assert_eq!(s.active_position, WIDTH - 1);
            assert_eq!(s.hold_progress, i as i32);
            assert_eq!(s.hold_total, HOLD_END as i32);
        }
        // Backward: walks WIDTH-2 down to 0.
        for i in 0..SWEEP_BACK {
            let s = scanner_state(SWEEP_FORWARD + HOLD_END + i);
            assert!(!s.is_holding);
            assert!(!s.is_moving_forward);
            assert_eq!(s.active_position, WIDTH - 2 - i as i32);
        }
        // Hold start: pinned at cell 0, progress counting up.
        let base = SWEEP_FORWARD + HOLD_END + SWEEP_BACK;
        for i in 0..HOLD_START {
            let s = scanner_state(base + i);
            assert!(s.is_holding, "frame {}", base + i);
            assert_eq!(s.active_position, 0);
            assert_eq!(s.hold_progress, i as i32);
            assert_eq!(s.hold_total, HOLD_START as i32);
        }
        // The cycle wraps cleanly: totalFrames = width + holdEnd +
        // (width-1) + holdStart.
        let a = scanner_state(0);
        let b = scanner_state(TOTAL_FRAMES);
        assert_eq!(a.active_position, b.active_position);
        assert_eq!(TOTAL_FRAMES, 54);
    }

    // ── calculateColorIndex ────────────────────────────────────────────

    #[test]
    fn color_index_trails_behind_the_movement_direction() {
        // Mid forward sweep (frame 4, lead on cell 4): cells 4..1 carry
        // indices 0..3, everything ahead of the lead is -1.
        assert_eq!(color_index(4, 4), 0);
        assert_eq!(color_index(4, 3), 1);
        assert_eq!(color_index(4, 2), 2);
        assert_eq!(color_index(4, 1), 3);
        // The 6-step trail reaches 4 cells back on an 8-wide row.
        assert_eq!(color_index(4, 0), 4);
        assert_eq!(color_index(4, 5), -1); // ahead of the lead

        // Backward sweep (frame 8+9+2 → backwardIndex 2, lead on cell 4):
        // the trail runs to the RIGHT of the lead.
        let f = SWEEP_FORWARD + HOLD_END + 2;
        assert_eq!(scanner_state(f).active_position, WIDTH - 2 - 2);
        assert_eq!(color_index(f, WIDTH - 2 - 2), 0);
        assert_eq!(color_index(f, WIDTH - 2 - 1), 1);
        assert_eq!(color_index(f, WIDTH - 5), -1); // left of the lead

        // During a hold the index slides out by holdProgress: the band
        // exits the row instead of freezing.
        let hold = SWEEP_FORWARD; // first end-hold frame
        assert_eq!(color_index(hold, WIDTH - 1), 0);
        assert_eq!(color_index(hold + 3, WIDTH - 1), 3);
        // Six frames in the band has fully left the 6-color row.
        assert_eq!(color_index(hold + 6, WIDTH - 1), TRAIL_STEPS as i32);
    }

    // ── createKnightRiderTrail fade ────────────────────────────────────

    #[test]
    fn fade_bottoms_out_at_min_alpha_across_holds() {
        // Start hold: progress 0 → 1.0, hold 30/30 → minAlpha.
        let base = SWEEP_FORWARD + HOLD_END + SWEEP_BACK;
        assert!((fade_factor(&scanner_state(base)) - 1.0).abs() < 1e-6);
        let mid = scanner_state(base + HOLD_START / 2);
        let expected = (1.0 - 0.5 * (1.0 - MIN_ALPHA)).max(MIN_ALPHA);
        assert!((fade_factor(&mid) - expected).abs() < 1e-6);
        let end = scanner_state(base + HOLD_START - 1);
        let expected_end = (1.0 - ((HOLD_START - 1) as f32 / HOLD_START as f32) * (1.0 - MIN_ALPHA))
            .max(MIN_ALPHA);
        assert!((fade_factor(&end) - expected_end).abs() < 1e-6);

        // Movement: fades up from minAlpha across the forward sweep.
        let first = scanner_state(0);
        assert!((fade_factor(&first) - MIN_ALPHA).abs() < 1e-6);
        let last = scanner_state(SWEEP_FORWARD - 1);
        assert!((fade_factor(&last) - 1.0).abs() < 1e-6);
    }

    // ── deriveTrailColors ──────────────────────────────────────────────

    #[test]
    fn trail_alphas_follow_the_original_falloff() {
        let colors = trail_colors(RGBA::from_ints(255, 0, 0, 255));
        assert_eq!(colors.len(), TRAIL_STEPS);
        let alphas: Vec<f32> = colors.iter().map(|(_, a)| *a).collect();
        assert_eq!(alphas[0], 1.0);
        assert_eq!(alphas[1], 0.9);
        // Exponential decay from index 2 on: 0.65^(i-1).
        assert!((alphas[2] - 0.65).abs() < 1e-6);
        assert!((alphas[3] - 0.65f32.powi(2)).abs() < 1e-6);
        assert!((alphas[4] - 0.65f32.powi(3)).abs() < 1e-6);
        assert!((alphas[5] - 0.65f32.powi(4)).abs() < 1e-6);
        // Strictly decreasing overall.
        assert!(alphas.windows(2).all(|w| w[0] > w[1]));
    }

    #[test]
    fn bloom_step_brightens_but_never_overflows() {
        // 1.15× on a near-white base clamps at 1.0.
        let colors = trail_colors(RGBA::from_ints(240, 250, 255, 255));
        let (rgb, _) = colors[1];
        let (r, g, b, _) = rgb.to_ints();
        assert_eq!(r, 255);
        assert_eq!(g, 255);
        assert_eq!(b, 255);
        // And a dim base actually brightens.
        let dim = trail_colors(RGBA::from_ints(100, 0, 0, 255));
        let (rgb1, _) = dim[1];
        let (r1, _, _, _) = rgb1.to_ints();
        assert_eq!(r1, (100.0_f32 * 1.15).round() as u8);
    }

    // ── Glyph choice (blocks style) ────────────────────────────────────

    #[test]
    fn blocks_style_glyphs_are_square_and_small_square() {
        let theme = test_theme();
        let spinner = AgentSpinnerOpencode::new("Working", &theme);
        // Lead cell at frame 0 is active.
        let (ch, _) = spinner.cell(0, 0);
        assert_eq!(ch, ACTIVE_CHAR);
        // Cell ahead of the lead is inactive.
        let (ch, _) = spinner.cell(0, 5);
        assert_eq!(ch, INACTIVE_CHAR);
        assert_eq!(ACTIVE_CHAR, '■');
        assert_eq!(INACTIVE_CHAR, '⬝');
    }

    // ── Shared surface ─────────────────────────────────────────────────

    #[test]
    fn trail_dims_behind_the_lead() {
        let theme = test_theme();
        let spinner = AgentSpinnerOpencode::new("Working", &theme);

        let lum = |c: RGBA| -> f32 {
            let (r, g, b, _) = c.to_ints();
            #[allow(clippy::cast_precision_loss)]
            let v = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
            v
        };
        // Mid-sweep on cell 4: the lead is brighter than every trail step.
        let frame = 4;
        let (_, lead_color) = spinner.cell(frame, 4);
        let lead_lum = lum(lead_color);
        for col in 1..4 {
            let (_, c) = spinner.cell(frame, 4 - col);
            assert!(lum(c) < lead_lum, "trail {col} not dimmer than the lead");
        }
        // Inactive cells are dimmer still.
        let (_, idle) = spinner.cell(frame, 7);
        assert!(lum(idle) < lead_lum);
    }

    #[test]
    fn scan_clock_runs_at_the_original_40ms_pacing() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerOpencode::new("Working", &theme);
        // 6 ticks = 200 ms = 5 frames of 40 ms.
        for _ in 0..6 {
            spinner.advance();
        }
        assert_eq!(spinner.scan_frame(), 5);
        // 60 ticks = 2 s = 50 frames.
        for _ in 6..60 {
            spinner.advance();
        }
        assert_eq!(spinner.scan_frame(), 50);
        // 108 ticks = 3.6 s = 90 frames: the scan index wraps mod 54 with
        // the same phase the original would be in.
        for _ in 60..108 {
            spinner.advance();
        }
        assert_eq!(spinner.scan_frame(), 90);
        assert_eq!(scanner_state(90).active_position, scanner_state(36).active_position);
    }

    #[test]
    fn identical_spinners_produce_identical_frames() {
        let theme = test_theme();
        let mut a = AgentSpinnerOpencode::new("Working", &theme);
        let mut b = AgentSpinnerOpencode::new("Working", &theme);
        for _ in 0..120 {
            a.advance();
            b.advance();
            assert_eq!(a.ticks_elapsed, b.ticks_elapsed);
            assert_eq!(a.scan_frame(), b.scan_frame());
        }
    }

    #[test]
    fn set_activity_swaps_label_without_disturbing_the_scan() {
        use crate::types::AgentActivity;
        let theme = test_theme();

        let mut spinner = AgentSpinnerOpencode::new(AgentActivity::Working.label(), &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let ticks_before = spinner.ticks_elapsed;

        spinner.set_activity(AgentActivity::for_tool("web_search"));
        assert_eq!(spinner.label, "searching");
        assert!(spinner.width() > WIDTH as usize);

        // Re-applying the SAME activity is a no-op; the scan clock is
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
        let mut spinner = AgentSpinnerOpencode::new(AgentActivity::Working.label(), &theme);
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
        let mut spinner = AgentSpinnerOpencode::new("Working", &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        spinner.render(&mut buf, 0, 0);
    }

    #[test]
    fn width_matches_spinner_layout() {
        let theme = test_theme();

        let spinner = AgentSpinnerOpencode::new("Working", &theme);
        assert_eq!(spinner.width(), WIDTH as usize + 1 + 7 + 1);

        let spinner = AgentSpinnerOpencode::new("", &theme);
        assert_eq!(spinner.width(), WIDTH as usize);
    }
}
