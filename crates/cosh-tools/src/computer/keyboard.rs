//! `computer_keyboard` — synthetic keystrokes into the currently focused element.
//!
//! The keyboard twin of [`super::pointer`]: it types into whatever element
//! holds keyboard focus (focus a target first with `computer_touch` action
//! `focus`). Uses `xa11y::input_sim()`'s keyboard backend; every call is
//! blocking, so it runs on tokio's blocking pool.
use xa11y::{input_sim, Key};

use super::types::{KeyboardOutput, ComputerKeyboard};

/// Maximum number of STEPS a `then` chain may contain (the root step counts).
/// A pathological chain would otherwise let one tool call drive an unbounded
/// sequence of synthetic inputs with no observation point in between; 8 covers
/// every realistic pipeline (type → enter, chord → navigate → submit, …).
pub const KEYBOARD_CHAIN_MAX_DEPTH: usize = 8;

/// Tap a key (optionally with modifiers held) or type literal text.
///
/// Exactly one of `key` or `text` must be provided — per step. A step may
/// chain the NEXT step via `then` (a keyboard pipeline): all steps run
/// sequentially in this single call, each on the element focused at that
/// moment; the FIRST failing step aborts the chain and its error is
/// returned (earlier steps' effects stay applied — they were real input
/// events). `key` accepts a single lowercase character or a named key
/// (`enter`, `escape`, `tab`, `space`, `backspace`, `delete`, `insert`,
/// `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`,
/// `f1`..`f12`); `held` lists modifiers (`shift`, `ctrl`, `alt`, `meta`) to
/// hold while tapping. `text` accepts any literal text — backends
/// synthesize case/shift — but ignores `held`. Chain depth is capped at
/// [`KEYBOARD_CHAIN_MAX_DEPTH`] steps.
///
/// # Errors
///
/// Returns `Err` when a step has neither or both of `key`/`text`, when a
/// step with `text` carries `held`, when a key name is unknown or an
/// uppercase character is passed (hold `shift` instead), when the `then`
/// chain exceeds [`KEYBOARD_CHAIN_MAX_DEPTH`] steps, or when the platform
/// input backend is unavailable.
pub async fn keyboard(input: &ComputerKeyboard) -> Result<KeyboardOutput, String> {
    // Validate the WHOLE chain up front (shape + depth) before sending any
    // synthetic event: a malformed step 3 must not leave steps 1-2 applied
    // with an error that reads like an execution failure.
    validate_chain(input, KEYBOARD_CHAIN_MAX_DEPTH)?;
    // `spawn_blocking` needs 'static — clone the (small) input struct in.
    let owned = input.clone();
    tokio::task::spawn_blocking(move || keyboard_blocking(&owned))
        .await
        .map_err(|e| format!("computer_keyboard: blocking task failed: {e}"))?
}

/// Validate every step of a `then` chain: exactly one of `key`/`text` per
/// step, `held` only with `key`, every key/modifier name resolvable, and
/// total depth within `remaining` steps. Key-name parsing runs HERE (not
/// just in `run_step`) so an unknown `key` or modifier anywhere in the
/// chain is rejected before a single synthetic event is sent.
fn validate_chain(step: &ComputerKeyboard, remaining: usize) -> Result<(), String> {
    if remaining == 0 {
        return Err(format!(
            "computer_keyboard: `then` chain exceeds {KEYBOARD_CHAIN_MAX_DEPTH} steps"
        ));
    }
    let key = step.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = step.text.as_deref().filter(|t| !t.is_empty());
    if step.key.is_some() && step.text.is_some() {
        return Err("computer_keyboard: provide `key` or `text`, not both".into());
    }
    match (key, text) {
        (None, None) => return Err("computer_keyboard: provide `key` or `text`".into()),
        (None, Some(_)) if step.held.is_some() => {
            return Err(
                "computer_keyboard: `held` only applies with `key` (text handles case itself)"
                    .into(),
            );
        }
        _ => {}
    }
    if let Some(key) = key {
        parse_key(key)?;
    }
    if let Some(held) = &step.held {
        parse_keys(&held.iter().map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>())?;
    }
    if let Some(next) = &step.then {
        validate_chain(next, remaining - 1)?;
    }
    Ok(())
}

