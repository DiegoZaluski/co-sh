//! Python runtime parity helpers: `repr()`, `str()`, `%g` formatting,
//! `json.dumps(..., ensure_ascii=False)` with the default separators, and
//! `round()` — the primitive spellings the prompt surface depends on.
//!
//! Every prompt-rendering decision model ported from Python renders through
//! these, so the strings a tokenizer sees match the upstream prompts byte
//! for byte. Models with native (non-ported) prompts have no reason to
//! touch this module.
//!
//! Two Python behaviours are reproduced deliberately:
//!
//! * `json.dumps(..., ensure_ascii=False)` renders with `", "` / `": "`
//!   separators (`py_json_dumps`).
//! * `str(value)` on scalar labels renders Python spellings (`None`, `True`)
//!   (`py_str`). Map keys arrive as strings over the JSON surface either
//!   backend uses.

use serde_json::Value;

/// Python `repr()` of a string for the `%r` message slots: single quotes
/// unless the string carries a single quote and no double quote, in which
/// case Python switches to double quotes.
pub fn py_repr_str(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{}\"", s)
    } else {
        format!("'{}'", s)
    }
}

/// Python `repr()` of a JSON scalar where a message renders one with `%r`:
/// strings gain the quoting above, `None` spells `None`, `True`/`False`
/// keep their Python spellings, lists render as `[a, b]`, and a dict
/// renders as a Python dict literal (`{'k': 'v'}`).
pub fn py_repr_value(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr_value).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_value(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python `%g` formatting (default precision 6): fixed notation with up to 6
/// significant digits and trailing zeros stripped inside
/// `1e-4 <= |x| < 1e6`, exponential form (`d.ddddde±NN`) outside it.
pub fn py_g(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".to_string()
        } else if x > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".to_string() } else { "0".to_string() };
    }
    // Exponential form: mantissa with (precision-1) decimals, trailing zeros
    // stripped, exponent signed and at least two digits.
    let exponential = |x: f64| -> String {
        let raw = format!("{:.5e}", x); // e.g. "1.23457e6" / "-1.2345e-5"
        let (mantissa, exponent) = raw.split_once('e').unwrap_or((raw.as_str(), "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = exponent.parse().unwrap_or(0);
        format!("{}e{}{:02}", mantissa, if e < 0 { '-' } else { '+' }, e.abs())
    };
    // The decimal exponent of the leading digit. `{:e}` gives the exact
    // rounded exponent, so a value sitting just below a power of ten
    // (`999999.99...`) classifies like Python does.
    let exp: i32 = format!("{:e}", x.abs())
        .split('e')
        .nth(1)
        .and_then(|e| e.parse().ok())
        .unwrap_or(0);
    if !(-4..6).contains(&exp) {
        return exponential(x);
    }
    // Fixed: 6 significant digits, trailing zeros stripped.
    let decimals = (6 - 1 - exp).max(0) as usize;
    let s = format!("{:.*}", decimals, x);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    // Rounding can carry into a new leading digit; Python re-classifies such
    // a value by its rounded exponent, so re-derive it from the formatted
    // number and fall back to exponential when it left the fixed range.
    let carried: i32 = format!("{:e}", s.parse::<f64>().unwrap_or(x))
        .split('e')
        .nth(1)
        .and_then(|e| e.parse().ok())
        .unwrap_or(exp);
    if (-4..6).contains(&carried) {
        s
    } else {
        exponential(x)
    }
}

/// `json.dumps(value, ensure_ascii=False)` with the default separators
/// `(", ", ": ")`: a space after every comma and colon, non-ASCII kept raw.
///
/// Parity note: byte-exact with `json.dumps` for the magnitudes the prompt
/// surface carries (criterion descriptions and state text are short scalars).
/// Extreme floats differ in exponent spelling (`1e+16` vs `1e16`), and
/// serde_json has no NaN/Infinity to begin with — unreachable here.
pub fn py_json_dumps(value: &Value) -> String {
    fn dump(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => dump_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    dump(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    dump_string(k, out);
                    out.push_str(": ");
                    dump(v, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    dump(value, &mut out);
    out
}

/// JSON string escaping with `ensure_ascii=False`: only `"`, `\`, and the
/// C0 controls are escaped; every other character is emitted as itself.
fn dump_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python `str(value)` for the scalar labels that reach option rendering.
///
/// Used where upstream stringifies non-string scalars: list-valued `criteria`
/// keys and schema enum values. JSON object keys are always strings, so
/// map-keyed criteria need no conversion.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => py_json_dumps(other),
    }
}

/// Python `round(value, ndigits=0)`: round half to even ("banker's rounding"),
/// which Rust's `f64::round` (half away from zero) does not reproduce. Used
/// where upstream rounds a float before casting to int.
pub fn py_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    let diff = value - floor;
    // Exactly .5 away from an integer rounds toward the even neighbour.
    if diff == 0.5 {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else if diff == -0.5 {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor - 1.0
        }
    } else {
        value.round()
    }
}

/// Python `float(value)` over the JSON surface: numbers pass through, bools
/// are 1.0/0.0 (`bool` is an `int` in Python), numeric strings parse, and
/// `None` or a non-numeric string raise — reported here as `None`.
pub fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            // Python float() strips whitespace and accepts inf/nan spellings.
            let trimmed = s.trim();
            match trimmed.to_ascii_lowercase().as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                "nan" => Some(f64::NAN),
                _ => trimmed.parse::<f64>().ok(),
            }
        }
        _ => None,
    }
}

/// Python `round(x, 4)`.
///
/// CPython's two-argument `round` is correctly-rounded decimal over the
/// binary value, which is exactly what Rust's `{:.4}` formatting implements
/// (round-half-even on the decimal expansion, agreeing with Python on the
/// observed tie cases like `0.03125 -> 0.0312`); the result is parsed back so
/// callers see a number, not a string. Verified against CPython on ties,
/// magnitudes and negative values; the earlier scale-round-scale form
/// diverged on halfway values such as `0.16665` (Python `0.1666`).
pub fn round4(x: f64) -> f64 {
    format!("{:.4}", x).parse().unwrap_or(x)
}

#[cfg(test)]
mod tests;
