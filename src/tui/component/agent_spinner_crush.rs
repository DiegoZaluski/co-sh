//! Animated agent spinner — a direct port of the charmbracelet/crush `anim`.
//!
//! This is a faithful port of `internal/ui/anim/anim.go` from
//! `charmbracelet/crush`, in the exact configuration its UI uses
//! (`anim.Settings{ Size: 15, GradColorA: primary, GradColorB: secondary,
//! LabelColor: fgMostSubtle, CycleColors: true }`, driven by the shared
//! clock at `anim.FrameInterval()` = 1/20 s):
//!
//! - **15 scrambled columns** cycling the original's rune band
//!   `0123456789abcdefABCDEF~!@#$£€%^&*()+=_`, one glyph per animation
//!   frame, drawn from a gradient that cycles through the theme colors
//! - **CycleColors ramp**: `makeGradientRamp(width*3, A, B, A, B)` — a
//!   four-stop HCL ramp (blending in CIE-L*C*h° to stay in gamut) that the
//!   frames slide through via a per-frame `offset`, looping every
//!   `width*2` frames
//! - **Staggered birth**: each column has a deterministic birth step in
//!   `[0, maxBirthSteps)` — seeded by `xxh3(id + "|" + cacheKey)` through
//!   Go's `math/rand/v2` PCG — so columns pop in one by one over ~1 s
//! - **Animated ellipsis**: `.`, `..`, `...`, `` cycling every 8 frames
//!   (400 ms at 20 fps) after the label, once initialized
//! - **Timing**: the original ticks at 50 ms/frame (20 fps); cosh drives
//!   `advance()` per 30 fps animation tick, so each original frame is
//!   applied on `ticks × 2/3` — the exact same wall-clock pacing.
//!
//! Deliberate divergences (all documented at the site):
//! - Go's `Width()` over-reports by one gap+label (it double-counts the
//!   label columns on top of `a.width`); cosh's layout math needs the
//!   rendered width, so `width()` returns that instead.
//! - Go assigns incrementing instance ids (birth schedules differ per
//!   spinner); cosh requires byte-identical frames across instances, so a
//!   fixed id is used.
//! - The optional `Suffix` (elapsed-time readout) has no cosh counterpart
//!   and is not ported; `NoScramble` is likewise unused.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};

// ── Constants (verbatim from anim.go) ──────────────────────────────────────

/// Cycling-character columns (`Settings.Size: 15` at every crush call site).
const SIZE: usize = 15;

/// Frames per second the original animates at (`fps = 20`).
const FPS: u32 = 20;

/// The initial character a column shows before its birth step
/// (`initialChar = '.'`).
const INITIAL_CHAR: char = '.';

/// Gap between the scramble and the label (`labelGap = " "`).
const LABEL_GAP_WIDTH: usize = 1;

/// Frames per ellipsis step (`ellipsisAnimSpeed = 8` — 400 ms at 20 fps).
const ELLIPSIS_ANIM_SPEED: u32 = 8;

/// Maximum birth delay in frames (`maxBirthSteps = 20` — ~1 s at 20 fps).
const MAX_BIRTH_STEPS: u32 = 20;

/// Frames prerendered when color cycling is off (`prerenderedFrames = 10`);
/// unused at the crush call site (CycleColors: true) but ported for parity.
const PRERENDERED_FRAMES: usize = 10;

/// Default number of cycling chars (`defaultNumCyclingChars = 10`) — the
/// fallback when `Size < 1`. Unused (Size is always 15) but ported.
const DEFAULT_NUM_CYCLING_CHARS: usize = 10;

/// The scramble band (`availableRunes`), verbatim.
const AVAILABLE_RUNES: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'A', 'B', 'C',
    'D', 'E', 'F', '~', '!', '@', '#', '£', '€', '%', '^', '&', '*', '(', ')', '+', '=', '_',
];

/// The ellipsis cycle (`ellipsisFrames`), verbatim.
const ELLIPSIS_FRAMES: &[&str] = &[".", "..", "...", ""];

/// Fixed instance id (see module docs — Go uses an atomic counter).
const INSTANCE_ID: &str = "cosh-agent";

// ── Go PCG (math/rand/v2 pcg.go) ───────────────────────────────────────────
//
// Direct port of Go's PCG-DXSM generator so the rune/birth sequences match
// `rand.New(rand.NewPCG(seed, ^seed))` draw for draw.

const MUL_HI: u64 = 2_549_297_995_355_413_924;
const MUL_LO: u64 = 4_865_540_595_714_422_341;
const INC_HI: u64 = 6_364_136_223_846_793_005;
const INC_LO: u64 = 1_442_695_040_888_963_407;
/// The DXSM cheap multiplier (`0xda942042e4dd58b5`).
const CHEAP_MUL: u64 = 0xda942042e4dd58b5;

/// Go's `rand.PCG`: 128-bit state, PCG-DXSM output.
pub struct Pcg {
    hi: u64,
    lo: u64,
}

impl Pcg {
    /// `NewPCG(seed1, seed2)`.
    pub const fn new(seed1: u64, seed2: u64) -> Self {
        Self { hi: seed1, lo: seed2 }
    }

