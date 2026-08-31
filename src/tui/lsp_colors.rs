//! Per-language label + logo color for the LSP status tags shown in the TUI
//! prompt footer. Kept in its own module so adding or tuning a language's
//! color is a one-line change — no SDK or theme coupling.
//!
//! The mapping is keyed by the LSP *server catalog name* (e.g.
//! `rust-analyzer`), not by language, so it stays a pure lookup over the
//! active-server list the harness reports. Languages not listed here fall
//! back to the theme's primary color at render time.

use cosh_tui::core::lib::rgba::RGBA;

/// (short label, logo background color) for a language server catalog name.
///
/// Colors are opaque (`alpha = 255`) logo colors: the text drawn on top uses
/// a luminance-based contrast, like the pending-queue rows.
pub fn lsp_tag(server: &str) -> Option<(&'static str, RGBA)> {
    let (label, rgb) = match server {
        "rust-analyzer" => ("Rust", (0xF7, 0x4C, 0x00)),
        "typescript-language-server" => ("JS", (0xF7, 0xDF, 0x1E)),
        "pyright" | "pylsp" => ("Python", (0x37, 0x76, 0xAB)),
        "gopls" => ("Go", (0x00, 0xAD, 0xD8)),
        "clangd" => ("C/C++", (0x51, 0x9A, 0xBA)),
        "zls" => ("Zig", (0xF7, 0xA4, 0x1D)),
        "jdtls" => ("Java", (0xE7, 0x6F, 0x00)),
        "ruby-lsp" => ("Ruby", (0xCC, 0x34, 0x2D)),
        "bash-language-server" => ("Bash", (0x4E, 0xAA, 0x25)),
        _ => return None,
    };
    let (r, g, b) = rgb;
    Some((label, RGBA::from_ints(r, g, b, 255)))
}
