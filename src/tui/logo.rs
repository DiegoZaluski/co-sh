pub const LOGO_WIDTH: usize = 28;

pub const LOGO: &[&str] = &[
    "                            ",
    " ▗████  ▄███▄  █████ ██     ",
    " ██    █▌ ▅ ▐█ ██▖   █████▖ ",
    " ██    █▌ ▀ ▐█   ▝██ ██  ██ ",
    " ▝████  ▀███▀  █████ ██  ██ ",
    "                            ",
];

pub const LOGO_CHAT: &[&str] = &[
    "                            ",
    " ▗████  ▄███▄  █████ ██     ",
    " ██    █▌ ▅ ▐█ ██▖   █████▖ ",
    " ██    █▌ ▀ ▐█   ▝██ ██  ██ ",
    " ▝████  ▀███▀  █████ ██  ██ ",
    "                            ",
];

/// The second letter of the cosh logo: the "O" at columns 7-13, rows 1-4 of
/// [`LOGO_CHAT`]. It is used as the chat-logo animation glyph.
pub const O_GLYPH: [&str; 4] = [" ▄███▄ ", "█▌ ▅ ▐█", "█▌ ▀ ▐█", " ▀███▀ "];

pub const O_GLYPH_W: usize = 7;
pub const O_GLYPH_H: usize = 4;

/// The laser beam's character set, rippling along the beam as it shoots.
const BEAM_CHARS: [char; 6] = ['@', '$', '#', ';', '.', ','];

/// The logo's maximum (and starting) health.
pub const MAX_HEALTH: u16 = 200;

/// Health regained per tick after [`HEAL_DELAY_SECS`] without damage.
const HEAL_AMOUNT: u16 = 10;
/// Seconds without damage before a heal tick fires.
const HEAL_DELAY_SECS: f64 = 10.0;
/// Seconds between heal ticks (while still below [`MAX_HEALTH`]).
const HEAL_INTERVAL_SECS: f64 = 1.0;
/// Number of cells reserved for the sleep "zzZ" text next to the glyph.
const ZZZ_SLOTS: usize = 3;
/// Seconds each "zzZ" frame is shown before the shift advances.
const ZZZ_FRAME_SECS: f64 = 0.9;

/// A piece of floating text shown around the logo: a damage number like
/// "-50" in red/orange, or a "+10" in light blue/green when the logo heals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatKind {
    Damage,
    Heal,
}

/// Theme colors the chat-logo animation needs, resolved from the active
/// theme by the caller.
#[derive(Debug, Clone, Copy)]
pub struct LogoColors {
    /// The theme's primary color (glyph outline, fill blend base).
    pub primary: ratatui::style::Color,
    /// The theme's background (gradient/fade target).
    pub background: ratatui::style::Color,
    /// Muted text: the sleeping glyph and its "zzZ" indicator.
    pub muted: ratatui::style::Color,
    /// Light blue end of the heal-number palette.
    pub info: ratatui::style::Color,
    /// Green end of the heal-number palette.
    pub success: ratatui::style::Color,
}

/// A floating damage/heal number with its own appearance clock. `t` runs
/// from `0.0` (spawned) to `>= LIFETIME` (gone).
struct FloatText {
    /// The raw value, e.g. 50 for a -50 hit or 10 for a +10 heal.
    amount: u16,
    kind: FloatKind,
    /// Signed drift from the O's left edge, in cells (may be negative).
    dx: f64,
    /// Age in seconds.
    t: f64,
}

impl FloatText {
    /// Seconds the text takes to fully materialize (color-strobing phase).
    const APPEAR: f64 = 0.28;
    /// Total lifetime in seconds, after which the text is dropped.
    const LIFETIME: f64 = 1.25;

    /// The characters currently visible (the text appears left-to-right).
    fn visible_text(&self) -> String {
        let sign = match self.kind {
            FloatKind::Damage => '-',
            FloatKind::Heal => '+',
        };
        let text = format!("{sign}{}", self.amount);
        let n = text.len();
        // First the sign pops in alone, then one digit every ~90ms.
        let shown = ((self.t - 0.05) / 0.09 * n as f64).ceil() as usize;
        text.chars().take(shown.min(n)).collect()
    }

    /// Rise/fall offset in cells: damage floats upward, heals sink gently.
    fn dy(&self) -> f64 {
        let v = self.t * 1.4;
        match self.kind {
            FloatKind::Damage => -1.0 - v,
            FloatKind::Heal => v * 0.7,
        }
    }
}

