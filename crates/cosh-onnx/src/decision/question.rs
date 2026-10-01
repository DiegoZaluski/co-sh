//! The question protocol: types, validation, normalisation and prompt
//! rendering — the crate-level decision contract.
//!
//! `choice` / `score` / `noul` is the option protocol the crate's decision
//! models speak: a question is validated (`check_question`), normalised to
//! the internal `{"t", "ins", "crit", "labels"?}` shape (`to_internal`), and
//! rendered into option texts (`render_options`). laya is the first model
//! ported on this contract; a future model that speaks the same protocol
//! reuses this module unchanged, and a model with a different prompt format
//! brings its own renderer instead.
//!
//! A question whose *shape* is wrong is rejected by name before anything is
//! rendered or tokenized; the error messages are the upstream texts
//! verbatim, so callers see identical failures on either backend.

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::pycompat::{py_json_dumps, py_repr_value as py_repr, py_str};

/// `QTYPES = {"choice": 0, "score": 1, "noul": 2}`.
pub const QTYPES: [(&str, u8); 3] = [("choice", 0), ("score", 1), ("noul", 2)];

/// `QTYPES[qtype]`; `None` for an unknown type name.
pub fn qtype_code(t: &str) -> Option<u8> {
    QTYPES
        .iter()
        .find(|(name, _)| *name == t)
        .map(|(_, code)| *code)
}

/// `QTYPE_NAMES[code]`.
pub fn qtype_name(q: u8) -> &'static str {
    match q {
        0 => "choice",
        1 => "score",
        _ => "noul",
    }
}

/// `serialize_state`: strings pass through; anything structured becomes
/// compact JSON (`json.dumps(state, ensure_ascii=False)`).
pub fn serialize_state(state: &Value) -> String {
    match state {
        Value::String(s) => s.clone(),
        other => py_json_dumps(other),
    }
}

/// `render_criterion`: strings pass through; anything structured becomes
/// compact JSON, so a rubric reads as JSON rather than a Python repr.
///
/// Upstream passes `default=str` for unserialisable objects; every value
/// reachable here is already a JSON value, so that branch cannot occur.
pub fn render_criterion(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_json_dumps(other),
    }
}

const DEFAULT_FALSE_LABEL: &str = "false";
const DEFAULT_TRUE_LABEL: &str = "true";
const DEFAULT_FALSE_TEXT: &str = "no, the statement does not hold";
const DEFAULT_TRUE_TEXT: &str = "yes, the statement holds";

/// `_resolve_noul_labels`: validate the optional `labels` mapping and return
/// `(false_label, true_label)` with the upstream error message verbatim.
///
/// `labels` is the optional `{"false": .., "true": ..}` mapping on a noul
/// question that rewords the option prefixes the model reads.
pub fn resolve_noul_labels(labels: Option<&Value>) -> Result<(String, String)> {
    const MSG: &str =
        "noul labels must map exactly 'false' and 'true' to distinct non-empty strings";
    // JSON null reaches here as `labels: None` upstream, so it selects the
    // defaults exactly like an absent key.
    let Some(labels) = labels.filter(|l| !l.is_null()) else {
        return Ok((
            DEFAULT_FALSE_LABEL.to_string(),
            DEFAULT_TRUE_LABEL.to_string(),
        ));
    };
    let Value::Object(map) = labels else {
        return Err(Error::Value(MSG.to_string()));
    };
    if map.len() != 2 || !map.contains_key("false") || !map.contains_key("true") {
        return Err(Error::Value(MSG.to_string()));
    }
    let Value::String(false_label) = &map["false"] else {
        return Err(Error::Value(MSG.to_string()));
    };
    let Value::String(true_label) = &map["true"] else {
        return Err(Error::Value(MSG.to_string()));
    };
    let false_label = false_label.trim();
    let true_label = true_label.trim();
    if false_label.is_empty() || true_label.is_empty() || false_label == true_label {
        return Err(Error::Value(MSG.to_string()));
    }
    Ok((false_label.to_string(), true_label.to_string()))
}

