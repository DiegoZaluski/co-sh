//! Astra-style starfield flourish for the session header (ported from the
//! Codex TUI's `chat_composer/sparkle.rs` + `sparkle_field.rs`).
//!
//! Small braille particles fade in, pulse and fade out on blank header cells
//! with smooth intensity transitions driven by a deterministic per-cell hash.
//! The component owns only its phase machine; the caller (the session header
//! render path) supplies the strip, the current session key and focus. Cells
//! that already carry text, modifiers, or diff directives are never touched,
//! and frames are not scheduled from here — the app's render loop redraws at
//! its normal cadence, and unchanged cells produce no terminal diff output.
//!
//! Session router only: armed once per session id, dismissed outside the
//! session, and finished after [`IDLE_TIMEOUT`] (the final [`IDLE_FADE`]
//! blends the whole field out smoothly, exactly like the original).

use std::time::{Duration, Instant};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

/// Total lifetime of one flourish.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Final stretch over which the whole field blends out.
const IDLE_FADE: Duration = Duration::from_secs(1);

/// Night window (local minutes since midnight) in which the flourish loops.
/// 19:30–06:00 is a year-round compromise between typical dusk and dawn at
/// mid latitudes (sunset 18:30–20:00, sunrise 05:00–07:00); tune here if a
/// different local reality demands it. Crossing midnight is handled.
const NIGHT_START_MINUTES: u16 = 19 * 60 + 30;
const NIGHT_END_MINUTES: u16 = 6 * 60;

/// Whether `local_minutes` (minutes since local midnight) falls inside the
/// night window in which the header flourish is allowed to play.
fn in_night_window(local_minutes: u16) -> bool {
    // The day is `NIGHT_END_MINUTES..NIGHT_START_MINUTES`; night is its
    // complement (which wraps across midnight).
    !(NIGHT_END_MINUTES..NIGHT_START_MINUTES).contains(&local_minutes)
}

/// Minimum strip width before any star is worth drawing.
const MIN_STRIP_WIDTH: u16 = 8;

/// Braille dot glyphs used as star bodies (same table as the original).
const DOTS: [&str; 8] = ["⠁", "⠂", "⠄", "⠈", "⠐", "⠠", "⡀", "⢀"];

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Phase {
    #[default]
    /// Not armed: outside the session router, or no session yet.
    Unarmed,
    /// Armed for the current session; waits for a renderable frame.
    Armed,
    /// Visible since the inner instant.
    Visible(Instant),
    /// The flourish played out; stays finished for this session id.
    Finished,
}

pub struct SparkleState {
    phase: Phase,
    /// Session id the current phase is bound to. Re-entering the session
    /// router with the same id never restarts a finished flourish; a new
    /// (or switched-to) session id arms a fresh one.
    session_key: Option<String>,
}

/// One render request for the flourish. Bundles the frame inputs so the
/// render entry points stay small.
pub struct SparkleFrame<'a> {
    /// Header strip the flourish may paint into (one row).
    pub area: Rect,
    /// Terminal cursor position, if known (its cell is never decorated).
    pub cursor: Option<(u16, u16)>,
    /// Region whose content must never be covered (e.g. the `← esc` hint).
    pub protected: Option<Rect>,
    /// Current session id (`None` when no session is open).
    pub session_key: Option<&'a str>,
    /// Whether the app is in the session router right now.
    pub in_session: bool,
    /// Whether the terminal currently has focus.
    pub terminal_focused: bool,
    /// Theme foreground, used as the bright end of the star blend.
    pub foreground: (u8, u8, u8),
    /// Local time in minutes since midnight. The flourish only plays (and
    /// only loops) inside the night window — see `in_night_window`.
    pub local_minutes: u16,
}

