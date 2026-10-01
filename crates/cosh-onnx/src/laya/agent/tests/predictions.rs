use super::*;

// ------------------------------------------------- test_temperature_loading.py: assert_predictions
/// `assert_predictions` with the fixed logits from the upstream test: with
/// `logits = [[0, 1, -1e4], [0, 1, 2], [0, 1, -1e4]]` the expected
/// probabilities are `softmax(arange(k) / temperature)`, independent of any
/// tokenizer or weights.
fn prediction_agent(
    temperature: [f64; 3],
    temperature_by_options: HashMap<String, f64>,
) -> OnnxAgent {
    let cfg = json!({"max_len": 64, "head_max_len": 32});
    bare_agent(
        cfg.as_object().unwrap().clone(),
        Box::new(FakeTok),
        Box::new(StubSession {
            logits: vec![
                vec![0.0, 1.0, -1e4],
                vec![0.0, 1.0, 2.0],
                vec![0.0, 1.0, -1e4],
            ],
            act_logits: vec![vec![0.0, 0.0], vec![0.0, 0.0], vec![0.0, 0.0]],
        }),
        temperature,
        temperature_by_options,
        HashMap::new(),
    )
}

fn assert_predictions(agent: &mut OnnxAgent, choice: f64, score: f64, noul: f64) {
    let questions = json!({
        "choice": {"type": "choice", "instructions": "Pick one", "criteria": ["a", "b"]},
        "score": {"type": "score", "instructions": "Rate", "criteria": ["low", "mid", "high"]},
        "noul": {"type": "noul", "instructions": "True?"}
    })
    .as_object()
    .unwrap()
    .clone();
    let out = agent
        .infer(&json!("hello"), &questions, None, None, None)
        .unwrap();
    let answers = &out["answers"];
    // softmax(arange(k) / temperature): the choice (k=2) and noul (k=2) read
    // the same row shape, the score (k=3) the wider one.
    let p2: Vec<f64> = (0..2).map(|i| (i as f64 / choice).exp()).collect();
    let z2: f64 = p2.iter().sum();
    let p2: Vec<f64> = p2.iter().map(|v| v / z2).collect();
    let actual: Vec<f64> = answers["choice"]["probabilities"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_f64)
        .collect();
    assert_eq!(actual.len(), 2);
    for (got, want) in actual.iter().zip(&p2) {
        assert!((got - want).abs() <= 1e-4, "choice: {} vs {}", got, want);
    }
    let p3: Vec<f64> = (0..3).map(|i| (i as f64 / score).exp()).collect();
    let z3: f64 = p3.iter().sum();
    let p3: Vec<f64> = p3.iter().map(|v| v / z3).collect();
    let actual: Vec<f64> = answers["score"]["probabilities"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_f64)
        .collect();
    assert_eq!(actual.len(), 3);
    for (got, want) in actual.iter().zip(&p3) {
        assert!((got - want).abs() <= 1e-4, "score: {} vs {}", got, want);
    }
    let pn: Vec<f64> = (0..2).map(|i| (i as f64 / noul).exp()).collect();
    let zn: f64 = pn.iter().sum();
    let want = pn[1] / zn;
    let got = answers["noul"]["noul"].as_f64().unwrap();
    assert!((got - want).abs() <= 1e-4, "noul: {} vs {}", got, want);
}

#[test]
fn predictions_follow_the_applied_temperatures() {
    let mut agent = prediction_agent([1.0, 2.0, 3.0], HashMap::new());
    assert_predictions(&mut agent, 1.0, 2.0, 3.0);
}

#[test]
fn bucket_temperatures_keep_precedence_over_type_temperatures() {
    let mut buckets: HashMap<String, f64> = HashMap::new();
    buckets.insert("choice:2".to_string(), 1.5);
    let mut agent = prediction_agent([2.0, 3.0, 4.0], buckets);
    // choice reads its bucket (1.5); score and noul fall back to their types.
    assert_predictions(&mut agent, 1.5, 3.0, 4.0);
}