/// Option texts in label-index order. Noul semantic order is
/// always `[false, true]`.
///
/// Only `null` and `""` count as "no description" — `0` and `false` are real
/// criterion values and keep their rendered form. A label with no description
/// is rendered as itself.
///
/// `q` is the internal question shape (`{"t", "ins", "crit", "labels"?}`);
/// `crit` is absent (`None` upstream) when the value is `Value::Null`.
///
/// Divergence (intentional): the four shape guards below return errors where
/// upstream would crash mid-function with an unrelated exception (e.g.
/// `AttributeError: 'NoneType' object has no attribute 'items'` for a choice
/// without criteria). Those shapes cannot reach `render_options` in either
/// backend because `Agent._check_question` rejects them first; the guards
/// exist so the Rust function stays total. No message here is upstream text.
pub fn render_options(q: &Value) -> Result<Vec<String>> {
    let t = q["t"].as_str().unwrap_or_default();
    let crit = q.get("crit");
    if t != "noul" && q.get("labels").is_some() {
        return Err(Error::Value(
            "labels is only supported for noul questions".to_string(),
        ));
    }
    match t {
        "choice" => {
            let mut out = Vec::new();
            let Some(Value::Object(map)) = crit else {
                return Err(Error::Value(
                    "choice criteria must be an object of label -> description".to_string(),
                ));
            };
            for (k, v) in map {
                let label = py_str(&Value::String(k.clone()));
                out.push(if v.is_null() || v == "" {
                    label
                } else {
                    format!("{}: {}", label, render_criterion(v))
                });
            }
            Ok(out)
        }
        "score" => {
            let mut out = Vec::new();
            let Some(Value::Array(levels)) = crit else {
                return Err(Error::Value(
                    "score criteria must be a list of level descriptions".to_string(),
                ));
            };
            for (i, c) in levels.iter().enumerate() {
                out.push(format!("level {}: {}", i, render_criterion(c)));
            }
            Ok(out)
        }
        "noul" => {
            let empty = serde_json::Map::new();
            let crit = match crit {
                None | Some(Value::Null) => &empty,
                Some(Value::Object(map)) => map,
                Some(_) => {
                    return Err(Error::Value(
                        "noul criteria must be an object with optional 'true'/'false' \
                         descriptions"
                            .to_string(),
                    ));
                }
            };
            let (false_label, true_label) = resolve_noul_labels(q.get("labels"))?;
            let false_crit = crit.get("false");
            let true_crit = crit.get("true");
            let false_text = match false_crit {
                Some(v) if !(v.is_null() || v == "") => render_criterion(v),
                _ => DEFAULT_FALSE_TEXT.to_string(),
            };
            let true_text = match true_crit {
                Some(v) if !(v.is_null() || v == "") => render_criterion(v),
                _ => DEFAULT_TRUE_TEXT.to_string(),
            };
            Ok(vec![
                format!("{}: {}", false_label, false_text),
                format!("{}: {}", true_label, true_text),
            ])
        }
        _ => Err(Error::Value(format!("unknown question type {:?}", t))),
    }
}

/// Python `type(value).__name__` over the JSON surface, for the `%s`-style
/// type mentions in validation messages. `Number` reads as `float` when it
/// carries a fractional value and `int` when it carries an integral one,
/// mirroring Python's int/float split: a `json!`-built `Value` (or a value
/// parsed from JSON without a decimal point) is an integer. Deserialized
/// values that always carried a decimal point read as `float`, which is the
/// closest observable approximation of the Python surface.
pub fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn question_err(qid: &str, msg: impl std::fmt::Display) -> Error {
    Error::Value(format!("question '{}': {}", qid, msg))
}
/// Python `sorted(QTYPES)` as it appears in the unknown-type message.
const SORTED_QTYPES_MSG: &str = "['choice', 'noul', 'score']";

