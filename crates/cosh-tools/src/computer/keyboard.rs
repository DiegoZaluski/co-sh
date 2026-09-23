//! Shared keyboard engine for the pipeline computer tools.
//!
//! `computer_control` and `computer_act` both chain keyboard steps (tap a
//! key, optionally with modifiers, or type literal text) through the same
//! recursive `then`/`wait` skeleton, so the parsing, validation helpers and
//! the dispatch live HERE, parameterized by the calling tool's error prefix.
//! The keystrokes go into whatever element holds keyboard focus NOW —
//! aiming them is the caller's job (a real click in `computer_control`, or
//! a semantic action in `computer_act`).
use xa11y::Key;

/// Maximum duration of a `wait` step, in milliseconds — the pipeline stays
/// one tool call, so a wait is a pause for the app to catch up, not a
/// sleep primitive.
pub const MAX_WAIT_MS: u64 = 10_000;

/// The keyboard fields of ONE pipeline step, tool-agnostic: both
/// `ComputerControl` and `ComputerAct` narrow into this before handing the
/// step to [`run_keyboard_step`].
#[derive(Debug, Clone, Default)]
pub struct KeyboardStep<'a> {
    /// Key to tap (single lowercase character or a named key).
    pub key: Option<&'a str>,
    /// Literal text to type.
    pub text: Option<&'a str>,
    /// Modifier keys held while tapping `key`.
    pub held: Option<&'a [String]>,
}

/// True when the step's keyboard fields make it a KEYBOARD step. Matches
/// the validators exactly (which trim `key` but take `text` as-is), so
/// validation and execution can never disagree about the step's kind.
pub fn is_keyboard_step(key: Option<&str>, text: Option<&str>) -> bool {
    key.is_some_and(|k| !k.trim().is_empty()) || text.is_some_and(|t| !t.is_empty())
}

/// True when the step's `wait` field makes it a WAIT step.
pub fn is_wait_step(wait_ms: Option<u64>) -> bool {
    wait_ms.is_some()
}

/// Execute ONE validated keyboard step and return its human-facing report.
///
/// `tool` prefixes every error (e.g. `computer_control`) so a shared
/// failure reads as coming from the tool the model actually called.
pub fn run_keyboard_step(
    sim: &xa11y::InputSim,
    step: &KeyboardStep,
    tool: &str,
) -> Result<String, String> {
    if let Some(text) = step.text.filter(|t| !t.is_empty()) {
        sim.keyboard()
            .type_text(text)
            .map_err(|e| super::errors::render(tool, "type text", &e))?;
        return Ok(format!("typed {text:?}"));
    }

    let key = step.key.unwrap_or_default().trim();
    let parsed = parse_key(key, tool)?;
    let held = parse_keys(step.held.unwrap_or_default(), tool)?;
    if held.is_empty() {
        sim.keyboard()
            .press(parsed)
            .map_err(|e| super::errors::render(tool, &format!("press {key}"), &e))?;
        Ok(format!("pressed {key}"))
    } else {
        let names: Vec<String> = held.iter().map(key_name).collect();
        sim.keyboard()
            .chord(parsed, &held)
            .map_err(|e| {
                super::errors::render(tool, &format!("chord {key} + {names:?}"), &e)
            })?;
        Ok(format!("pressed {key} with {} held", names.join(",")))
    }
}

/// Pause for a validated `wait` step and return its report fragment.
pub fn run_wait_step(wait_ms: u64) -> String {
    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
    format!("waited {wait_ms} ms")
}

/// Parse a key name into a [`Key`].
///
/// Named keys are matched case-insensitively; a single CHARACTER is taken
/// literally — an uppercase letter is REJECTED (the caller must hold
/// `shift` explicitly), matching the `Key::Char` contract in xa11y.
pub fn parse_key(name: &str, tool: &str) -> Result<Key, String> {
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
                    "{tool}: uppercase `{name}` — pass the lowercase key with `held: [\"shift\"]`"
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
            return Err(format!("{tool}: unknown key `{name}`"));
        }
    };
    Ok(key)
}

