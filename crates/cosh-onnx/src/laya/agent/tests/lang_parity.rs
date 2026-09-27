use super::*;

// ------------------------------------------------- test_onnx_lang_parity.py
#[test]
fn lang_override_changes_the_calibrated_probabilities() {
    let mut agent = bare_onnx(
        json!({"de": {"temperature": [3.0, 3.0, 3.0]}}),
        vec![vec![2.0, 1.0, 0.0], vec![2.0, 0.0, 0.0]],
        vec![vec![0.7, 0.3], vec![0.4, 0.6]],
    );
    let base = agent.infer(&json!("some state text"), &questions(), None, None, None).unwrap();
    let de = agent
        .infer(&json!("some state text"), &questions(), Some("de"), None, None)
        .unwrap();
    let missing = agent
        .infer(&json!("some state text"), &questions(), Some("fr"), None, None)
        .unwrap();

    let p_base = base["answers"]["dept"]["probabilities"]["billing"]
        .as_f64()
        .unwrap();
    let p_de = de["answers"]["dept"]["probabilities"]["billing"]
        .as_f64()
        .unwrap();
    let p_missing = missing["answers"]["dept"]["probabilities"]["billing"]
        .as_f64()
        .unwrap();

    // lang/override changes the calibrated probabilities
    assert_ne!(p_base, p_de, "base={} de={}", p_base, p_de);
    // temperature 3 > 1 softens the distribution: the argmax probability falls
    assert!(p_de < p_base, "base={} de={}", p_base, p_de);
    // lang/unconfigured language falls back to base
    assert_eq!(p_missing, p_base);
    // a hyphen subtag resolves to the base language
    let de_de = agent
        .infer(&json!("some state text"), &questions(), Some("de-DE"), None, None)
        .unwrap();
    assert_eq!(
        de_de["answers"]["dept"]["probabilities"],
        de["answers"]["dept"]["probabilities"]
    );
    // the noul question is scaled the same way
    let noul_base = base["answers"]["flag"]["noul"].as_f64().unwrap();
    let noul_de = de["answers"]["flag"]["noul"].as_f64().unwrap();
    assert_ne!(noul_base, noul_de);
}

#[test]
fn lang_override_guards_the_three_float_shape() {
    // The upstream test asserts the guard exists in `__init__`'s source; here
    // the same guard runs for real through the load-time parser.
    let mut agent = bare_onnx(json!({}), vec![], vec![]);
    let err = agent
        .apply_lang_temperatures(
            &json!({"de": {"temperature": [1.0, 2.0]}})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap_err();
    match &err {
        Error::Value(msg) => assert_eq!(
            msg,
            "Language override 'de' temperature must be a list of 3 floats"
        ),
        other => panic!("expected the 3-float ValueError, got {:?}", other),
    }
    let err = agent
        .apply_lang_temperatures(
            &json!({"de": {"temperature": "no"}})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap_err();
    match &err {
        Error::Value(msg) => assert_eq!(
            msg,
            "Language override 'de' temperature must be a list of 3 floats"
        ),
        other => panic!("expected the 3-float ValueError, got {:?}", other),
    }
}

