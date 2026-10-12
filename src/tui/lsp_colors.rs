//! Per-language label + logo color for the LSP status tags shown in the TUI
//! prompt footer. Kept in its own module so adding or tuning a language's
//! color is a one-line change — no SDK or theme coupling.
//!
//! The mapping is keyed by the LSP *server catalog name* (e.g.
//! `rust-analyzer`), not by language, so it stays a pure lookup over the
//! active-server list the harness reports. Languages not listed here fall
//! back to the theme's primary color at render time.
//!
//! Colors are the languages' official logo/brand colors (see the
//! per-entry source); when a project has no brand color the entry notes
//! what was sampled instead.

use cosh_tui::core::lib::rgba::RGBA;

/// (short label, logo color) for a language server catalog name.
///
/// Colors are opaque (`alpha = 255`) logo colors: the render paints the tag's
/// FONT with them, on the default background.
pub fn lsp_tag(server: &str) -> Option<(&'static str, RGBA)> {
    let (label, rgb) = match server {
        // ── Systems ──────────────────────────────────────────────────
        // Rust orange (rust-lang.org brand).
        "rust-analyzer" => ("Rust", (0xF7, 0x4C, 0x00)),
        // LLVM blue (llvm.org logo).
        "clangd" => ("C/C++", (0x51, 0x9A, 0xBA)),
        // Zig orange (ziglang.org logo).
        "zls" => ("Zig", (0xF7, 0xA4, 0x1D)),
        // ── Web / scripting ──────────────────────────────────────────
        // Go blue (go.dev logo).
        "gopls" => ("Go", (0x00, 0xAD, 0xD8)),
        // Python blue (python.org logo, lower half).
        "pyright" | "pylsp" => ("Python", (0x37, 0x76, 0xAB)),
        // JavaScript yellow (MDN JS logo).
        "typescript-language-server" => ("JS", (0xF7, 0xDF, 0x1E)),
        // Ruby red (ruby-lang.org logo).
        "ruby-lsp" => ("Ruby", (0xCC, 0x34, 0x2D)),
        // PHP purple (php.net logo).
        "php-actor" => ("PHP", (0x77, 0x7B, 0xB4)),
        // ── BEAM ─────────────────────────────────────────────────────
        // Elixir brand purple (elixir-lang.org/trademarks).
        "elixir-ls" => ("Elixir", (0x88, 0x47, 0xB9)),
        // Erlang red (erlang.org logo).
        "erlang-ls" => ("Erlang", (0xA9, 0x05, 0x33)),
        // ── JVM ──────────────────────────────────────────────────────
        // Java orange-red (oracle.com/java logo).
        "jdtls" => ("Java", (0xE7, 0x6F, 0x00)),
        // Kotlin brand purple (kotlinlang.org).
        "kotlin-language-server" => ("Kotlin", (0x7F, 0x52, 0xFF)),
        // Clojure blue (clojure.org logo).
        "clojure-lsp" => ("Clojure", (0x58, 0x81, 0xD8)),
        // ── Functional ───────────────────────────────────────────────
        // Haskell purple (haskell.org brand).
        "haskell-language-server" => ("Haskell", (0x5D, 0x4F, 0x85)),
        // OCaml orange (ocaml.org logo).
        "ocamllsp" => ("OCaml", (0xF2, 0x91, 0x00)),
        // ── Shell / config ───────────────────────────────────────────
        // Bash green (gnu.org bash logo).
        "bash-language-server" => ("Bash", (0x4E, 0xAA, 0x25)),
        // Lua navy (lua.org logo).
        "lua-language-server" => ("Lua", (0x00, 0x00, 0x80)),
        // Taplo red, sampled from the official taplo-icon.png.
        "taplo" => ("TOML", (0xE4, 0x3A, 0x25)),
        // YAML red (yaml.org logo).
        "yaml-language-server" => ("YAML", (0xCB, 0x17, 0x1E)),
        // Marksman is monochrome (white glyph on black); white is the
        // readable half on dark terminals.
        "marksman" => ("MD", (0xFF, 0xFF, 0xFF)),
        // ── Web markup ───────────────────────────────────────────────
        // HTML5 orange (W3C HTML5 logo).
        "vscode-html-language-server" => ("HTML", (0xE3, 0x4F, 0x26)),
        // CSS3 blue (W3C CSS logo).
        "vscode-css-language-server" => ("CSS", (0x15, 0x72, 0xB6)),
        // Docker blue (docker.com logo).
        "dockerfile-language-server-nodejs" => ("Docker", (0x24, 0x96, 0xED)),
        // ── Nix ──────────────────────────────────────────────────────
        // Nix dark blue (NixOS logo).
        "nixd" => ("Nix", (0x52, 0x77, 0xC3)),
        _ => return None,
    };
    let (r, g, b) = rgb;
    Some((label, RGBA::from_ints(r, g, b, 255)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every server the SDK catalog can spawn must resolve to a tag —
    /// otherwise it renders in the generic primary color.
    #[test]
    fn every_catalog_server_has_a_tag() {
        for spec in cosh_sdk::lsp::CATALOG {
            assert!(
                lsp_tag(spec.name).is_some(),
                "`{}` is in the SDK catalog but has no LSP tag",
                spec.name
            );
        }
    }

    #[test]
    fn unknown_servers_still_fall_back() {
        assert!(lsp_tag("definitely-not-a-server").is_none());
    }
}
