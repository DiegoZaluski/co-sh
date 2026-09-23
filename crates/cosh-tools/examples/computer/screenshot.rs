//! Demonstrate `computer_screenshot` — capture the desktop (or an app /
//! shell-surface region), optionally annotating matched elements with
//! numbered boxes and returning a legend of copy-ready selectors.
//!
//! The screenshot is how the model SEES; the accessibility tree is how it
//! ADDRESSES. The annotated capture bridges the two: every box drawn on
//! the image maps to a selector (`button[name='Export']:nth(7)`) that
//! `computer_control`/`computer_act` accept directly.
//!
//! This example runs the IMAGE-MATH offline — the coordinate mapping every
//! consumer of a screenshot needs — using the exact output struct the tool
//! returns. The capture itself needs a desktop session.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-screenshot
//! ```

use cosh_tools::computer::types::{ComputerScreenshot, LegendEntryOutput, ScreenshotOutput};

fn main() {
    // ── Input shapes (wire payloads the MCP schema accepts) ──
    //
    // {}                                  → full desktop, no annotation
    // {"app": "Reports"}                  → the app's window region
    // {"region": [0, 0, 800, 600]}        → a display region (x, y, w, h)
    // {"app": "Reports", "annotate": true} → ANNOTATED capture: a labeled
    //     box on every matching element (selector defaults to "*" when
    //     annotating) + a legend mapping each tag to a selector
    //
    // ANNOTATION REQUIRES `annotate: true` — a bare selector is an element
    // CAPTURE (just those pixels) and annotation is mutually exclusive with
    // `region`. With `annotate: true` the selector selects WHICH elements
    // get boxes; `nth` does not apply (the legend covers every match —
    // narrow the selector instead). Comma alternations are rejected up
    // front: `:nth(k)` would bind to the last clause only, so the legend
    // would name a different element than the box it labels.
    let annotated = ComputerScreenshot {
        app: Some("Reports".into()),
        selector: Some("button".into()),
        annotate: true,
        ..Default::default()
    };
    println!("annotated capture input: {annotated:?}");

    // The input IS the wire payload the MCP tool receives — deserialize
    // this shape to show the schema advertises exactly these fields:
    let annotated_wire =
        r#"{"app": "Reports", "selector": "button", "annotate": true}"#;
    let parsed: ComputerScreenshot =
        serde_json::from_str(annotated_wire).expect("valid wire shape");
    println!("wire {annotated_wire} → {parsed:?}");

    // ── The output, and the mapping math ──
    //
    // The delivered image may be DOWNSCALED (the payload is capped), so
    // image pixels are NOT desktop pixels. ScreenshotOutput reports the
    // mapping: desktop = desktop_origin + image_coord * desktop_scale.
    let out = sample_output();
    println!(
        "delivered image: {}x{} ({} bytes)",
        out.width, out.height, out.bytes
    );

    // Suppose the model spots the Export button's box at image pixel
    // (410, 252) — the legend says the box is tag B7 — but this tool call
    // needs desktop pixels (e.g. a coordinate click on a canvas):
    let image_coord = (410.0_f64, 252.0_f64);
    let desktop = (
        out.desktop_origin.0 as f64 + image_coord.0 * out.desktop_scale.0,
        out.desktop_origin.1 as f64 + image_coord.1 * out.desktop_scale.1,
    );
    println!(
        "image {image_coord:?} → desktop ({:.0}, {:.0})",
        desktop.0, desktop.1
    );

    // ── The legend: image tags become selectors ──
    //
    // One entry per drawn box. Prefer the element form (selector) over
    // mapped coordinates — the element resolves from CURRENT bounds; the
    // coordinate form is only for node-less canvas targets.
    for entry in &out.legend {
        println!(
            "tag {} → {} ({} {})",
            entry.tag,
            entry.selector,
            entry.role,
            entry.name.as_deref().unwrap_or("")
        );
    }

    // Capture (needs a desktop session):
    //     let out = computer::screenshot(&annotated).await?;
}

/// A sample output shaped exactly like the tool's return value, so the
/// mapping math above is runnable offline.
fn sample_output() -> ScreenshotOutput {
    ScreenshotOutput {
        width: 800,
        height: 600,
        desktop_origin: (0, 0),
        desktop_scale: (1.0, 1.0),
        bytes: 42,
        legend: vec![LegendEntryOutput {
            tag: "B7".into(),
            selector: "button[name='Export']:nth(7)".into(),
            role: "button".into(),
            name: Some("Export".into()),
            color: [255, 0, 0],
        }],
        omitted: Vec::new(),
        truncated: 0,
        images: Vec::new(),
    }
}
