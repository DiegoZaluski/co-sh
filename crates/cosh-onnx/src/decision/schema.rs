//! Schema-driven decisions: turn a JSON schema into decision questions.
//!
//! The mapping is the documented subset: an object of properties, each an
//! enum choice, a boolean, or a bounded integer scale. Anything that cannot
//! be answered from a fixed option set (free strings, arrays, nested
//! objects) is rejected with an error that names the path. The pydantic
//! helpers have no Rust counterpart — Rust callers hand a
//! `serde_json::Value` schema directly.

use serde_json::{Map, Value};

use crate::decision::model::PredictRunner;
use crate::error::{Error, Result};

pub const MAX_PROPERTIES: usize = 32;
pub const MAX_OPTIONS: usize = 32;
pub const MAX_SCORE_LEVELS: usize = 10;

/// A schema cannot be expressed as Laya questions; the message names the path.
/// Tagged as `Error::Schema` so callers can distinguish it from a plain
/// invalid-input [`Error::Value`] (upstream it subclasses `ValueError` but is
/// caught separately).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct SchemaError(pub String);

impl DecisionResult {
    /// JSON shape of the dataclass, mirroring the Python field order
    /// (`values`, `confidence`, `probabilities`, `answers`, `usage`,
    /// `routing`).
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("values".into(), Value::Object(self.values.clone()));
        m.insert("confidence".into(), Value::Object(self.confidence.clone()));
        m.insert(
            "probabilities".into(),
            Value::Object(self.probabilities.clone()),
        );
        m.insert("answers".into(), Value::Object(self.answers.clone()));
        m.insert(
            "usage".into(),
            self.usage.clone().unwrap_or(Value::Null),
        );
        m.insert(
            "routing".into(),
            self.routing.clone().unwrap_or(Value::Null),
        );
        Value::Object(m)
    }
}

/// The detailed result of `decide(..., return_details=true)`.
///
/// `values` is the schema-shaped output. `confidence` and `probabilities` are
/// keyed by field, and `answers` is Laya's raw answer per field.
#[derive(Debug, Clone, Default)]
pub struct DecisionResult {
    pub values: Map<String, Value>,
    pub confidence: Map<String, Value>,
    pub probabilities: Map<String, Value>,
    pub answers: Map<String, Value>,
    pub usage: Option<Value>,
    pub routing: Option<Value>,
}

/// One planned schema field: its name, the question kind it maps to, the
/// generated question, and (choice/score) the projection data.
///
/// `options` mirrors the upstream `_Field.options` — choice `(label, value)`
/// pairs in enum order — and `minimum` is the score level-0 value.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub kind: &'static str, // "choice" | "score" | "noul"
    pub question: Map<String, Value>,
    pub options: Vec<(String, Value)>,
    pub minimum: Option<i64>,
}

/// `_schema_of` has no Rust counterpart: callers pass the schema `Value`
/// itself, and `plan_from_json_schema` reports the non-object branch with the
/// same message.
///
/// Intentionally NOT shared with `question::json_type_name`: upstream
/// `structured.py` renders every number as `float` (the pydantic surface),
/// while the question-protocol messages split `int`/`float` like Python's
/// `type(value).__name__`. Deduplicating the two would change parity text.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn field_err(msg: impl Into<String>) -> Error {
    Error::Schema(msg.into())
}

/// `_enum_field`: an enum property becomes a choice over its stringified
/// values; an all-boolean enum collapses to a noul.
fn enum_field(
    path: &str,
    name: &str,
    values: &[Value],
    description: Option<&str>,
) -> Result<Field> {
    if values.len() > MAX_OPTIONS {
        return Err(field_err(format!(
            "{}: {} options exceeds MAX_OPTIONS={}",
            path,
            values.len(),
            MAX_OPTIONS
        )));
    }
    if values.is_empty() {
        return Err(field_err(format!("{}: 'enum' must not be empty", path)));
    }
    if values.iter().all(|v| v.is_boolean()) {
        return noul_field(path, name, description);
    }
    // ("null" if v is None else str(v), v)
    let options: Vec<(String, Value)> = values
        .iter()
        .map(|v| {
            let label = if v.is_null() {
                "null".to_string()
            } else {
                crate::pycompat::py_str(v)
            };
            (label, v.clone())
        })
        .collect();
    let labels: std::collections::HashSet<&str> =
        options.iter().map(|(l, _)| l.as_str()).collect();
    if labels.len() != options.len() {
        return Err(field_err(format!(
            "{}: enum values produce duplicate choice labels",
            path
        )));
    }
    let mut criteria = Map::new();
    for (label, _) in &options {
        criteria.insert(label.clone(), Value::Null);
    }
    let mut question = Map::new();
    question.insert("type".into(), Value::String("choice".into()));
    question.insert(
        "instructions".into(),
        Value::String(
            description
                .map(str::to_string)
                .unwrap_or_else(|| format!("What is `{}`?", name)),
        ),
    );
    question.insert("criteria".into(), Value::Object(criteria));
    Ok(Field {
        name: name.to_string(),
        kind: "choice",
        question,
        options,
        minimum: None,
    })
}

