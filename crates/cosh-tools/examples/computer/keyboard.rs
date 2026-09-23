//! Demonstrate `keyboard` — the typing engine shared by `computer_control`
//! and `computer_act`: key NAMES parse case-insensitively (with aliases),
//! single characters are taken literally, UPPERCASE characters are
//! REJECTED (hold `shift` explicitly), and modifiers come from a small
//! alias table (`ctrl`/`control`, `alt`/`option`, `meta`/`cmd`/`command`/
//! `super`/`win`).
//!
//! Key design rule: an uppercase letter never silently lowercases. A
//! silently-lowercased `A` would send the wrong key AND report success —
//! the tool refuses it and points at `held: ["shift"]` instead. `text`
//! handles case itself (it types literally), which is why it rejects
//! `held`.
//!
//! Everything here runs fully offline — the parsers are pure functions.
//! The simulated keystrokes need a desktop session (and land in whatever
//! element holds keyboard focus NOW — aiming them is a click's job, see
//! computer-control).
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-keyboard
//! ```

use cosh_tools::computer::keyboard::{is_keyboard_step, key_name, parse_key, parse_keys};
use cosh_tools::computer::types::ComputerControl;
use xa11y::Key;

fn main() {
    const TOOL: &str = "computer_control";

    // ── Named keys: case-insensitive, with aliases ──
    //
    // enter|return, escape|esc, delete|del, up|arrowup, down|arrowdown,
    // left|arrowleft, right|arrowright, pageup, pagedown, home, end, tab,
    // space, backspace, insert, f1..f12.
    for name in ["enter", "RETURN", "esc", "Escape", "arrowup", "f12"] {
        println!("{name:8} → {:?}", parse_key(name, TOOL).expect("valid"));
    }

    // ── Single characters: literal, lowercase only ──
    assert_eq!(parse_key("a", TOOL), Ok(Key::Char('a')));
    assert_eq!(parse_key("5", TOOL), Ok(Key::Char('5')));
    let err = parse_key("A", TOOL).expect_err("uppercase is rejected");
    println!("uppercase: {err}");

    // ── Modifiers: aliases collapse onto four keys ──
    //
    // shift | ctrl|control | alt|option | meta|cmd|command|super|win.
    let held = parse_keys(
        &["ctrl".into(), "CONTROL".into(), "win".into()],
        TOOL,
    )
    .expect("all aliases resolve");
    println!(
        "ctrl + CONTROL + win → {:?}",
        held.iter().map(key_name).collect::<Vec<_>>()
    );

    // Unknown modifiers name the accepted set:
    if let Err(e) = parse_keys(&["hyper".into()], TOOL) {
        println!("unknown modifier: {e}");
    }

    // ── Step classification: what makes a step a KEYBOARD step? ──
    //
    // `key` counts after TRIM (a whitespace-only key is not a step);
    // `text` counts AS-IS (a literal space is real text — a spacebar
    // keystroke). The validators and the dispatcher share this exact
    // helper, so they can never disagree about a step's kind.
    assert!(is_keyboard_step(Some("  enter  ".trim()), None));
    assert!(is_keyboard_step(None, Some(" ")), "a literal space IS text");
    assert!(!is_keyboard_step(Some(" "), None));
    assert!(!is_keyboard_step(None, None));

    // ── Building a chord step on the wire ──
    //
    // key `a` + held `ctrl` = select-all, on the wire:
    let chord_wire = r#"{"key": "a", "held": ["ctrl"]}"#;
    let select_all: ComputerControl =
        serde_json::from_str(chord_wire).expect("valid wire shape");
    println!("chord wire: {chord_wire} → {select_all:?}");

    // Uppercase INTENT is spelled with an explicit shift hold — the only
    // way to type a capital `A` as a keystroke (or use `text` instead):
    let capital_wire = r#"{"key": "a", "held": ["shift"]}"#;
    let capital: ComputerControl =
        serde_json::from_str(capital_wire).expect("valid wire shape");
    println!("shift+`a` wire: {capital_wire} → {capital:?}");

    // Dispatch (needs a desktop session; typing lands in the FOCUSED
    // element — click it first, see computer-control):
    // computer::control(&select_all).await
}
