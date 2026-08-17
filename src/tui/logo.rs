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
pub const O_GLYPH: [&str; 4] = [
    " ▄███▄ ",
    "█▌ ▅ ▐█",
    "█▌ ▀ ▐█",
    " ▀███▀ ",
];

pub const O_GLYPH_W: usize = 7;
pub const O_GLYPH_H: usize = 4;

/// The laser beam's character set, rippling along the beam as it shoots.
const BEAM_CHARS: [char; 6] = ['@', '$', '#', ';', '.', ','];

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
        }
    }

    /// Reset the animation to its idle state (no red fill, no beam).
    pub fn reset(&mut self) {
        self.fill = 0.0;
        self.fired = false;
        self.beam_len = 0.0;
        self.bob_phase = 0.0;
        self.beam_phase = 0.0;
        self.moving = false;
        self.positioned = false;
        self.bounds = None;
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
        self.bounds = Some((
            area.x as f64,
            max_x as f64,
            area.y as f64,
            max_y as f64,
        ));
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
        primary: ratatui::style::Color,
        bg: ratatui::style::Color,
    ) {
        let max_x = area.right().saturating_sub(O_GLYPH_W as u16);
        let max_y = area.bottom().saturating_sub(O_GLYPH_H as u16);
        if max_x <= area.x || max_y <= area.y {
            return;
        }

        let bob = self.bob_offset();
        let o_x = self.pos_x.clamp(area.x as f64, max_x as f64);
        let o_y = (self.pos_y + bob).clamp(area.y as f64, max_y as f64);
        let ox = o_x.round() as u16;
        let oy = o_y.round() as u16;

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
                        let col = blend_color(eye, primary, f);
                        cell.set_char(ch);
                        cell.set_style(ratatui::style::Style::default().fg(col));
                    } else if !is_space {
                        // O outline: primary color with a gentle gradient for
                        // a rounded feel.
                        let dist_edge =
                            ((gc as f64 - 3.0).powi(2) + (gr as f64 - 1.5).powi(2)).sqrt();
                        let bri = 0.55 + (1.0 - dist_edge / 4.0).clamp(0.0, 1.0) * 0.45;
                        cell.set_char(ch);
                        cell.set_style(ratatui::style::Style::default().fg(
                            blend_color(primary, bg, bri),
                        ));
                    }
                    // Outer spaces of the glyph: leave the background alone.
                }
            }
        }

        // ── The laser beam ─────────────────────────────────────────────────
        if self.fired && self.beam_len > 0.5 {
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
    }
}

fn red_color() -> ratatui::style::Color {
    ratatui::style::Color::Rgb(255, 60, 60)
}

/// The medium red the eye dims to at the trough of each pulse.
fn medium_red() -> ratatui::style::Color {
    ratatui::style::Color::Rgb(110, 18, 28)
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
            ratatui::style::Color::Rgb(120, 120, 255),
            ratatui::style::Color::Rgb(7, 7, 10),
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
        assert!(beam_chars >= 1, "expected laser characters, got {beam_chars}");
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
        logo.render(&mut buf, area, cursor.0, cursor.1, primary, bg);
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
                    ) => '*',
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
            ratatui::style::Color::Rgb(120, 120, 255),
            ratatui::style::Color::Rgb(7, 7, 10),
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
}