/// Parse modifier names into [`Key`]s (shared by click/drag `held` and
/// keyboard chords).
pub fn parse_keys(names: &[String], tool: &str) -> Result<Vec<Key>, String> {
    names
        .iter()
        .map(|n| match n.to_ascii_lowercase().as_str() {
            "shift" => Ok(Key::Shift),
            "ctrl" | "control" => Ok(Key::Ctrl),
            "alt" | "option" => Ok(Key::Alt),
            "meta" | "cmd" | "command" | "super" | "win" => Ok(Key::Meta),
            other => Err(format!(
                "{tool}: unknown modifier `{other}` (expected shift, ctrl, alt or meta)"
            )),
        })
        .collect()
}

/// Human-facing name of a modifier/`Key` (for the `sent` report).
pub fn key_name(key: &Key) -> String {
    match key {
        Key::Shift => "shift".to_string(),
        Key::Ctrl => "ctrl".to_string(),
        Key::Alt => "alt".to_string(),
        Key::Meta => "meta".to_string(),
        Key::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// Handle for the pipeline tools: both share the process-wide input
/// simulator, so a down/up or drag never splits across devices.
pub(crate) fn shared_sim() -> Result<xa11y::InputSim, String> {
    super::shared_input_sim()
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    const TOOL: &str = "computer_control";

    /// Named keys parse case-insensitively, with every documented alias.
    #[test]
    fn named_keys_parse_case_insensitively_with_aliases() {
        let pairs = [
            ("ENTER", Key::Enter),
            ("enter", Key::Enter),
            ("return", Key::Enter),
            ("esc", Key::Escape),
            ("Escape", Key::Escape),
            ("backspace", Key::Backspace),
            ("tab", Key::Tab),
            ("space", Key::Space),
            ("del", Key::Delete),
            ("delete", Key::Delete),
            ("insert", Key::Insert),
            ("up", Key::ArrowUp),
            ("ArrowUp", Key::ArrowUp),
            ("down", Key::ArrowDown),
            ("arrowdown", Key::ArrowDown),
            ("left", Key::ArrowLeft),
            ("arrowleft", Key::ArrowLeft),
            ("right", Key::ArrowRight),
            ("arrowright", Key::ArrowRight),
            ("home", Key::Home),
            ("end", Key::End),
            ("pageup", Key::PageUp),
            ("pagedown", Key::PageDown),
        ];
        for (name, expected) in pairs {
            assert_eq!(parse_key(name, TOOL), Ok(expected), "parsing `{name}`");
        }
    }

    /// A single character parses literally — and an uppercase one is
    /// REJECTED with the shift advice, never silently lowercased (a
    /// silently-lowercased `A` would send the wrong key and report success).
    #[test]
    fn single_character_is_literal_and_uppercase_is_rejected() {
        assert_eq!(parse_key("a", TOOL), Ok(Key::Char('a')));
        assert_eq!(parse_key("5", TOOL), Ok(Key::Char('5')));
        assert_eq!(parse_key("-", TOOL), Ok(Key::Char('-')));
        let err = parse_key("A", TOOL).expect_err("uppercase must be rejected");
        assert!(err.contains("shift"), "must advise `held: [shift]`: {err}");
        assert!(err.starts_with("computer_control: "), "{err}");
        // Multi-character unknown names are unknown keys, not char literals.
        assert!(parse_key("enter two", TOOL).is_err());
        assert!(parse_key("fn", TOOL).is_err());
    }

    /// F-keys: F1 through F12 parse, F0/F13/overflow do not.
    #[test]
    fn function_keys_parse_within_bounds() {
        for n in 1..=12 {
            assert_eq!(parse_key(&format!("f{n}"), TOOL), Ok(Key::F(n)));
            assert_eq!(parse_key(&format!("F{n}"), TOOL), Ok(Key::F(n)));
        }
        for bad in ["f0", "f13", "f999", "fx"] {
            assert!(parse_key(bad, TOOL).is_err(), "`{bad}` must not parse");
        }
    }

    /// Modifier parsing: every documented alias maps to its Key; an unknown
    /// modifier names the accepted set.
    #[test]
    fn modifiers_parse_with_aliases_and_reject_unknowns() {
        let pairs: [(&str, Key); 11] = [
            ("shift", Key::Shift),
            ("SHIFT", Key::Shift),
            ("ctrl", Key::Ctrl),
            ("control", Key::Ctrl),
            ("alt", Key::Alt),
            ("option", Key::Alt),
            ("meta", Key::Meta),
            ("cmd", Key::Meta),
            ("command", Key::Meta),
            ("super", Key::Meta),
            ("win", Key::Meta),
        ];
        for (name, expected) in pairs {
            assert_eq!(parse_keys(&[name.into()], TOOL), Ok(vec![expected]), "{name}");
        }
        let err = parse_keys(&["hyper".into()], TOOL).expect_err("unknown modifier");
        assert!(err.contains("shift, ctrl, alt or meta"), "{err}");
    }

    /// Step classification matches the validators' contract exactly: a
    /// whitespace-only `key` is NOT a keyboard step (the validators trim
    /// `key`); `text` is taken AS-IS (non-empty = a keyboard step, even a
    /// single space — a literal space is a real keystroke); any `wait` is a
    /// wait step.
    #[test]
    fn step_classification_agrees_with_the_validators() {
        assert!(is_keyboard_step(Some("enter"), None));
        assert!(is_keyboard_step(Some("  enter  "), None));
        assert!(!is_keyboard_step(Some(" "), None));
        assert!(!is_keyboard_step(Some(""), None));
        assert!(is_keyboard_step(None, Some("hello")));
        assert!(is_keyboard_step(None, Some(" ")), "a literal space is text");
        assert!(!is_keyboard_step(None, Some("")));
        assert!(!is_keyboard_step(None, None));
        assert!(is_wait_step(Some(0)));
        assert!(is_wait_step(Some(500)));
        assert!(!is_wait_step(None));
    }

    /// `key_name` renders the human-facing names the `sent` report uses —
    /// modifiers spell out, chars pass through, and every other key renders
    /// via Debug with a distinct, non-empty name.
    #[test]
    fn key_name_renders_modifiers_and_chars() {
        assert_eq!(key_name(&Key::Shift), "shift");
        assert_eq!(key_name(&Key::Ctrl), "ctrl");
        assert_eq!(key_name(&Key::Alt), "alt");
        assert_eq!(key_name(&Key::Meta), "meta");
        assert_eq!(key_name(&Key::Char('a')), "a");
        // Non-modifier named keys render via Debug — the exact text is
        // xa11y's choice; the contract is "non-empty and pairwise distinct"
        // across the common named keys (a collision would make the `sent`
        // report indistinguishable between different taps).
        let keys = [Key::Enter, Key::Escape, Key::Tab, Key::ArrowUp, Key::F(1)];
        let names: Vec<String> = keys.iter().map(key_name).collect();
        for name in &names {
            assert!(!name.is_empty(), "named key rendered empty");
        }
        for (i, a) in names.iter().enumerate() {
            for b in names.iter().skip(i + 1) {
                assert_ne!(a, b, "two named keys render identically");
            }
        }
    }

    /// `held` list handling: an EMPTY list is accepted (a plain keypress,
    /// no modifiers) and multiple modifiers keep their given order.
    #[test]
    fn held_modifier_lists_accept_empty_and_preserve_order() {
        assert_eq!(parse_keys(&[], TOOL), Ok(Vec::new()));
        let held = ["shift".to_string(), "ctrl".to_string()];
        let parsed = parse_keys(&held, TOOL).expect("two modifiers parse");
        assert_eq!(parsed, vec![Key::Shift, Key::Ctrl]);
    }
}
