use super::*;

// --------------------------------------------------------------- mock predict sees only k criteria

#[test]
fn mock_predict_sees_only_the_shortlist() {
    let (full, sentinel) = full_criteria();
    // cosine vs [1, 0]: tech=1, sales=0.707, billing=0, other=0. k=2 ->
    // tech, sales.
    let agent = Recorder::new();
    let questions = json!({
        "intent": {"type": "choice", "instructions": "Which desk?", "criteria": full},
        "urgency": score_question(),
        "refund": noul_question(),
        "note": "leave me alone",
    });
    let state = json!("I was charged twice");
    let result = predict_shortlist(
        &agent,
        &state,
        questions.as_object().expect("map"),
        &TableEmbed::from_pairs(&full_vectors()),
        2,
    )
    .expect("predict_shortlist");

    assert_eq!(
        agent.calls.lock().unwrap_or_else(|e| e.into_inner()).len(),
        1,
        "predict/called once"
    );
    let (got_state, got_questions) = {
        let calls = agent.calls.lock().unwrap_or_else(|e| e.into_inner());
        (calls[0].0.clone(), calls[0].1.clone())
    };
    assert_eq!(got_state, state, "predict/state is the same value");
    let intent = &got_questions["intent"];
    let kept: Vec<&String> = intent["criteria"]
        .as_object()
        .expect("map")
        .keys()
        .collect();
    assert_eq!(
        kept,
        ["tech", "sales"],
        "predict/choice criteria are only the top 2"
    );
    assert_eq!(
        intent["criteria"]["tech"], "bugs",
        "predict/kept description is the original value"
    );
    assert_eq!(
        got_questions["urgency"], questions["urgency"],
        "predict/score question is unchanged"
    );
    assert_eq!(
        got_questions["refund"], questions["refund"],
        "predict/noul question is unchanged"
    );
    assert_eq!(
        got_questions["note"], questions["note"],
        "predict/non-dict question is unchanged"
    );
    assert_eq!(
        questions["intent"]["criteria"], full,
        "caller/criteria unchanged"
    );
    assert_eq!(full["billing"], sentinel, "caller/sentinel intact");

    let internal = to_internal(intent).expect("internal");
    let crit_keys: Vec<&String> = internal["crit"].as_object().expect("map").keys().collect();
    assert_eq!(
        crit_keys,
        ["tech", "sales"],
        "predict/internal choice keys are the shortlist"
    );
    assert_eq!(
        result["answers"]["intent"]["choice"],
        json!("tech"),
        "result/choice is the mock's first shortlisted label"
    );
    let shortlist_meta = &result["shortlist"]["intent"];
    assert_eq!(
        shortlist_meta["labels"],
        json!(["tech", "sales"]),
        "result/shortlist labels"
    );
    let scores: Vec<f64> = shortlist_meta["scores"]
        .as_array()
        .expect("scores")
        .iter()
        .map(|v| v.as_f64().expect("number"))
        .collect();
    assert!(
        scores[0] > scores[1] && scores[1] > 0.0,
        "result/scores descend"
    );
    assert_eq!(
        (shortlist_meta["k"].as_i64(), shortlist_meta["n"].as_i64()),
        (Some(2), Some(4)),
        "result/shortlist k and n"
    );
    assert_eq!(
        shortlist_meta["passthrough"],
        json!(false),
        "result/not a passthrough"
    );
    assert!(
        result["shortlist"].get("urgency").is_none(),
        "result/non-choice questions are absent from shortlist meta"
    );
}

#[test]
fn shortlist_key_is_on_the_copy() {
    // The predict return is copied before shortlist is attached.
    struct Holding {
        seen: std::sync::Mutex<Option<Map<String, Value>>>,
    }
    impl PredictRunner for Holding {
        fn predict(&self, _state: &Value, questions: &Map<String, Value>) -> Result<Value> {
            *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(questions.clone());
            Ok(json!({"model": "fake", "answers": {}}))
        }
    }
    let runner = Holding {
        seen: std::sync::Mutex::new(None),
    };
    let out = predict_shortlist(
        &runner,
        &json!("pay me"),
        json!({"intent": {"type": "choice", "criteria": criteria_value()}})
            .as_object()
            .expect("map"),
        &BoomEmbed,
        4,
    )
    .expect("predict_shortlist");
    assert!(
        out.get("shortlist").is_some()
            && runner
                .seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .expect("seen")
                .get("shortlist")
                .is_none(),
        "result/shortlist key is on the copy"
    );
}

#[test]
fn pass_through_reaches_predict_unchanged() {
    let (full, _sentinel) = full_criteria();
    let original_q = json!({"type": "choice", "instructions": "Which desk?", "criteria": full});
    let agent = Recorder::new();
    let out = predict_shortlist(
        &agent,
        &json!("I was charged twice"),
        json!({"intent": original_q}).as_object().expect("map"),
        &BoomEmbed,
        4,
    )
    .expect("predict_shortlist");
    assert_eq!(
        out["shortlist"]["intent"]["passthrough"],
        json!(true),
        "pass/flag"
    );
    let agent99 = Recorder::new();
    let out99 = predict_shortlist(
        &agent99,
        &json!("x"),
        json!({"intent": {"type": "choice", "instructions": "Which desk?", "criteria": full}})
            .as_object()
            .expect("map"),
        &BoomEmbed,
        99,
    )
    .expect("predict_shortlist k=99");
    assert_eq!(
        out99["shortlist"]["intent"]["passthrough"],
        json!(true),
        "pass/k > n flag"
    );
}

// --------------------------------------------------------------- list criteria stay a list, in rank order
#[test]
fn list_criteria_reach_predict_in_rank_order() {
    let list_q_embed = TableEmbed::from_pairs(&[
        ("Which?\nhello", vec![1.0, 0.0]),
        ("alpha", vec![0.0, 1.0]),
        ("beta", vec![1.0, 0.0]),
        ("gamma", vec![0.0, 0.0]),
    ]);
    let list_questions = json!({
        "intent": {"type": "choice", "instructions": "Which?", "criteria": ["alpha", "beta", "gamma"]}
    });
    let agent = Recorder::new();
    predict_shortlist(
        &agent,
        &json!("hello"),
        list_questions.as_object().expect("map"),
        &list_q_embed,
        2,
    )
    .expect("predict_shortlist");
    let received_questions = {
        let calls = agent.calls.lock().unwrap_or_else(|e| e.into_inner());
        calls[0].1.clone()
    };
    let received = &received_questions["intent"]["criteria"];
    assert_eq!(
        received,
        &json!(["beta", "alpha"]),
        "list/predict receives a list of k labels"
    );
    assert_eq!(
        list_questions["intent"]["criteria"],
        json!(["alpha", "beta", "gamma"]),
        "list/caller criteria unchanged"
    );
    // Agent._to_internal accepts them and keys follow the shortlist
    let internal = to_internal(&received_questions["intent"]).expect("internal");
    let crit_keys: Vec<&String> = internal["crit"].as_object().expect("map").keys().collect();
    assert_eq!(
        crit_keys,
        ["beta", "alpha"],
        "list/internal keys follow the shortlist"
    );
}
