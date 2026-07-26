//! **Highlight Spinner Demo** — shows off the luminous sweep effect.
//!
//! Run with:
//! ```sh
//! cargo run --bin highlight-demo
//! ```
//!
//! Press `q` / `Esc` / `Ctrl+C` to quit. The demo cycles through several
//! spinner configurations every few seconds, demonstrating:
//!
//! - Default theme-like colours (warm orange sweep over muted text)
//! - Cool cyan highlight
//! - Vibrant magenta highlight
//! - A fast, tight beam
//! - A slow, wide atmospheric glow

#![allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Borders, Paragraph};

// Include the spinner module from the TUI component directory.
#[path = "../tui/component/spinner_highlight.rs"]
mod spinner_highlight;

use cosh_tui::core::lib::rgba::RGBA;
use spinner_highlight::HighlightSpinner;

// Demo presets

struct DemoPreset {
    label: &'static str,
    text: &'static str,
    highlight: RGBA,
    base: RGBA,
    tweak: fn(&mut HighlightSpinner),
}

const PRESETS: &[DemoPreset] = &[
    DemoPreset {
        label: "↻ Warm beam (default)",
        text: "✦ Loading your workspace …",
        highlight: RGBA::from_ints(255, 107, 48, 255), // warm orange
        base: RGBA::from_ints(128, 128, 128, 255),     // muted grey
        tweak: |_| {},
    },
    DemoPreset {
        label: "↻ Cool cyan glow",
        text: "⟡ Connecting to server …",
        highlight: RGBA::from_ints(86, 182, 194, 255), // cyan
        base: RGBA::from_ints(100, 120, 130, 255),     // slate
        tweak: |_| {},
    },
    DemoPreset {
        label: "↻ Vibrant magenta",
        text: "✦ Processing data streams …",
        highlight: RGBA::from_ints(192, 97, 203, 255), // magenta
        base: RGBA::from_ints(130, 110, 140, 255),     // mauve
        tweak: |_| {},
    },
    DemoPreset {
        label: "↻ Tight fast beam",
        text: "⚡ Compiling modules …",
        highlight: RGBA::from_ints(76, 255, 120, 255), // bright green
        base: RGBA::from_ints(90, 110, 90, 255),       // forest
        tweak: |s: &mut HighlightSpinner| {
            s.with_speed(0.018)
                .with_primary_sigma(0.04)
                .with_glow_sigma(0.12);
        },
    },
    DemoPreset {
        label: "↻ Wide atmospheric glow",
        text: "⟡ Warming up caches …",
        highlight: RGBA::from_ints(255, 200, 100, 255), // warm gold
        base: RGBA::from_ints(120, 110, 90, 255),       // taupe
        tweak: |s: &mut HighlightSpinner| {
            s.with_speed(0.005)
                .with_primary_sigma(0.15)
                .with_glow_sigma(0.35)
                .with_trail_scale(3.5);
        },
    },
    DemoPreset {
        label: "↻ Cycling messages ✨",
        text: "⏳ Step 1 of 4 …",
        highlight: RGBA::from_ints(255, 200, 100, 255), // warm gold
        base: RGBA::from_ints(120, 110, 90, 255),       // taupe
        tweak: |s: &mut HighlightSpinner| {
            s.with_speed(0.010).with_messages(
                &[
                    "⏳ Step 1 of 4 — gathering data …",
                    "⏳ Step 2 of 4 — analyzing results …",
                    "⏳ Step 3 of 4 — rendering output …",
                    "⏳ Step 4 of 4 — finalizing …",
                ],
                Some(&[360, 360, 360, 360]),
            );
        },
    },
];

// Helpers

const fn rgba_color(rgba: RGBA) -> ratatui::style::Color {
    let (r, g, b, _) = rgba.to_ints();
    ratatui::style::Color::Rgb(r, g, b)
}

// Main