/// A chat-logo animation built from the "O" glyph of the cosh logo.
///
/// The O's center gradually turns red and then pulses between a strong and a
/// medium red, while a laser-like beam of `@$#;.,` characters shoots from it
/// toward the user's typing position in the prompt,
/// moving dynamically with each keystroke. The O itself drifts toward the
/// typing position with a subtle head-like motion, but it is constrained to
/// stay above the prompt: only the laser may cross into the prompt area.
pub struct ChatLogo {
    /// Top-left of the O glyph in screen coordinates, smoothed toward the target.
    pos_x: f64,
    pos_y: f64,
    /// Beam target: the current typing position in the prompt.
    target_x: f64,
    target_y: f64,
    /// Fraction of the red-fill phase completed (0.0 → 1.0).
    fill: f64,
    /// Whether the laser has been fired yet (starts after the fill phase).
    fired: bool,
    /// Beam length in cells, animated outward from the O toward the target.
    beam_len: f64,
    /// Head-like bob phase, driven by typing rhythm.
    bob_phase: f64,
    /// Beam ripple phase (animates the `@$#;.,` characters flowing outward).
    beam_phase: f64,
    /// Phase of the red-eye pulse, oscillating between strong and medium red.
    pulse_phase: f64,
    /// Time of the last keystroke (for the typing pulse).
    last_keystroke: std::time::SystemTime,
    /// Whether the O is currently moving toward the target (head motion).
    moving: bool,
    /// Whether `pos_x`/`pos_y` have been initialized to the anchor position.
    positioned: bool,
    /// Movement bounds `(min_x, max_x, min_y, max_y)` for the O's top-left,
    /// derived from the reserved band so the glyph never drifts into the
    /// prompt below.
    bounds: Option<(f64, f64, f64, f64)>,
    /// Current health (0..=MAX_HEALTH). At 0 the logo falls asleep.
    health: u16,
    /// Seconds since the last successful hit; drives the heal timer.
    since_damage: f64,
    /// Countdown to the next heal tick once [`HEAL_DELAY_SECS`] has elapsed.
    heal_in: f64,
    /// Floating damage/heal numbers currently shown around the logo.
    floats: Vec<FloatText>,
    /// Hit-flash timer in seconds (the glyph flares white-orange on impact).
    hit_flash: f64,
    /// Total clicks registered, used to jitter float positions deterministically.
    click_count: u32,
    /// Rotation counter of the sleep "zzZ" text: the uppercase `Z` sits at
    /// index `zzz_phase % ZZZ_SLOTS` and shifts left each frame.
    zzz_phase: usize,
    /// Accumulated seconds toward the next "zzZ" shift.
    zzz_clock: f64,
}

impl ChatLogo {
    pub const fn new() -> Self {
        Self {
            pos_x: 0.0,
            pos_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            fill: 0.0,
            fired: false,
            beam_len: 0.0,
            bob_phase: 0.0,
            beam_phase: 0.0,
            pulse_phase: 0.0,
            last_keystroke: std::time::UNIX_EPOCH,
            moving: false,
            positioned: false,
            bounds: None,
            health: MAX_HEALTH,
            since_damage: 0.0,
            heal_in: HEAL_INTERVAL_SECS,
            floats: Vec::new(),
            hit_flash: 0.0,
            click_count: 0,
            zzz_phase: 0,
            zzz_clock: 0.0,
        }
    }

    /// Reset the animation to its idle state (no red fill, no beam) and the
    /// mini game to a fresh round: full health, awake, no floating numbers.
    pub fn reset(&mut self) {
        self.fill = 0.0;
        self.fired = false;
        self.beam_len = 0.0;
        self.bob_phase = 0.0;
        self.beam_phase = 0.0;
        self.moving = false;
        self.positioned = false;
        self.bounds = None;
        self.health = MAX_HEALTH;
        self.since_damage = 0.0;
        self.heal_in = HEAL_INTERVAL_SECS;
        self.floats.clear();
        self.hit_flash = 0.0;
        self.zzz_phase = 0;
        self.zzz_clock = 0.0;
    }

    /// The logo's current health (0..=[`MAX_HEALTH`]).
    pub const fn health(&self) -> u16 {
        self.health
    }

    /// Whether the logo is asleep (health reached zero).
    pub const fn is_asleep(&self) -> bool {
        self.health == 0
    }

    /// Precise glyph hit test: does screen cell `(x, y)` land on a drawn,
    /// non-space character of the O glyph? Clicks on the glyph's interior
    /// spaces or anywhere outside it do not count.
    fn glyph_hit(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        // Must mirror render(): while asleep the bob is frozen to zero, so
        // the hit region always matches the visible glyph position.
        let bob = if self.is_asleep() {
            0.0
        } else {
            self.bob_offset()
        };
        let ox = self.pos_x.round() as i64;
        let oy = (self.pos_y + bob).round() as i64;
        let gx = i64::from(x) - ox;
        let gy = i64::from(y) - oy;
        if gx < 0 || gy < 0 {
            return None;
        }
        let (gc, gr) = (gx as usize, gy as usize);
        let row = O_GLYPH.get(gr)?;
        let ch = row.chars().nth(gc)?;
        if ch == ' ' {
            return None;
        }
        Some((gc, gr))
    }

    /// Register a click at screen cell `(x, y)`. Returns `true` when the
    /// click hit an actual glyph cell and dealt damage.
    ///
    /// Damage scales with the vertical position of the hit: the top row of
    /// the glyph deals the most (50), the bottom row the least (10), with
    /// the intermediate rows proportionally in between. When the logo is
    /// already asleep, clicks still spawn a floating number but deal nothing.
    pub fn click_logo(&mut self, x: u16, y: u16) -> bool {
        let Some((_gc, gr)) = self.glyph_hit(x, y) else {
            return false;
        };
        self.click_count += 1;
        // Linear ramp: row 0 → 50, row O_GLYPH_H-1 → 10.
        let max_dmg = 50.0_f64;
        let min_dmg = 10.0_f64;
        let frac = gr as f64 / (O_GLYPH_H as f64 - 1.0).max(1.0);
        let amount = (max_dmg - (max_dmg - min_dmg) * frac).round() as u16;
        if self.health > 0 {
            self.health = self.health.saturating_sub(amount);
            self.since_damage = 0.0;
            self.heal_in = HEAL_INTERVAL_SECS;
            self.hit_flash = 0.18;
        }
        // Alternating left/right jitter keeps consecutive numbers readable.
        let dx = if self.click_count.is_multiple_of(2) {
            8.0
        } else {
            -4.0
        };
        self.floats.push(FloatText {
            amount,
            kind: FloatKind::Damage,
            dx,
            t: 0.0,
        });
        true
    }