/// `Agent._check_question`: reject a question that cannot be answered, naming
/// it and what to fix.
///
/// `render_options` reads `criteria` in the shape the question's type expects
/// and the decision head needs at least one option, so a malformed definition
/// used to surface from three frames down as something that names neither the
/// question nor the problem.
pub fn check_question(qid: &str, qdef: &Value) -> Result<()> {
    let Some(def) = qdef.as_object() else {
        return Err(question_err(
            qid,
            format!("definition must be a dict, got {}", json_type_name(qdef)),
        ));
    };
    let t_value = def.get("type").cloned().unwrap_or(Value::Null);
    let t = t_value.as_str();
    let Some(t) = t else {
        return Err(question_err(
            qid,
            format!(
                "unknown type {}; use one of {}",
                py_repr(&t_value),
                SORTED_QTYPES_MSG
            ),
        ));
    };
    if qtype_code(t).is_none() {
        return Err(question_err(
            qid,
            format!(
                "unknown type {}; use one of {}",
                py_repr(&t_value),
                SORTED_QTYPES_MSG
            ),
        ));
    }
    if !def.contains_key("instructions") {
        return Err(question_err(
            qid,
            "no 'instructions'; add the text the model should answer",
        ));
    }
    let crit = def.get("criteria").cloned().unwrap_or(Value::Null);
    if t == "choice" {
        let (is_dict, is_list, is_empty) = match &crit {
            Value::Object(m) => (true, false, m.is_empty()),
            Value::Array(a) => (false, true, a.is_empty()),
            _ => (false, false, true),
        };
        if !is_dict && !is_list {
            return Err(question_err(
                qid,
                "a choice question takes 'criteria' as a dict of \
                 label -> description, or a list of labels",
            ));
        }
        if is_empty {
            return Err(question_err(
                qid,
                "a choice question needs at least one criterion",
            ));
        }
        // A label is used as a dict key when a list of labels is normalised, so
        // a list, dict or set label raised `TypeError: unhashable type` from
        // three frames down. Labels are rendered as option text, so a nested
        // structure has no meaning here.
        let labels: Vec<Value> = if is_list {
            crit.as_array().unwrap().clone()
        } else {
            crit.as_object()
                .unwrap()
                .keys()
                .map(|k| Value::String(k.clone()))
                .collect()
        };
        for (i, label) in labels.iter().enumerate() {
            if label.is_array() || label.is_object() {
                return Err(question_err(
                    qid,
                    format!(
                        "choice label {} is a {}; a label is rendered as option text \
                         and used as the answer key, so it must be a scalar (a string, \
                         number or None), got {}",
                        i,
                        json_type_name(label),
                        py_repr(label)
                    ),
                ));
            }
        }
    } else if t == "score" {
        let Value::Array(levels) = &crit else {
            return Err(question_err(
                qid,
                "a score question takes 'criteria' as a list of level \
                 descriptions, index 0 first",
            ));
        };
        if levels.is_empty() {
            return Err(question_err(
                qid,
                "a score question needs at least one level",
            ));
        }
        if let Some(idx) = levels.iter().position(Value::is_null) {
            return Err(question_err(
                qid,
                format!(
                    "score level {} is null; give every level a description, index 0 first",
                    idx
                ),
            ));
        }
    } else {
        // noul: `crit is not None and not isinstance(crit, dict)` first, then
        // the keyed-only-'true'/'false' rule on an actual dict.
        match &crit {
            Value::Null => {}
            Value::Object(m) => {
                let mut keys: Vec<String> = m.keys().map(|k| k.to_lowercase()).collect();
                keys.sort();
                if keys.iter().any(|k| k != "true" && k != "false") {
                    let rendered = keys
                        .iter()
                        .map(|k| format!("'{}'", k))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(question_err(
                        qid,
                        format!(
                            "a noul question takes 'criteria' keyed only 'true'/'false' \
                             (either or both, and omitted is fine), got [{}]. Those keys \
                             are the option texts the model reads; any other key was \
                             silently dropped and replaced with the defaults. If you want \
                             the answer worded differently, keep 'criteria' keyed \
                             'true'/'false' and set 'labels' instead.",
                            rendered
                        ),
                    ));
                }
            }
            _ => {
                return Err(question_err(
                    qid,
                    "a noul question takes 'criteria' as a dict with optional \
                     'true'/'false' descriptions, or omits it",
                ));
            }
        }
    }
    if let Some(labels) = def.get("labels") {
        if t != "noul" {
            return Err(question_err(
                qid,
                "'labels' is only supported for noul questions",
            ));
        }
        if let Err(e) = resolve_noul_labels(Some(labels)) {
            return Err(question_err(qid, e));
        }
    }
    Ok(())
}

/// `Agent._to_internal`: normalise a (already validated) public question into
/// the internal `{"t", "ins", "crit", "labels"?}` shape.
///
/// Callers run [`check_question`] first, exactly as `system_one` does; a
/// missing `type` key is reported with the Python `KeyError` text for parity.
pub fn to_internal(qdef: &Value) -> Result<Map<String, Value>> {
    let Some(def) = qdef.as_object() else {
        return Err(Error::Value(format!(
            "definition must be a dict, got {}",
            json_type_name(qdef)
        )));
    };
    let Some(t) = def.get("type").and_then(Value::as_str) else {
        return Err(Error::Value("KeyError: 'type'".to_string()));
    };
    let mut crit = def.get("criteria").cloned().unwrap_or(Value::Null);
    if t == "choice" {
        if let Value::Array(items) = &crit {
            // {c: None for c in crit}
            let mut m = Map::new();
            for c in items {
                m.insert(py_str(c), Value::Null);
            }
            crit = Value::Object(m);
        }
    } else if t == "noul"
        && let Value::Object(m) = &crit
    {
        // Normalize boolean literal keys to string keys ("true"/"false");
        // over the JSON surface that is the `.lower()` half of the rule.
        let mut n = Map::new();
        for (k, v) in m {
            n.insert(k.to_lowercase(), v.clone());
        }
        crit = Value::Object(n);
    }
    let ins_raw = def.get("instructions").cloned().unwrap_or(Value::Null);
    // Non-string instructions are serialised with `ensure_ascii=False`,
    // matching `serialize_state` and `render_criterion`. The default escaped
    // non-ASCII to literal `\uXXXX`, which the tokenizer then read as escape
    // text: on the English checkpoint one German question answered noul=0.1652
    // as a dict and noul=0.2650 as the identical plain string.
    let ins = if ins_raw.is_string() {
        ins_raw
    } else {
        Value::String(py_json_dumps(&ins_raw))
    };
    let mut q = Map::new();
    q.insert("t".into(), Value::String(t.to_string()));
    q.insert("ins".into(), ins);
    q.insert("crit".into(), crit);
    if let Some(labels) = def.get("labels") {
        q.insert("labels".into(), labels.clone());
    }
    Ok(q)
}
