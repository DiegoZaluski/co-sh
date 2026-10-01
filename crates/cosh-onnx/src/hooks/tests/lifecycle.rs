//! Ported tests: the ONNXAgent parity, mutation and context sections of
//! `tests/test_hooks.py` — start fires before the encode, rewrites reach the
//! sequence, the run_id is shared per call and the end context carries usage.

use super::*;

// ------------------------------------------------- ONNXAgent parity (upstream ~897-920)
#[test]
fn onnx_hooks_fire_start_then_end_and_see_the_model_id() {
    let _registry = registry_isolation();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let models = Arc::new(Mutex::new(Vec::new()));
    let mut agent = agent();
    agent.model_id = "acme/published-model".to_string();
    let models2 = Arc::clone(&models);
    let seen_start = Arc::clone(&seen);
    let seen_end = Arc::clone(&seen);
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(move |_ctx: &mut PredictContext| {
            seen_start.lock().unwrap().push("start".to_string());
            Ok(())
        })),
        on_predict_end: Some(Arc::new(move |ctx: &mut PredictContext| {
            seen_end.lock().unwrap().push("end".to_string());
            models2.lock().unwrap().push(ctx.model.clone().unwrap());
            Ok(())
        })),
        ..PerCall::default()
    };
    let out = agent
        .system_one(&json!("s"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["start", "end"]);
    // onnx/returns the inference result (the stub decode, not a hook payload).
    assert_eq!(out["model"], json!("laya-rl-agent-onnx"));
    // onnx/context model is the agent model_id.
    assert_eq!(*models.lock().unwrap(), vec!["acme/published-model"]);
}

#[test]
fn onnx_skip_short_circuits_inference() {
    let _registry = registry_isolation();
    // `_never` upstream: the session must never run when a start hook skips.
    let mut agent = agent();
    agent.session = Box::new(FailingSession);
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(|ctx: &mut PredictContext| {
            ctx.skip(vec![json!({"model": "cached"})]);
            Ok(())
        })),
        ..PerCall::default()
    };
    let out = agent
        .system_one(&json!("s"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(out, json!({"model": "cached"}));
}

// ------------------------------------------------- mutation (upstream ~127-149)
#[test]
fn start_hook_rewrite_is_what_gets_encoded() {
    let _registry = registry_isolation();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = recorded_agent(Arc::clone(&seen));
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(|ctx: &mut PredictContext| {
            ctx.states = vec![json!("rewritten")];
            Ok(())
        })),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("orig"), &questions(), None, None, None, &per_call)
        .unwrap();
    let seen = seen.lock().unwrap();
    // The first encode call carries the rewritten state (the shared state
    // text goes through serialize_state + mask replacement).
    assert!(
        seen[0].contains("rewritten") && !seen[0].contains("orig"),
        "start rewrite is what gets encoded: {:?}",
        *seen
    );
}

#[test]
fn end_hook_rewrite_is_returned() {
    let _registry = registry_isolation();
    let agent = agent();
    let per_call = PerCall {
        on_predict_end: Some(Arc::new(|ctx: &mut PredictContext| {
            ctx.results = Some(vec![json!({"model": "replaced"})]);
            Ok(())
        })),
        ..PerCall::default()
    };
    let out = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(out, json!({"model": "replaced"}));
}

// ------------------------------------------------- context semantics (upstream ~65-125)
#[test]
fn run_id_is_shared_by_start_and_end_and_differs_per_call() {
    let _registry = registry_isolation();
    let agent = agent();
    let ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..2 {
        let ids2 = Arc::clone(&ids);
        let ids3 = Arc::clone(&ids);
        let per_call = PerCall {
            on_predict_start: Some(Arc::new(move |ctx: &mut PredictContext| {
                ids2.lock().unwrap().push(ctx.run_id.clone());
                Ok(())
            })),
            on_predict_end: Some(Arc::new(move |ctx: &mut PredictContext| {
                ids3.lock().unwrap().push(ctx.run_id.clone());
                Ok(())
            })),
            ..PerCall::default()
        };
        agent
            .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
            .unwrap();
    }
    let ids = ids.lock().unwrap();
    // Per call: start and end share one run_id; across calls they differ.
    assert_eq!(ids[0], ids[1]);
    assert_eq!(ids[2], ids[3]);
    assert_ne!(ids[0], ids[2]);
    assert!(!ids[0].is_empty());
}

#[test]
fn end_context_sees_usage_and_elapsed_ms() {
    let _registry = registry_isolation();
    let agent = agent();
    let usage: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let elapsed: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
    let usage2 = Arc::clone(&usage);
    let elapsed2 = Arc::clone(&elapsed);
    let per_call = PerCall {
        on_predict_end: Some(Arc::new(move |ctx: &mut PredictContext| {
            *usage2.lock().unwrap() = ctx.usage.clone();
            *elapsed2.lock().unwrap() = ctx.elapsed_ms;
            Ok(())
        })),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    let usage = usage.lock().unwrap().clone().unwrap();
    // The stub tokenizer encodes the state text, so the input count is
    // positive and the output count is zero (ONNX decodes without logits).
    assert!(usage["input_tokens"].as_u64().unwrap() > 0, "{}", usage);
    assert_eq!(usage["output_tokens"], json!(0));
    assert!((*elapsed.lock().unwrap()).is_some());
}
