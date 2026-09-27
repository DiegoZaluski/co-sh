//! `Prediction`: the typed read-only view over a result object.

use serde_json::json;

use cosh_onnx::decision::model::Prediction;

/// A full result object in the shape `decide` returns: one of each question
/// type plus usage and routing metadata (the Router result shape).
fn full_result() -> Prediction {
    Prediction::from(json!({
        "model": "laya-rl-agent-onnx",
        "answers": {
            "dept": {"type": "choice", "choice": "billing",
                     "probabilities": {"billing": 0.94, "tech": 0.06},
                     "confidence": 0.88, "answer_confidence": 0.94},
            "urgency": {"type": "score", "score": 1.84,
                        "confidence": 0.7, "answer_confidence": 0.61},
            "churn": {"type": "noul", "noul": 0.892,
                      "confidence": 0.892, "answer_confidence": 0.892}
        },
        "usage": {"input_tokens": 42, "output_tokens": 0},
        "routing": {"model": "english", "reason": "latin english"}
    }))
}

#[test]
fn choice_score_and_noul_read_from_the_answer_contract() {
    let p = full_result();
    assert_eq!(p.choice("dept"), Some("billing"));
    assert!((p.score("urgency").unwrap() - 1.84).abs() < 1e-9);
    assert!((p.noul("churn").unwrap() - 0.892).abs() < 1e-9);
}

#[test]
fn both_confidence_numbers_are_exposed() {
    let p = full_result();
    assert!((p.confidence("dept").unwrap() - 0.88).abs() < 1e-9);
    assert!((p.answer_confidence("dept").unwrap() - 0.94).abs() < 1e-9);
    // score and noul carry both numbers too.
    assert!(p.confidence("urgency").is_some());
    assert!(p.answer_confidence("churn").is_some());
}

#[test]
fn absent_fields_answer_none_instead_of_panicking() {
    let p = full_result();
    // No such question.
    assert_eq!(p.choice("missing"), None);
    assert_eq!(p.score("missing"), None);
    assert_eq!(p.noul("missing"), None);
    assert_eq!(p.confidence("missing"), None);
    assert_eq!(p.answer_confidence("missing"), None);
    // A wrong-typed value reads as None, not a panic.
    let odd = Prediction::from(json!({"answers": {"x": {"choice": 3}}}));
    assert_eq!(odd.choice("x"), None);
}

#[test]
fn empty_and_shell_results_answer_none() {
    // The empty-questions response upstream returns.
    let empty = Prediction::from(json!({
        "model": "laya-rl-agent-onnx", "answers": {},
        "usage": {"input_tokens": 0, "output_tokens": 0}
    }));
    assert_eq!(empty.answers().len(), 0);
    assert_eq!(empty.choice("dept"), None);
    assert_eq!(empty.input_tokens(), Some(0));
    assert!(empty.routing().is_none());
    assert_eq!(empty.routing_model(), None);

    // No answers key at all.
    let bare = Prediction::from(json!({"model": "x"}));
    assert_eq!(bare.answers().len(), 0);
    assert_eq!(bare.input_tokens(), None);
}

#[test]
fn usage_and_routing_read_from_a_router_result() {
    let p = full_result();
    assert_eq!(p.input_tokens(), Some(42));
    assert_eq!(p.routing_model(), Some("english"));
    assert_eq!(
        p.routing()
            .unwrap()
            .get("reason")
            .and_then(serde_json::Value::as_str),
        Some("latin english")
    );
}

#[test]
fn raw_round_trips_the_whole_object() {
    let raw = json!({"answers": {}, "custom_extension": [1, 2, 3]});
    let p = Prediction::from(raw.clone());
    assert_eq!(p.raw(), &raw);
}
