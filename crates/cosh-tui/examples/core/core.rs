//! Demonstrate `cosh_tui::core` — the rendering primitives and widget
//! library: colors, borders, styled text, unicode width utilities,
//! attributes, syntax styles, the layout tree, and the widget set.
//!
//! Everything renders into an in-memory `ratatui::buffer::Buffer` (no
//! terminal needed), which is then printed to stdout row by row.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example core
//! ```

use cosh_tui::core::border::{
    BorderSidesConfig, BorderStyle, border_chars,
};
use cosh_tui::core::layout::LayoutTree;
use cosh_tui::core::lib::rgba::{ColorInput, RGBA, parse_color};
use cosh_tui::core::lib::styled_text::{StyledText, TextChunk, UrlLink, string_to_styled_text};
use cosh_tui::core::lib::unicode_util::{graphemes_with_width, str_display_width, word_wrap};
use cosh_tui::core::renderable::{Renderable, RenderableNode, RootRenderable};
use cosh_tui::core::renderables::ascii_font::ASCIIFontRenderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::code::CodeRenderable;
use cosh_tui::core::renderables::diff::{DiffRenderable, DiffViewMode};
use cosh_tui::core::renderables::input::InputRenderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;
use cosh_tui::core::renderables::scroll_bar::{ScrollBarOrientation, ScrollBarRenderable};
use cosh_tui::core::renderables::scroll_box::ScrollBoxRenderable;
use cosh_tui::core::renderables::select::{SelectOption, SelectRenderable};
use cosh_tui::core::renderables::slider::{SliderOrientation, SliderRenderable};
use cosh_tui::core::renderables::tab_select::{TabSelectOption, TabSelectRenderable};
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::renderables::text_table::TextTableRenderable;
use cosh_tui::core::renderables::textarea::{TextareaOptions, TextareaRenderable};
use cosh_tui::core::syntax_style::SyntaxStyle;
use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers, TextAttributes};
use cosh_tui::core::utils::{
    TextAttributeOptions, attributes_with_link, create_text_attributes, get_link_id,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Render `widget` into a fresh `width`×`height` buffer and print it.
fn show(label: &str, widget: &dyn Renderable, width: u16, height: u16) {
    println!("== {label} ==");
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    widget.render_self(&mut buf, Rect::new(0, 0, width, height));
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buf[(x, y)].symbol());
        }
        println!("  |{row}|");
    }
    println!();
}

