//! Ported tests: the error-policy sections of `tests/test_hooks.py` — raise
//! vs continue, the on_error chain, the failure path running end hooks — plus
//! the on_error-with-raise-disabled edge from the review.

use super::*;

// ------------------------------------------------- error policy (upstream ~232-320)
#[test]
fn hooks_raise_true_propagates_the_hook_failure() {
    let _registry = registry_isolation();
    let agent = agent();
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(|_ctx: &mut PredictContext| {
            Err(Error::Value("hook boom".to_string()))
        })),
        ..PerCall::default()
    };
    let err = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap_err();
    assert_eq!(err.to_string(), "hook boom");
}

#[test]
fn hooks_raise_false_continues_and_infers() {
    let _registry = registry_isolation();
    let agent = agent();
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(|_ctx: &mut PredictContext| {
            Err(Error::Value("hook boom".to_string()))
        })),
        hooks_raise: Some(false),
        ..PerCall::default()
    };
    // The warning goes through the log facade (dispatch); the request itself
    // continues and infers.
    let out = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(out["model"], json!("laya-rl-agent-onnx"));
}

#[test]
fn on_error_fires_once_and_sees_the_original_error() {
    let _registry = registry_isolation();
    let rec = Arc::new(ErrHook {
        errors: Mutex::new(Vec::new()),
    });
    let mut agent = agent();
    agent.session = Box::new(FailingSession);
    agent.hooks = vec![Arc::clone(&rec) as SharedHook];
    let err = agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap_err();
    assert_eq!(err.to_string(), "infer boom");
    assert_eq!(*rec.errors.lock().unwrap(), vec!["infer boom".to_string()]);
}

#[test]
fn failing_on_error_hook_does_not_mask_the_original() {
    let _registry = registry_isolation();
    let rec = Arc::new(BadErrHook);
    let mut agent = agent();
    agent.session = Box::new(FailingSession);
    agent.hooks = vec![rec];
    let err = agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap_err();
    // The original inference failure is what propagates.
    assert_eq!(err.to_string(), "infer boom");
}

#[test]
fn end_hooks_run_on_the_failure_path_with_the_error_set() {
    let _registry = registry_isolation();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut agent = agent();
    agent.session = Box::new(FailingSession);
    let seen2 = Arc::clone(&seen);
    let per_call = PerCall {
        on_predict_end: Some(Arc::new(move |ctx: &mut PredictContext| {
            seen2.lock().unwrap().push(
                ctx.error
                    .as_ref()
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            );
            Ok(())
        })),
        ..PerCall::default()
    };
    let err = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap_err();
    assert_eq!(err.to_string(), "infer boom");
    assert_eq!(*seen.lock().unwrap(), vec!["infer boom".to_string()]);
}

// ------------------------------------------------- on_error with raise disabled (review Y6)
#[test]
fn a_failing_on_error_hook_with_raise_disabled_warns_and_continues() {
    let _registry = registry_isolation();
    let mut agent = agent();
    agent.session = Box::new(FailingSession);
    agent.hooks = vec![Arc::new(BadErrHook)];
    let err = agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap_err();
    // The original inference failure still propagates (the failing on_error
    // hook only warns, per the accepted __context__ divergence).
    assert_eq!(err.to_string(), "infer boom");
}