fn main() -> io::Result<()> {
    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _ = terminal.clear();

    // Spinners
    // Create one spinner per preset.
    let mut spinners: Vec<HighlightSpinner> = PRESETS
        .iter()
        .map(|p| {
            let mut s = HighlightSpinner::new(p.text, p.highlight, p.base);
            (p.tweak)(&mut s);
            s
        })
        .collect();

    let mut preset_idx = 0;
    let mut last_switch = Instant::now();
    const SWITCH_INTERVAL: Duration = Duration::from_secs(5);
    let mut quit = false;

    // Main loop
    let tick_rate = Duration::from_millis(16); // ≈60 fps

    while !quit {
        // Switch preset every few seconds.
        if last_switch.elapsed() >= SWITCH_INTERVAL {
            preset_idx = (preset_idx + 1) % PRESETS.len();
            last_switch = Instant::now();

            // Reset the spinner for the new preset.
            spinners[preset_idx].reset();
            spinners[preset_idx].set_text(PRESETS[preset_idx].text);
            spinners[preset_idx]
                .set_colors(PRESETS[preset_idx].highlight, PRESETS[preset_idx].base);
            (PRESETS[preset_idx].tweak)(&mut spinners[preset_idx]);
        }

        // Advance all spinners (even inactive ones so they stay in sync).
        for spinner in &mut spinners {
            spinner.advance(0.033);
        }

        // Render
        terminal.draw(|frame| {
            let area = frame.area();
            let layout = layout_demo(area);

            // Global title.
            let title = Paragraph::new("✦ Highlight Spinner Demo ✦")
                .alignment(Alignment::Center)
                .bold()
                .fg(rgba_color(PRESETS[preset_idx].highlight));
            frame.render_widget(title, layout.title);

            // Active spinner card.
            let preset = &PRESETS[preset_idx];
            let spinner = &spinners[preset_idx];

            // Card border showing the preset label.
            let label_style = ratatui::style::Style::default().fg(rgba_color(preset.highlight));
            let card = Block::default()
                .borders(Borders::ALL)
                .border_style(label_style)
                .title(preset.label);
            frame.render_widget(card, layout.card);

            // Render the spinner inside the card.
            let inner = layout.card.inner(Margin {
                horizontal: 2,
                vertical: 2,
            });
            spinner.render(frame.buffer_mut(), inner.x, inner.y);

            // Footer with instructions.
            let footer_text = format!(
                "Auto-cycling · Preset {}/{} · Press q to quit",
                preset_idx + 1,
                PRESETS.len()
            );
            let footer = Paragraph::new(footer_text)
                .alignment(Alignment::Center)
                .fg(ratatui::style::Color::DarkGray);
            frame.render_widget(footer, layout.footer);

            // Colour legend — show the active RGB values + message index.
            let (hr, hg, hb, _) = preset.highlight.to_ints();
            let (br, bg, bb, _) = preset.base.to_ints();
            let msg_count = spinner.messages_count();
            let msg_info = if msg_count > 1 {
                format!(
                    "  ·  msg {}/{} ({}f)",
                    spinner.current_index() + 1,
                    msg_count,
                    spinner.current_duration(),
                )
            } else {
                String::new()
            };
            let legend_text = format!(
                "Highlight  rgb({:>3},{:>3},{:>3})   ·   Base  rgb({:>3},{:>3},{:>3}){msg_info}",
                hr, hg, hb, br, bg, bb,
            );
            let legend = Paragraph::new(legend_text)
                .alignment(Alignment::Center)
                .fg(ratatui::style::Color::DarkGray);
            frame.render_widget(legend, layout.legend);
        })?;

        // Input handling
        if event::poll(tick_rate)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => quit = true,
                KeyCode::Enter => {
                    // Manual preset advance.
                    preset_idx = (preset_idx + 1) % PRESETS.len();
                    last_switch = Instant::now();
                    spinners[preset_idx].reset();
                    spinners[preset_idx].set_text(PRESETS[preset_idx].text);
                    spinners[preset_idx]
                        .set_colors(PRESETS[preset_idx].highlight, PRESETS[preset_idx].base);
                    (PRESETS[preset_idx].tweak)(&mut spinners[preset_idx]);
                }
                _ => {}
            }
        }
    }

    // Cleanup
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = stdout.execute(LeaveAlternateScreen);
    let _ = stdout.flush();

    Ok(())
}

// Layout helper

use ratatui::layout::{Constraint, Direction, Layout, Margin};

struct DemoLayout {
    title: Rect,
    card: Rect,
    legend: Rect,
    footer: Rect,
}

fn layout_demo(area: Rect) -> DemoLayout {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Length(7), // card
            Constraint::Length(1), // legend
            Constraint::Length(1), // footer
            Constraint::Min(0),    // bottom padding
        ])
        .split(area);

    DemoLayout {
        title: vert[0],
        card: vert[1],
        legend: vert[2],
        footer: vert[3],
    }
}