/// `_noul_field`.
fn noul_field(path: &str, name: &str, description: Option<&str>) -> Result<Field> {
    let _ = path;
    let mut question = Map::new();
    question.insert("type".into(), Value::String("noul".into()));
    question.insert(
        "instructions".into(),
        Value::String(
            description
                .map(str::to_string)
                .unwrap_or_else(|| format!("Is `{}` true?", name)),
        ),
    );
    Ok(Field {
        name: name.to_string(),
        kind: "noul",
        question,
        options: Vec::new(),
        minimum: None,
    })
}

/// `_score_field`: a bounded integer becomes a score over the level
/// descriptions `"lo" ..= "hi"`.
fn score_field(
    path: &str,
    name: &str,
    prop: &Map<String, Value>,
    description: Option<&str>,
) -> Result<Field> {
    let lo = prop.get("minimum").and_then(Value::as_i64);
    let hi = prop.get("maximum").and_then(Value::as_i64);
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Err(field_err(format!(
            "{}: a numeric field needs integer 'minimum' and 'maximum' to become a score",
            path
        )));
    };
    if hi < lo {
        return Err(field_err(format!(
            "{}: 'maximum' {} is below 'minimum' {}",
            path, hi, lo
        )));
    }
    let span = (hi - lo + 1) as usize;
    if span > MAX_SCORE_LEVELS {
        return Err(field_err(format!(
            "{}: {} levels exceeds MAX_SCORE_LEVELS={}; narrow the range or use an enum",
            path, span, MAX_SCORE_LEVELS
        )));
    }
    let levels: Vec<Value> = (lo..=hi).map(|v| Value::String(v.to_string())).collect();
    let mut question = Map::new();
    question.insert("type".into(), Value::String("score".into()));
    question.insert(
        "instructions".into(),
        Value::String(
            description
                .map(str::to_string)
                .unwrap_or_else(|| format!("Score `{}` from {} to {}", name, lo, hi)),
        ),
    );
    question.insert("criteria".into(), Value::Array(levels));
    Ok(Field {
        name: name.to_string(),
        kind: "score",
        question,
        options: Vec::new(),
        minimum: Some(lo),
    })
}