    /// Notify the logo that the user typed at the given prompt position.
    /// `x`/`y` are the screen coordinates of the text cursor.
    pub fn note_keystroke(&mut self, x: u16, y: u16) {
        self.target_x = x as f64;
        self.target_y = y as f64;
        self.last_keystroke = std::time::SystemTime::now();
    }

    /// Ensure the O is positioned at the anchor (center of its area, near the
    /// top so it can bob within the reserved band). Also records the band so
    /// `advance` keeps the glyph from drifting into the prompt.
    pub fn anchor(&mut self, area: ratatui::layout::Rect) {
        let max_x = area.right().saturating_sub(O_GLYPH_W as u16);
        let max_y = area.bottom().saturating_sub(O_GLYPH_H as u16);
        // Guard: when the area is too small for the glyph, min > max which
        // would make f64::clamp panic in advance(). Skip storing bounds so
        // the position is unconstrained until a larger area is provided.
        if max_x <= area.x || max_y <= area.y {
            return;
        }
        self.bounds = Some((area.x as f64, max_x as f64, area.y as f64, max_y as f64));
        if !self.positioned {
            self.pos_x = area.x as f64 + (area.width as f64 / 2.0) - (O_GLYPH_W as f64 / 2.0);
            self.pos_y = area.y as f64 + 1.0;
            self.positioned = true;
        }
    }

    /// Advance the animation by `dt` seconds. `target_x`/`target_y` should be
    /// the current typing position in the prompt.
    pub fn advance(&mut self, dt: f64, target_x: f64, target_y: f64) {
        self.target_x = target_x;
        self.target_y = target_y;

        // ── Mini game: floating numbers, hit flash, heal, sleep ────────────
        // These run before the animation branches below because they apply
        // even while the logo is asleep (floats keep drifting, the heal timer
        // keeps ticking) or still in its fill phase.
        self.since_damage += dt;
        self.hit_flash = (self.hit_flash - dt).max(0.0);
        for f in &mut self.floats {
            f.t += dt;
        }
        self.floats.retain(|f| f.t < FloatText::LIFETIME);

        // Recovery: after HEAL_DELAY_SECS without damage, +10 health per
        // HEAL_INTERVAL_SECS, never above MAX_HEALTH. Healing wakes a
        // sleeping logo back up.
        if self.health < MAX_HEALTH && self.since_damage >= HEAL_DELAY_SECS {
            self.heal_in -= dt;
            // Catch up on missed ticks after a frame hitch, but spawn a
            // single aggregated float so a long stall doesn't stack a pile
            // of "+10"s on top of each other.
            let mut healed = 0u16;
            while self.heal_in <= 0.0 {
                self.heal_in += HEAL_INTERVAL_SECS;
                let before = self.health;
                self.health = (self.health + HEAL_AMOUNT).min(MAX_HEALTH);
                healed += self.health - before;
            }
            if healed > 0 {
                self.floats.push(FloatText {
                    amount: healed,
                    kind: FloatKind::Heal,
                    dx: 8.0,
                    t: 0.0,
                });
            }
        }

        // ── Sleeping state ─────────────────────────────────────────────────
        // At zero health the O freezes exactly where it is: no motion, no
        // laser, no eye pulse — only the "zzZ" shift keeps cycling.
        if self.health == 0 {
            self.zzz_clock += dt;
            while self.zzz_clock >= ZZZ_FRAME_SECS {
                self.zzz_clock -= ZZZ_FRAME_SECS;
                self.zzz_phase += 1;
            }
            return;
        }

        // Fill phase: the O's center gradually turns red.
        if !self.fired {
            self.fill = (self.fill + dt * 0.6).min(1.0);
            if self.fill >= 1.0 {
                self.fired = true;
            }
            return;
        }

        // Beam phase: the laser grows from the O toward the typing position.
        let dist = self.distance_to_target();
        self.beam_len = (self.beam_len + dt * 30.0).min(dist);

        // Head-like motion: the O drifts toward the target, smoothly following
        // the beam's movement and rhythm. The drift is clamped to the reserved
        // band so the glyph never enters the prompt box below.
        let target_speed = (dist * 0.35).clamp(1.5, 8.0);
        self.moving = dist > 1.0;
        if self.moving {
            let dx = self.target_x - self.pos_x;
            let dy = self.target_y - self.pos_y;
            let d = (dx * dx + dy * dy).sqrt();
            if d > 0.0 {
                let step = (dt * target_speed).min(d);
                self.pos_x += dx / d * step;
                self.pos_y += dy / d * step;
            }
        }
        if let Some((min_x, max_x, min_y, max_y)) = self.bounds {
            self.pos_x = self.pos_x.clamp(min_x, max_x);
            self.pos_y = self.pos_y.clamp(min_y, max_y);
        }

        // Subtle bob: a gentle head-like sway driven by the typing rhythm.
        self.bob_phase += dt * 6.0;
        self.beam_phase += dt * 14.0;
        // Red-eye pulse tempo once the center is fully charged.
        self.pulse_phase += dt * 2.0;
    }

    /// Vertical head-bob offset to apply when drawing the O. Its amplitude
    /// decays as the time since the last keystroke grows, so it pulses when
    /// the user is typing and settles to rest when idle.
    fn bob_offset(&self) -> f64 {
        let idle = self
            .last_keystroke
            .elapsed()
            .map_or(0.0, |d| d.as_secs_f64());
        let energy = (1.0 - (idle / 3.0).clamp(0.0, 1.0)).powf(2.0);
        self.bob_phase.sin() * 0.25 * energy
    }

