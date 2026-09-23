//! Tests for the input engines: pointer validation and the keyboard parse/parse/classify tables.
#[cfg(test)]
mod pointer_validation_tests {
    use crate::computer::mouse::{MIN_DRAG_MS, validate};
    use crate::computer::types::{ComputerControl, PointerAction, PointerAnchor};

    const TOOL: &str = "computer_control";

    fn step() -> ComputerControl {
        ComputerControl::default()
    }

    #[test]
    fn element_and_coordinate_steps_pass() {
        let element = ComputerControl {
            app: Some("Safari".into()),
            selector: Some("button[name='OK']".into()),
            ..step()
        };
        let coordinate = ComputerControl {
            x: Some(10),
            y: Some(20),
            ..step()
        };
        assert!(validate(&element, TOOL).is_ok());
        assert!(validate(&coordinate, TOOL).is_ok());
    }

    #[test]
    fn scroll_requires_delta() {
        let s = ComputerControl {
            action: Some(PointerAction::Scroll),
            x: Some(1),
            y: Some(1),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`dx` and/or `dy`"), "err: {err}");
    }

    #[test]
    fn drag_mixed_endpoints_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Drag),
            x: Some(1),
            y: Some(1),
            x2: Some(2),
            y2: Some(2),
            to_selector: Some("other".into()),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn nth_zero_rejected() {
        let s = ComputerControl {
            app: Some("Safari".into()),
            selector: Some("button".into()),
            nth: Some(0),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");
    }

    #[test]
    fn count_zero_rejected() {
        let s = ComputerControl {
            x: Some(1),
            y: Some(1),
            count: Some(0),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");
    }

    #[test]
    fn click_without_target_rejected() {
        let err = validate(&step(), TOOL).unwrap_err();
        assert!(err.contains("requires a target"), "err: {err}");
    }

    #[test]
    fn anchor_without_selector_rejected() {
        let s = ComputerControl {
            x: Some(1),
            y: Some(1),
            anchor: Some(PointerAnchor::TopLeft),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`anchor`"), "err: {err}");
    }

    #[test]
    fn move_with_count_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Move),
            x: Some(1),
            y: Some(1),
            count: Some(2),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`count` applies to click"), "err: {err}");
    }

    #[test]
    fn short_drag_duration_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Drag),
            x: Some(1),
            y: Some(1),
            x2: Some(2),
            y2: Some(2),
            duration_ms: Some(MIN_DRAG_MS - 1),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("duration_ms"), "err: {err}");
    }
}

#[cfg(test)]
mod keyboard_tests {
    use xa11y::Key;

    use crate::computer::keyboard::{
        is_keyboard_step, is_wait_step, key_name, parse_key, parse_keys,
    };

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
            assert_eq!(
                parse_keys(&[name.into()], TOOL),
                Ok(vec![expected]),
                "{name}"
            );
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
