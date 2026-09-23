//! Demonstrate `computer_act` — the SEMANTIC action pipeline: instead of
//! synthesizing clicks, it invokes the element's accessibility action layer
//! (press, focus, toggle, expand, set_value, select_text, …). The target is
//! an element matched by `selector` under an app scope (`name`/`pid`) or a
//! shell `surface`.
//!
//! Choose `computer_act` over `computer_control` when the intent is
//! semantic ("check this checkbox", "set this field") and the control
//! exposes an accessibility action — it auto-waits for actionability and
//! never depends on pointer position. Reach for `computer_control` when
//! you need a REAL OS click (moving keyboard focus between fields) or raw
//! coordinate work.
//!
//! What runs offline here: step construction and the tool's own semantic
//! validation (`touch::validate` — touch.rs IS the act engine). Dispatch
//! needs a desktop session.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-act
//! ```

use cosh_tools::computer::touch::validate;
use cosh_tools::computer::types::{ActAction, ComputerAct, SurfaceKind};

fn main() {
    // ── One semantic step: press the Export button in an app ──
    let export = ComputerAct {
        name: Some("Reports".into()),
        selector: Some("button[name='Export']".into()),
        action: Some(ActAction::Press), // the default
        // Actions AUTO-WAIT for the app AND a visible+enabled match —
        // unlike snapshot/screenshot, which capture immediately.
        timeout_ms: Some(5000),
        ..Default::default()
    };
    assert!(validate(&export, "computer_act").is_ok());

    // Chain them (shell && semantics): each step runs only if the previous
    // succeeded.
    let fill_and_export = ComputerAct {
        name: Some("Reports".into()),
        selector: Some("text_field[name='Filename']".into()),
        action: Some(ActAction::SetValue),
        value: Some("2026-q3".into()),
        ..Default::default()
    }
    .chain(ComputerAct {
        wait: Some(400), // standalone wait step: no other fields
        ..Default::default()
    })
    .chain(ComputerAct {
        name: Some("Reports".into()),
        selector: Some("button[name='Export']".into()),
        action: Some(ActAction::Press),
        ..Default::default()
    });
    assert!(validate(&fill_and_export, "computer_act").is_ok());
    println!("semantic pipeline validated (3 steps)");

    // ── Keyboard steps in an act chain ──
    //
    // `key` + `held` = a chord (ctrl+a selects all); `text` types literally
    // and REJECTS `held` (text handles case itself). Uppercase intent is
    // spelled `key: "a", held: ["shift"]`, never `key: "A"`.
    //
    // NOTE the validator split: `touch::validate` validates SEMANTIC steps
    // (they carry a selector); keyboard/wait steps are validated by the
    // shared keyboard engine when the chain walks. So a keyboard step fed
    // to the semantic validator is itself an error — with guidance naming
    // the right engine:
    let select_all = ComputerAct {
        key: Some("a".into()),
        held: Some(vec!["ctrl".into()]),
        ..Default::default()
    };
    if let Err(e) = validate(&select_all, "computer_act") {
        println!("keyboard step under the semantic validator: {e}");
    }

    // ── Validation errors, offline ──
    //
    // A semantic step MUST have a selector and exactly one app scope:
    let no_selector = ComputerAct {
        action: Some(ActAction::Press),
        name: Some("Reports".into()),
        ..Default::default()
    };
    if let Err(e) = validate(&no_selector, "computer_act") {
        println!("missing selector rejected: {e}");
    }

    // Payload-carrying actions require their payload:
    let set_value_without_value = ComputerAct {
        name: Some("Reports".into()),
        selector: Some("text_field[name='Filename']".into()),
        action: Some(ActAction::SetNumericValue),
        ..Default::default()
    };
    if let Err(e) = validate(&set_value_without_value, "computer_act") {
        println!("missing payload rejected: {e}");
    }

    // ── Shell-surface targeting ──
    //
    // `surface` replaces `name`/`pid` — the target is part of the DESKTOP,
    // not any app (the menu bar, the taskbar, an open flyout). See the
    // computer-surface example for the full tour.
    let press_taskbar_item = ComputerAct {
        surface: Some(SurfaceKind::Taskbar),
        selector: Some("button[name='Files']".into()),
        ..Default::default()
    };
    assert!(validate(&press_taskbar_item, "computer_act").is_ok());

    // Full dispatch (needs a desktop session):
    // computer::act(&fill_and_export).await
}

/// Append `next` to the END of the chain. A local extension trait —
/// `ComputerAct` comes from another crate, so an inherent `impl` there
/// would be illegal (orphan rule).
trait Chain: Sized {
    fn chain(self, next: ComputerAct) -> Self;
}

impl Chain for ComputerAct {
    fn chain(mut self, next: ComputerAct) -> Self {
        let mut tail = &mut self;
        while tail.then.is_some() {
            tail = tail.then.as_deref_mut().unwrap();
        }
        tail.then = Some(Box::new(next));
        self
    }
}