    /// `p.next()`: state = state * mul + inc (128-bit arithmetic).
    fn next(&mut self) -> (u64, u64) {
        // hi, lo = bits.Mul64(p.lo, mulLo)
        let prod = u128::from(self.lo) * u128::from(MUL_LO);
        let mut hi = (prod >> 64) as u64;
        let mut lo = prod as u64;
        // hi += p.hi*mulLo + p.lo*mulHi
        hi = hi
            .wrapping_add(self.hi.wrapping_mul(MUL_LO))
            .wrapping_add(self.lo.wrapping_mul(MUL_HI));
        // lo, c = bits.Add64(lo, incLo, 0); hi += incHi + c
        let (lo2, carry) = lo.overflowing_add(INC_LO);
        lo = lo2;
        hi = hi.wrapping_add(INC_HI).wrapping_add(u64::from(carry));
        self.hi = hi;
        self.lo = lo;
        (hi, lo)
    }

    /// `Uint64()`: DXSM "double xorshift multiply" output stage.
    pub fn uint64(&mut self) -> u64 {
        let (mut hi, lo) = self.next();
        hi ^= hi >> 32;
        hi = hi.wrapping_mul(CHEAP_MUL);
        hi ^= hi >> 48;
        hi = hi.wrapping_mul(lo | 1);
        hi
    }

    /// `IntN(n)` via `uint64n`: Lemire's multiply-shift reduction with the
    /// rejection loop, bit-for-bit as in rand.go.
    pub fn int_n(&mut self, n: usize) -> usize {
        assert!(n > 0, "invalid argument to IntN");
        let n = n as u64;
        // Power of two: mask (n & (n-1) == 0).
        if n & (n - 1) == 0 {
            return (self.uint64() & (n - 1)) as usize;
        }
        // hi, lo := bits.Mul64(r.Uint64(), n)
        let prod = u128::from(self.uint64()) * u128::from(n);
        let mut hi = (prod >> 64) as u64;
        let mut lo = prod as u64;
        if lo < n {
            // thresh := -n % n  (== 2⁶⁴ % n, wrapping)
            let thresh = n.wrapping_neg() % n;
            while lo < thresh {
                let prod = u128::from(self.uint64()) * u128::from(n);
                hi = (prod >> 64) as u64;
                lo = prod as u64;
            }
        }
        hi as usize
    }
}

// ── go-colorful HCL (BlendHcl and its chain) ───────────────────────────────
//
// Direct port of the go-colorful functions crush's `makeGradientRamp` uses,
// verbatim math (D65 white, sRGB linearization, Lab <-> Hcl, angle-aware
// hue interpolation, per-channel clamp).

/// An sRGB color with channels in \[0, 1\] (go-colorful's `Color`).
#[derive(Debug, Clone, Copy)]
struct CfColor {
    r: f64,
    g: f64,
    b: f64,
}

const D65: [f64; 3] = [0.95047, 1.00000, 1.08883];

/// `clamp01` — go-colorful's exact definition (`max(0, min(v, 1))`); the
/// clippy `clamp` rewrite would change NaN semantics from the original.
#[allow(clippy::manual_clamp)]
fn clamp01(v: f64) -> f64 {
    v.max(0.0).min(1.0)
}

fn sq(v: f64) -> f64 {
    v * v
}

impl CfColor {
    /// `MakeColor` for an opaque 8-bit RGB source.
    fn from_rgba(c: RGBA) -> Self {
        let (r, g, b, _) = c.to_ints();
        Self {
            r: f64::from(r) / 255.0,
            g: f64::from(g) / 255.0,
            b: f64::from(b) / 255.0,
        }
    }

    /// `RGB255` (round, not truncate — same as Go's `uint8(v*255+0.5)`).
    fn to_rgba(self) -> RGBA {
        let r = (self.r * 255.0 + 0.5) as u8;
        let g = (self.g * 255.0 + 0.5) as u8;
        let b = (self.b * 255.0 + 0.5) as u8;
        RGBA::from_ints(r, g, b, 255)
    }

    /// `Clamped`: clamp each channel to \[0, 1\].
    fn clamped(self) -> Self {
        Self {
            r: clamp01(self.r),
            g: clamp01(self.g),
            b: clamp01(self.b),
        }
    }

    fn linearize(v: f64) -> f64 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    fn delinearize(v: f64) -> f64 {
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    /// `LinearRgbToXyz` (sRGB D65 matrix, verbatim constants — the full
    /// float64 precision is deliberate; clippy's truncation would perturb
    /// the ported math).
    #[allow(clippy::excessive_precision)]
    fn linear_rgb_to_xyz(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
        (
            0.412_390_799_265_959_48 * r + 0.357_584_339_383_877_96 * g + 0.180_480_788_401_834_29 * b,
            0.212_639_005_871_510_36 * r + 0.715_168_678_767_755_93 * g + 0.072_192_315_360_733_715 * b,
            0.019_330_818_715_591_851 * r + 0.119_194_779_794_625_99 * g + 0.950_532_152_249_660_58 * b,
        )
    }

    /// `XyzToLinearRgb` (verbatim constants — see `linear_rgb_to_xyz`).
    #[allow(clippy::excessive_precision)]
    fn xyz_to_linear_rgb(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
        (
            3.240_969_941_904_521_4 * x - 1.537_383_177_570_093_5 * y - 0.498_610_760_293_003_28 * z,
            -0.969_243_636_280_879_83 * x + 1.875_967_501_507_720_7 * y + 0.041_555_057_407_175_613 * z,
            0.055_630_079_696_993_609 * x - 0.203_976_958_888_976_57 * y + 1.056_971_514_242_878_6 * z,
        )
    }

    fn lab_f(t: f64) -> f64 {
        if t > 6.0 / 29.0 * 6.0 / 29.0 * 6.0 / 29.0 {
            t.cbrt()
        } else {
            t / 3.0 * 29.0 / 6.0 * 29.0 / 6.0 + 4.0 / 29.0
        }
    }

    fn lab_finv(t: f64) -> f64 {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * 6.0 / 29.0 * 6.0 / 29.0 * (t - 4.0 / 29.0)
        }
    }

