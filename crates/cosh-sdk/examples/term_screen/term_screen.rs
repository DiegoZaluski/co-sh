//! Demonstrate the `term_screen` module: build a virtual terminal, feed it
//! a realistic program transcript with escape sequences, read back the
//! screen, inspect cells and attributes, and drive the escape parser
//! directly.
//!
//! Ported from WezTerm (see docs/sdk/term_screen/term_screen.md for the attribution
//! note). This example covers the API only.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example term-screen
//! ```

use cosh_sdk::term_screen::cell::{Cell, CellAttributes, Intensity};
use cosh_sdk::term_screen::core::{
    Terminal, TerminalConfiguration, TerminalSize, color::ColorAttribute,
};
use cosh_sdk::term_screen::escape_parser::parser::Parser;
use std::sync::Arc;

/// Minimal configuration: default palette, no scrollback.
#[derive(Debug)]
struct DemoConfig;

impl TerminalConfiguration for DemoConfig {
    fn color_palette(&self) -> cosh_sdk::term_screen::core::color::ColorPalette {
        cosh_sdk::term_screen::core::color::ColorPalette::default()
    }
    fn scrollback_size(&self) -> usize {
        0
    }
}

fn new_term(rows: usize, cols: usize) -> Terminal {
    Terminal::new(
        TerminalSize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        Arc::new(DemoConfig),
        "demo",
        "0.1",
        Box::new(std::io::sink()), // input goes nowhere in this demo
    )
}

fn main() {
    // ── 1. Build a terminal and feed it a transcript ────────────────────────
    // The transcript mixes plain text with common escape sequences:
    //   \x1b[31m   red foreground (SGR)
    //   \x1b[1m    bold
    //   \x1b[2J    clear screen
    //   \x1b[H     move cursor home
    println!("== 1. feed a transcript ==");
    let mut term = new_term(5, 40);
    let transcript = "\x1b[2J\x1b[HHello, \x1b[1m\x1b[31mworld\x1b[0m!\r\nSecond line\r\nThird line";
    term.advance_bytes(transcript.as_bytes());

    let screen = term.screen();
    println!("  physical_rows={}", screen.physical_rows);
    println!("  scrollback_rows={}", screen.scrollback_rows());
    // for_each_phys_line walks every physical row in order.
    let mut rows = Vec::new();
    screen.for_each_phys_line(|_idx, line| rows.push(line.as_str().to_string()));
    for (i, row) in rows.iter().enumerate() {
        println!("  row {i}: {row:?}");
    }
    println!();

    // ── 2. Inspect cells and attributes ─────────────────────────────────────
    // Row 0: "Hello, " (default) + "world" (bold + red) + "!" (default).
    println!("== 2. cell attributes ==");
    screen.for_each_phys_line(|_idx, line| {
        if line.as_str().starts_with("Hello") {
            for x in 7..12 {
                let cell = line.get_cell(x).unwrap();
                println!(
                    "  cell[{x}] {:?} bold={} fg={:?}",
                    cell.str(),
                    cell.attrs().intensity() == Intensity::Bold,
                    cell.attrs().foreground(),
                );
            }
        }
    });
    println!();

    // ── 3. Escape parser directly: bytes -> actions ─────────────────────────
    println!("== 3. Parser (bytes -> actions) ==");
    let mut parser = Parser::new();
    let actions = parser.parse_as_vec(b"abc\x1b[31mdef\x1b[0m");
    for (i, action) in actions.iter().enumerate() {
        println!("  [{i}] {action:?}");
    }
    println!();

    // ── 4. Cursor movement + overwrite ──────────────────────────────────────
    println!("== 4. cursor movement ==");
    let mut term = new_term(3, 20);
    term.advance_bytes(b"aaaa bbbb\r\n");
    // \x1b[H moves the cursor to row 1, column 1; the next write overwrites.
    term.advance_bytes(b"\x1b[Hxxxx");
    let mut rows = Vec::new();
    term.screen()
        .for_each_phys_line(|_idx, line| rows.push(line.as_str().to_string()));
    for row in rows {
        println!("  {row:?}");
    }
    println!();

    // ── 5. Building lines and cells by hand ─────────────────────────────────
    println!("== 5. hand-built lines ==");
    // CellAttributes is a bitfield struct: set_* methods mutate and return
    // &mut Self. Intensity and ColorAttribute are the typed values.
    let mut attrs = CellAttributes::blank();
    attrs
        .set_intensity(Intensity::Bold)
        .set_foreground(ColorAttribute::PaletteIndex(1)); // ANSI red
    let line = cosh_sdk::term_screen::core::Line::from_text("Hand-built", &attrs, 1, None);
    println!("  line: {:?} len={}", line.as_str(), line.len());
    let cell = Cell::new('X', attrs);
    println!("  cell: {:?} width={}", cell.str(), cell.width());
    println!();

    // ── 6. Multiple chunks are handled correctly ────────────────────────────
    println!("== 6. chunked input ==");
    let mut term = new_term(2, 20);
    // An escape sequence split across two advance_bytes calls.
    term.advance_bytes(b"\x1b[3");
    term.advance_bytes(b"1mchunked");
    term.advance_bytes(b"\x1b[0m");
    term.screen().for_each_phys_line(|_idx, line| {
        // An empty trailing row has no cells; guard before inspecting.
        let bold = line
            .get_cell(0)
            .map(|c| c.attrs().intensity() == Intensity::Bold)
            .unwrap_or(false);
        println!("  {:?} bold={bold:?}", line.as_str());
    });
}
