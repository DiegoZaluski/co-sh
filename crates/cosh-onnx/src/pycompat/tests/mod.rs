//! Ported tests: the parity helpers of `tests/test_confidence.py` and the
//! pure parts of `tests/test_criteria.py` from the laya repository, with the
//! same cases and the same expected values.

use serde_json::json;

use crate::pycompat::{py_g, py_json_dumps, py_repr_value, py_round, py_str, round4};

// --------------------------------------------------------------- py_g (%g formatting)
#[test]
fn py_g_matches_python_g_formatting() {
    // Values verified against CPython `"%g" % x` (precision 6).
    assert_eq!(py_g(0.5), "0.5");
    assert_eq!(py_g(5.0), "5");
    assert_eq!(py_g(1234567.0), "1.23457e+06");
    assert_eq!(py_g(123456.0), "123456");
    assert_eq!(py_g(0.0001), "0.0001");
    assert_eq!(py_g(1.2345e-5), "1.2345e-05");
}

// --------------------------------------------------------------- py_repr_value
#[test]
fn py_repr_value_renders_python_scalars() {
    assert_eq!(py_repr_value(&json!(null)), "None");
    assert_eq!(py_repr_value(&json!(true)), "True");
    assert_eq!(py_repr_value(&json!(false)), "False");
    assert_eq!(py_repr_value(&json!("x")), "'x'");
    assert_eq!(py_repr_value(&json!(3)), "3");
    assert_eq!(py_repr_value(&json!(2.5)), "2.5");
    assert_eq!(py_repr_value(&json!([1, "a"])), "[1, 'a']");
    assert_eq!(py_repr_value(&json!({"k": 1})), "{'k': 1}");
}

// --------------------------------------------------------------- round4
#[test]
fn round4_matches_python_round_4() {
    // Values verified against CPython `round(x, 4)` — including the exact
    // dyadic ties where Python's banker's rounding is observable.
    assert_eq!(round4(0.123456), 0.1235);
    assert_eq!(round4(0.5), 0.5);
    assert_eq!(round4(0.16665), 0.1666);
    assert_eq!(round4(0.03125), 0.0312);
    assert_eq!(round4(0.09375), 0.0938);
    assert_eq!(round4(0.15625), 0.1562);
    assert_eq!(round4(0.21875), 0.2188);
    assert_eq!(round4(0.28125), 0.2812);
    assert_eq!(round4(0.12345), 0.1235);
    assert_eq!(round4(0.00005), 0.0001);
    assert_eq!(round4(-1.23456), -1.2346);
}

// --------------------------------------------------------------- str / dumps parity helpers
#[test]
fn py_str_matches_python_spellings() {
    assert_eq!(py_str(&serde_json::Value::Null), "None");
    assert_eq!(py_str(&json!(true)), "True");
    assert_eq!(py_str(&json!(false)), "False");
    assert_eq!(py_str(&json!(1.5)), "1.5");
}

#[test]
fn py_json_dumps_matches_ensure_ascii_false() {
    assert_eq!(py_json_dumps(&json!([{"a": 1}, "b"])), r#"[{"a": 1}, "b"]"#);
}

#[test]
fn py_round_is_half_to_even() {
    assert_eq!(py_round(0.5), 0.0);
    assert_eq!(py_round(1.5), 2.0);
    assert_eq!(py_round(2.5), 2.0);
    assert_eq!(py_round(-0.5), 0.0);
    assert_eq!(py_round(1.2), 1.0);
    assert_eq!(py_round(2.7), 3.0);
}