    /// Distance (in cells) from the O's center to the current beam target.
    fn distance_to_target(&self) -> f64 {
        let (bx, by) = self.beam_origin();
        let dx = self.target_x - bx;
        let dy = self.target_y - by;
        (dx * dx + dy * dy).sqrt()
    }

    /// Screen position of the O's hole center, where the laser starts.
    fn beam_origin(&self) -> (f64, f64) {
        (
            self.pos_x + (O_GLYPH_W as f64 / 2.0),
            self.pos_y + (O_GLYPH_H as f64 / 2.0),
        )
    }

    /// Render the animation into `buf` within `area` (the reserved band above
    /// the prompt). `cursor_x`/`cursor_y` is the beam target (typing position).
    ///
    /// The O glyph is clamped to stay inside `area`, so it never enters the
    /// prompt box below; only the laser beam may cross the area boundary.
    pub fn render(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        area: ratatui::layout::Rect,
        cursor_x: u16,
        cursor_y: u16,
        colors: LogoColors,
    ) {
        let LogoColors {
            primary,
            background: bg,
            muted,
            info,
            success,
        } = colors;
        let max_x = area.right().saturating_sub(O_GLYPH_W as u16);
        let max_y = area.bottom().saturating_sub(O_GLYPH_H as u16);
        if max_x <= area.x || max_y <= area.y {
            return;
        }

        let bob = if self.is_asleep() {
            0.0
        } else {
            self.bob_offset()
        };
        let o_x = self.pos_x.clamp(area.x as f64, max_x as f64);
        let o_y = (self.pos_y + bob).clamp(area.y as f64, max_y as f64);
        let ox = o_x.round() as u16;
        let oy = o_y.round() as u16;
        let asleep = self.is_asleep();
        // Impact flash: for ~0.18s after a hit the whole glyph flares toward
        // a hot white, decaying smoothly back to its normal colors.
        let flash = (self.hit_flash / 0.18).clamp(0.0, 1.0);

        // Fill levels: interior cells (rows 1-2, cols 2-4 of the glyph) fill
        // bottom-up with red as `self.fill` grows.
        let f = self.fill.clamp(0.0, 1.0);

        for (gr, row) in O_GLYPH.iter().enumerate() {
            let gy = oy.saturating_add(gr as u16);
            for (gc, ch) in row.chars().enumerate() {
                let gx = ox.saturating_add(gc as u16);
                let is_interior = (1..=2).contains(&gr) && (2..=4).contains(&gc);
                let is_space = ch == ' ';

                if let Some(cell) = buf.cell_mut((gx, gy)) {
                    if is_interior {
                        // "The center of the O gradually turns red, then pulses
                        // between a strong and a medium red": the original glyph
                        // marks (▅ / ▀) are only recolored, never replaced with
                        // new characters. Spaces are left untouched.
                        if is_space {
                            continue;
                        }
                        let pulse = 0.5 + 0.5 * self.pulse_phase.sin(); // 0.0..=1.0
                        let eye = blend_color(red_color(), medium_red(), pulse);
                        let mut col = blend_color(eye, primary, f);
                        if asleep {
                            col = muted;
                        } else if flash > 0.0 {
                            col = blend_color(flash_color(), col, flash);
                        }
                        cell.set_char(ch);
                        cell.set_style(ratatui::style::Style::default().fg(col));
                    } else if !is_space {
                        // O outline: primary color with a gentle gradient for
                        // a rounded feel.
                        let dist_edge =
                            ((gc as f64 - 3.0).powi(2) + (gr as f64 - 1.5).powi(2)).sqrt();
                        let bri = 0.55 + (1.0 - dist_edge / 4.0).clamp(0.0, 1.0) * 0.45;
                        let mut col = blend_color(primary, bg, bri);
                        if asleep {
                            // Sleeping: the whole glyph rests in the muted
                            // text color, with the same edge gradient so the
                            // rounded shading is preserved.
                            col = blend_color(muted, bg, bri);
                        } else if flash > 0.0 {
                            col = blend_color(flash_color(), col, flash);
                        }
                        cell.set_char(ch);
                        cell.set_style(ratatui::style::Style::default().fg(col));
                    }
                    // Outer spaces of the glyph: leave the background alone.
                }
            }
        }

        // ── The laser beam ─────────────────────────────────────────────────
        if !asleep && self.fired && self.beam_len > 0.5 {
            // The beam starts at the edge of the O glyph (the bottom edge when
            // the target is below), so it never crosses the red center.
            let (bx, by) = (o_x + O_GLYPH_W as f64 / 2.0, o_y + O_GLYPH_H as f64 / 2.0);
            let dx = cursor_x as f64 - bx;
            let dy = cursor_y as f64 - by;
            let d = (dx * dx + dy * dy).sqrt();
            if d > 0.0 {
                let ux = dx / d;
                let uy = dy / d;
                // Offset the origin to the glyph edge along the shot direction:
                // half the glyph height/width away from the center, so the beam
                // exits the outline instead of starting inside the O.
                let ex = if ux >= 0.0 {
                    O_GLYPH_W as f64 / 2.0
                } else {
                    -(O_GLYPH_W as f64 / 2.0)
                };
                let ey = if uy >= 0.0 {
                    O_GLYPH_H as f64 / 2.0 - 1.0
                } else {
                    -(O_GLYPH_H as f64 / 2.0 - 1.0)
                };
                let start_x = bx + ex;
                let start_y = by + ey;
                let sx = ((start_x - bx) * ux + (start_y - by) * uy).max(0.0);
                let len = self.beam_len.min(d).max(sx);
                // The beam may cross the area boundary (into the prompt), so it
                // is only clipped by the buffer itself. Characters ripple along
                // the beam: bright head toward the cursor, dim tail near the O,
                // starting past the glyph edge.
                //
                // The beam is synchronized to the eye's pulse clock: every time
                // the eye throbs, a red swell fans out of the O and sweeps along
                // the beam toward the target, so the laser literally carries the
                // same "heartbeat" as the pulsing center.
                const PI: f64 = std::f64::consts::PI;
                let cycle = (self.pulse_phase % (2.0 * PI)) / (2.0 * PI); // 0..1 per beat
                let ppos = cycle; // pulse position along the beam: 0 = O edge, 1 = target
                let pw = 0.22; // pulse half-width (in normalized beam length)
                for i in (sx as u16)..=(len as u16) {
                    let x = (bx + ux * i as f64).round() as u16;
                    let y = (by + uy * i as f64).round() as u16;
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        let t = i as f64 / len;
                        let ch = BEAM_CHARS[(i as usize + self.beam_phase as usize) % 6];
                        // Cross-section of the traveling swell: peaks at `ppos`
                        // and falls off to each side along the beam.
                        let spread = ((t - ppos) / pw).powi(2);
                        let swell = (-spread).exp(); // 0..1
                        // Keep a soft gradient (bright toward the cursor) and add
                        // the red swell on top, so the pulse is clearly visible.
                        let amt = (0.25 + 0.45 * t + 0.45 * swell).clamp(0.0, 1.0);
                        let col = blend_color(red_color(), bg, amt);
                        cell.set_char(ch);
                        cell.set_style(ratatui::style::Style::default().fg(col));
                    }
                }
            }
        }

