//! `computer_keyboard` — synthetic keystrokes into the currently focused element.
//!
//! The keyboard twin of [`super::pointer`]: it types into whatever element
//! holds keyboard focus (focus a target first with `computer_touch` action
//! `focus`). Uses `xa11y::input_sim()`'s keyboard backend; every call is
//! blocking, so it runs on tokio's blocking pool.
use xa11y::{input_sim, Key};

use super::types::{KeyboardOutput, ComputerKeyboard};

/// Tap a key (optionally with modifiers held) or type literal text.
///
/// Exactly one of `key` or `text` must be provided. `key` accepts a single
/// lowercase character or a named key (`enter`, `escape`, `tab`, `space`,
/// `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`, `home`,
/// `end`, `pageup`, `pagedown`, `f1`..`f12`); `held` lists modifiers
/// (`shift`, `ctrl`, `alt`, `meta`) to hold while tapping. `text` accepts
/// any literal text — backends synthesize case/shift — but ignores `held`.
///
/// # Errors
///
/// Returns `Err` when neither or both of `key`/`text` are given, when a key
/// name is unknown or an uppercase character is passed (hold `shift`
/// instead), or when the platform input backend is unavailable.
pub async fn keyboard(input: &ComputerKeyboard) -> Result<KeyboardOutput, String> {
    let key = input.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = input.text.as_deref().filter(|t| !t.is_empty());
    // Presence check FIRST (before trimming/empty-filtering): two fields
    // supplied together is caller error even if one is blank.
    if input.key.is_some() && input.text.is_some() {
        return Err("computer_keyboard: provide `key` or `text`, not both".into());
    }
    match (key, text) {
        (None, None) => return Err("computer_keyboard: provide `key` or `text`".into()),
        (None, Some(_)) if input.held.is_some() => {
            return Err(
                "computer_keyboard: `held` only applies with `key` (text handles case itself)".into(),
            );
        }
        _ => {}
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || keyboard_blocking(&input))
        .await
        .map_err(|e| format!("computer_keyboard: blocking task failed: {e}"))?
}

fn keyboard_blocking(input: &ComputerKeyboard) -> Result<KeyboardOutput, String> {
    let sim = input_sim().map_err(|e| format!("computer_keyboard: input backend: {e}"))?;
    let key = input.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = input.text.as_deref().filter(|t| !t.is_empty());

    if let Some(text) = text {
        sim.keyboard()
            .type_text(text)
            .map_err(|e| format!("computer_keyboard: type_text: {e}"))?;
        return Ok(KeyboardOutput {
            sent: format!("typed {text:?}"),
        });
    }

    let key = key.unwrap_or_default();
    let parsed = parse_key(key)?;
    let held_names: Vec<String> = input
        .held
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let held = parse_keys(&held_names)?;
    let sent = if held.is_empty() {
        sim.keyboard()
            .press(parsed)
            .map_err(|e| format!("computer_keyboard: press {key}: {e}"))?;
        format!("pressed {key}")
    } else {
        let names: Vec<String> = held.iter().map(key_name).collect();
        sim.keyboard()
            .chord(parsed, &held)
            .map_err(|e| format!("computer_keyboard: chord {key} + {names:?}: {e}"))?;
        format!("pressed {key} with {} held", names.join(","))
    };

    Ok(KeyboardOutput { sent })
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
