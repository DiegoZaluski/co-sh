//! Demonstrate `computer_apps` — the enumeration tool: which applications
//! are running (with PIDs and the foreground flag), or which element holds
//! keyboard focus right now (`target: "focused"` — the cheap "where do my
//! keystrokes go?" check before typing).
//!
//! The `focused` query is the ground truth the input tools previously
//! guessed at: a chain that needs typing can CHECK the focused element
//! instead of assuming the last click landed.
//!
//! The apps tool talks to the live desktop, so full dispatch needs a
//! session. What runs offline here: input construction and the focus-walk
//! PATTERN against xa11y's mock provider — the same Locator-driven walk
//! the tool performs on a real tree.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-apps
//! ```

use std::sync::Arc;
use std::time::Duration;

use cosh_tools::computer::types::{AppsTarget, ComputerApps};
use xa11y::{App, Element, Locator, Provider, mock};

fn main() {
    // ── Input shapes (serde wire spelling in comments) ──
    //
    // {}                        → target "all" (default): every running app
    // {"target": "all"}         → same
    // {"target": "focused"}     → foreground app + its keyboard-focused
    //                             element
    // {"target": "focused",
    //  "timeout_ms": 5000}      → wait up to 5 s for a foreground app
    let all = ComputerApps::default();
    let focused = ComputerApps {
        target: Some(AppsTarget::Focused),
        timeout_ms: Some(5000),
    };
    let _ = (all, focused);

    // Dispatch (needs a desktop session). The result is the untagged enum
    // AppsResult — the variant follows the query:
    //
    //     let out = computer::apps(&all).await?;
    //     // AppsResult::All(AppsOutput { apps: Vec<AppInfo>, count })
    //     //   AppInfo { name, pid, foreground }
    //     for app in &out.apps { … }
    //
    //     let out = computer::apps(&focused).await?;
    //     // AppsResult::Focused(FocusedOutput { app (name), pid,
    //     //   focused_element: Option<FocusedElement> { role, name, … } })
    //     // focused_element is None when focus sits on the window itself.

    // ── The focus-walk pattern, offline against the mock provider ──
    //
    // The tool resolves the foreground application, then walks DOWN its
    // tree following each element's focused child until it reaches the
    // leaf that actually holds keyboard focus. The same walk works on any
    // provider — here, xa11y's mock (dev-dependency feature
    // `test-support`), which stands in for a real desktop:
    let provider: Arc<dyn Provider> = mock::build_provider();

    // Resolve the app the same way the tool does — through the INJECTED
    // provider (`by_pid_with`; the bare `App::by_pid` would use the global
    // real-desktop provider). The mock registers ONE application —
    // "TestApp" — at pid 1234; `MOCK_SHELL_PID` (4242) belongs to the
    // shell's taskbar surface, deliberately distinct:
    let app = App::by_pid_with(mock::build_provider(), 1234, Duration::from_secs(3))
        .expect("the mock provider registers TestApp at pid 1234");

    // A root locator scoped to the app resolves elements from the tree —
    // the scope is the app's element DATA (App exposes it publicly):
    let button = Locator::new(provider, Some(app.data.clone()), "button")
        .wait_visible(Duration::from_secs(1))
        .expect("the mock shell carries buttons");
    println!("resolved element: {}", element_summary(&button));

    // Full dispatch (needs a desktop session):
    // computer::apps(&focused).await
}

/// One-line summary of a resolved element (role + name), the vocabulary
/// selectors are written against.
fn element_summary(element: &Element) -> String {
    let data = element.data();
    match data.name.as_deref() {
        Some(name) => format!("{:?} '{name}'", data.role),
        None => format!("{:?}", data.role),
    }
}
