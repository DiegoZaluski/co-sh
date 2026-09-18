//! Tests for the session-header starfield (`super`, the sparkle component).

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{IDLE_TIMEOUT, SparkleFrame, SparkleState};

const FG: (u8, u8, u8) = (220, 220, 220);
const BG: (u8, u8, u8) = (7, 7, 10);
/// 23:00 local — deep inside the night window.
const NIGHT: u16 = 23 * 60;
/// 12:00 local — well outside the night window.
const DAY: u16 = 12 * 60;

/// A `width`x1 header strip with the theme background and some header text.
fn header_buf(width: u16, text: &str) -> Buffer {
    let area = Rect::new(0, 0, width, 1);
    let mut buf = Buffer::empty(area);
    for x in 0..width {
        buf[(x, 0)].set_style(Style::default().bg(Color::Rgb(BG.0, BG.1, BG.2)));
    }
    for (i, ch) in text.chars().enumerate() {
        buf[(i as u16, 0)].set_char(ch);
    }
    buf
}

fn visible(buf: &Buffer) -> Vec<(u16, String)> {
    let w = buf.area.width;
    (0..w)
        .filter_map(|x| {
            let s = buf[(x, 0)].symbol();
            (s != " ").then_some((x, s.to_owned()))
        })
        .collect()
}

/// In-session, focused frame over `area` at `minutes` since local midnight.
fn frame<'a>(area: Rect, session_key: &'a str, minutes: u16) -> SparkleFrame<'a> {
    SparkleFrame {
        area,
        cursor: None,
        protected: None,
        session_key: Some(session_key),
        in_session: true,
        terminal_focused: true,
        foreground: FG,
        local_minutes: minutes,
    }
}

#[test]
fn outside_the_session_router_nothing_is_drawn() {
    let mut buf = header_buf(60, "← esc");
    let mut sparkle = SparkleState::new();
    sparkle.render_at(
        SparkleFrame {
            in_session: false,
            ..frame(buf.area, "s1", NIGHT)
        },
        Instant::now(),
        &mut buf,
    );
    let stars = visible(&buf);
    assert_eq!(stars.len(), 4, "only the drawn text remains: {stars:?}");
    assert!(
        stars.iter().all(|(x, _)| *x < 5),
        "no star glyphs outside the text: {stars:?}"
    );
}

#[test]
fn armed_flourish_appears_only_on_blank_cells_and_is_deterministic() {
    let mut buf = header_buf(60, "← esc");
    let start = Instant::now();
    let mut sparkle = SparkleState::new();
    sparkle.render_at(frame(buf.area, "s1", NIGHT), start, &mut buf);
    // A single header row has only a handful of blank slots; at least one
    // star must have appeared and all stars sit on cells that were blank.
    let stars = visible(&buf);
    assert!(
        stars.iter().any(|(x, _)| *x > 5),
        "stars appeared: {stars:?}"
    );
    assert!(
        stars[..4].iter().all(|(x, _)| *x < 5),
        "text cells untouched: {stars:?}"
    );
    // Deterministic: the same elapsed time reproduces the same field.
    let mut again = header_buf(60, "← esc");
    let mut second = SparkleState::new();
    second.render_at(frame(again.area, "s1", NIGHT), start, &mut again);
    assert_eq!(visible(&buf), visible(&again));
    // Stars use the braille dot table; the text cells are untouched.
    for (x, s) in &stars {
        let is_dot = super::DOTS.iter().any(|d| *d == s.as_str());
        if *x >= 5 {
            assert!(is_dot, "x={x} s={s}");
        } else {
            assert!(!is_dot, "text cell replaced: x={x} s={s}");
        }
    }
}

#[test]
fn daytime_never_draws_even_armed() {
    let mut buf = header_buf(120, "");
    let start = Instant::now();
    let mut sparkle = SparkleState::new();
    sparkle.render_at(frame(buf.area, "s1", DAY), start, &mut buf);
    assert_eq!(visible(&buf).len(), 0, "daytime: nothing drawn");
    // Later the same day: still nothing.
    sparkle.render_at(
        frame(buf.area, "s1", DAY),
        start + Duration::from_secs(300),
        &mut buf,
    );
    assert_eq!(visible(&buf).len(), 0);
    // Night falls: the armed flourish starts right away.
    let mut night = header_buf(120, "");
    sparkle.render_at(frame(night.area, "s1", NIGHT), start, &mut night);
    assert!(
        !visible(&night).is_empty(),
        "the flourish starts once night begins"
    );
}