fn keyboard_blocking(input: &ComputerKeyboard) -> Result<KeyboardOutput, String> {
    let sim = input_sim().map_err(|e| format!("computer_keyboard: input backend: {e}"))?;
    // Walk the chain iteratively, running one step at a time and joining the
    // step reports with " → " so the model sees exactly what was executed.
    // Validated up front (see `validate_chain`), so every step here is
    // well-formed; a BACKEND failure mid-chain aborts with the error of the
    // failing step (earlier steps stay applied — they were real events).
    let mut report = String::new();
    let mut step = input;
    loop {
        let sent = run_step(&sim, step)?;
        if !report.is_empty() {
            report.push_str(" → ");
        }
        report.push_str(&sent);
        match &step.then {
            Some(next) => step = next,
            None => return Ok(KeyboardOutput { sent: report }),
        }
    }
}

/// Execute ONE validated step (exactly one of `key`/`text`) and return its
/// human-facing report.
fn run_step(sim: &xa11y::InputSim, step: &ComputerKeyboard) -> Result<String, String> {
    let key = step.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = step.text.as_deref().filter(|t| !t.is_empty());

    if let Some(text) = text {
        sim.keyboard()
            .type_text(text)
            .map_err(|e| format!("computer_keyboard: type_text: {e}"))?;
        return Ok(format!("typed {text:?}"));
    }

    let key = key.unwrap_or_default();
    let parsed = parse_key(key)?;
    let held_names: Vec<String> = step
        .held
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let held = parse_keys(&held_names)?;
    if held.is_empty() {
        sim.keyboard()
            .press(parsed)
            .map_err(|e| format!("computer_keyboard: press {key}: {e}"))?;
        Ok(format!("pressed {key}"))
    } else {
        let names: Vec<String> = held.iter().map(key_name).collect();
        sim.keyboard()
            .chord(parsed, &held)
            .map_err(|e| format!("computer_keyboard: chord {key} + {names:?}: {e}"))?;
        Ok(format!("pressed {key} with {} held", names.join(",")))
    }
}

/// Parse a key name into a [`Key`].
///
/// Named keys are matched case-insensitively; a single CHARACTER is taken
/// literally — an uppercase letter is REJECTED (the caller must hold
/// `shift` explicitly), matching the `Key::Char` contract in xa11y.
fn parse_key(name: &str) -> Result<Key, String> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "delete" | "del" => Key::Delete,
        "insert" => Key::Insert,
        "up" | "arrowup" => Key::ArrowUp,
        "down" | "arrowdown" => Key::ArrowDown,
        "left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrowright" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        _ if name.chars().count() == 1 => {
            // Validate the ORIGINAL character, not the lowercased one — a
            // silently-lowercased uppercase letter would send the wrong key
            // and report success.
            let ch = name.chars().next().unwrap_or_default();
            if ch.is_uppercase() {
                return Err(format!(
                    "computer_keyboard: uppercase `{name}` — pass the lowercase key with `held: [\"shift\"]`"
                ));
            }
            Key::Char(ch)
        }
        _ if lower.starts_with('f')
            && lower.len() <= 3
            && lower[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            Key::F(lower[1..].parse::<u8>().unwrap_or(1))
        }
        _ => {
            return Err(format!("computer_keyboard: unknown key `{name}`"));
        }
    };
    Ok(key)
}

/// Parse a list of modifier names into [`Key`]s.
fn parse_keys(names: &[String]) -> Result<Vec<Key>, String> {
    names
        .iter()
        .map(|n| match n.as_str() {
            "shift" => Ok(Key::Shift),
            "ctrl" | "control" => Ok(Key::Ctrl),
            "alt" | "option" => Ok(Key::Alt),
            "meta" | "cmd" | "command" | "super" | "win" => Ok(Key::Meta),
            other => Err(format!(
                "computer_keyboard: unknown modifier `{other}` (expected shift, ctrl, alt or meta)"
            )),
        })
        .collect()
}