impl SparkleState {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Unarmed,
            session_key: None,
        }
    }

    /// Render the flourish into `frame.area` (one header row).
    pub fn render(&mut self, frame: SparkleFrame<'_>, buf: &mut Buffer) {
        self.render_at(frame, Instant::now(), buf);
    }

    /// Whether the flourish is currently playing (Visible phase). The idle
    /// render loop uses this to keep its poll timeout short — a time-driven
    /// animation would otherwise step at the (much slower) idle redraw rate.
    pub const fn is_animating(&self) -> bool {
        matches!(self.phase, Phase::Visible(_))
    }

    fn render_at(&mut self, frame: SparkleFrame<'_>, now: Instant, buf: &mut Buffer) {
        // Dismiss outside the session router or without a session; a new
        // session id arms a fresh flourish.
        let key_changed = self.session_key.as_deref() != frame.session_key;
        if key_changed {
            self.session_key = frame.session_key.map(str::to_owned);
            self.phase = if frame.in_session && frame.session_key.is_some() {
                Phase::Armed
            } else {
                Phase::Unarmed
            };
        } else if !frame.in_session {
            self.phase = Phase::Unarmed;
            return;
        }

        // The flourish plays only during the night window; a cycle caught
        // running when morning arrives pauses, and a finished one waits for
        // the next night to start a fresh loop.
        let in_window = in_night_window(frame.local_minutes);
        match self.phase {
            Phase::Unarmed => return,
            Phase::Finished if !in_window => return,
            Phase::Finished => self.phase = Phase::Armed,
            Phase::Visible(_) if !in_window => {
                // Morning arrived mid-cycle: pause; the loop resumes when
                // the next night window opens.
                self.phase = Phase::Armed;
                return;
            }
            Phase::Armed if !in_window => return,
            _ => {}
        }

        let mut since = match self.phase {
            Phase::Visible(since) => Some(since),
            Phase::Armed => None,
            Phase::Unarmed | Phase::Finished => return,
        };
        let mut elapsed =
            since.map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        if elapsed >= IDLE_TIMEOUT {
            if !in_window {
                self.phase = Phase::Finished;
                return;
            }
            // Loop: restart the cycle immediately. The per-star sine makes
            // the re-entry fade back in smoothly instead of popping.
            self.phase = Phase::Armed;
            since = None;
            elapsed = Duration::ZERO;
        }
        if !frame.terminal_focused || frame.area.width < MIN_STRIP_WIDTH || frame.area.height == 0 {
            // Stay armed: the flourish simply waits for a renderable frame.
            return;
        }
        let since = since.unwrap_or(now);
        self.phase = Phase::Visible(since);

        // Global fade over the final IDLE_FADE of the lifetime.
        let fade_start = IDLE_TIMEOUT - IDLE_FADE;
        let visibility = if elapsed > fade_start {
            (IDLE_TIMEOUT - elapsed).as_secs_f32() / IDLE_FADE.as_secs_f32()
        } else {
            1.0
        };
        render_stars(
            frame.area,
            frame.cursor,
            frame.protected,
            elapsed.min(fade_start),
            frame.foreground,
            visibility,
            buf,
        );
    }
}

/// Paint the deterministic starfield. Only blank, unstyled true-color cells
/// outside the cursor and protected content can be decorated; the caller
/// supplies elapsed time and visibility. Mirrors
/// `sparkle_field.rs::render_stars` exactly.
fn render_stars(
    area: Rect,
    cursor: Option<(u16, u16)>,
    protected: Option<Rect>,
    elapsed: Duration,
    foreground: (u8, u8, u8),
    visibility: f32,
    buf: &mut Buffer,
) {
    let time = elapsed.as_secs_f32();
    for y in area.y..area.bottom() {
        let mut occupied_until = area.x;
        for x in area.x..area.right() {
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            if x < occupied_until {
                continue;
            }
            if cell.symbol() != " " {
                occupied_until = x.saturating_add(UnicodeWidthStr::width(cell.symbol()) as u16);
                continue;
            }
            if cursor == Some((x, y))
                || protected.is_some_and(|p| p.contains(ratatui::layout::Position::new(x, y)))
                || !cell.modifier.is_empty()
                || cell.diff_option != CellDiffOption::None
            {
                continue;
            }
            let Color::Rgb(r, g, b) = cell.bg else {
                continue;
            };
            let mut hash = u64::from(y - area.y) * 65537 + u64::from(x - area.x);
            hash = (hash ^ (hash >> 16)).wrapping_mul(0x45d9f3b);
            hash = (hash ^ (hash >> 16)).wrapping_mul(0x45d9f3b);
            hash ^= hash >> 16;
            if hash % 5 != 0 {
                continue;
            }
            let phase =
                (time / (4.0 + (hash % 31) as f32 / 10.0) + (hash % 997) as f32 / 997.0).fract();
            let brightness = (phase * std::f32::consts::PI).sin().powi(12) * 0.55 * visibility;
            if brightness < 0.04 {
                continue;
            }
            cell.set_symbol(DOTS[(hash / 161 % 8) as usize]);
            let (br, bg_blend, bb) = blend(foreground, (r, g, b), brightness);
            cell.set_fg(Color::Rgb(br, bg_blend, bb));
        }
    }
}

/// Linear blend of `fg` toward `bg` by `alpha` (same helper as the original).
fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let r = (fg.0 as f32 * alpha + bg.0 as f32 * (1.0 - alpha)) as u8;
    let g = (fg.1 as f32 * alpha + bg.1 as f32 * (1.0 - alpha)) as u8;
    let b = (fg.2 as f32 * alpha + bg.2 as f32 * (1.0 - alpha)) as u8;
    (r, g, b)
}

#[cfg(test)]
#[path = "sparkle_tests.rs"]
mod tests;