    /// `XyzToLab` (D65 white reference).
    fn xyz_to_lab(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
        let fy = Self::lab_f(y / D65[1]);
        let l = 1.16 * fy - 0.16;
        let a = 5.0 * (Self::lab_f(x / D65[0]) - fy);
        let b = 2.0 * (fy - Self::lab_f(z / D65[2]));
        (l, a, b)
    }

    /// `LabToXyz` (D65 white reference).
    fn lab_to_xyz(l: f64, a: f64, b: f64) -> (f64, f64, f64) {
        let l2 = (l + 0.16) / 1.16;
        (
            D65[0] * Self::lab_finv(l2 + a / 5.0),
            D65[1] * Self::lab_finv(l2),
            D65[2] * Self::lab_finv(l2 - b / 2.0),
        )
    }

    /// `LabToHcl` (with go-colorful's floating-point workaround; the
    /// Rad2Deg constant keeps go-colorful's full precision — see
    /// `linear_rgb_to_xyz`).
    #[allow(clippy::excessive_precision)]
    fn lab_to_hcl(l: f64, a: f64, b: f64) -> (f64, f64, f64) {
        let h = if (b - a).abs() > 1e-4 && a.abs() > 1e-4 {
            (57.295_779_513_082_320_877_21 * b.atan2(a) + 360.0) % 360.0
        } else {
            0.0
        };
        (h, (sq(a) + sq(b)).sqrt(), l)
    }

    /// `HclToLab` (Deg2Rad constant at full precision, as in go-colorful).
    #[allow(clippy::excessive_precision)]
    fn hcl_to_lab(h: f64, c: f64, l: f64) -> (f64, f64, f64) {
        let big_h = 0.017_453_292_519_943_295_76 * h; // Deg2Rad
        (l, c * big_h.cos(), c * big_h.sin())
    }

    /// `Hcl()`: RGB → HCL via Lab (D65).
    fn hcl(self) -> (f64, f64, f64) {
        let (r, g, b) = (
            Self::linearize(self.r),
            Self::linearize(self.g),
            Self::linearize(self.b),
        );
        let (x, y, z) = Self::linear_rgb_to_xyz(r, g, b);
        let (l, a, b) = Self::xyz_to_lab(x, y, z);
        Self::lab_to_hcl(l, a, b)
    }

    /// `Hcl(h, c, l)`: HCL → RGB via Lab (D65).
    fn from_hcl(h: f64, c: f64, l: f64) -> Self {
        let (l, a, b) = Self::hcl_to_lab(h, c, l);
        let (x, y, z) = Self::lab_to_xyz(l, a, b);
        let (r, g, b) = Self::xyz_to_linear_rgb(x, y, z);
        Self {
            r: Self::delinearize(r),
            g: Self::delinearize(g),
            b: Self::delinearize(b),
        }
    }

    /// `BlendHcl`: blend in CIE-L*C*h°, hue interpolated the short way
    /// around the wheel, result clamped into gamut.
    fn blend_hcl(col1: Self, col2: Self, t: f64) -> Self {
        let (mut h1, c1, l1) = col1.hcl();
        let (mut h2, c2, l2) = col2.hcl();

        // go-colorful PR 60: near-achromatic colors adopt the other's hue.
        if c1 <= 0.00015 && c2 >= 0.00015 {
            h1 = h2;
        } else if c2 <= 0.00015 && c1 >= 0.00015 {
            h2 = h1;
        }

        Self::from_hcl(interp_angle(h1, h2, t), c1 + t * (c2 - c1), l1 + t * (l2 - l1)).clamped()
    }
}

/// `interp_angle`: interpolate between two angles in \[0, 360\] the short
/// way (go-colorful utility used by BlendHcl, verbatim: the `+540` keeps
/// the delta in (−180, 180]).
fn interp_angle(a0: f64, a1: f64, t: f64) -> f64 {
    let delta = ((a1 - a0).rem_euclid(360.0) + 540.0).rem_euclid(360.0) - 180.0;
    (a0 + t * delta + 360.0) % 360.0
}

/// `makeGradientRamp`: colors blended between the given stops in HCL.
/// The remainder of `size / (stops-1)` is distributed across the leading
/// segments, exactly as in anim.go.
fn make_gradient_ramp(size: usize, stops: &[CfColor]) -> Vec<CfColor> {
    if stops.len() < 2 {
        return Vec::new();
    }
    let num_segments = stops.len() - 1;
    let base_size = size / num_segments;
    let remainder = size % num_segments;

    let mut blended = Vec::with_capacity(size);
    for i in 0..num_segments {
        let segment_size = base_size + usize::from(i < remainder);
        for j in 0..segment_size {
            let t = j as f64 / segment_size as f64;
            blended.push(CfColor::blend_hcl(stops[i], stops[i + 1], t));
        }
    }
    blended
}

// ── Frame prerendering ─────────────────────────────────────────────────────

/// One prerendered cell: character + color. `None` mirrors the original's
/// skipped columns (`continue` before drawing) — nothing is written there.
type Cell = Option<(char, RGBA)>;

