//! Ported tests: `tests/test_structured.py` from the laya repository, with the
//! same cases and the same expected values. The pydantic section has no Rust
//! counterpart (Rust callers pass a JSON-schema `Value` directly) and is
//! dropped; every other check is ported.

use serde_json::{json, Map, Value};

use crate::decision::schema::{
    answers_to_json, decide, plan_from_json_schema, questions_from_json_schema,
};
use crate::error::{Error, Result};

fn check_raises_schema<T>(name: &str, got: &Result<T>, want: &str) {
    match got {
        Err(Error::Schema(msg))
        | Err(Error::Value(msg))
        | Err(Error::Runtime(msg))
        | Err(Error::Timeout(msg)) => {
            assert_eq!(msg, want, "case: {}", name);
        }
        Ok(_) => panic!("{}: did not raise SchemaError", name),
    }
}

fn bad(schema: Value) -> Result<serde_json::Map<String, Value>> {
    questions_from_json_schema(&schema)
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "department": {"type": "string", "enum": ["billing", "support", "sales"],
                           "description": "Which team?"},
            "urgency": {"type": "integer", "minimum": 0, "maximum": 2},
            "needs_human": {"type": "boolean"},
            "priority": {"enum": [1, 2, 3]},
        },
    })
}

// --------------------------------------------------------------- mapping
#[test]
fn mapping() {
    let questions = questions_from_json_schema(&schema()).unwrap();
    let department = &questions["department"];
    // enum is a choice
    assert_eq!(department["type"], json!("choice"));
    // description becomes instructions
    assert_eq!(department["instructions"], json!("Which team?"));
    // enum labels
    let labels: Vec<&str> = department["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(labels, vec!["billing", "support", "sales"]);
    // bounded number is a score
    assert_eq!(questions["urgency"]["type"], json!("score"));
    // score levels
    assert_eq!(questions["urgency"]["criteria"], json!(["0", "1", "2"]));
    // boolean is noul
    assert_eq!(questions["needs_human"]["type"], json!("noul"));
    // integer enum becomes a choice
    assert_eq!(questions["priority"]["type"], json!("choice"));
    // integer enum labels are strings
    let priority_labels: Vec<&str> = questions["priority"]["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(priority_labels, vec!["1", "2", "3"]);
    // plan has one field per property
    assert_eq!(plan_from_json_schema(&schema()).unwrap().len(), 4);
    // nullable boolean remains noul
    let nullable = questions_from_json_schema(&json!({
        "type": "object",
        "properties": {"a": {"type": ["null", "boolean"]}}
    }))
    .unwrap();
    assert_eq!(nullable["a"]["type"], json!("noul"));
}

// --------------------------------------------------------------- projection
#[test]
fn projection() {
    let answers = answers_fixture();
    let values = answers_to_json(&answers, &schema()).unwrap();
    // choice value
    assert_eq!(values["department"], json!("billing"));
    // score is the argmax level
    assert_eq!(values["urgency"], json!(2));
    // noul is a bool
    assert_eq!(values["needs_human"], json!(true));
    // integer enum keeps its type
    assert_eq!(values["priority"], json!(2));
    assert!(values["priority"].is_i64() || values["priority"].is_u64());
    // false noul
    let values = answers_to_json(
        &serde_json::from_str::<Map<String, Value>>(
            r#"{"x": {"type": "noul", "noul": 0.2}}"#,
        )
        .unwrap(),
        &json!({"type": "object", "properties": {"x": {"type": "boolean"}}}),
    )
    .unwrap();
    assert_eq!(values["x"], json!(false));
}

fn answers_fixture() -> Map<String, Value> {
    serde_json::from_str(
        r#"{
        "department": {"type": "choice", "choice": "billing", "confidence": 0.9,
                       "probabilities": {"billing": 0.9, "support": 0.1, "sales": 0.0}},
        "urgency": {"type": "score", "score": 1.2, "confidence": 0.5,
                    "probabilities": {"0": 0.1, "1": 0.2, "2": 0.7}, "legend": {}},
        "needs_human": {"type": "noul", "noul": 0.8, "confidence": 0.8},
        "priority": {"type": "choice", "choice": "2", "confidence": 0.7,
                     "probabilities": {"1": 0.2, "2": 0.7, "3": 0.1}}
    }"#,
    )
    .unwrap()
}

// --------------------------------------------------------------- rejections
#[test]
fn rejections() {
    check_raises_schema(
        "free string",
        &bad(json!({"type": "object", "properties": {"a": {"type": "string"}}})),
        "properties.a: a free string cannot be a fixed option set; use 'enum' or a boolean",
    );
    check_raises_schema(
        "multiple non-null types",
        &bad(json!({"type": "object", "properties": {"a": {"type": ["boolean", "integer"],
                                                            "minimum": 0, "maximum": 2}}})),
        "properties.a: 'type' has multiple non-null types; unions are not supported",
    );
    check_raises_schema(
        "nullable union of multiple types",
        &bad(json!({"type": "object", "properties": {"a": {"type": ["null", "integer", "boolean"],
                                                            "minimum": 0, "maximum": 2}}})),
        "properties.a: 'type' has multiple non-null types; unions are not supported",
    );
    check_raises_schema(
        "array",
        &bad(json!({"type": "object",
                     "properties": {"a": {"type": "array", "items": {"type": "string"}}}})),
        "properties.a: arrays are not supported; ask one field per element",
    );
    check_raises_schema(
        "nested object",
        &bad(json!({"type": "object", "properties": {"a": {"type": "object", "properties": {}}}})),
        "properties.a: nested objects are not supported; flatten the schema",
    );
    check_raises_schema(
        "$ref",
        &bad(json!({"type": "object", "properties": {"a": {"$ref": "#/$defs/X"}}})),
        "properties.a: $ref/recursion is not supported; flatten the schema",
    );
    check_raises_schema(
        "unbounded number",
        &bad(json!({"type": "object", "properties": {"a": {"type": "integer", "minimum": 0}}})),
        "properties.a: a numeric field needs integer 'minimum' and 'maximum' to become a score",
    );
    check_raises_schema(
        "score too wide",
        &bad(json!({"type": "object",
                     "properties": {"a": {"type": "integer", "minimum": 0, "maximum": 100}}})),
        "properties.a: 101 levels exceeds MAX_SCORE_LEVELS=10; narrow the range or use an enum",
    );
    let too_many: Map<String, Value> = (0..33)
        .map(|i| (format!("p{}", i), json!({"type": "boolean"})))
        .collect();
    check_raises_schema(
        "too many properties",
        &bad(json!({"type": "object", "properties": too_many})),
        "33 properties exceeds MAX_PROPERTIES=32",
    );
    let too_many_options: Vec<Value> = (0..33).map(|i| json!(format!("v{}", i))).collect();
    check_raises_schema(
        "too many options",
        &bad(json!({"type": "object", "properties": {"a": {"type": "string", "enum": too_many_options}}})),
        "properties.a: 33 options exceeds MAX_OPTIONS=32",
    );
    for colliding in [json!([1, "1"]), json!([null, "null"]), json!([true, "True"])] {
        check_raises_schema(
            &format!("colliding enum {}", colliding),
            &bad(json!({"type": "object", "properties": {"x": {"enum": colliding}}})),
            "properties.x: enum values produce duplicate choice labels",
        );
    }
    // colliding enum names the field
    let err = questions_from_json_schema(&json!({
        "type": "object", "properties": {"x": {"enum": [1, "1"]}}
    }))
    .unwrap_err()
    .to_string();
    assert_eq!(err, "properties.x: enum values produce duplicate choice labels");
    check_raises_schema(
        "non-object root",
        &bad(json!({"type": "array"})),
        "the top level must be an object with 'properties'",
    );
    check_raises_schema(
        "empty properties",
        &bad(json!({"type": "object", "properties": {}})),
        "'properties' must be a non-empty object",
    );
}

// --------------------------------------------------------------- decide
struct FakeRunner {
    answers: Map<String, Value>,
    calls: Vec<Map<String, Value>>,
}

impl FakeRunner {
    fn new(answers: Map<String, Value>) -> Self {
        Self { answers, calls: Vec::new() }
    }
}

impl crate::decision::model::PredictRunner for FakeRunner {
    fn predict(&mut self, state: &Value, questions: &Map<String, Value>) -> Result<Value> {
        let mut call = Map::new();
        call.insert("state".into(), state.clone());
        call.insert("questions".into(), Value::Object(questions.clone()));
        self.calls.push(call);
        Ok(json!({
            "answers": Value::Object(self.answers.clone()),
            "usage": {"input_tokens": 1, "output_tokens": 0},
            "routing": {"model": "english"}
        }))
    }
}

#[test]
fn decide_schema_flow() {
    let mut runner = FakeRunner::new(answers_fixture());
    let out = decide(&mut runner, &json!("some state"), Some(&schema()), None, false).unwrap();
    assert_eq!(out["department"], json!("billing"));
    // builds questions
    assert_eq!(
        runner.calls[0]["questions"]["department"]["type"],
        json!("choice")
    );
    // forwards state
    assert_eq!(runner.calls[0]["state"], json!("some state"));
}

#[test]
fn decide_details() {
    let mut runner = FakeRunner::new(answers_fixture());
    let details = decide(&mut runner, &json!("some state"), Some(&schema()), None, true)
        .unwrap();
    assert_eq!(details["confidence"]["department"], json!(0.9));
    assert_eq!(
        details["probabilities"]["needs_human"],
        json!({"false": 0.2, "true": 0.8})
    );
    assert_eq!(details["usage"], json!({"input_tokens": 1, "output_tokens": 0}));
    assert_eq!(details["routing"], json!({"model": "english"}));
}

#[test]
fn decide_questions_pass_through_returns_answers() {
    let mut runner = FakeRunner::new(
        serde_json::from_str::<Map<String, Value>>(
            r#"{"a": {"type": "noul", "noul": 0.9, "confidence": 0.9}}"#,
        )
        .unwrap(),
    );
    let out = decide(
        &mut runner,
        &json!("s"),
        None,
        Some(
            &serde_json::from_str::<Map<String, Value>>(
                r#"{"a": {"type": "noul", "instructions": "?"}}"#,
            )
            .unwrap(),
        ),
        false,
    )
    .unwrap();
    assert_eq!(out, json!({"a": {"type": "noul", "noul": 0.9, "confidence": 0.9}}));
}

#[test]
fn decide_requires_exactly_one_of_schema_or_questions() {
    let mut runner = FakeRunner::new(answers_fixture());
    let err = decide(&mut runner, &json!("s"), None, None, false).unwrap_err();
    assert_eq!(err.to_string(), "pass exactly one of schema= or questions=");
    let err = decide(&mut runner, &json!("s"), Some(&schema()), Some(&Map::new()), false)
        .unwrap_err();
    assert_eq!(err.to_string(), "pass exactly one of schema= or questions=");
}

// --------------------------------------------------------------- nullable via anyOf (pydantic v2)
#[test]
fn nullable_via_any_of() {
    // Pydantic v2 renders Optional[X] as {"anyOf": [<X>, {"type": "null"}]},
    // the same nullable intent as the list form type:["string","null"]. Both
    // must map to the underlying field, not raise.
    let nullable = json!({
        "type": "object",
        "properties": {
            "dept": {"anyOf": [{"type": "string", "enum": ["billing", "sales"]}, {"type": "null"}],
                     "description": "Which team?"},
            "score": {"anyOf": [{"type": "integer", "minimum": 0, "maximum": 2}, {"type": "null"}]},
            "flag": {"anyOf": [{"type": "boolean"}, {"type": "null"}]},
        },
    });
    let nq = questions_from_json_schema(&nullable).unwrap();
    // anyOf enum is a choice
    assert_eq!(nq["dept"]["type"], json!("choice"));
    // anyOf carries the outer description
    assert_eq!(nq["dept"]["instructions"], json!("Which team?"));
    // anyOf bounded integer is a score
    assert_eq!(nq["score"]["type"], json!("score"));
    // anyOf boolean is a noul
    assert_eq!(nq["flag"]["type"], json!("noul"));
    // oneOf is accepted the same way
    let one_of = questions_from_json_schema(&json!({
        "type": "object",
        "properties": {"a": {"oneOf": [{"enum": ["x", "y"]}, {"type": "null"}]}}
    }))
    .unwrap();
    assert_eq!(one_of["a"]["type"], json!("choice"));
    // a union of two real types stays ambiguous and is rejected
    check_raises_schema(
        "two real branches rejected",
        &bad(json!({"type": "object",
                     "properties": {"a": {"anyOf": [{"type": "boolean"}, {"type": "integer"}]}}})),
        "properties.a: only 'Optional[...]' unions (one non-null branch) are supported, got 2",
    );
}