/// Human-facing name of a modifier/`Key` (for the `sent` report).
fn key_name(key: &Key) -> String {
    match key {
        Key::Shift => "shift".to_string(),
        Key::Ctrl => "ctrl".to_string(),
        Key::Alt => "alt".to_string(),
        Key::Meta => "meta".to_string(),
        Key::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod chain_validation_tests {
    use super::{validate_chain, KEYBOARD_CHAIN_MAX_DEPTH};
    use crate::computer::types::ComputerKeyboard;

    fn key_step(key: &str) -> ComputerKeyboard {
        ComputerKeyboard {
            key: Some(key.to_string()),
            ..ComputerKeyboard::default()
        }
    }

    fn text_step(text: &str) -> ComputerKeyboard {
        ComputerKeyboard {
            text: Some(text.to_string()),
            ..ComputerKeyboard::default()
        }
    }

    /// A single valid step (each of the two modes) passes validation.
    #[test]
    fn single_step_passes() {
        assert!(validate_chain(&key_step("enter"), KEYBOARD_CHAIN_MAX_DEPTH).is_ok());
        assert!(validate_chain(&text_step("olá"), KEYBOARD_CHAIN_MAX_DEPTH).is_ok());
    }

    /// A two-step pipeline (the canonical type-then-enter case) passes.
    #[test]
    fn two_step_chain_passes() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(key_step("enter"))),
            ..text_step("olá mundo")
        };
        assert!(validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).is_ok());
    }

    /// Both `key` and `text` on ONE step is rejected — including when the
    /// violation is nested deep in the chain.
    #[test]
    fn key_and_text_together_rejected_at_depth() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(ComputerKeyboard {
                then: Some(Box::new(ComputerKeyboard {
                    key: Some("a".into()),
                    text: Some("b".into()),
                    ..ComputerKeyboard::default()
                })),
                ..key_step("tab")
            })),
            ..text_step("start")
        };
        let err = validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    /// A step with NEITHER `key` nor `text` is rejected (empty then-step).
    #[test]
    fn empty_step_rejected() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(ComputerKeyboard::default())),
            ..text_step("olá")
        };
        let err = validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("provide `key` or `text`"), "err: {err}");
    }

    /// `held` with `text` is rejected at ANY chain depth.
    #[test]
    fn held_with_text_rejected_in_then_step() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(ComputerKeyboard {
                text: Some("x".into()),
                held: Some(vec!["shift".into()]),
                ..ComputerKeyboard::default()
            })),
            ..text_step("olá")
        };
        let err = validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("`held` only applies with `key`"), "err: {err}");
    }

    /// An UNKNOWN key name is rejected by the preflight — even when it sits
    /// in a `then` step AFTER a valid text step: no synthetic event may be
    /// sent from a chain that will fail (the doc promises validation before
    /// the first event).
    #[test]
    fn unknown_key_in_then_step_rejected_before_any_event() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(key_step("INVALID"))),
            ..text_step("hello")
        };
        let err = validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("unknown key `INVALID`"), "err: {err}");
    }

    /// An unknown MODIFIER name is likewise rejected by the preflight.
    #[test]
    fn unknown_modifier_in_then_step_rejected() {
        let chain = ComputerKeyboard {
            then: Some(Box::new(ComputerKeyboard {
                key: Some("a".into()),
                held: Some(vec!["hyper".into()]),
                ..ComputerKeyboard::default()
            })),
            ..text_step("hello")
        };
        let err = validate_chain(&chain, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("unknown modifier `hyper`"), "err: {err}");
    }

    /// A chain of exactly MAX_DEPTH steps is accepted (boundary: not off-by-one).
    #[test]
    fn chain_at_max_depth_is_accepted() {
        // Build a chain of exactly KEYBOARD_CHAIN_MAX_DEPTH key steps.
        let mut root = key_step("a");
        for _ in 1..KEYBOARD_CHAIN_MAX_DEPTH {
            root = ComputerKeyboard {
                then: Some(Box::new(root)),
                ..key_step("b")
            };
        }
        let depth_ok = KEYBOARD_CHAIN_MAX_DEPTH;
        assert!(validate_chain(&root, depth_ok).is_ok());
    }

    /// One MORE step than the cap is rejected, naming the cap.
    #[test]
    fn chain_over_max_depth_is_rejected() {
        // KEYBOARD_CHAIN_MAX_DEPTH + 1 steps.
        let mut root = key_step("a");
        for _ in 0..KEYBOARD_CHAIN_MAX_DEPTH {
            root = ComputerKeyboard {
                then: Some(Box::new(root)),
                ..key_step("b")
            };
        }
        let err = validate_chain(&root, KEYBOARD_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(
            err.contains(&KEYBOARD_CHAIN_MAX_DEPTH.to_string()),
            "error must name the cap: {err}"
        );
    }
}