        // ── Sleep indicator: the "zzZ" shift ───────────────────────────────
        // Three slots next to the glyph. Exactly one is an uppercase `Z`,
        // the others lowercase `z`; each frame the `Z` shifts one slot,
        // wrapping from the last slot back to the first — so the sequence is
        // `zzZ` → `Zzz` → `zZz` → `zzZ` → …
        if asleep {
            let zx = ox.saturating_add(O_GLYPH_W as u16 + 1);
            let zy = oy;
            // Phase 0 starts with the `Z` at the last index ("zzZ"); each
            // frame moves it one slot right with wraparound.
            let z_slot = (ZZZ_SLOTS - 1 + self.zzz_phase) % ZZZ_SLOTS;
            for i in 0..ZZZ_SLOTS {
                let ch = if i == z_slot { 'Z' } else { 'z' };
                if let Some(cell) = buf.cell_mut((zx + i as u16, zy)) {
                    cell.set_char(ch);
                    cell.set_style(
                        ratatui::style::Style::default()
                            .fg(muted)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    );
                }
            }
        }

        // ── Floating damage / heal numbers ─────────────────────────────────
        // 90s fighting-game style: bold digits that materialize left-to-right
        // while strobing through a hot palette, then drift away and dissolve
        // toward the background at the end of their life.
        for f in &self.floats {
            let text = f.visible_text();
            if text.is_empty() {
                continue;
            }
            let base = f.dx + if f.dx < 0.0 { -(text.len() as f64) } else { 0.0 };
            let fx = (o_x + base).round() as i64;
            let fy = (o_y + f.dy()).round() as i64;
            // Fade in over the first 120ms; dissolve over the last 350ms.
            let fade_in = (f.t / 0.12).clamp(0.0, 1.0);
            let fade_out = ((FloatText::LIFETIME - f.t) / 0.35).clamp(0.0, 1.0);
            let fade = fade_in * fade_out;
            for (i, ch) in text.chars().enumerate() {
                let x = fx + i as i64;
                if x < 0 || fy < 0 || fy > i64::from(u16::MAX) {
                    continue;
                }
                let (x, y) = (x as u16, fy as u16);
                if let Some(cell) = buf.cell_mut((x, y)) {
                    let col = float_color(f, i, info, success, bg);
                    let col = if fade >= 1.0 {
                        col
                    } else {
                        blend_color(col, bg, fade)
                    };
                    cell.set_char(ch);
                    cell.set_style(
                        ratatui::style::Style::default()
                            .fg(col)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    );
                }
            }
        }
    }
}

fn red_color() -> ratatui::style::Color {
    ratatui::style::Color::Rgb(255, 60, 60)
}

/// The medium red the eye dims to at the trough of each pulse.
fn medium_red() -> ratatui::style::Color {
    ratatui::style::Color::Rgb(110, 18, 28)
}

/// The hot white the glyph flares toward for a fraction of a second on impact.
fn flash_color() -> ratatui::style::Color {
    ratatui::style::Color::Rgb(255, 240, 200)
}

