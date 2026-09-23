//! Demonstrate `mouse` — the pointer engine behind `computer_control`'s
//! pointer steps: click/double-click/right-click, move, press-and-hold
//! pairs for custom drags, wheel scroll, and NATIVE drags (press →
//! interpolated movement at ~60 Hz → release, paced by `duration_ms`).
//!
//! Targeting has two forms, per step:
//!
//! - **Element** (`app`/`pid` or `surface` + `selector`): the point
//!   resolves from the element's CURRENT bounds — tree-grounded, never
//!   stale, safe in Build mode.
//! - **Coordinate** (`x`/`y`): raw desktop pixels, usually mapped from a
//!   screenshot. Position-dependent — the pointer may be somewhere else by
//!   the time the click lands — so guardrails confine it to Yolo/Command.
//!
//! What runs offline here: step construction and pointer validation (the
//! same `mouse::validate` the tool runs first). Dispatch needs a desktop.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-mouse
//! ```

use cosh_tools::computer::mouse::{DEFAULT_DRAG_MS, MIN_DRAG_MS, validate};
use cosh_tools::computer::types::{ComputerControl, PointerAction, PointerAnchor};

fn main() {
    const TOOL: &str = "computer_control";

    // ── Element click — the default, and the focus mover ──
    //
    // A click on an element moves OS keyboard focus to it; that is how the
    // NEXT step's typing finds the right field (see computer-control).
    let click_ok = ComputerControl {
        app: Some("Reports".into()),
        selector: Some("button[name='OK']".into()),
        ..Default::default()
    };
    assert!(validate(&click_ok, TOOL).is_ok());
    println!("element click: valid");

    // ── Anchor: where inside the bounds the point lands ──
    //
    // center (default) | top_left | top_right | bottom_left | bottom_right.
    // Half-open bounds: top_left is the last point INSIDE, not the edge.
    let click_overflow = ComputerControl {
        app: Some("Files".into()),
        selector: Some("list_item[name='report.csv']".into()),
        anchor: Some(PointerAnchor::TopRight), // the ⋯ button zone
        ..Default::default()
    };
    assert!(validate(&click_overflow, TOOL).is_ok());

    // ── Drag between elements — the native form ──
    //
    // press at the start, interpolate the pointer path (~60 Hz) across
    // duration_ms, release at the end. The element form keeps BOTH ends
    // tree-grounded; `x2`/`y2` may substitute for `to_selector`, but
    // mixing forms is rejected.
    let drag_card = ComputerControl {
        app: Some("Board".into()),
        selector: Some("list_item[name='todo']".into()),
        action: Some(PointerAction::Drag),
        to_selector: Some("list_item[name='done']".into()),
        duration_ms: Some(DEFAULT_DRAG_MS), // 150; floor is MIN_DRAG_MS = 50
        ..Default::default()
    };
    assert!(validate(&drag_card, TOOL).is_ok());
    println!(
        "element→element drag: valid (duration {} ms, min {})",
        DEFAULT_DRAG_MS, MIN_DRAG_MS
    );

    // ── Validation errors, offline ──
    let cases: Vec<(&str, ComputerControl)> = vec![
        // A drag needs BOTH endpoints:
        (
            "drag without an end point",
            ComputerControl {
                action: Some(PointerAction::Drag),
                x: Some(100),
                y: Some(100),
                ..Default::default()
            },
        ),
        // Scroll requires its delta:
        (
            "scroll without dx/dy",
            ComputerControl {
                action: Some(PointerAction::Scroll),
                x: Some(100),
                y: Some(100),
                ..Default::default()
            },
        ),
        // A selector without an app scope has nothing to resolve against:
        (
            "selector without app/pid/surface",
            ComputerControl {
                selector: Some("button[name='OK']".into()),
                ..Default::default()
            },
        ),
    ];
    for (label, step) in cases {
        if let Err(e) = validate(&step, TOOL) {
            println!("{label} rejected: {e}");
        }
    }

    // ── Press-and-hold pairs — custom drag choreography ──
    //
    // `down` at one point, moves, `up` elsewhere — for drags the native
    // form cannot express (drop onto a moving target, freehand paths).
    // `up` needs no target at all.
    let _press = ComputerControl {
        action: Some(PointerAction::Down),
        x: Some(100),
        y: Some(100),
        ..Default::default()
    };
    let _release = ComputerControl {
        action: Some(PointerAction::Up),
        ..Default::default()
    };

    // Dispatch (needs a desktop session):
    // computer::control(&drag_card).await
}