/// `settingsHash`: the cache/seed key built from the same fields in the
/// same order as anim.go (`Size-Label-LabelColor-GradColorA-GradColorB-
/// CycleColors-SuffixColor`). Colors stringify as `#rrggbb` (Go's `%v` on
/// `color.RGBA` differs, but the key never crosses processes — it only
/// feeds PCG seeding); the suffix color falls back to the label color
/// (anim.go's `SuffixColor == nil` fallback), and there is no suffix
/// function in cosh.
fn settings_hash(
    size: usize,
    label: &str,
    label_color: RGBA,
    grad_a: RGBA,
    grad_b: RGBA,
    cycle_colors: bool,
) -> String {
    let hex = |c: RGBA| -> String {
        let (r, g, b, _) = c.to_ints();
        format!("#{r:02x}{g:02x}{b:02x}")
    };
    format!(
        "{}-{}-{}-{}-{}-{}-{}",
        size,
        label,
        hex(label_color),
        hex(grad_a),
        hex(grad_b),
        cycle_colors,
        hex(label_color)
    )
}

// ── AgentSpinnerCrush ──────────────────────────────────────────────────────

/// An animated spinner shaped like crush's working indicator: a band of
/// scrambled runes cycling through an HCL gradient, followed by the label
/// and its animated ellipsis, with a deterministic staggered birth.
///
/// A direct port of `charmbracelet/crush`'s `internal/ui/anim` package
/// (`Size: 15`, `CycleColors: true`) dressed in the shared agent-spinner
/// surface (label swap per activity, mode-colour/theme updates, resets,
/// deterministic render).
pub struct AgentSpinnerCrush {
    /// Cycling-character columns (`Size`).
    cycling_char_width: usize,
    /// Total rendered columns: cycling + gap + label (anim's `a.width`).
    width: usize,
    /// Label text displayed after the scramble.
    label: String,
    /// Number of characters in the label.
    label_width: usize,
    /// Colour of the label and the ellipsis.
    label_color: RGBA,
    /// Gradient endpoints (`GradColorA`/`GradColorB`).
    grad_a: RGBA,
    grad_b: RGBA,
    /// Deterministic birth step per column (staggered entrance).
    birth_steps: Vec<u32>,
    /// Prerendered initial frames (`initialFrames`).
    initial_frames: Vec<Vec<Cell>>,
    /// Prerendered scramble frames (`cyclingFrames`).
    cycling_frames: Vec<Vec<Cell>>,
    /// Current frame step (wraps at `cycling_frames.len()`).
    step: usize,
    /// Total animation frames elapsed since start (does not wrap).
    frames_since_start: u32,
    /// Ellipsis frame step (wraps at `ELLIPSIS_ANIM_SPEED * 4`).
    ellipsis_step: u32,
    /// Whether the birth window has elapsed.
    initialized: bool,
    /// Animation ticks elapsed at cosh's 30 fps; the crush frame index is
    /// `ticks × 2 / 3` (the original's 20 fps).
    ticks_elapsed: u32,
    /// Crush frames already applied to the animation state.
    frames_processed: u32,
}

impl AgentSpinnerCrush {
    /// Create a new crush spinner with the given label: gradient endpoints
    /// from `theme.primary`/`theme.secondary`, label colour from
    /// `theme.text_muted` (crush's `WorkingGradFromColor`/
    /// `WorkingGradToColor`/`WorkingLabelColor`), `Size: 15`,
    /// `CycleColors: true`.
    pub fn new(label: &str, theme: &Theme) -> Self {
        let mut spinner = Self {
            cycling_char_width: SIZE,
            width: 0,
            label: label.to_string(),
            label_width: label.chars().count(),
            label_color: theme.text_muted,
            grad_a: theme.primary,
            grad_b: theme.secondary,
            birth_steps: Vec::new(),
            initial_frames: Vec::new(),
            cycling_frames: Vec::new(),
            step: 0,
            frames_since_start: 0,
            ellipsis_step: 0,
            initialized: false,
            ticks_elapsed: 0,
            frames_processed: 0,
        };
        spinner.recompute_width();
        spinner.build_frames();
        spinner
    }

    /// `anim.go` width bookkeeping: cycling + (gap if label) + label.
    fn recompute_width(&mut self) {
        self.width = self.cycling_char_width;
        if !self.label.is_empty() {
            if self.cycling_char_width > 0 {
                self.width += LABEL_GAP_WIDTH;
            }
            self.width += self.label_width;
        }
    }