/// Color of one character of a floating damage/heal number.
///
/// Damage numbers strobe through a fighting-game palette (yellow → orange →
/// red) while materializing, then settle into a steady hot orange-red; heal
/// numbers do the same between the theme's light blue (`info`) and green
/// (`success`). Characters that appeared earlier in the reveal are already a
/// step further along the palette.
fn float_color(
    f: &FloatText,
    char_idx: usize,
    info: ratatui::style::Color,
    success: ratatui::style::Color,
    bg: ratatui::style::Color,
) -> ratatui::style::Color {
    // Age of this specific character since it became visible.
    let age = f.t - 0.05 - char_idx as f64 * 0.09;
    let settle = (age / FloatText::APPEAR).clamp(0.0, 1.0);
    match f.kind {
        FloatKind::Damage => {
            let hot = ratatui::style::Color::Rgb(255, 230, 90); // yellow
            let mid = ratatui::style::Color::Rgb(255, 140, 40); // orange
            let base = ratatui::style::Color::Rgb(255, 70, 40); // red-orange
            let strobe = if settle < 0.5 {
                blend_color(mid, hot, settle * 2.0)
            } else {
                blend_color(base, mid, (settle - 0.5) * 2.0)
            };
            blend_color(base, strobe, 1.0 - settle * 0.85)
        }
        FloatKind::Heal => {
            let strobe = blend_color(success, info, settle);
            blend_color(strobe, bg, 0.15 * (1.0 - settle))
        }
    }
}

