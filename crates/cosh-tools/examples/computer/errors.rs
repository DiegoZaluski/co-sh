//! Demonstrate the shared error renderer (`computer::errors::render`) —
//! every xa11y error variant mapped to next-step guidance, so a failure
//! TEACHES the model what to do instead of just failing.
//!
//! Why this exists: before the renderer, a broken platform condition
//! surfaced as an opaque `Platform error (-1)` — the model could not tell
//! a missing permission from a stale selector from a stuck app. The
//! renderer maps every variant (with a non_exhaustive wildcard so future
//! xa11y errors degrade gracefully) and preserves the Diagnosis verbatim:
//! what was waited for, the selector, what was LAST observed.
//!
//! Runs fully offline — building the errors is the point.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-errors
//! ```

use cosh_tools::computer::errors::render;
use xa11y::{Diagnosis, Error};

fn main() {
    const TOOL: &str = "computer_control";

    // ── A selector that matched nothing — the most common failure ──
    //
    // The error carries a Diagnosis: the condition, the selector, what was
    // last observed. The rendered message preserves it VERBATIM after the
    // guidance — the model reads "matched button \"Export\"
    // (visible=false)" and knows the element EXISTS but is hidden.
    let stale_selector = Error::SelectorNotMatched {
        selector: "button[name='Export']".into(),
        diagnosis: Some(Box::new(
            Diagnosis::new()
                .condition("press target actionable (visible && enabled)")
                .selector("button[name='Export']")
                .last_observed("matched button \"Export\" (visible=false, enabled=true)"),
        )),
    };
    println!("── SelectorNotMatched ──\n{}\n", render(TOOL, "press", &stale_selector));

    // ── Permission denied — the platform's own consent instructions ──
    //
    // The renderer passes the platform's instructions through: the fix is
    // a Settings toggle, not a code change.
    let denied = Error::PermissionDenied {
        instructions: "grant Accessibility access in System Settings".into(),
    };
    println!("── PermissionDenied ──\n{}\n", render(TOOL, "click", &denied));

    // ── The bridge is off — Chromium/Electron without a11y rendering ──
    let bridge_off = Error::AccessibilityNotEnabled {
        app: "Code".into(),
        instructions: "launch with --force-renderer-accessibility".into(),
    };
    println!(
        "── AccessibilityNotEnabled ──\n{}\n",
        render(TOOL, "snapshot", &bridge_off)
    );

    // ── Timeouts, unsupported actions, and the unknown frontier ──
    //
    // Variants that mean "the app is slow" suggest bounded retries; those
    // that mean "this backend cannot do that" suggest a different action;
    // anything NEW from a future xa11y release hits the non_exhaustive
    // wildcard instead of breaking the build.
    let transient: Vec<(&str, Error)> = vec![
        ("Timeout", Error::Timeout {
            elapsed: std::time::Duration::from_secs(3),
            diagnosis: Some(Box::new(
                Diagnosis::new()
                    .condition("visible")
                    .selector("dialog[name='Export…']")
                    .last_observed("2 matches; both visible=false"),
            )),
        }),
        ("ActionNotSupported", Error::ActionNotSupported {
            action: "expand".into(),
            role: xa11y::Role::Button,
        }),
        ("Platform", Error::Platform {
            code: -1,
            message: "pixel format not supported".into(),
        }),
    ];
    for (variant, err) in transient {
        let msg = render(TOOL, "act", &err);
        println!("── {variant} ──\n{msg}\n");
    }

    // The pattern for tool authors: route every error site through
    // `render(tool, ctx, &e)` — the tool prefix keeps a shared failure
    // reading as coming from the tool the model actually called, and the
    // ctx names the operation ("press", "type text", "resolve app").
}
