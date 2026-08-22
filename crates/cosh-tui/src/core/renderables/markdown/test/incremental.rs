use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA};

use super::super::MarkdownRenderable;
use super::super::estimate_height;

/// Render `content` at `width` and return the buffer's glyph rows as strings,
/// so two renders can be compared structurally (styles are ignored here; the
/// md.rs tests cover styling).
fn render_rows(content: &str, width: u16, height: u16) -> Vec<String> {
    let md = MarkdownRenderable::new(Some(content.to_string()));
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    md.render_self(&mut buf, Rect::new(0, 0, width, height));
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buf.cell((x, y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        })
        .collect()
}

/// Simulate streaming: build a renderable once and feed it growing content
/// via `set_content`, comparing every intermediate frame against a fresh
/// instance rendering the same content. The incremental block parser + block
/// render cache must be indistinguishable from a full rebuild.
#[test]
fn incremental_streaming_matches_full_render() {
    let steps: [String; 6] = [
        "# Title".to_string(),
        "# Title\n\nSome paragraph".to_string(),
        "# Title\n\nSome paragraph that now wraps around a bit\n\n- list item 1\n- list item 2".to_string(),
        "# Title\n\nSome paragraph that now wraps around a bit\n\n- list item 1\n- list item 2\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```".to_string(),
        "# Title\n\nSome paragraph that now wraps around a bit\n\n- list item 1\n- list item 2\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |".to_string(),
        "# Title\n\nSome paragraph that now wraps around a bit\n\n- list item 1\n- list item 2\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n> done".to_string(),
    ];
    const W: u16 = 40;
    const H: u16 = 60;

    let mut streaming = MarkdownRenderable::new(None);
    for step in &steps {
        streaming.set_content(step.clone());
        assert_eq!(
            render_rows(step, W, H),
            render_instance_rows(&streaming, W, H),
            "streaming frame mismatch for step {step:?}"
        );
    }
}

fn render_instance_rows(md: &MarkdownRenderable, width: u16, height: u16) -> Vec<String> {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    md.render_self(&mut buf, Rect::new(0, 0, width, height));
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buf.cell((x, y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        })
        .collect()
}

/// Repeated renders must be deterministic (cache hits produce the same
/// output as the first fill).
#[test]
fn repeated_render_is_deterministic() {
    let content = "# Head\n\nText **bold** `code`.\n\n```\ncode line\n```\n";
    let first = render_rows(content, 40, 20);
    // Fresh instances share nothing; determinism across instances pins
    // cache-independent correctness. Instance-level reuse is covered by
    // `cached_rerender_matches_fresh`.
    let second = render_rows(content, 40, 20);
    assert_eq!(first, second);
}

/// Rendering an instance twice with unchanged inputs must equal a fresh
/// instance's render — i.e. cache hits are faithful.
#[test]
fn cached_rerender_matches_fresh() {
    let content = "| A | B |\n|---|---|\n| long cell data | x |\n\npara after table\n";
    let cached = MarkdownRenderable::new(Some(content.to_string()));
    let warm = render_instance_rows(&cached, 30, 20);
    let again = render_instance_rows(&cached, 30, 20);
    assert_eq!(warm, again);
    assert_eq!(warm, render_rows(content, 30, 20));
}

/// A style change must invalidate cached renders: new colours appear even
/// though the block raws (and therefore cache keys) did not change.
#[test]
fn style_change_invalidates_cache() {
    use ratatui::style::Color;

    // Inline code gets an explicit fg derived from the configured fg colour,
    // so a fg change must be visible on screen.
    let content = "`code`";
    let mut md = MarkdownRenderable::new(Some(content.to_string()));
    md.set_fg(Some(ColorInput::RGBA(RGBA::from_ints(255, 255, 255, 255))));
    let mut buf1 = Buffer::empty(Rect::new(0, 0, 40, 4));
    md.render_self(&mut buf1, Rect::new(0, 0, 40, 4));

    md.set_fg(Some(ColorInput::RGBA(RGBA::from_ints(10, 200, 30, 255))));
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 40, 4));
    md.render_self(&mut buf2, Rect::new(0, 0, 40, 4));

    let fg1 = buf1.cell((0, 0)).unwrap().style().fg;
    let fg2 = buf2.cell((0, 0)).unwrap().style().fg;
    assert_ne!(fg1, fg2, "fg change must be reflected despite cache");
    // inline_code_fg = text * 9/10 + 25 per channel (see MarkdownPalette).
    let (r, g, b, _) = RGBA::from_ints(10, 200, 30, 255).to_ints();
    let derive = |v: u8| ((u16::from(v) * 9 / 10 + 25).min(255)) as u8;
    assert_eq!(fg2, Some(Color::Rgb(derive(r), derive(g), derive(b))));
}

/// A width change must re-layout (and re-cache), not reuse rows rendered for
/// another width.
#[test]
fn width_change_relayouts_cached_blocks() {
    let content = "a very long paragraph line that definitely wraps when narrow";
    let wide = render_rows(content, 80, 8);
    let narrow = render_rows(content, 20, 12);
    assert!(narrow.iter().any(|r| !r.trim().is_empty()));
    // Wide layout fits on fewer lines than narrow.
    let wide_lines = wide.iter().filter(|r| !r.trim().is_empty()).count();
    let narrow_lines = narrow.iter().filter(|r| !r.trim().is_empty()).count();
    assert!(narrow_lines > wide_lines);
}

/// estimate_height stays in lockstep with the block pipeline by construction:
/// for multi-block documents it must never under-count the glyph rows the
/// renderer produces.
#[test]
fn estimate_covers_multi_block_documents() {
    let docs = [
        "# H\n\ntext\n\n- a\n- b\n\n```\ncode\n```\n\n> quote\n",
        "para one\n\npara two\n\n---\n\n| X |\n|---|\n| y |",
    ];
    for doc in &docs {
        for w in [14u16, 30, 60, 120] {
            let est = estimate_height(doc, w);
            let rows = render_rows(doc, w, est.saturating_add(30));
            let glyph_rows = rows
                .iter()
                .rposition(|r| !r.trim().is_empty())
                .map_or(0, |i| i + 1);
            assert!(
                usize::from(est) >= glyph_rows,
                "estimate {est} < glyph rows {glyph_rows} for {doc:?} at width {w}"
            );
        }
    }
}
