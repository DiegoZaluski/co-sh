//! Shell-surface component — resolution of an OS shell surface (menu bar,
//! Dock/taskbar/panel, tray status items, desktop, flyouts) as a targeting
//! root.
//!
//! This is a COMPONENT, not a tool: it knows how to turn a
//! [`SurfaceKind`] into a [`ShellSurface`] handle (whose `locator` /
//! `as_element` slot into the exact same selector flow apps use) and how
//! to enforce the `name | pid | surface` targeting exclusivity, and
//! nothing else. The observation tools (`computer_snapshot`,
//! `computer_screenshot`) and the semantic pipeline (`computer_act`)
//! compose it; a future tool that needs shell targeting can reuse it
//! without carrying any of them along.
//!
//! Enumeration is read-only by xa11y's contract: listing a surface never
//! opens, closes, focuses or presses anything.
use std::sync::Arc;
use std::time::Duration;

use xa11y::{Provider, ShellSurface, ShellSurfaceKind};

use super::types::{ComputerAct, ComputerSnapshot, ComputerScreenshot, SurfaceKind};

/// Map the wire enum onto xa11y's kind vocabulary — total, so a missed arm
/// fails compilation rather than silently targeting the wrong surface.
pub(crate) fn kind(kind: SurfaceKind) -> ShellSurfaceKind {
    match kind {
        SurfaceKind::MenuBar => ShellSurfaceKind::MenuBar,
        SurfaceKind::StatusItems => ShellSurfaceKind::StatusItems,
        SurfaceKind::Taskbar => ShellSurfaceKind::Taskbar,
        SurfaceKind::Panel => ShellSurfaceKind::Panel,
        SurfaceKind::Dock => ShellSurfaceKind::Dock,
        SurfaceKind::Desktop => ShellSurfaceKind::Desktop,
        SurfaceKind::Flyout => ShellSurfaceKind::Flyout,
        SurfaceKind::Unknown => ShellSurfaceKind::Unknown,
    }
}

/// Shared targeting validation for `name | pid | surface` (exactly one).
///
/// `tool` names the calling tool in the error messages; `app_field` is
/// `"name"` (snapshot/act) or `"app"` (screenshot), matching each tool's
/// wire shape.
///
/// # Errors
///
/// Returns `Err` when two or zero targets are given.
fn validate_targeting(
    tool: &str,
    app_field: &str,
    app: Option<&str>,
    pid: Option<u32>,
    surface: Option<SurfaceKind>,
    allow_rootless: bool,
) -> Result<(), String> {
    // A blank/whitespace app name is ABSENT: a model padding a name with
    // spaces must get the clean "provide" error here, not a dispatch
    // panic downstream (the dispatch matches trim the name the same way).
    let app = app.map(str::trim).filter(|s| !s.is_empty());
    let has_app = app.is_some();
    let has_surface = surface.is_some();
    let given = usize::from(has_app) + usize::from(pid.is_some()) + usize::from(has_surface);
    if given == 0 {
        // `computer_screenshot` has legal ROOTLESS forms (full display,
        // `region`) — the caller's later clauses police those; every other
        // consumer requires exactly one root.
        return if allow_rootless {
            Ok(())
        } else {
            Err(format!("{tool}: provide {app_field}, `pid` or `surface`"))
        };
    }
    if given > 1 {
        return Err(if has_app && pid.is_some() {
            format!("{tool}: provide {app_field} or `pid`, not both")
        } else {
            format!(
                "{tool}: target ONE of {app_field}/`pid`/`surface` — an app and a shell \
                 surface cannot share a call"
            )
        });
    }
    Ok(())
}

/// Targeting check for `computer_snapshot` (`name`/`pid`/`surface`).
pub fn validate_snapshot(input: &ComputerSnapshot) -> Result<(), String> {
    validate_targeting(
        "computer_snapshot",
        "`name`",
        input.name.as_deref(),
        input.pid,
        input.surface,
        false,
    )
}