fn main() {
    // ── 1. Colors: RGBA construction and ColorInput parsing ───────────────
    let a = RGBA::from_hex("#ff8800");
    let b = RGBA::from_ints(255, 136, 0, 255);
    assert_eq!(a, b);
    assert_eq!(parse_color(ColorInput::String("orange".into())), RGBA::from_hex("#ffa500"));
    assert_eq!(parse_color(ColorInput::String("transparent".into())).to_ints(), (0, 0, 0, 0));
    // Invalid hex falls back to the magenta sentinel.
    assert_eq!(RGBA::from_hex("#nope").to_ints(), (255, 0, 255, 255));
    println!("== 1. colors ==");
    println!("  from_hex #ff8800 == from_ints: {}", a == b);
    println!("  \"orange\" -> {}", parse_color(ColorInput::String("orange".into())));
    println!();

    // ── 2. Borders: styles and side configs ───────────────────────────────
    let rounded = border_chars(BorderStyle::Rounded);
    let sides = BorderSidesConfig { top: true, right: false, bottom: true, left: false };
    println!("== 2. borders ==");
    println!("  rounded top_left: {}", rounded.top_left);
    println!(
        "  sides enabled: top={} right={} bottom={} left={}",
        sides.top, sides.right, sides.bottom, sides.left
    );
    println!();

    // ── 3. Unicode width utilities ────────────────────────────────────────
    println!("== 3. unicode widths ==");
    println!("  \"abc\" -> {} cols", str_display_width("abc"));
    println!("  \"中文字\" -> {} cols", str_display_width("中文字"));
    println!("  flag 🇧🇷 -> {} cols", str_display_width("\u{1F1E7}\u{1F1F7}"));
    println!("  wrap \"hello world foo\" @ 8 -> {:?}", word_wrap("hello world foo", 8));
    for (g, w) in graphemes_with_width("a中") {
        println!("  grapheme {g:?} width {w}");
    }
    println!();

    // ── 4. Text attributes and packing ────────────────────────────────────
    let attrs = create_text_attributes(TextAttributeOptions {
        bold: true,
        underline: true,
        ..Default::default()
    });
    let with_link = attributes_with_link(attrs, 42);
    println!("== 4. attributes ==");
    println!("  bold|underline bits: {attrs:#x}");
    println!("  bold set: {}", attrs & TextAttributes::BOLD.bits() != 0);
    println!("  link id round-trip: {} -> {}", with_link, get_link_id(with_link));
    println!();

    // ── 5. SyntaxStyle registry ───────────────────────────────────────────
    let mut styles = SyntaxStyle::create();
    let _ = styles.register_style(
        "keyword",
        &cosh_tui::core::syntax_style::StyleDefinitionInput {
            fg: Some("#ffb464".into()),
            bg: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
        },
    );
    let merged = styles.merge_styles(&["keyword", "keyword.control"]);
    println!("== 5. syntax styles ==");
    println!("  registered names: {:?}", styles.get_registered_names());
    println!(
        "  merged keyword fg: {}",
        merged.fg.map(|c| c.to_string()).unwrap_or_else(|| "none".into())
    );
    println!();

    // ── 6. LayoutTree: two stacked panels ─────────────────────────────────
    let mut tree = LayoutTree::new();
    let grow = taffy::Style { flex_grow: 1.0, ..taffy::Style::default() };
    let top = tree.new_leaf(grow.clone());
    let bottom = tree.new_leaf(grow);
    let col = tree.new_container(
        taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::length(80.0),
                height: taffy::Dimension::length(24.0),
            },
            ..taffy::Style::default()
        },
        &[top, bottom],
    );
    tree.add_child(tree.root, col);
    tree.compute_layout(80.0, 24.0);
    let top_layout = tree.layout(top);
    println!("== 6. layout tree ==");
    println!(
        "  top node at ({:.0}, {:.0}) size {:.0}x{:.0}",
        top_layout.location.x, top_layout.location.y, top_layout.size.width, top_layout.size.height
    );
    println!();

    // ── 7. TextRenderable with styled chunks ──────────────────────────────
    let mut text = TextRenderable::new(Some(StyledText {
        chunks: vec![
            TextChunk { text: "status: ".into(), fg: None, bg: None, attributes: 0, link: None },
            TextChunk {
                text: "ok".into(),
                fg: Some(RGBA::from_hex("#22c55e")),
                bg: None,
                attributes: create_text_attributes(TextAttributeOptions {
                    bold: true,
                    ..Default::default()
                }),
                link: None,
            },
        ],
    }));
    text.set_wrap_mode(cosh_tui::core::renderables::text::WrapMode::Word);
    show("7. TextRenderable", &text, 20, 1);

    // ── 8. BoxRenderable: bordered container with a child ─────────────────
    let mut panel = BoxRenderable::new();
    panel.set_border(true);
    panel.set_border_style(BorderStyle::Rounded);
    panel.set_border_color(Some("#00AAFF".into()));
    panel.set_title(Some("Panel".into()));
    panel.set_border_sides(BorderSidesConfig::ALL);
    panel.add_child(Box::new(text));
    show("8. BoxRenderable", &panel, 20, 3);

    // ── 9. MarkdownRenderable ─────────────────────────────────────────────
    let md = MarkdownRenderable::new(Some(
        "# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n".into(),
    ));
    show("9. MarkdownRenderable", &md, 30, 6);

    // ── 10. InputRenderable ───────────────────────────────────────────────
    let mut input = InputRenderable::new(Some("hi".into()));
    input.set_placeholder("Type here…".into());
    input.set_max_length(20);
    input.set_value("hello"); // cursor jumps to the end of the value
    input.insert_text(" world");
    input.delete_char_backward();
    println!("== 10. InputRenderable ==");
    println!("  value after insert + delete: {:?}", input.value());
    show("  rendered (placeholder, empty value)",
        &{
            let mut empty = InputRenderable::new(None);
            empty.set_placeholder("Type here…".into());
            empty
        },
        20, 1);
    show("  rendered (with value)", &input, 20, 1);

    // ── 11. TextareaRenderable ────────────────────────────────────────────
    let mut area = TextareaRenderable::new(TextareaOptions {
        initial_value: Some("line one".into()),
        ..Default::default()
    });
    // `insert_text` inserts at the current cursor position. The cursor starts
    // at offset 0, so the new text lands at the front.
    area.insert_text("\nline two");
    println!("== 11. TextareaRenderable ==");
    println!("  after insert_text at cursor 0: {:?}", area.value());
    area.goto_buffer_end();
    area.insert_text("\nline three");
    println!("  after goto_buffer_end + insert: {:?}", area.value());
    println!();

    // ── 12. SelectRenderable ──────────────────────────────────────────────
    let mut select = SelectRenderable::new();
    select.set_options(vec![
        SelectOption { name: "rust".into(), description: "systems language".into() },
        SelectOption { name: "go".into(), description: "concurrent".into() },
        SelectOption { name: "python".into(), description: "scripting".into() },
    ]);
    select.move_down(1);
    println!("== 12. SelectRenderable ==");
    println!(
        "  selected after move_down(1): {:?}",
        select.selected_option().map(|o| o.name.as_str())
    );
    show("  rendered", &select, 20, 4);

    // ── 13. TabSelectRenderable ───────────────────────────────────────────
    let mut tabs = TabSelectRenderable::new();
    tabs.set_options(vec![
        TabSelectOption { name: "chat".into(), description: "conversation".into() },
        TabSelectOption { name: "files".into(), description: "explorer".into() },
    ]);
    tabs.move_right();
    println!("== 13. TabSelectRenderable ==");
    println!("  selected index after move_right: {}", tabs.selected_index());
    show("  rendered", &tabs, 24, 3);

    // ── 14. SliderRenderable ──────────────────────────────────────────────
    let mut slider = SliderRenderable::new(SliderOrientation::Horizontal);
    slider.set_min(0.0);
    slider.set_max(100.0);
    slider.set_value(42.0);
    println!("== 14. SliderRenderable ==");
    println!("  value: {}", slider.value());
    show("  rendered", &slider, 30, 1);

    // ── 15. ScrollBarRenderable ───────────────────────────────────────────
    let mut bar = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    bar.set_scroll_size(100.0);
    bar.set_viewport_size(20.0);
    bar.set_scroll_position(30.0);
    println!("== 15. ScrollBarRenderable ==");
    println!("  position: {}", bar.scroll_position());
    show("  rendered", &bar, 1, 10);

    // ── 16. ScrollBoxRenderable ───────────────────────────────────────────
    let mut viewport = ScrollBoxRenderable::new();
    viewport.set_content_size(100, 500);
    viewport.scroll_by(0, 40);
    println!("== 16. ScrollBoxRenderable ==");
    println!("  scroll_y after scroll_by(0, 40): {}", viewport.scroll_y());
    println!();

    // ── 17. CodeRenderable ────────────────────────────────────────────────
    let code = CodeRenderable::new(
        Some("fn main() {\n    println!(\"hi\");\n}".into()),
        Some("rust".into()),
    );
    show("17. CodeRenderable", &code, 24, 3);

    // ── 18. DiffRenderable ────────────────────────────────────────────────
    let diff = DiffRenderable::new(Some(
        "--- a/lib.rs\n+++ b/lib.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n".into(),
    ));
    show("18. DiffRenderable (unified)", &diff, 40, 6);
    let mut split = DiffRenderable::new(Some(
        "--- a/lib.rs\n+++ b/lib.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n".into(),
    ));
    split.set_view_mode(DiffViewMode::Split);
    show("    (split)", &split, 40, 6);

    // ── 19. TextTableRenderable ───────────────────────────────────────────
    let chunk = |s: &str| vec![TextChunk {
        text: s.into(),
        fg: None,
        bg: None,
        attributes: 0,
        link: None,
    }];
    let table = TextTableRenderable::new(Some(vec![
        vec![chunk("file"), chunk("size")],
        vec![chunk("lib.rs"), chunk("1.2 kB")],
    ]));
    show("19. TextTableRenderable", &table, 24, 5);

    // ── 20. TextNodeRenderable (tree of styled text) ──────────────────────
    let mut root = cosh_tui::core::renderables::text_node::TextNodeRenderable::new(
        cosh_tui::core::renderables::text_node::TextNodeOptions::default(),
    );
    root.add(cosh_tui::core::renderables::text_node::TextNodeAddItem::Text("Hello ".into()));
    let mut bold = cosh_tui::core::renderables::text_node::TextNodeRenderable::new(
        cosh_tui::core::renderables::text_node::TextNodeOptions {
            attributes: Some(TextAttributes::BOLD.bits()),
            ..Default::default()
        },
    );
    bold.add(cosh_tui::core::renderables::text_node::TextNodeAddItem::Text("world".into()));
    root.add(cosh_tui::core::renderables::text_node::TextNodeAddItem::Node(bold));
    let chunks = root.to_chunks(&cosh_tui::core::renderables::text_node::InheritedStyle::new());
    println!("== 20. TextNodeRenderable ==");
    println!("  chunks: {:?}", chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>());
    println!(
        "  second chunk bold: {}",
        chunks[1].attributes & TextAttributes::BOLD.bits() != 0
    );
    println!();

    // ── 21. ASCIIFontRenderable ───────────────────────────────────────────
    let logo = ASCIIFontRenderable::new(Some("COSH".into()), Some("tiny".into()), Some("#22c55e".into()));
    show("21. ASCIIFontRenderable", &logo, 10, 1);

    // ── 22. RenderableNode and RootRenderable ─────────────────────────────
    let mut root_node = RenderableNode::new(Some("custom".into()));
    let fresh_text = TextRenderable::new(Some(string_to_styled_text("nested")));
    let idx = root_node.add_child(Box::new(fresh_text));
    println!("== 22. RenderableNode / RootRenderable ==");
    println!("  RenderableNode child index: {idx}");
    println!("  RenderableNode children: {}", root_node.children_ref().len());
    let mut app_root = RootRenderable::new();
    let panel_id = app_root.add_child(Box::new(panel));
    println!("  RootRenderable child index: {panel_id}");
    println!("  RootRenderable children: {}", app_root.children().len());
    println!("  root id: {:?}", app_root.id());
    println!();

    // ── 23. Mouse events ──────────────────────────────────────────────────
    let mut ev = MouseEvent::new(
        MouseEventType::Down,
        MouseButton::Left,
        3,
        4,
        MouseModifiers::none(),
    );
    ev.stop_propagation();
    ev.prevent_default();
    println!("== 23. MouseEvent ==");
    println!("  event at ({}, {})", ev.x, ev.y);
    println!("  is_left_click (Down): {}", ev.is_left_click());
    let up = MouseEvent::new(
        MouseEventType::Up,
        MouseButton::Left,
        3,
        4,
        MouseModifiers::none(),
    );
    println!("  is_left_click (Up): {}", up.is_left_click());
    println!();

    // ── 24. StyledText with a link ────────────────────────────────────────
    let linked = StyledText {
        chunks: vec![TextChunk {
            text: "open docs".into(),
            fg: Some(RGBA::from_hex("#66ccff")),
            bg: None,
            attributes: create_text_attributes(TextAttributeOptions {
                underline: true,
                ..Default::default()
            }),
            link: Some(UrlLink { url: "https://example.com".into() }),
        }],
    };
    println!("== 24. StyledText + UrlLink ==");
    println!("  link: {}", linked.chunks[0].link.as_ref().unwrap().url);
    println!("  plain: {:?}", string_to_styled_text("plain").chunks[0].text);
}
