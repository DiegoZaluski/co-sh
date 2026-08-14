//! Demonstrate `cosh_tui::solid` — the reactive rendering layer: the
//! component catalogue, `DynamicRenderable`, slot placeholders, the plugin
//! `SlotRegistry`, reconciler DOM ops, and the utility helpers.
//!
//! Everything renders into an in-memory `ratatui::buffer::Buffer` (no
//! terminal needed). The scaffolded parts (renderer traits, hooks,
//! scrollback) are noted but not exercised since they have no concrete
//! implementation yet.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example solid
//! ```

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::styled_text::string_to_styled_text;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::solid::elements::catalogue::{
    LineBreakRenderable, LinkRenderable, SpanRenderable, create_component, register_component,
};
use cosh_tui::solid::elements::extras::DynamicRenderable;
use cosh_tui::solid::elements::slot::{SlotRenderable, TextSlotRenderable};
use cosh_tui::solid::plugins::slot::{PluginErrorEvent, SlotMode, SlotRegistry};
use cosh_tui::solid::reconciler::{
    create_element, create_text_node, insert_node, remove_node, set_property,
};
use cosh_tui::solid::utils::id_counter::get_next_id;
use cosh_tui::solid::utils::log::set_debug;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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
    // ── 1. The component catalogue ─────────────────────────────────────────
    // "span" and "br" are built-in; unknown tags return None.
    let span = create_component("span").expect("span is built-in");
    println!("== 1. component catalogue ==");
    println!("  built-in \"span\" -> {:?}", span.id());
    println!("  unknown \"nope\" -> {:?}", create_component("nope").is_none());
    println!();

    // ── 2. Registering a custom component ──────────────────────────────────
    // Custom tags can be added with a constructor closure.
    register_component("status", Box::new(|| -> Box<dyn Renderable> {
        let mut s = SpanRenderable::new();
        s.set_text("ready".into());
        Box::new(s)
    }));
    let status = create_component("status").expect("registered above");
    let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
    status.render_self(&mut buf, Rect::new(0, 0, 8, 1));
    println!("== 2. custom component ==");
    println!("  rendered: |{}|", (0..8).map(|x| buf[(x, 0)].symbol().to_string()).collect::<String>());
    println!();

    // ── 3. SpanRenderable: text + attributes + link ────────────────────────
    let mut bold = SpanRenderable::new();
    bold.set_text("Bold".into());
    bold.set_attributes(1); // bit 0 = bold
    show("3. SpanRenderable (bold)", &bold, 6, 1);
    let mut link = LinkRenderable::new("https://example.com".into());
    link.set_text("docs".into());
    show("    LinkRenderable", &link, 12, 1);
    let br = LineBreakRenderable::new();
    show("    LineBreakRenderable", &br, 2, 1);

    // ── 4. DynamicRenderable: resolve a component by name ──────────────────
    let dynamic = DynamicRenderable::try_new("span").expect("span is built-in");
    println!("== 4. DynamicRenderable ==");
    println!("  try_new(\"span\") -> Some({:?})", dynamic.id());
    println!("  try_new(\"nope\") -> {:?}", DynamicRenderable::try_new("nope").is_none());
    println!();

    // ── 5. Slot placeholders ───────────────────────────────────────────────
    let mut slot = SlotRenderable::new();
    let mut text_slot = TextSlotRenderable::new();
    text_slot.set_slot_parent(42);
    slot.register_child(42, Box::new(text_slot));
    println!("== 5. slots ==");
    println!("  after register_child: child_count={}", slot.child_count());
    println!("  current_child present: {}", slot.current_child().is_some());
    let moved = slot.remove_child(42).expect("child exists");
    slot.register_child(99, moved); // re-parented without destruction
    println!("  after re-parent: child_count={}", slot.child_count());
    println!();

    // ── 6. SlotRegistry: plugin fragments ──────────────────────────────────
    let mut registry = SlotRegistry::new();
    registry.register("statusbar", "clock", Box::new(|| -> Box<dyn Renderable> {
        Box::new(TextRenderable::new(Some(string_to_styled_text("12:00"))))
    }));
    registry.register("statusbar", "git", Box::new(|| -> Box<dyn Renderable> {
        Box::new(TextRenderable::new(Some(string_to_styled_text("main"))))
    }));
    registry.on_error(Box::new(|event: &PluginErrorEvent| {
        println!("  [error] {} in {}: {}", event.plugin_id, event.slot_name, event.error);
    }));
    let entries = registry.resolve("statusbar", SlotMode::Append);
    println!("== 6. SlotRegistry ==");
    println!("  slot_count: {}", registry.slot_count());
    println!("  has_entries: {}", registry.has_entries("statusbar"));
    println!("  Append resolves {} entries: {:?}", entries.len(), entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>());
    println!("  SingleWinner resolves {} entry", registry.resolve("statusbar", SlotMode::SingleWinner).len());
    registry.report_error(&PluginErrorEvent {
        plugin_id: "git".into(),
        slot_name: "statusbar".into(),
        phase: "render".into(),
        source: "test".into(),
        error: "boom".into(),
    });
    registry.unregister("statusbar");
    println!("  after unregister, has_entries: {}", registry.has_entries("statusbar"));
    println!();

    // ── 7. Reconciler DOM ops ──────────────────────────────────────────────
    let mut panel: Box<dyn Renderable> = Box::new(BoxRenderable::new());
    set_property(&mut panel, "border", "true");
    set_property(&mut panel, "title", "output");
    set_property(&mut panel, "background", "#101010");

    let mut text = create_text_node("hello");
    insert_node(&mut panel, &mut text, None);

    let element = create_element("span");
    println!("== 7. reconciler ==");
    println!("  create_element(\"span\") -> {:?}", element.id());
    println!("  panel children after insert: {}", panel.children().len());
    let child_id = panel.children()[0].id().to_string();
    remove_node(&mut panel, &child_id);
    println!("  panel children after remove: {}", panel.children().len());
    println!();

    // ── 8. Reconciler set_property on a box ────────────────────────────────
    let mut boxed: Box<dyn Renderable> = Box::new(BoxRenderable::new());
    set_property(&mut boxed, "border", "true");
    set_property(&mut boxed, "title", "panel");
    let mut inner = create_text_node("body");
    insert_node(&mut boxed, &mut inner, None);
    show("8. set_property-built box", boxed.as_ref(), 16, 3);

    // ── 9. Utility helpers ─────────────────────────────────────────────────
    set_debug(true);
    println!("== 9. utils ==");
    println!("  is_debug: {}", cosh_tui::solid::utils::log::is_debug());
    println!("  get_next_id(\"span\"): {}", get_next_id("span"));
    println!("  get_next_id(\"span\"): {}", get_next_id("span"));
    println!("  get_next_id(\"box\"): {}", get_next_id("box"));
    set_debug(false);
    println!("  is_debug after off: {}", cosh_tui::solid::utils::log::is_debug());
    println!();

    // ── 10. Scaffolded parts (informational only) ──────────────────────────
    // The renderer traits, hooks, and scrollback are declared but not yet
    // backed by implementations — we note their existence without using them.
    println!("== 10. scaffolded API surface ==");
    println!("  Renderer/RendererOptions traits: declared in solid::renderer");
    println!("  hooks (on_keyboard/on_focus/...): empty stubs");
    println!("  scrollback writer: returns empty snapshot");
    let ttdf = cosh_tui::solid::time_to_first_draw::TimeToFirstDrawRenderable::new();
    println!("  TimeToFirstDrawRenderable: placeholder, renders nothing");
    let _ = ttdf;

    // ── 11. Composition: catalogue widgets side by side ────────────────────
    // Container widgets (BoxRenderable) draw only their own frame in
    // `render_self` — children are drawn by the managed Renderer, which
    // walks the whole tree. In the direct buffer path, render each leaf.
    let mut s1 = SpanRenderable::new();
    s1.set_text("hello ".into());
    let mut s2 = SpanRenderable::new();
    s2.set_text("world".into());
    s2.set_attributes(1); // bold
    show("11. solid spans", &s1, 8, 1);
    show("    second span (bold)", &s2, 8, 1);

    let mut container = BoxRenderable::new();
    container.set_border(true);
    container.set_border_color(Some("#00AAFF".into()));
    container.set_title(Some("solid demo".into()));
    show("    box frame only (children need the managed renderer)", &container, 20, 3);
    let _ = RGBA::from_ints(0, 0, 0, 0);
}