/// Targeting check for `computer_act` (`name`/`pid`/`surface` on a
/// semantic step).
pub fn validate_act(step: &ComputerAct) -> Result<(), String> {
    validate_targeting(
        "computer_act",
        "`name`",
        step.name.as_deref(),
        step.pid,
        step.surface,
        false,
    )
}

/// Targeting check for `computer_screenshot` (`app`/`pid`/`surface`).
///
/// Rootless calls are LEGAL here (full display and `region` captures) —
/// this check only rejects MIXED roots; the tool's own clauses police
/// "root without selector" and "annotate without root".
pub fn validate_screenshot(input: &ComputerScreenshot) -> Result<(), String> {
    validate_targeting(
        "computer_screenshot",
        "`app`",
        input.app.as_deref(),
        input.pid,
        input.surface,
        true,
    )
}

/// Resolve a surface kind against an EXPLICIT provider — the seam the
/// mock-provider tests use (they supply the mock instead of the live
/// desktop session).
///
/// # Errors
///
/// Returns `Err` when the platform accessibility API is unreachable or
/// the platform has no surface of this kind right now (e.g. a flyout that
/// is not open — that is honest scope, not a failure).
pub fn resolve_with(
    provider: Arc<dyn Provider>,
    surface: SurfaceKind,
    timeout: Duration,
) -> Result<ShellSurface, String> {
    ShellSurface::by_kind_with(provider.clone(), kind(surface), timeout).map_err(|e| {
        // Field-test finding (X11/i3 desktop): a miss is only honest
        // scope when the platform DOES classify surfaces. Enrich the error
        // with a live census so an empty classification (WM whose bar
        // never registers an AT-SPI dock frame — i3bar, polybar — or a
        // compositor the backend cannot see through) is diagnosable from
        // the error alone, instead of reading as a missing flyout.
        let census = ShellSurface::list_with(Arc::clone(&provider))
            .map(|surfaces| {
                if surfaces.is_empty() {
                    "census: 0 shell surfaces enumerated — this backend classifies \
                     AT-SPI Frames with `window-type: dock` only; WMs that draw \
                     their bar without one (i3bar, polybar, swaybar without a \
                     tray bridge) legitimately enumerate nothing"
                        .to_string()
                } else {
                    format!(
                        "census: {} shell surface(s) present: {}",
                        surfaces.len(),
                        surfaces
                            .iter()
                            .map(|s| s.kind.to_snake_case())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            })
            .unwrap_or_else(|e| format!("census unavailable: {e}"));
        format!(
            "computer surface `{}`: {e}; {census}",
            surface_label(surface)
        )
    })
}

/// Resolve a surface kind against the global singleton provider — the
/// production path (same singleton every other tool reaches apps through).
///
/// # Errors
///
/// Same as [`resolve_with`]; the singleton itself failing to construct is
/// reported verbatim (missing macOS Accessibility permission, no AT-SPI2
/// bus on Linux, …).
pub fn resolve(surface: SurfaceKind, timeout: Duration) -> Result<ShellSurface, String> {
    let provider = xa11y::provider().map_err(|e| {
        super::errors::render(
            "computer",
            &format!("initialize the accessibility provider (for surface `{}`)", surface_label(surface)),
            &e,
        )
    })?;
    resolve_with(provider, surface, timeout)
}

/// Wire-facing label of a surface kind (matches the snake_case serde
/// renaming), for error and output messages.
pub fn surface_label(surface: SurfaceKind) -> &'static str {
    match surface {
        SurfaceKind::MenuBar => "menu_bar",
        SurfaceKind::StatusItems => "status_items",
        SurfaceKind::Taskbar => "taskbar",
        SurfaceKind::Panel => "panel",
        SurfaceKind::Dock => "dock",
        SurfaceKind::Desktop => "desktop",
        SurfaceKind::Flyout => "flyout",
        SurfaceKind::Unknown => "unknown",
    }
}