fn blend_color(
    fg: ratatui::style::Color,
    bg: ratatui::style::Color,
    amount: f64,
) -> ratatui::style::Color {
    let (r1, g1, b1) = match fg {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let (r2, g2, b2) = match bg {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    ratatui::style::Color::Rgb(
        (r1 as f64 * amount + r2 as f64 * (1.0 - amount)) as u8,
        (g1 as f64 * amount + g2 as f64 * (1.0 - amount)) as u8,
        (b1 as f64 * amount + b2 as f64 * (1.0 - amount)) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_colors() -> LogoColors {
        LogoColors {
            primary: ratatui::style::Color::Rgb(120, 120, 255),
            background: ratatui::style::Color::Rgb(7, 7, 10),
            muted: ratatui::style::Color::Rgb(90, 90, 110),
            info: ratatui::style::Color::Rgb(95, 212, 203),
            success: ratatui::style::Color::Rgb(92, 184, 122),
        }
    }

    fn non_space_cells(buf: &ratatui::buffer::Buffer) -> Vec<(u16, u16, char)> {
        let mut out = Vec::new();
        for (i, c) in buf.content.iter().enumerate() {
            let w = buf.area.width as usize;
            let ch = c.symbol().chars().next().unwrap_or(' ');
            if ch != ' ' {
                out.push(((i % w) as u16, (i / w) as u16, ch));
            }
        }
        out
    }

    #[test]
    fn fill_phase_gradually_turns_red_and_fires() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 20, 10);
        logo.anchor(area);

        logo.advance(1.0, 10.0, 9.0);
        assert!(!logo.fired);
        assert!(logo.fill > 0.0 && logo.fill < 1.0);

        logo.advance(10.0, 10.0, 9.0);
        assert!(logo.fired);
    }

    #[test]
    fn beam_grows_toward_target() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 20, 10);
        logo.anchor(area);
        logo.advance(10.0, 10.0, 9.0);
        assert!(logo.fired);
        logo.beam_len = 0.0;

        logo.advance(0.1, 10.0, 9.0);
        assert!(logo.beam_len > 0.5);

        let dist = logo.distance_to_target();
        logo.advance(10.0, 10.0, 9.0);
        assert!(logo.beam_len <= dist + 1e-6);
    }

    #[test]
    fn o_drifts_toward_typing_position_but_stays_in_band() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 24, 8);
        logo.anchor(area);
        logo.advance(10.0, 10.0, 7.0); // fill + fire

        let start_x = logo.pos_x;
        // Target far to the right and below: the O moves right and down, but
        // never past the band bottom (area.bottom - glyph height).
        for _ in 0..120 {
            logo.advance(0.05, 20.0, 20.0);
        }
        assert!(logo.pos_x > start_x);
        assert!(logo.pos_x <= (area.right().saturating_sub(O_GLYPH_W as u16)) as f64 + 0.5);

        let max_y = (area.bottom().saturating_sub(O_GLYPH_H as u16)) as f64;
        assert!(
            logo.pos_y <= max_y + 0.5,
            "O escaped the band: pos_y={}",
            logo.pos_y
        );
    }

    #[test]
    fn render_draws_logo_o_glyph_with_red_center_and_laser() {
        let area = ratatui::layout::Rect::new(0, 0, 24, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut logo = ChatLogo::new();
        logo.anchor(area);
        logo.advance(10.0, 16.0, 9.0); // fully filled and fired
        logo.advance(0.2, 16.0, 9.0); // let the beam grow

        logo.render(
            &mut buf,
            area,
            16,
            9,
            test_colors(),
        );

        // The real logo glyph cells are drawn.
        let cells = non_space_cells(&buf);
        assert!(
            cells.iter().any(|(_, _, c)| *c == '▄'),
            "expected the O glyph top, got {:?}",
            cells
        );
        assert!(cells.iter().any(|(_, _, c)| *c == '▀'));

        // The central marks (▅ and ▀) turned red after full charge and stay
        // original glyph cells (no extra characters over the O). Because the
        // eye pulses between a strong and a medium red, any red-dominant value
        // counts as "lit".
        let reds = buf
            .content
            .iter()
            .filter(|c| {
                matches!(c.symbol(), "▅" | "▀")
                    && matches!(
                        c.fg,
                        ratatui::style::Color::Rgb(r, g, b) if r > g && r > b
                    )
            })
            .count();
        assert!(reds >= 2, "expected a fully red center, got {reds}");

        // No block-ladder characters were introduced over the glyph (the
        // █ glyph cells are part of the original cosh logo, so only the
        // ░▒▓ ladder is forbidden).
        let ladder = buf
            .content
            .iter()
            .filter(|c| matches!(c.symbol(), "░" | "▒" | "▓"))
            .count();
        assert_eq!(ladder, 0, "extra block chars drawn over the O: {ladder}");

        // The laser is drawn with its character set.
        let beam_chars = cells
            .iter()
            .filter(|(_, _, c)| matches!(c, '@' | '$' | '#' | ';' | '.' | ','))
            .count();
        assert!(
            beam_chars >= 1,
            "expected laser characters, got {beam_chars}"
        );
    }

    /// Render a frame to an ASCII grid: beam chars as `.`, red-colored cells
    /// as `*`, everything else non-space as `o`, spaces as blank. Used only to
    /// eyeball the animation phases.
    fn ascii_frame(
        logo: &mut ChatLogo,
        area: ratatui::layout::Rect,
        cursor: (u16, u16),
        primary: ratatui::style::Color,
        bg: ratatui::style::Color,
    ) -> String {
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut colors = test_colors();
        colors.primary = primary;
        colors.background = bg;
        logo.render(&mut buf, area, cursor.0, cursor.1, colors);
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                let ch = cell.symbol().chars().next().unwrap_or(' ');
                let col = cell.fg;
                let mark = match ch {
                    '@' | '$' | '#' | ';' | '.' | ',' => '.',
                    _ if matches!(
                        col,
                        ratatui::style::Color::Rgb(r, g, b) if r > g && r > b
                    ) =>
                    {
                        '*'
                    }
                    ' ' => ' ',
                    _ => 'o',
                };
                out.push(mark);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn glyph_stays_above_area_bottom_even_when_target_is_below() {
        let area = ratatui::layout::Rect::new(0, 0, 24, 12);
        let mut logo = ChatLogo::new();
        logo.anchor(area);
        logo.advance(10.0, 12.0, 11.0); // fire

        // Long chase with the cursor far below the band.
        for _ in 0..300 {
            logo.advance(0.05, 12.0, 25.0);
        }

        // Render and verify no non-space glyph cell appears below the band.
        let mut buf = ratatui::buffer::Buffer::empty(area);
        logo.render(
            &mut buf,
            area,
            12,
            25,
            test_colors(),
        );
        let glyph_bottom_limit = area.bottom();
        for (_, y, ch) in non_space_cells(&buf) {
            // Beam cells may cross the boundary; glyph cells must not.
            if matches!(ch, '@' | '$' | '#' | ';' | '.' | ',') {
                continue;
            }
            assert!(
                y < glyph_bottom_limit,
                "glyph cell at y={y} escaped the band (limit {glyph_bottom_limit})"
            );
        }
    }

    #[test]
    fn snapshot_frames() {
        let area = ratatui::layout::Rect::new(0, 0, 24, 10);
        let primary = ratatui::style::Color::Rgb(120, 120, 255);
        let bg = ratatui::style::Color::Rgb(7, 7, 10);

        let mut logo = ChatLogo::new();
        logo.anchor(area);
        logo.advance(0.4, 16.0, 9.0); // early fill
        let f1 = ascii_frame(&mut logo, area, (16, 9), primary, bg);

        logo.advance(3.0, 16.0, 9.0); // fired, beam starting
        let f2 = ascii_frame(&mut logo, area, (16, 9), primary, bg);

        logo.advance(0.3, 16.0, 9.0); // beam extended
        let f3 = ascii_frame(&mut logo, area, (16, 9), primary, bg);

        logo.advance(0.3, 20.0, 9.0); // cursor moved; beam follows
        logo.advance(0.1, 20.0, 9.0);
        let f4 = ascii_frame(&mut logo, area, (20, 9), primary, bg);

        println!(
            "frame 1 (filling):\n{f1}\nframe 2 (fired):\n{f2}\nframe 3 (beam):\n{f3}\nframe 4 (beam follows cursor):\n{f4}"
        );
        for f in [&f1, &f2, &f3, &f4] {
            assert!(f.contains('o'), "glyph missing:\n{f}");
        }
        assert!(f2.contains('*') || f3.contains('*'), "red center missing");
        assert!(f3.contains('.'), "beam missing");
    }

    /// The O's top-left once anchored: area center horizontally, row +1.
    fn anchored_origin(logo: &ChatLogo) -> (u16, u16) {
        (logo.pos_x.round() as u16, logo.pos_y.round() as u16)
    }

    /// A known non-space glyph cell per row of the O glyph, used to drive
    /// clicks: (glyph_col for each row 0..=3).
    fn solid_cols() -> [usize; O_GLYPH_H] {
        let mut out = [0; O_GLYPH_H];
        for (gr, row) in O_GLYPH.iter().enumerate() {
            out[gr] = row
                .chars()
                .position(|c| c != ' ')
                .expect("each glyph row has a solid cell");
        }
        out
    }

    #[test]
    fn click_deals_damage_by_row() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 30, 10);
        logo.anchor(area);
        let (ox, oy) = anchored_origin(&logo);
        let cols = solid_cols();

        // Top row: 50 damage.
        assert!(logo.click_logo(ox + cols[0] as u16, oy));
        assert_eq!(logo.health(), MAX_HEALTH - 50);

        // Bottom row: 10 damage.
        assert!(logo.click_logo(ox + cols[3] as u16, oy + 3));
        assert_eq!(logo.health(), MAX_HEALTH - 60);

        // Middle rows are proportional, strictly between the extremes.
        let before = logo.health();
        assert!(logo.click_logo(ox + cols[1] as u16, oy + 1));
        let mid_top = before - logo.health();
        assert!(mid_top > 10 && mid_top < 50, "row 1 damage: {mid_top}");
        let before = logo.health();
        assert!(logo.click_logo(ox + cols[2] as u16, oy + 2));
        let mid_bottom = before - logo.health();
        assert!(mid_bottom > 10 && mid_bottom < 50, "row 2 damage: {mid_bottom}");
        assert!(mid_top > mid_bottom, "higher rows must hurt more");
    }

    #[test]
    fn click_misses_spaces_and_surroundings() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 30, 10);
        logo.anchor(area);
        let (ox, oy) = anchored_origin(&logo);

        // The interior hole of the O (row 1, col 2 is a space) does not count.
        assert!(!logo.click_logo(ox + 2, oy + 1));
        // One cell above and one cell to the left of the glyph do not count.
        assert!(!logo.click_logo(ox + 3, oy - 1));
        assert!(!logo.click_logo(ox - 1, oy + 1));
        assert_eq!(logo.health(), MAX_HEALTH);
    }

    #[test]
    fn logo_heals_after_ten_seconds_and_never_above_max() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 30, 10);
        logo.anchor(area);
        let (ox, oy) = anchored_origin(&logo);
        assert!(logo.click_logo(ox + 1, oy)); // top-row hit: -50

        // 9.9s without damage: no heal yet.
        logo.advance(9.9, 10.0, 9.0);
        assert_eq!(logo.health(), MAX_HEALTH - 50);

        // Past the delay: the first +10 tick fires, with a heal float.
        logo.advance(1.1, 10.0, 9.0);
        assert_eq!(logo.health(), MAX_HEALTH - 40);
        assert!(logo.floats.iter().any(|f| f.kind == FloatKind::Heal));

        // One tick per second while recovering.
        logo.advance(1.0, 10.0, 9.0);
        assert_eq!(logo.health(), MAX_HEALTH - 30);

        // Health is capped at MAX_HEALTH even after a long idle stretch.
        logo.advance(30.0, 10.0, 9.0);
        assert_eq!(logo.health(), MAX_HEALTH);
    }

    #[test]
    fn logo_falls_asleep_at_zero_and_zzz_shifts() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 40, 10);
        logo.anchor(area);
        logo.advance(10.0, 10.0, 9.0); // fired
        let (ox, oy) = anchored_origin(&logo);

        // Four top-row hits (50 each) empty the 200 health.
        for _ in 0..4 {
            assert!(logo.click_logo(ox + 1, oy));
        }
        assert_eq!(logo.health(), 0);
        assert!(logo.is_asleep());

        // The position freezes: advancing no longer moves the glyph (kept
        // below ZZZ_FRAME_SECS so the zzZ phase is still in its first frame).
        let (fx, fy) = (logo.pos_x, logo.pos_y);
        logo.advance(0.5, 30.0, 9.0);
        assert_eq!((logo.pos_x, logo.pos_y), (fx, fy));

        // zzZ: phase 0 renders "zzZ" right of the glyph, in bold.
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let muted = ratatui::style::Color::Rgb(90, 90, 110);
        logo.render(
            &mut buf,
            area,
            10,
            9,
            test_colors(),
        );
        let zx = ox + O_GLYPH_W as u16 + 1;
        let text: String = (0..3)
            .map(|i| buf[(zx + i, oy)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(text, "zzZ");
        assert!(buf[(zx + 2, oy)].modifier.contains(ratatui::style::Modifier::BOLD));
        assert_eq!(buf[(zx + 2, oy)].fg, muted);

        // While asleep the glyph renders in the muted color (interior cells
        // get exactly `muted`; outline cells blend it with the background).
        assert_eq!(buf[(ox + 3, oy + 1)].fg, muted);

        // After one frame the uppercase Z wraps to the first slot: "Zzz".
        logo.advance(ZZZ_FRAME_SECS + 0.01, 10.0, 9.0);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        logo.render(
            &mut buf,
            area,
            10,
            9,
            test_colors(),
        );
        let text: String = (0..3)
            .map(|i| buf[(zx + i, oy)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(text, "Zzz");

        // And again: "zZz".
        logo.advance(ZZZ_FRAME_SECS + 0.01, 10.0, 9.0);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        logo.render(
            &mut buf,
            area,
            10,
            9,
            test_colors(),
        );
        let text: String = (0..3)
            .map(|i| buf[(zx + i, oy)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(text, "zZz");
    }

    #[test]
    fn damage_float_appears_bold_and_fades_out() {
        let mut logo = ChatLogo::new();
        let area = ratatui::layout::Rect::new(0, 0, 40, 12);
        logo.anchor(area);
        let (ox, oy) = anchored_origin(&logo);
        assert!(logo.click_logo(ox + 1, oy)); // top-row hit → "-50"

        // Shortly after the click the number is materializing.
        logo.advance(0.15, 10.0, 11.0);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        logo.render(
            &mut buf,
            area,
            10,
            11,
            test_colors(),
        );
        let has_bold_digit = (0..area.width).any(|x| {
            (0..area.height).any(|y| {
                let c = &buf[(x, y)];
                matches!(c.symbol(), "-" | "5" | "0")
                    && c.modifier.contains(ratatui::style::Modifier::BOLD)
            })
        });
        assert!(has_bold_digit, "expected a bold damage number on screen");

        // After the lifetime elapses the number is gone.
        logo.advance(FloatText::LIFETIME + 0.5, 10.0, 11.0);
        assert!(logo.floats.is_empty());
    }
}
