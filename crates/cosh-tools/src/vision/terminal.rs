//! Terminal screen rendering via `WezTerm`'s virtual terminal emulator.
//!
//! Wraps `cosh_sdk::term_screen` to provide a clean API for processing
//! terminal output and extracting screen state.

use cosh_sdk::term_screen::{
    cell::{Intensity, SemanticType, Underline, VerticalAlign, color::ColorAttribute},
    core::{Terminal, TerminalConfiguration, TerminalSize},
    escape_parser::csi::Blink,
    surface::{CursorShape, CursorVisibility, line::Line},
};

use serde::Serialize;
use std::sync::Arc;

/// Schema the model passes to the vision tool.
///
/// Defaults to 24 rows, 80 columns, full metadata.
#[derive(Debug, Clone)]
pub struct TerminalInput {
    /// Raw terminal output to process (supports ANSI/escape sequences).
    pub output: String,
    /// Number of terminal rows (default: 24).
    pub rows: usize,
    /// Number of terminal columns (default: 80).
    pub cols: usize,
    /// When true returns plain text only; default false returns full JSON metadata.
    pub plain: bool,
}

impl Default for TerminalInput {
    fn default() -> Self {
        Self {
            output: String::new(),
            rows: 24,
            cols: 80,
            plain: false,
        }
    }
}

#[derive(Debug, Clone)]
struct VisionConfig;

impl TerminalConfiguration for VisionConfig {
    fn color_palette(&self) -> cosh_sdk::term_screen::core::color::ColorPalette {
        cosh_sdk::term_screen::core::color::ColorPalette::default()
    }
    fn scrollback_size(&self) -> usize {
        0
    }
}

/// Structured color value for a terminal cell.
///
/// Serialized with an explicit `type` tag so the model can distinguish
/// indexed palette colors from true-color RGB values.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ColorInfo {
    /// Indexed ANSI/256-color palette (0-255).
    Palette { index: u8 },
    /// True-color RGB value.
    Rgb { r: u8, g: u8, b: u8 },
}

fn color_to_info(color: ColorAttribute) -> ColorInfo {
    match color {
        ColorAttribute::PaletteIndex(idx) => ColorInfo::Palette { index: idx },
        ColorAttribute::TrueColorWithDefaultFallback(srgb)
        | ColorAttribute::TrueColorWithPaletteFallback(srgb, _) => {
            let (r, g, b, _) = srgb.to_srgb_u8();
            ColorInfo::Rgb { r, g, b }
        }
        ColorAttribute::Default => {
            // Unreachable: caller skips when color is Default
            ColorInfo::Rgb { r: 0, g: 0, b: 0 }
        }
    }
}

/// An OSC 8 hyperlink attached to a terminal cell.
#[derive(Debug, Serialize)]
pub struct HyperlinkInfo {
    pub uri: String,
}

/// A single visible cell on the terminal screen with full styling metadata.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct CellInfo {
    pub char: String,
    pub col: usize,
    pub row: usize,
    /// Number of columns this cell occupies (1 normal, 2 for wide/CJK/emoji).
    pub width: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline_color: Option<ColorInfo>,

    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strikethrough: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub blink: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub invisible: bool,
    /// Previous line wraps into this one (continuation marker).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wrapped: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub overline: bool,

    /// Semantic type: Output (typed by program), Input (from user), Prompt (shell chrome).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_type: Option<String>,
    /// Vertical alignment: `BaseLine`, Superscript, or Subscript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_align: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<ColorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<ColorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hyperlink: Option<HyperlinkInfo>,
}

/// Snapshot of the full terminal screen state.
#[derive(Debug, Serialize)]
pub struct TerminalOutput {
    pub rows: usize,
    pub cols: usize,
    /// Visible screen content as plain text lines (each line ends with `\n`).
    pub content: Vec<String>,
    pub cursor_x: usize,
    /// `VisibleRowIndex` from the terminal model — generally >= 0 but typed i64 by the SDK.
    pub cursor_y: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_visibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_shape: Option<String>,
    pub cells: Vec<CellInfo>,
}

/// Process terminal output through a virtual terminal emulator.
///
/// Interprets escape sequences for cursor positioning, colors, and text layout.
/// By default returns JSON with full metadata (cells, cursor, hyperlinks, styles).
/// When `input.plain` is true, returns plain text only.
///
/// # Errors
///
/// Returns `Err` if processing fails.
pub fn terminal(input: &TerminalInput) -> Result<String, String> {
    let config = Arc::new(VisionConfig);

    let rows = input.rows.max(1);
    let cols = input.cols.max(1);

    let mut term = Terminal::new(
        TerminalSize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        config,
        "Vision",
        "0.1",
        Box::new(std::io::sink()),
    );

    term.advance_bytes(input.output.as_bytes());

    let screen = term.screen();
    let physical_rows = screen.physical_rows;
    let total_rows = screen.scrollback_rows();
    let offset = total_rows.saturating_sub(physical_rows);

    let mut lines = Vec::with_capacity(physical_rows);
    let mut cells = Vec::new();

    screen.for_each_phys_line(|phys_idx, line| {
        if phys_idx >= offset {
            let row = phys_idx - offset;
            extract_cells(line, row, &mut cells);
            lines.push(line.as_str().to_string() + "\n");
        }
    });

    if input.plain {
        return Ok(lines.join(""));
    }

    let cursor = term.cursor_pos();

    let cursor_visibility = (cursor.visibility != CursorVisibility::Visible)
        .then_some(format!("{:?}", cursor.visibility));

    let cursor_shape =
        (cursor.shape != CursorShape::Default).then_some(format!("{:?}", cursor.shape));

    let state = TerminalOutput {
        rows: input.rows,
        cols: input.cols,
        content: lines,
        cursor_x: cursor.x,
        cursor_y: cursor.y,
        cursor_visibility,
        cursor_shape,
        cells,
    };

    serde_json::to_string(&state).map_err(|e| e.to_string())
}

fn extract_cells(line: &Line, row: usize, cells: &mut Vec<CellInfo>) {
    line.visible_cells().for_each(|cell| {
        let attrs = cell.attrs();

        let hyperlink = attrs.hyperlink().map(|link| HyperlinkInfo {
            uri: link.uri().to_string(),
        });

        let underline_color = (attrs.underline_color() != ColorAttribute::Default)
            .then(|| color_to_info(attrs.underline_color()));

        let foreground = (attrs.foreground() != ColorAttribute::Default)
            .then(|| color_to_info(attrs.foreground()));

        let background = (attrs.background() != ColorAttribute::Default)
            .then(|| color_to_info(attrs.background()));

        cells.push(CellInfo {
            char: cell.str().to_string(),
            col: cell.cell_index(),
            row,
            width: cell.width(),

            intensity: (attrs.intensity() != Intensity::Normal)
                .then_some(format!("{:?}", attrs.intensity())),
            underline: (attrs.underline() != Underline::None)
                .then_some(format!("{:?}", attrs.underline())),
            underline_color,

            italic: attrs.italic(),
            strikethrough: attrs.strikethrough(),
            reverse: attrs.reverse(),
            blink: attrs.blink() != Blink::None,
            invisible: attrs.invisible(),
            wrapped: attrs.wrapped(),
            overline: attrs.overline(),

            semantic_type: (attrs.semantic_type() != SemanticType::Output)
                .then_some(format!("{:?}", attrs.semantic_type())),
            vertical_align: (attrs.vertical_align() != VerticalAlign::BaseLine)
                .then_some(format!("{:?}", attrs.vertical_align())),

            foreground,
            background,
            hyperlink,
        });
    });
}