/// `_field`: dispatch one property to its question kind, rejecting every shape
/// outside the documented subset with the upstream messages verbatim.
fn field(path: &str, name: &str, prop: &Map<String, Value>) -> Result<Field> {
    let description = prop.get("description").and_then(Value::as_str);
    // Pydantic v2 renders `Optional[X]` as `{"anyOf": [<X>, {"type": "null"}]}`
    // with no top-level type/enum/const, the same nullable shape the list form
    // `type: ["string", "null"]` already handles below. Unwrap the single
    // non-null branch (carrying the outer description) so `Optional[...]` maps
    // instead of raising. A union of two real types is genuinely ambiguous and
    // still rejected.
    if !["const", "enum", "type"].iter().any(|k| prop.contains_key(*k)) {
        let union = prop.get("anyOf").or_else(|| prop.get("oneOf"));
        if let Some(union) = union {
            let Some(branches) = union.as_array() else {
                return Err(field_err(format!(
                    "{}: only 'Optional[...]' unions (one non-null branch) are supported, got 0",
                    path
                )));
            };
            let non_null: Vec<&Map<String, Value>> = branches
                .iter()
                .filter_map(Value::as_object)
                .filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"))
                .collect();
            if non_null.len() != 1 {
                return Err(field_err(format!(
                    "{}: only 'Optional[...]' unions (one non-null branch) are supported, got {}",
                    path,
                    non_null.len()
                )));
            }
            let mut branch = non_null[0].clone();
            if let Some(desc) = description {
                branch
                    .entry("description")
                    .or_insert(Value::String(desc.to_string()));
            }
            return field(path, name, &branch);
        }
    }
    if let Some(const_value) = prop.get("const") {
        return enum_field(path, name, std::slice::from_ref(const_value), description);
    }
    if let Some(enum_values) = prop.get("enum").and_then(Value::as_array) {
        return enum_field(path, name, enum_values, description);
    }

    let mut jtype = prop.get("type").cloned();
    if let Some(Value::Array(types)) = &jtype {
        // nullable: ["string", "null"]
        let non_null: Vec<&Value> = types.iter().filter(|t| t.as_str() != Some("null")).collect();
        if non_null.len() > 1 {
            return Err(field_err(format!(
                "{}: 'type' has multiple non-null types; unions are not supported",
                path
            )));
        }
        jtype = non_null.first().map(|t| (*t).clone());
    }
    match jtype.as_ref().and_then(Value::as_str) {
        Some("boolean") => noul_field(path, name, description),
        Some("string") => Err(field_err(format!(
            "{}: a free string cannot be a fixed option set; use 'enum' or a boolean",
            path
        ))),
        Some("integer") | Some("number") => score_field(path, name, prop, description),
        Some("array") => Err(field_err(format!(
            "{}: arrays are not supported; ask one field per element",
            path
        ))),
        Some("object") => Err(field_err(format!(
            "{}: nested objects are not supported; flatten the schema",
            path
        ))),
        _ if prop.contains_key("$ref") => Err(field_err(format!(
            "{}: $ref/recursion is not supported; flatten the schema",
            path
        ))),
        _ => Err(field_err(format!(
            "{}: unsupported schema {}",
            path,
            crate::pycompat::py_json_dumps(&Value::Object(prop.clone()))
        ))),
    }
}

/// `plan_from_json_schema`: validate a JSON schema and return one planned
/// field per property.
pub fn plan_from_json_schema(schema: &Value) -> Result<Vec<Field>> {
    let Some(map) = schema.as_object() else {
        return Err(field_err(format!(
            "expected a JSON schema object, got {}",
            json_type_name(schema)
        )));
    };
    if !matches!(map.get("type").and_then(Value::as_str), None | Some("object"))
        || !map.contains_key("properties")
    {
        return Err(field_err(
            "the top level must be an object with 'properties'",
        ));
    }
    let Some(properties) = map.get("properties").and_then(Value::as_object) else {
        return Err(field_err("'properties' must be a non-empty object"));
    };
    if properties.is_empty() {
        return Err(field_err("'properties' must be a non-empty object"));
    }
    if properties.len() > MAX_PROPERTIES {
        return Err(field_err(format!(
            "{} properties exceeds MAX_PROPERTIES={}",
            properties.len(),
            MAX_PROPERTIES
        )));
    }
    properties
        .iter()
        .map(|(name, prop)| {
            let Some(prop) = prop.as_object() else {
                return Err(field_err(format!(
                    "properties.{}: property must be an object, got {}",
                    name,
                    json_type_name(prop)
                )));
            };
            field(&format!("properties.{}", name), name, prop)
        })
        .collect()
}

/// `questions_from_json_schema`: turn a JSON schema into Laya questions (a
/// documented subset; see the module docs).
pub fn questions_from_json_schema(schema: &Value) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for f in plan_from_json_schema(schema)? {
        out.insert(f.name.clone(), Value::Object(f.question));
    }
    Ok(out)
}

