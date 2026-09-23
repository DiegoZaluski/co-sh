//! Demonstrate `computer_control` — the desktop-control pipeline tool: one
//! step is EITHER a pointer action (click/scroll/drag) OR a keyboard action
//! (`key`/`text`), chained via `then` with shell `&&` semantics.
//!
//! The canonical pattern: CLICK an element (the real click moves OS keyboard
//! focus — the one mechanism that works on every toolkit), THEN type into
//! the focused field, THEN tap `enter`. Targeting is per step; element
//! targets resolve from the element's CURRENT bounds at dispatch time,
//! never stale.
//!
//! What runs offline here: pipeline construction, per-step pointer
//! validation (`mouse::validate`) and key-name parsing (`keyboard::parse_key`)
//! — the exact checks the tool runs before touching the platform. The actual
//! dispatch (`computer::control`) synthesizes real input and needs a desktop
//! session; see the `_dispatch` placeholder at the bottom.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-control
//! ```

use cosh_tools::computer::keyboard::parse_key;
use cosh_tools::computer::mouse::{validate, MIN_DRAG_MS};
use cosh_tools::computer::types::{ComputerControl, PointerAction, PointerAnchor};

fn main() {
    // ── The canonical pipeline: click the field, type, settle, confirm ──
    let rename_file = ComputerControl {
        // `app` + `selector` = the ELEMENT form: the click lands on the
        // element's current bounds AND moves keyboard focus to it.
        app: Some("Files".into()),
        selector: Some("text_field[name='Filename']".into()),
        ..Default::default()
    };
    let type_new_name = ComputerControl {
        // No pointer fields → a KEYBOARD step. Text goes into whatever
        // holds focus NOW — the field the previous step clicked.
        text: Some("quarterly-report".into()),
        ..Default::default()
    };
    let settle = ComputerControl {
        // A standalone WAIT step (no other fields): let the dialog settle
        // before the next step acts. Capped at 10 s — a pause, not a sleep
        // primitive.
        wait: Some(600),
        ..Default::default()
    };
    let confirm = ComputerControl {
        key: Some("enter".into()),
        ..Default::default()
    };

    // Chain them: each step runs only if the previous succeeded; the first
    // failure aborts the chain and reports the point of failure.
    let pipeline = rename_file
        .clone()
        .chain(type_new_name)
        .chain(settle)
        .chain(confirm);
    println!("pipeline: {}", describe(&pipeline));

    // ── Offline validation — the same checks the tool runs first ──
    //
    // A `drag` needs BOTH endpoints and a duration >= MIN_DRAG_MS:
    let drag = ComputerControl {
        action: Some(PointerAction::Drag),
        x: Some(100),
        y: Some(100),
        x2: Some(400),
        y2: Some(300),
        duration_ms: Some(MIN_DRAG_MS),
        ..Default::default()
    };
    match validate(&drag, "computer_control") {
        Ok(()) => println!("drag step: valid"),
        Err(e) => println!("drag step rejected: {e}"),
    }

    // Half a drag is rejected before any event is sent:
    let half_drag = ComputerControl {
        action: Some(PointerAction::Drag),
        x: Some(100),
        y: Some(100),
        ..Default::default()
    };
    if let Err(e) = validate(&half_drag, "computer_control") {
        println!("half drag rejected: {e}");
    }

    // Key names parse case-insensitively — but an UPPERCASE character is
    // REJECTED (hold `shift` explicitly; a silently lowercased `A` would
    // send the wrong key and report success):
    if let Err(e) = parse_key("A", "computer_control") {
        println!("uppercase key rejected: {e}");
    }
    println!("'a' parses to: {:?}", parse_key("a", "computer_control"));

    // ── The coordinate form — canvas/custom widgets without nodes ──
    //
    // Raw desktop pixels, usually mapped from a computer_screenshot image
    // via desktop_origin/desktop_scale (see the computer-screenshot
    // example). Position-dependent: reach for it only when the target has
    // NO accessibility node. Guardrails deny it in Build mode — the
    // approval dialog may move the pointer between measure and click.
    let _canvas_click = ComputerControl {
        action: Some(PointerAction::Click),
        x: Some(512),
        y: Some(384),
        ..Default::default()
    };

    // Element form with anchor + modifiers — ctrl+click the top-right of a
    // row (e.g. its overflow ⋯ button):
    let _ctrl_click_corner = ComputerControl {
        app: Some("Files".into()),
        selector: Some("list_item[name='report.csv']".into()),
        anchor: Some(PointerAnchor::TopRight),
        held: Some(vec!["ctrl".into()]),
        ..Default::default()
    };

    // Full dispatch (needs a desktop session):
    // computer::control(&pipeline).await
}

/// Append `next` to the END of the chain (the last step's `then` is empty).
/// A local extension trait — `ComputerControl` comes from another crate, so
/// an inherent `impl` there would be illegal (orphan rule).
pub trait Chain {
    fn chain(self, next: ComputerControl) -> Self;
}

impl Chain for ComputerControl {
    fn chain(mut self, next: ComputerControl) -> Self {
        let mut tail = &mut self;
        while tail.then.is_some() {
            tail = tail.then.as_deref_mut().unwrap();
        }
        tail.then = Some(Box::new(next));
        self
    }
}

/// Mirror of the tool's `sent` report: one fragment per step, ` → `-joined.
fn describe(step: &ComputerControl) -> String {
    let mut out = Vec::new();
    let mut current = Some(step);
    while let Some(s) = current {
        if let Some(text) = &s.text {
            out.push(format!("typed {text:?}"));
        } else if let Some(key) = &s.key {
            out.push(format!("pressed {key}"));
        } else if let Some(ms) = s.wait {
            out.push(format!("waited {ms} ms"));
        } else if let Some(sel) = &s.selector {
            out.push(format!("click `{sel}`"));
        } else {
            out.push(format!("click @{},{}", s.x.unwrap_or(0), s.y.unwrap_or(0)));
        }
        current = s.then.as_deref();
    }
    out.join(" → ")
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    #[test]
    fn the_pipeline_reads_as_documented() {
        let pipeline = ComputerControl {
            app: Some("Files".into()),
            selector: Some("text_field[name='Filename']".into()),
            ..Default::default()
        }
        .chain(ComputerControl {
            text: Some("quarterly-report".into()),
            ..Default::default()
        })
        .chain(ComputerControl {
            wait: Some(600),
            ..Default::default()
        })
        .chain(ComputerControl {
            key: Some("enter".into()),
            ..Default::default()
        });
        assert_eq!(
            describe(&pipeline),
            "click `text_field[name='Filename']` → \
             typed \"quarterly-report\" → waited 600 ms → pressed enter"
        );
    }
}
