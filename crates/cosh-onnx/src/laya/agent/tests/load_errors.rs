use super::*;

// ------------------------------------------------- test_load_errors.py: options over budget
#[test]
fn options_over_budget_raise_and_name_the_question() {
    let agent = bare_onnx(json!({}), vec![], vec![]);
    // `build_sequence` never drops markers by itself: the guard fires only
    // when the whole sequence overflows `max_len` and the `[m for m in
    // markers if m < max_len]` filter cuts them. Upstream pins this with the
    // shipped budget (512/192, 140 options -> 126 markers); the same cut
    // mechanism at a scaled-down budget is max_len=16 with five 3-token
    // options, which keeps 2 of 5 markers.
    let many = json!({
        "q": {"type": "choice", "instructions": "Which department?",
              "criteria": ["a", "b", "c", "d", "e"]}
    });
    let err = agent
        .infer(
            &json!("hello"),
            many.as_object().unwrap(),
            None,
            Some(16),
            Some(3),
        )
        .unwrap_err();
    match &err {
        Error::Value(msg) => {
            // options over budget/names the question
            assert!(msg.contains("'q'"), "{}", msg);
            // options over budget/reports the budget
            assert!(msg.contains("head_max_len"), "{}", msg);
            assert_eq!(msg, "question 'q' options exceed head_max_len=3");
        }
        other => panic!("expected the over-budget ValueError, got {:?}", other),
    }
    // ...and a question that does fit still answers, so the guard is not
    // refusing everything: two long options are capped to the per-option
    // budget, both markers survive, and the head answers.
    let fits = json!({
        "q": {"type": "choice", "instructions": "Pick one",
              "criteria": {"department": null, "billing": null}}
    });
    let got = agent
        .infer(
            &json!("hello"),
            fits.as_object().unwrap(),
            None,
            Some(16),
            Some(3),
        )
        .unwrap();
    assert!(got["answers"]["q"]["choice"].is_string());
}