/// `_project`: map Laya answers onto schema values (choice value, integer
/// level, boolean).
fn project(answers: &Map<String, Value>, fields: &[Field]) -> Map<String, Value> {
    let mut values = Map::new();
    for f in fields {
        let Some(answer) = answers.get(&f.name) else {
            continue;
        };
        match f.kind {
            "noul" => {
                let p = answer.get("noul").and_then(Value::as_f64).unwrap_or(0.0);
                values.insert(f.name.clone(), Value::Bool(p >= 0.5));
            }
            "score" => {
                let probs = answer.get("probabilities");
                let level: i64 = match probs.and_then(Value::as_object) {
                    Some(obj) if !obj.is_empty() => {
                        // max(range(len(probs)), key=lambda i: float(probs.get(str(i), probs.get(i, 0.0))))
                        let mut best: Option<(usize, f64)> = None;
                        for i in 0..obj.len() {
                            let p = obj
                                .get(&i.to_string())
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0);
                            if best.is_none_or(|(_, bp)| p > bp) {
                                best = Some((i, p));
                            }
                        }
                        best.map(|(i, _)| i as i64).unwrap_or(0)
                    }
                    _ => {
                        // int(round(float(answer["score"]))) - minimum; Python's
                        // round() is half-to-even, not Rust's half-away-from-zero.
                        // No lower clamp: upstream reports the raw projected
                        // value even when a below-minimum `score` drives it
                        // negative, and callers see the same number.
                        let score = answer.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                        (crate::pycompat::py_round(score) as i64) - f.minimum.unwrap_or(0)
                    }
                };
                let min = f.minimum.unwrap_or(0);
                values.insert(f.name.clone(), Value::from(min + level));
            }
            _ => {
                // choice: map the reported label back to its enum value
                let label = answer
                    .get("choice")
                    .map(crate::pycompat::py_str)
                    .unwrap_or_default();
                let value = f
                    .options
                    .iter()
                    .find(|(lbl, _)| *lbl == label)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| Value::String(label));
                values.insert(f.name.clone(), value);
            }
        }
    }
    values
}

/// `answers_to_json`: project Laya answers onto the schema values (choice
/// value, integer level, boolean).
pub fn answers_to_json(answers: &Map<String, Value>, schema: &Value) -> Result<Map<String, Value>> {
    let fields = plan_from_json_schema(schema)?;
    Ok(project(answers, &fields))
}

/// `_details`: build the `DecisionResult` for `decide(..., return_details=true)`.
fn details(
    values: Map<String, Value>,
    answers: &Map<String, Value>,
    result: &Map<String, Value>,
) -> DecisionResult {
    let mut confidence = Map::new();
    let mut probabilities = Map::new();
    for (name, answer) in answers {
        confidence.insert(
            name.clone(),
            Value::from(answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0)),
        );
        if answer.get("type").and_then(Value::as_str) == Some("noul") {
            let p = answer.get("noul").and_then(Value::as_f64).unwrap_or(0.0);
            let mut pair = Map::new();
            pair.insert("false".into(), Value::from(round4(1.0 - p)));
            pair.insert("true".into(), Value::from(round4(p)));
            probabilities.insert(name.clone(), Value::Object(pair));
        } else {
            probabilities.insert(
                name.clone(),
                answer.get("probabilities").cloned().unwrap_or(Value::Object(Map::new())),
            );
        }
    }
    DecisionResult {
        values,
        confidence,
        probabilities,
        answers: answers.clone(),
        usage: result.get("usage").cloned(),
        routing: result.get("routing").cloned(),
    }
}

/// Python `round(x, 4)` on the values this code path produces — now shared
/// with the ONNX decode path as `crate::pycompat::round4`.
fn round4(x: f64) -> f64 {
    crate::pycompat::round4(x)
}


/// `decide`: answer `state` against a schema (or explicit questions) and
/// return the decided values.
///
/// Pass exactly one of `schema` or `questions`. With `schema`, the values
/// follow the schema (choice values, integer levels, booleans). With
/// `questions`, the raw answers are returned. `return_details` returns a
/// `DecisionResult` instead of the plain values.
pub fn decide(
    runner: &impl PredictRunner,
    state: &Value,
    schema: Option<&Value>,
    questions: Option<&Map<String, Value>>,
    return_details: bool,
) -> Result<Value> {
    if schema.is_none() == questions.is_none() {
        return Err(Error::Value(
            "pass exactly one of schema= or questions=".to_string(),
        ));
    }
    let mut fields: Option<Vec<Field>> = None;
    let questions = match schema {
        Some(schema) => {
            let planned = plan_from_json_schema(schema)?;
            fields = Some(planned);
            let mut qs = Map::new();
            for f in fields.as_ref().unwrap() {
                qs.insert(f.name.clone(), Value::Object(f.question.clone()));
            }
            qs
        }
        None => questions.unwrap().clone(),
    };
    let result = runner.predict(state, &questions)?;
    let result_obj = result.as_object().cloned().unwrap_or_default();
    let answers = result_obj
        .get("answers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let values = match &fields {
        Some(fields) => project(&answers, fields),
        None => answers.clone(),
    };
    if return_details {
        return Ok(details(values, &answers, &result_obj).to_value());
    }
    Ok(Value::Object(values))
}
