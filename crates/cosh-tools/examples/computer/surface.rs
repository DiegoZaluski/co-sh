//! Demonstrate `surface` — targeting the parts of the desktop OUTSIDE any
//! application: the menu bar, taskbar/panel/dock, tray status items, the
//! desktop itself, and transient flyouts (notification centers, shell
//! context menus — anything open RIGHT NOW).
//!
//! A shell surface is a third targeting root, mutually exclusive with
//! `app`/`pid`: pass `surface` to `computer_snapshot`, `computer_act` or
//! `computer_screenshot` and their selectors resolve from the surface's
//! element tree instead of an application's.
//!
//! This example runs the surface VOCABULARY offline (`SurfaceKind` → its
//! human label, and the wire spellings). Resolution against the live shell
//! (`surface::resolve`) needs a desktop session — and on X11 window
//! managers like i3, enumeration legitimately returns nothing: only AT-SPI
//! dock frames are classified there. The tool's error carries a live census
//! of what the platform DID report, so an empty result is diagnosable from
//! the message alone. The rich surface world (menu bar, dock, tray) lives
//! on macOS and GNOME-family desktops.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-surface
//! ```

use cosh_tools::computer::surface::surface_label;
use cosh_tools::computer::types::{ComputerAct, ComputerSnapshot, SurfaceKind};

fn main() {
    // ── The vocabulary — every surface kind and its wire spelling ──
    //
    // snake_case on the wire: "menu_bar", "status_items", "taskbar",
    // "panel", "dock", "desktop", "flyout", "unknown".
    let kinds = [
        (SurfaceKind::MenuBar, "menu_bar"),
        (SurfaceKind::StatusItems, "status_items"),
        (SurfaceKind::Taskbar, "taskbar"),
        (SurfaceKind::Panel, "panel"),
        (SurfaceKind::Dock, "dock"),
        (SurfaceKind::Desktop, "desktop"),
        (SurfaceKind::Flyout, "flyout"),
        (SurfaceKind::Unknown, "unknown"),
    ];
    for (kind, wire) in kinds {
        // Round-trip through the wire spelling (Deserialize is the derive
        // the wire uses — a payload like "menu_bar" becomes the enum):
        let parsed: SurfaceKind = serde_json::from_str(&format!("\"{wire}\""))
            .expect("the snake_case wire spelling parses");
        assert_eq!(parsed, kind);
        println!("{wire:14} → {}", surface_label(kind));
    }

    // ── Targeting shapes ──
    //
    // Snapshot a shell surface (which apps are docked? what's in the
    // tray?) — same outline output as an app snapshot:
    let taskbar_snapshot = ComputerSnapshot {
        surface: Some(SurfaceKind::Taskbar),
        ..Default::default()
    };
    println!("taskbar snapshot: {taskbar_snapshot:?}");

    // Act on a surface element — press a taskbar icon by its selector:
    let press_taskbar_icon = ComputerAct {
        surface: Some(SurfaceKind::Taskbar),
        selector: Some("button[name='Files']".into()),
        ..Default::default()
    };
    println!("taskbar press: {press_taskbar_icon:?}");

    // Transient flyouts exist ONLY while open: the caller performs the
    // press that opens them on a real element first, then re-enumerates —
    // this is why a flyout snapshot can legitimately be empty.
    let open_flyout = ComputerAct {
        surface: Some(SurfaceKind::Flyout),
        selector: Some("switch[name='Bluetooth']".into()),
        ..Default::default()
    };
    println!("flyout switch: {open_flyout:?}");

    // The inputs ARE the wire payloads the MCP tools receive — deserialize
    // each to show the shape is what the schema advertises:
    let snapshot_wire = r#"{"surface": "taskbar"}"#;
    let _: ComputerSnapshot =
        serde_json::from_str(snapshot_wire).expect("valid wire shape");
    println!("taskbar snapshot wire: {snapshot_wire}");

    for (label, wire) in [
        (
            "act on a taskbar icon",
            r#"{"surface": "taskbar", "selector": "button[name='Files']"}"#,
        ),
        (
            "flip a flyout switch",
            r#"{"surface": "flyout", "selector": "switch[name='Bluetooth']"}"#,
        ),
    ] {
        let _: ComputerAct = serde_json::from_str(wire).expect("valid wire shape");
        println!("{label} wire: {wire}");
    }

    // Dispatch (needs a desktop session — and a shell that exposes the
    // surface kind; see the platform notes in docs/tools/computer/):
    //     let out = computer::snapshot(&taskbar_snapshot).await?;
}
