//! Ported tests: the structural checks of `tests/test_hooks_api.py` (the
//! event set, the class-level hook defaults) and the BaseHook no-op section
//! of `tests/test_hooks.py`.

use super::*;

// ------------------------------------------------- test_hooks_api.py structure
#[test]
fn hook_event_set_is_the_upstream_six() {
    let _registry = registry_isolation();
    assert_eq!(
        HOOK_EVENTS,
        [
            "on_predict_start",
            "on_predict_end",
            "on_route",
            "on_load",
            "on_evict",
            "on_error"
        ]
    );
}

#[test]
fn class_defaults_match_the_upstream_hook_defaults() {
    let _registry = registry_isolation();
    // ONNXAgent class attributes: hooks_raise=True, hooks_concurrent=True,
    // hooks_timeout=None, no lock. `bare_agent` assembles exactly these.
    let agent = agent();
    assert!(agent.hooks.is_empty());
    assert!(agent.hooks_raise);
    assert!(agent.hooks_concurrent);
    assert_eq!(agent.hooks_timeout, None);
    assert!(agent.hooks_lock.is_none());
}


// ------------------------------------------------- BaseHook no-op (upstream ~963-982)
#[test]
fn a_no_op_hook_is_harmless() {
    let _registry = registry_isolation();
    struct NoopHook;
    impl Hook for NoopHook {}
    let mut agent = agent();
    agent.add_hook(Arc::new(NoopHook));
    let out = agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap();
    assert_eq!(out["model"], json!("laya-rl-agent-onnx"));
}


// ------------------------------------------------- predict alias (test_hooks_api.py)
#[test]
fn predict_is_an_alias_of_system_one() {
    let _registry = registry_isolation();
    // `ONNXAgent.predict is ONNXAgent.system_one` upstream.
    let mut agent = agent();
    let via_system_one = agent
        .system_one(
            &json!("s"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap();
    let via_predict = agent
        .predict(
            &json!("s"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap();
    assert_eq!(via_system_one, via_predict);
}

