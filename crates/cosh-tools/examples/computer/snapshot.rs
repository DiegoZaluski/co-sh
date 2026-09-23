//! Demonstrate `computer_snapshot` — read the application's accessibility
//! tree as an outline (or JSON): role, name, value and state flags per
//! element, optionally scoped to a subtree by selector.
//!
//! This is the tool the model calls FIRST and re-reads after every action —
//! it is how the desktop becomes legible: "what windows does this app
//! have?", "what does this dialog contain?", "is the button disabled?"
//! State flags ride on the outline (`[disabled]`, `[focused]`,
//! `[checked]`), so the answer usually needs no second call.
//!
//! The snapshot tool talks to the live desktop for dispatch; what runs
//! offline here is input construction — including the exact JSON wire
//! shapes the MCP tool receives, since `ComputerSnapshot` deserializes
//! from the same payloads.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-snapshot
//! ```

use cosh_tools::computer::types::{ComputerSnapshot, SnapshotFormat};

fn main() {
    // ── The whole-app snapshot ──
    //
    // Wire: {"name": "Reports"} or {"pid": 4242} — exactly one scope.
    let whole_app = ComputerSnapshot {
        name: Some("Reports".into()),
        ..Default::default()
    };
    println!("whole app: {whole_app:?}");

    // ── A subtree, not the whole tree ──
    //
    // `selector` narrows to the matching element; `nth` picks among
    // multiple matches (1-based). CSS-like selectors, e.g.
    // "window[name='Main'] > group".
    let dialog_only = ComputerSnapshot {
        pid: Some(4242),
        selector: Some("window[name='Export…']".into()),
        nth: Some(1),
        ..Default::default()
    };
    println!("dialog subtree: {dialog_only:?}");

    // ── Depth and format ──
    //
    // `max_depth` (default 12, hard cap 40) keeps runaway web-embedded
    // trees inside the tool-result budget; `format: "json"` returns
    // structured role/name/value/children nodes instead of the outline.
    let json_shallow = ComputerSnapshot {
        name: Some("Reports".into()),
        max_depth: Some(4),
        format: Some(SnapshotFormat::Json),
        ..Default::default()
    };
    println!("shallow json: {json_shallow:?}");

    for (label, wire) in [
        // The inputs ARE the wire payloads the MCP tool receives:
        ("whole app (tree)", r#"{"name": "Reports"}"#),
        (
            "dialog subtree",
            r#"{"pid": 4242, "selector": "window[name='Export…']", "nth": 1}"#,
        ),
        (
            "shallow json",
            r#"{"name": "Reports", "max_depth": 4, "format": "json"}"#,
        ),
    ] {
        // Deserialize-from-wire works — the shape the schema advertises:
        let input: ComputerSnapshot = serde_json::from_str(wire).expect("valid wire shape");
        println!("{label}: {wire} → {:?}", input);
    }

    // Dispatch (needs a desktop session). The output is SnapshotOutput
    // { app, pid, snapshot (rendered outline/json), elements (count) }:
    //
    //     let out = computer::snapshot(&dialog_only).await?;
    //     println!("{}", out.snapshot);

    // ── Reading the outline leads to the next tool ──
    //
    // A snapshot line like
    //
    //     button 'Export' [enabled=false]
    //
    // answers "can I press it?" (no — wait: computer_wait Enabled) and
    // hands you the selector (`button[name='Export']`) that every other
    // computer tool accepts.
}
