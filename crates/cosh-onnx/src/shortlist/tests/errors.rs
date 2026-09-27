use super::*;

// --------------------------------------------------------------- errors
fn assert_value_error(name: &str, run: impl FnOnce() -> Result<Vec<Value>>) {
    match run() {
        Err(Error::Value(msg)) => {
            assert!(!msg.is_empty(), "{name}: empty message");
        }
        other => panic!("{name}: expected Error::Value, got {other:?}"),
    }
}

#[test]
fn k_zero_and_negative_raise() {
    let embed = TableEmbed::from_pairs(&option_texts_table());
    let criteria = criteria_value();
    assert_value_error("err/k=0", || shortlist(&json!("pay me"), &criteria, &embed, 0));
    // `k negative` and the bool/float/str branches are unrepresentable with a
    // typed usize (documented omission).
}

#[test]
fn empty_criteria_and_duplicates_raise() {
    let embed = TableEmbed::from_pairs(&option_texts_table());
    assert_value_error("err/empty dict", || shortlist(&json!("pay me"), &json!({}), &embed, 1));
    assert_value_error("err/empty list", || shortlist(&json!("pay me"), &json!([]), &embed, 1));
    assert_value_error("err/duplicate label", || {
        shortlist(&json!("pay me"), &json!(["alpha", "alpha"]), &embed, 1)
    });
}

#[test]
fn missing_criteria_raises() {
    let embed = TableEmbed::from_pairs(&option_texts_table());
    let mut agent = Recorder::new();
    let err = predict_shortlist(
        &mut agent,
        &json!("pay me"),
        json!({"intent": {"type": "choice"}}).as_object().expect("map"),
        &embed,
        1,
    )
    .expect_err("missing criteria");
    assert!(
        matches!(err, Error::Value(_)),
        "err/missing criteria raises ValueError"
    );
}

#[test]
fn bad_embed_shape_raises_before_predict() {
    struct BadShape;
    impl EmbedFn for BadShape {
        fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f64>>> {
            Ok(vec![vec![0.0; 4]])
        }
    }
    let (full, _sentinel) = full_criteria();
    let mut agent = Recorder::new();
    let err = predict_shortlist(
        &mut agent,
        &json!("pay me"),
        json!({"intent": {"type": "choice", "instructions": "Which desk?", "criteria": full}})
            .as_object()
            .expect("map"),
        &BadShape,
        2,
    )
    .expect_err("bad embed shape");
    assert!(matches!(err, Error::Value(_)), "err/bad embed shape");
    assert_eq!(agent.calls.len(), 0, "err/bad shape does not call predict");
}

#[test]
fn predict_must_return_a_dict() {
    struct NullRunner;
    impl PredictRunner for NullRunner {
        fn predict(&mut self, _state: &Value, _questions: &Map<String, Value>) -> Result<Value> {
            Ok(Value::Null)
        }
    }
    let mut runner = NullRunner;
    let err = predict_shortlist(
        &mut runner,
        &json!("pay me"),
        json!({"intent": {"type": "choice", "criteria": criteria_value()}})
            .as_object()
            .expect("map"),
        &BoomEmbed,
        4,
    )
    .expect_err("predict must return a dict");
    assert!(
        matches!(err, Error::Value(ref m) if m.contains("must return a dict")),
        "err/non-dict predict return: {err:?}"
    );
}