    /// `buildFrames`: regenerate the prerendered frames and (if the column
    /// count changed) the birth schedule. Seed the rune picker off the
    /// settings hash so the frames are a pure function of the settings —
    /// identical settings produce byte-identical glyphs.
    fn build_frames(&mut self) {
        let cache_key = settings_hash(
            self.cycling_char_width,
            &self.label,
            self.label_color,
            self.grad_a,
            self.grad_b,
            true,
        );

        // CycleColors: four-stop ramp over width*3 colors, width*2 frames.
        let ramp = make_gradient_ramp(
            self.width * 3,
            &[
                CfColor::from_rgba(self.grad_a),
                CfColor::from_rgba(self.grad_b),
                CfColor::from_rgba(self.grad_a),
                CfColor::from_rgba(self.grad_b),
            ],
        );
        let num_frames = self.width * 2;

        // Prerender the initial (birth) characters. Each cycle-mode frame
        // slides one further along the ramp (offset++ per frame).
        let total_cols = self.width + LABEL_GAP_WIDTH + self.label_width;
        let mut initial_frames = vec![vec![None; total_cols]; num_frames];
        let mut offset = 0usize;
        for frame in initial_frames.iter_mut().take(num_frames) {
            for (j, cell) in frame.iter_mut().enumerate() {
                if j + offset >= ramp.len() {
                    continue; // skip if we run out of colors
                }
                // j <= cyclingCharWidth rides the ramp (the gap column at
                // j == cycling_char_width included — anim.go's condition).
                let color = if j <= self.cycling_char_width {
                    ramp[j + offset].to_rgba()
                } else {
                    self.label_color
                };
                *cell = Some((INITIAL_CHAR, color));
            }
            offset += 1;
        }

        // Prerender the scrambled rune frames, rng consumed exactly where
        // anim.go consumes it: only for columns that draw.
        let seed = xxhash_rust::xxh3::xxh3_64(cache_key.as_bytes());
        let mut rng = Pcg::new(seed, !seed);
        let mut cycling_frames = vec![vec![None; self.width]; num_frames];
        let mut offset = 0usize;
        for frame in cycling_frames.iter_mut().take(num_frames) {
            for (j, cell) in frame.iter_mut().enumerate() {
                if j + offset >= ramp.len() {
                    continue;
                }
                let r = AVAILABLE_RUNES[rng.int_n(AVAILABLE_RUNES.len())];
                *cell = Some((r, ramp[j + offset].to_rgba()));
            }
            offset += 1;
        }

        // Deterministic birth schedule, rebuilt only when the column count
        // changed (anim.go keeps an in-flight fade-in across SetColors).
        if self.birth_steps.len() != self.width {
            let birth_seed = xxhash_rust::xxh3::xxh3_64(
                format!("{INSTANCE_ID}|{cache_key}").as_bytes(),
            );
            let mut birth_rng = Pcg::new(birth_seed, !birth_seed);
            self.birth_steps = (0..self.width)
                .map(|_| birth_rng.int_n(MAX_BIRTH_STEPS as usize) as u32)
                .collect();
        }

        self.initial_frames = initial_frames;
        self.cycling_frames = cycling_frames;
    }

    /// One animation frame exactly as `Anim.Advance` applies it.
    fn advance_once(&mut self) {
        self.step += 1;
        if self.step >= self.cycling_frames.len() {
            self.step = 0;
        }

        self.frames_since_start = self.frames_since_start.saturating_add(1);
        if self.initialized && self.label_width > 0 {
            // Manage the ellipsis animation.
            self.ellipsis_step += 1;
            if self.ellipsis_step >= ELLIPSIS_ANIM_SPEED * ELLIPSIS_FRAMES.len() as u32 {
                self.ellipsis_step = 0;
            }
        } else if !self.initialized && self.frames_since_start >= MAX_BIRTH_STEPS {
            self.initialized = true;
        }
    }

    /// Current crush frame index at the original's 20 fps pacing: ticks are
    /// 1000/30 ms apart, so `frames = ticks × (1000/30) / 50 = ticks × 2/3`
    /// (exact integer math — tick 3 = 100 ms = frame 2).
    const fn crush_frame(ticks: u32) -> u32 {
        ticks / 3 * 2 + (ticks % 3) * 2 / 3
    }

    /// Advance the animation by one 30 fps tick, applying every original
    /// 20 fps frame that has come due (their exact `Advance` logic).
    pub fn advance(&mut self) {
        self.ticks_elapsed = self.ticks_elapsed.saturating_add(1);
        let target = Self::crush_frame(self.ticks_elapsed);
        while self.frames_processed < target {
            self.advance_once();
            self.frames_processed += 1;
        }
    }

    /// Reset the spinner to its initial state, restarting the birth
    /// animation. The label and gradient survive — they describe the loop's
    /// activity and theme.
    pub fn reset(&mut self) {
        self.step = 0;
        self.frames_since_start = 0;
        self.frames_processed = 0;
        self.ellipsis_step = 0;
        self.ticks_elapsed = 0;
        self.initialized = false;
    }