#[test]
fn flourish_loops_at_night_instead_of_finishing() {
    let mut buf = header_buf(120, "");
    let start = Instant::now();
    let mut sparkle = SparkleState::new();
    sparkle.render_at(frame(buf.area, "s1", NIGHT), start, &mut buf);
    let first_cycle = visible(&buf);
    assert!(!first_cycle.is_empty());
    // Past the first deadline, still at night: a fresh cycle is running.
    let mut cycle2 = header_buf(120, "");
    sparkle.render_at(
        frame(cycle2.area, "s1", NIGHT),
        start + IDLE_TIMEOUT + Duration::from_secs(5),
        &mut cycle2,
    );
    assert!(
        !visible(&cycle2).is_empty(),
        "the flourish loops during the night window"
    );
    // The fresh cycle matches a first cycle 5s in (deterministic restart).
    let mut reference = header_buf(120, "");
    let mut second = SparkleState::new();
    second.render_at(
        frame(reference.area, "s1", NIGHT),
        start + Duration::from_secs(5),
        &mut reference,
    );
    assert_eq!(visible(&cycle2), visible(&reference));
}

#[test]
fn morning_pauses_the_loop_until_the_next_night() {
    let mut buf = header_buf(120, "");
    let start = Instant::now();
    let mut sparkle = SparkleState::new();
    sparkle.render_at(frame(buf.area, "s1", NIGHT), start, &mut buf);
    assert!(!visible(&buf).is_empty());
    // Sunrise (06:00 is past the window end): the running cycle pauses.
    let mut morning = header_buf(120, "");
    sparkle.render_at(
        frame(morning.area, "s1", 6 * 60),
        start + Duration::from_secs(10),
        &mut morning,
    );
    assert_eq!(visible(&morning).len(), 0, "nothing draws by day");
    sparkle.render_at(
        frame(morning.area, "s1", DAY),
        start + IDLE_TIMEOUT * 2,
        &mut morning,
    );
    assert_eq!(visible(&morning).len(), 0);
    // Next night: a fresh cycle starts again.
    let mut next_night = header_buf(120, "");
    sparkle.render_at(
        frame(next_night.area, "s1", NIGHT),
        start + IDLE_TIMEOUT * 2,
        &mut next_night,
    );
    sparkle.render_at(
        frame(next_night.area, "s1", NIGHT),
        start + IDLE_TIMEOUT * 2 + Duration::from_millis(500),
        &mut next_night,
    );
    assert!(
        !visible(&next_night).is_empty(),
        "the loop resumes the next night"
    );
}

#[test]
fn cursor_cell_and_narrow_strips_are_protected() {
    let mut buf = header_buf(60, "");
    let start = Instant::now();
    let mut sparkle = SparkleState::new();
    sparkle.render_at(
        SparkleFrame {
            cursor: Some((0, 0)),
            ..frame(buf.area, "s1", NIGHT)
        },
        start,
        &mut buf,
    );
    assert_eq!(buf[(0, 0)].symbol(), " ");
    // A strip too narrow to matter never draws.
    let mut tiny = header_buf(4, "");
    sparkle.render_at(frame(tiny.area, "s3", NIGHT), start, &mut tiny);
    assert_eq!(visible(&tiny).len(), 0);
}

#[test]
fn night_window_boundaries_and_midnight_wrap() {
    // 19:29 out, 19:30 in; 05:59 in, 06:00 out; 00:00 in, 12:00 out.
    assert!(!super::in_night_window(19 * 60 + 29));
    assert!(super::in_night_window(19 * 60 + 30));
    assert!(super::in_night_window(5 * 60 + 59));
    assert!(!super::in_night_window(6 * 60));
    assert!(super::in_night_window(0));
    assert!(!super::in_night_window(12 * 60));
}
