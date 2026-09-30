//! Ported tests: `tests/test_onnx_lang_parity.py`, the empty-questions section
//! of `tests/test_empty_questions.py`, the temperature-parsing section of
//! `tests/test_temperature_loading.py`, and the options-over-budget section of
//! `tests/test_load_errors.py` — everything that does not need a torch
//! checkpoint or onnxruntime. The stub-session / fake-tokenizer technique is
//! the upstream one: `_infer` is driven with fixed logits so decoding is
//! deterministic.
//!
//! JSON-surface notes:
//! - Python `float("nan")` / `inf` entries exist only as the strings
//!   "NaN"/"Infinity"/"-Infinity" over JSON; the float-object cases cannot be
//!   built in `serde_json` and are covered by the string forms plus
//!   `clamp_temperature`'s own tests.
//! - The torch `Agent`-shaped states (`{"text": ...}`, message lists) are
//!   plain JSON values here.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::{FakeTok, LangTemperatures, OnnxAgent, StubSession, bare_agent};
use crate::decision::confidence::clamp_temperature_default;
use crate::error::Error;
use crate::pycompat::py_repr_value;

mod empty_questions;
mod lang_parity;
mod load_errors;
mod predictions;
mod temperature;
/// `_bare_onnx` from `test_onnx_lang_parity.py`: a bare agent with a fake
/// tokenizer, a stub session and the given lang overrides (clamped the same
/// way the loader clamps them).
fn bare_onnx(lang_temperatures: Value, logits: Vec<Vec<f32>>, act: Vec<Vec<f32>>) -> OnnxAgent {
    let mut langs: HashMap<String, LangTemperatures> = HashMap::new();
    if let Value::Object(map) = &lang_temperatures {
        for (l, cfg) in map {
            let norm = l.split('-').next().unwrap_or("").to_lowercase();
            let temperature: Vec<f64> = cfg["temperature"]
                .as_array()
                .map(|a| a.iter().map(clamp_temperature_default).collect())
                .unwrap_or_default();
            let mut tbo: HashMap<String, f64> = HashMap::new();
            if let Some(obj) = cfg.get("temperature_by_options").and_then(Value::as_object) {
                for (k, v) in obj {
                    tbo.insert(k.clone(), clamp_temperature_default(v));
                }
            }
            langs.insert(
                norm,
                LangTemperatures {
                    temperature,
                    temperature_by_options: tbo,
                },
            );
        }
    }
    let cfg = json!({"max_len": 64, "head_max_len": 32});
    bare_agent(
        cfg.as_object().unwrap().clone(),
        Box::new(FakeTok),
        Box::new(StubSession {
            logits,
            act_logits: act,
        }),
        [1.0, 1.0, 1.0],
        HashMap::new(),
        langs,
    )
}

fn questions() -> Map<String, Value> {
    json!({
        "dept": {"type": "choice", "instructions": "Which team?",
                 "criteria": {"billing": "money", "support": "help", "sales": "buy"}},
        "flag": {"type": "noul", "instructions": "Urgent?"}
    })
    .as_object()
    .unwrap()
    .clone()
}

#[test]
fn checkpoint_relative_keys_strips_prefix_with_deterministic_precedence() {
    use super::checkpoint_relative_keys;
    // Both spellings of the same artifact: the explicit repo-relative key
    // must win over the bare one, regardless of HashMap iteration order —
    // run the conversion repeatedly and require an identical result.
    let map: HashMap<String, String> = [
        ("english/laya.onnx".to_string(), "g1".to_string()),
        ("laya.onnx".to_string(), "g2".to_string()),
        ("english/rl_agent_config.json".to_string(), "c1".to_string()),
        ("rl_agent_config.json".to_string(), "c2".to_string()),
        ("tokenizer/tokenizer.json".to_string(), "t1".to_string()),
    ]
    .into_iter()
    .collect();
    let first = checkpoint_relative_keys(&map, "english/");
    for _ in 0..32 {
        let again = checkpoint_relative_keys(&map, "english/");
        assert_eq!(first, again);
    }
    assert_eq!(first.get("laya.onnx").unwrap(), "g1");
    assert_eq!(first.get("rl_agent_config.json").unwrap(), "c1");
    // Keys without the prefix pass through untouched.
    assert_eq!(first.get("tokenizer/tokenizer.json").unwrap(), "t1");
    assert_eq!(first.len(), 3);
}

#[test]
fn checkpoint_relative_keys_is_identity_without_a_prefix() {
    use super::checkpoint_relative_keys;
    let map: HashMap<String, String> = [
        ("onnx".to_string(), "g1".to_string()),
        ("laya.onnx".to_string(), "g2".to_string()),
    ]
    .into_iter()
    .collect();
    // Local checkpoints: no subfolder, no stripping. The `onnx` alias
    // passes through — `verify_digests` resolves it to the graph path.
    assert_eq!(checkpoint_relative_keys(&map, ""), map);
}