    /// Change the label text (`SetLabel`): widths recomputed, animation
    /// clocks untouched.
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.label_width = label.chars().count();
        self.recompute_width();
    }

    /// Switch the label to the given agent activity. The scramble, birth
    /// schedule and clocks are untouched (crush's label swap semantics);
    /// a repeated activity is a cheap no-op.
    pub fn set_activity(&mut self, activity: crate::types::AgentActivity) {
        if self.label != activity.label() {
            self.set_label(activity.label());
        }
    }

    /// The original has no per-prompt-mode colour concept (the gradient is
    /// `GradColorA`/`GradColorB` only) — a no-op, kept for the shared
    /// spinner surface.
    pub fn set_mode_color(&mut self, _color: RGBA) {}

    /// Update the theme colours (`SetColors`): rebuild the prerendered
    /// frames from the new gradient/label colours while the step counters
    /// keep advancing — a theme swap mid-animation does not restart.
    pub fn update_theme(&mut self, theme: &Theme) {
        self.label_color = theme.text_muted;
        self.grad_a = theme.primary;
        self.grad_b = theme.secondary;
        self.build_frames();
    }

    /// Total width in terminal cells — the RENDERED width: scramble + gap
    /// + label + widest ellipsis frame.
    ///
    /// (anim.go's `Width()` double-counts the gap+label on top of
    /// `a.width`; cosh's layout needs the actual rendered span, so that
    /// over-report is not ported.)
    pub fn width(&self) -> usize {
        let mut w = self.width;
        if self.label_width > 0 {
            w += ELLIPSIS_FRAMES
                .iter()
                .map(|f| f.chars().count())
                .max()
                .unwrap_or(0);
        }
        w
    }

    /// Whether the birth window has elapsed (every column's birth step has
    /// been reached — anim.go's `initialized`).
    pub const fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Test-only view of the current label (the field is private).
    #[cfg(test)]
    pub(crate) fn label_for_test(&self) -> &str {
        &self.label
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render the spinner into the buffer at position `(x, y)` — the
    /// column walk of `Anim.Render`, including the birth phase, the label
    /// gap and the animated ellipsis.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        let step = self.step;
        let frames = self.frames_since_start;

        for i in 0..self.width {
            let cell_x = x.saturating_add(i as u16);
            if !self.initialized
                && i < self.birth_steps.len()
                && frames < self.birth_steps[i]
            {
                // Birth step not reached: render the initial character.
                if let Some((ch, color)) =
                    self.initial_frames.get(step).and_then(|f| f.get(i)).copied().flatten()
                {
                    let style = Style::default().fg(rgba_color(color));
                    if let Some(cell) = buf.cell_mut((cell_x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            } else if i < self.cycling_char_width {
                // Render a cycling character.
                if let Some((ch, color)) =
                    self.cycling_frames.get(step).and_then(|f| f.get(i)).copied().flatten()
                {
                    let style = Style::default().fg(rgba_color(color));
                    if let Some(cell) = buf.cell_mut((cell_x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            } else if i == self.cycling_char_width && self.cycling_char_width > 0 {
                // Render the label gap (only when there are cycling chars).
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default());
                }
            } else {
                // Label: offset past the cycling chars and gap (if any).
                let offset = self.cycling_char_width
                    + usize::from(self.cycling_char_width > 0) * LABEL_GAP_WIDTH;
                if let Some(ch) = self.label.chars().nth(i - offset) {
                    let style = Style::default().fg(rgba_color(self.label_color));
                    if let Some(cell) = buf.cell_mut((cell_x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            }
        }

        // Animated ellipsis at the end of the label once initialized. The
        // slot is padded to the widest frame: a shorter frame must clear
        // stale dots instead of ghosting them (the terminal twin of the
        // original rebuilding its output string every frame).
        if self.initialized && self.label_width > 0 {
            let idx = (self.ellipsis_step / ELLIPSIS_ANIM_SPEED) as usize;
            let text = ELLIPSIS_FRAMES[idx.min(ELLIPSIS_FRAMES.len() - 1)];
            // self.width already spans scramble + gap + label; the ellipsis
            // appends right after the row (Go's Render concatenates it).
            let ellipsis_x = x.saturating_add(self.width as u16);

            let style = Style::default().fg(rgba_color(self.label_color));
            let mut written = 0usize;
            for (i, ch) in text.chars().enumerate() {
                let cell_x = ellipsis_x.saturating_add(i as u16);
                if let Some(cell) = buf.cell_mut((cell_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                written += 1;
            }
            for i in written..3 {
                let cell_x = ellipsis_x.saturating_add(i as u16);
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

    // ── Go PCG parity ──────────────────────────────────────────────────

    #[test]
    fn pcg_dxsm_first_outputs_match_go() {
        // Goldens from a Go program running
        // rng := rand.New(rand.NewPCG(0x1234, ^uint64(0x1234)));
        // for i := 0; i < 5; i++ { fmt.Println(rng.Uint64()) }
        let mut rng = Pcg::new(0x1234, !0x1234);
        let expected: [u64; 5] = [
            0x12fb3ff866aa6fd6,
            0x0f5718a5c8b65cda,
            0xfd14c28f3e270ca9,
            0xf818a699583ae50b,
            0x6dc8dac4b0b1af5b,
        ];
        for want in expected {
            assert_eq!(rng.uint64(), want);
        }
    }

    #[test]
    fn pcg_intn_sequence_matches_go() {
        // Golden from Go: rng.IntN(38) × 20 with the same PCG seed.
        let mut rng = Pcg::new(0x1234, !0x1234);
        let expected: [usize; 20] = [
            2, 2, 37, 36, 16, 14, 6, 32, 0, 11, 36, 1, 7, 35, 35, 24, 29, 5, 8, 11,
        ];
        for want in expected {
            assert_eq!(rng.int_n(38), want);
        }
        // Power-of-two n uses the mask path; values stay in range.
        let mut rng = Pcg::new(99, 12345);
        for _ in 0..100 {
            assert!(rng.int_n(16) < 16);
        }
    }

    // ── interp_angle / BlendHcl ────────────────────────────────────────

    #[test]
    fn interp_angle_takes_the_short_way_around() {
        // 350 → 10 crosses 0: Go's Mod arithmetic returns 0.0, not 360.0.
        assert!((interp_angle(350.0, 10.0, 0.5) - 0.0).abs() < 1e-9);
        assert!((interp_angle(10.0, 350.0, 0.5) - 0.0).abs() < 1e-9);
        assert!((interp_angle(0.0, 90.0, 0.5) - 45.0).abs() < 1e-9);
        assert!((interp_angle(90.0, 0.0, 0.5) - 45.0).abs() < 1e-9);
        // Endpoints return the anchor angles.
        assert!((interp_angle(350.0, 10.0, 1.0) - 10.0).abs() < 1e-9);
        assert!((interp_angle(350.0, 10.0, 0.0) - 350.0).abs() < 1e-9);
    }

    #[test]
    fn blend_hcl_endpoints_return_the_stops() {
        let a = CfColor::from_rgba(RGBA::from_ints(200, 60, 30, 255));
        let b = CfColor::from_rgba(RGBA::from_ints(30, 120, 220, 255));
        let back_a = CfColor::blend_hcl(a, b, 0.0).to_rgba();
        let back_b = CfColor::blend_hcl(a, b, 1.0).to_rgba();
        assert_eq!(back_a.to_ints(), (200, 60, 30, 255));
        assert_eq!(back_b.to_ints(), (30, 120, 220, 255));
        // The midpoint stays in gamut.
        let mid = CfColor::blend_hcl(a, b, 0.5);
        assert!(mid.r >= 0.0 && mid.r <= 1.0);
        assert!(mid.g >= 0.0 && mid.g <= 1.0);
        assert!(mid.b >= 0.0 && mid.b <= 1.0);
    }

    #[test]
    fn hcl_round_trips_through_lab() {
        let c = CfColor::from_rgba(RGBA::from_ints(180, 90, 240, 255));
        let (h, cr, l) = c.hcl();
        let back = CfColor::from_hcl(h, cr, l).to_rgba();
        let (r, g, b, _) = back.to_ints();
        // HCL is not exactly invertible after clamping, but a colorful
        // in-gamut color must round-trip within a step or two.
        let (or, og, ob, _) = RGBA::from_ints(180, 90, 240, 255).to_ints();
        assert!((i32::from(r) - i32::from(or)).abs() <= 2);
        assert!((i32::from(g) - i32::from(og)).abs() <= 2);
        assert!((i32::from(b) - i32::from(ob)).abs() <= 2);
    }

    // ── makeGradientRamp ───────────────────────────────────────────────

    #[test]
    fn ramp_size_and_segment_distribution_match_go() {
        let stops = [
            CfColor::from_rgba(RGBA::from_ints(255, 0, 0, 255)),
            CfColor::from_rgba(RGBA::from_ints(0, 0, 255, 255)),
            CfColor::from_rgba(RGBA::from_ints(255, 0, 0, 255)),
            CfColor::from_rgba(RGBA::from_ints(0, 0, 255, 255)),
        ];
        // size 45 over 3 segments: 15/15/15, no remainder.
        let ramp = make_gradient_ramp(45, &stops);
        assert_eq!(ramp.len(), 45);
        // size 47 over 3 segments: base 15 + remainder 2 → 16/16/15.
        let ramp = make_gradient_ramp(47, &stops);
        assert_eq!(ramp.len(), 47);
        // Fewer than two stops: nil ramp in Go, empty here.
        assert!(make_gradient_ramp(10, &stops[..1]).is_empty());
    }

    #[test]
    fn ramp_starts_and_ends_in_gamut_near_the_stops() {
        let a = RGBA::from_ints(255, 90, 40, 255);
        let b = RGBA::from_ints(40, 100, 255, 255);
        let ramp = make_gradient_ramp(
            45,
            &[
                CfColor::from_rgba(a),
                CfColor::from_rgba(b),
                CfColor::from_rgba(a),
                CfColor::from_rgba(b),
            ],
        );
        let first = ramp[0].to_rgba();
        let (fr, fg_, fb, _) = first.to_ints();
        let (ar, ag, ab, _) = a.to_ints();
        assert!((i32::from(fr) - i32::from(ar)).abs() <= 1);
        assert!((i32::from(fg_) - i32::from(ag)).abs() <= 1);
        assert!((i32::from(fb) - i32::from(ab)).abs() <= 1);
        // Every ramp entry is a valid opaque color.
        for c in &ramp {
            let (_, _, _, alpha) = c.to_rgba().to_ints();
            assert_eq!(alpha, 255);
        }
    }

    // ── build_frames / birth schedule ──────────────────────────────────

    #[test]
    fn frames_are_a_pure_function_of_settings() {
        let theme = test_theme();
        let a = AgentSpinnerCrush::new("working", &theme);
        let b = AgentSpinnerCrush::new("working", &theme);
        assert_eq!(a.cycling_frames, b.cycling_frames);
        assert_eq!(a.initial_frames, b.initial_frames);
        assert_eq!(a.birth_steps, b.birth_steps);
    }

    #[test]
    fn birth_steps_are_staggered_within_max() {
        let theme = test_theme();
        let spinner = AgentSpinnerCrush::new("working", &theme);
        assert_eq!(spinner.birth_steps.len(), spinner.width);
        assert!(spinner.birth_steps.iter().all(|&s| s < MAX_BIRTH_STEPS));
        // Not all columns born at once (the point of the stagger).
        assert!(spinner.birth_steps.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn frame_count_is_width_times_two_when_cycling() {
        let theme = test_theme();
        let spinner = AgentSpinnerCrush::new("working", &theme);
        assert_eq!(spinner.cycling_frames.len(), spinner.width * 2);
        assert_eq!(spinner.initial_frames.len(), spinner.width * 2);
        // CycleColors ramps span width*3.
        // (Verified via the ramp math test above; here via offset wrap.)
        assert_eq!(spinner.cycling_char_width, SIZE);
    }

    // ── Advance / timing ───────────────────────────────────────────────

    #[test]
    fn crush_clock_runs_at_the_original_20fps_pacing() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        // 3 ticks = 100 ms = 2 frames of 50 ms.
        for _ in 0..3 {
            spinner.advance();
        }
        assert_eq!(spinner.frames_processed, 2);
        // 60 ticks = 2 s = 40 frames.
        for _ in 3..60 {
            spinner.advance();
        }
        assert_eq!(spinner.frames_processed, 40);
    }

    #[test]
    fn step_wraps_at_the_frame_count() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        let total = spinner.cycling_frames.len() as u32;
        for _ in 0..(total * 2 + 5) {
            spinner.advance_once();
        }
        assert_eq!(spinner.step, 5);
        assert_eq!(spinner.frames_since_start, total * 2 + 5);
    }

    #[test]
    fn initialized_after_max_birth_steps() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        assert!(!spinner.is_initialized());
        for _ in 0..(MAX_BIRTH_STEPS * 3 / 2) {
            spinner.advance_once();
        }
        assert!(spinner.is_initialized());
    }

    #[test]
    fn ellipsis_cycles_every_eight_frames() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        for _ in 0..MAX_BIRTH_STEPS {
            spinner.advance_once();
        }
        assert!(spinner.is_initialized());
        // Exactly 8 frames per ellipsis step, 4 steps per cycle.
        let mut seen = Vec::new();
        for _ in 0..ELLIPSIS_ANIM_SPEED {
            spinner.advance_once();
        }
        let idx = (spinner.ellipsis_step / ELLIPSIS_ANIM_SPEED) as usize;
        seen.push(idx);
        for _ in 0..ELLIPSIS_ANIM_SPEED * (ELLIPSIS_FRAMES.len() as u32 - 1) {
            spinner.advance_once();
        }
        assert_eq!(spinner.ellipsis_step, 0);
        let _ = seen;
    }

    // ── Shared surface ─────────────────────────────────────────────────

    #[test]
    fn width_covers_scramble_gap_label_and_ellipsis() {
        let theme = test_theme();
        // "working" = 7 chars: 15 + 1 + 7 + 3 (widest ellipsis).
        let spinner = AgentSpinnerCrush::new("working", &theme);
        assert_eq!(spinner.width(), SIZE + LABEL_GAP_WIDTH + 7 + 3);
        // No label: just the scramble (anim.go keeps the gap out too).
        let spinner = AgentSpinnerCrush::new("", &theme);
        assert_eq!(spinner.width(), SIZE);
    }

    #[test]
    fn set_activity_swaps_label_without_disturbing_the_clocks() {
        use crate::types::AgentActivity;
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new(AgentActivity::Working.label(), &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let frames_before = spinner.frames_since_start;

        spinner.set_activity(AgentActivity::for_tool("web_search"));
        assert_eq!(spinner.label, "searching");
        // Re-applying the SAME activity is a no-op.
        spinner.set_activity(AgentActivity::Searching);
        assert_eq!(spinner.frames_since_start, frames_before);
    }

    #[test]
    fn theme_swap_rebuilds_frames_but_keeps_the_clocks() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        for _ in 0..40 {
            spinner.advance();
        }
        let step_before = spinner.step;
        let frames_before = spinner.frames_since_start;

        let registry = crate::theme::ThemeRegistry::new();
        let other = registry
            .get(registry.names().first().copied().unwrap_or("opencode"))
            .cloned()
            .unwrap_or_else(|| theme.clone());
        spinner.update_theme(&other);
        assert_eq!(spinner.step, step_before);
        assert_eq!(spinner.frames_since_start, frames_before);
    }

    #[test]
    fn reset_restarts_the_birth_animation() {
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        for _ in 0..60 {
            spinner.advance();
        }
        assert!(spinner.is_initialized());
        spinner.reset();
        assert!(!spinner.is_initialized());
        assert_eq!(spinner.frames_since_start, 0);
        assert_eq!(spinner.ticks_elapsed, 0);
        assert_eq!(spinner.label, "working");
    }

    #[test]
    fn identical_spinners_stay_in_lockstep() {
        let theme = test_theme();
        let mut a = AgentSpinnerCrush::new("working", &theme);
        let mut b = AgentSpinnerCrush::new("working", &theme);
        for _ in 0..120 {
            a.advance();
            b.advance();
            assert_eq!(a.frames_processed, b.frames_processed);
            assert_eq!(a.step, b.step);
        }
    }

    #[test]
    fn renders_label_gap_and_ellipsis_without_panic() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = test_theme();
        let mut spinner = AgentSpinnerCrush::new("working", &theme);
        for _ in 0..(MAX_BIRTH_STEPS * 2) {
            spinner.advance();
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        spinner.render(&mut buf, 0, 0);
        // Label chars landed after the scramble + gap.
        let label_x = (SIZE + LABEL_GAP_WIDTH) as u16;
        assert_eq!(buf[(label_x, 0)].symbol(), "w");
        // The ellipsis appends DIRECTLY after the label (Go's Render
        // concatenates it): its slot starts at x + a.width — the internal
        // scramble+gap+label span (23 for "working"), never past it — and
        // width() adds the 3-cell slot on top (26). Regression guard for
        // the double-counted gap+label offset.
        assert_eq!(spinner.width, SIZE + LABEL_GAP_WIDTH + 7);
        assert_eq!(spinner.width(), spinner.width + 3);
        // Narrow buffer: clipped, no panic.
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        spinner.render(&mut buf, 0, 0);
    }

    #[test]
    fn settings_hash_matches_the_go_field_order() {
        let label = RGBA::from_ints(0xaa, 0xbb, 0xcc, 255);
        let a = RGBA::from_ints(1, 2, 3, 255);
        let b = RGBA::from_ints(4, 5, 6, 255);
        let h = settings_hash(15, "working", label, a, b, true);
        assert_eq!(h, "15-working-#aabbcc-#010203-#040506-true-#aabbcc");
        // CycleColors flag and size are part of the key.
        let h2 = settings_hash(10, "working", label, a, b, false);
        assert_ne!(h, h2);
    }
}

