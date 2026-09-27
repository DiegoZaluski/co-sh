//! Ported tests: the process-wide default-hook registry of
//! `tests/test_hooks.py` (defaults run first, append/clear semantics) and the
//! defaults-set-after-construction edge from the review.

use super::*;

// ------------------------------------------------- process-wide defaults (upstream ~1000-1040)
#[test]
fn defaults_run_before_installed_and_per_call_hooks() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_default = Arc::clone(&log);
    crate::hooks::set_default_hooks(
        Vec::new(),
        None,
        Some(Arc::new(move |_ctx: &mut PredictContext| {
            log_default.lock().unwrap().push("default".to_string());
            Ok(())
        })),
    );
    let mut agent = agent();
    agent.add_hook(tag(&log, "installed"));
    let log2 = Arc::clone(&log);
    let per_call = PerCall {
        on_predict_end: Some(Arc::new(move |_ctx: &mut PredictContext| {
            log2.lock().unwrap().push("percall".to_string());
            Ok(())
        })),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    clear_default_hooks();
    // End order: defaults, then installed, then per-call (start has no
    // default start hook, so only the installed hook logs there).
    assert_eq!(
        *log.lock().unwrap(),
        vec!["installed:start", "default", "installed:end", "percall"]
    );
    assert!(default_hooks().is_empty());
}

#[test]
fn add_default_hook_appends_in_order_and_clear_empties() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    crate::hooks::add_default_hook(tag(&log, "a"));
    crate::hooks::add_default_hook(tag(&log, "b"));
    let mut agent = agent();
    agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap();
    clear_default_hooks();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["a:start", "b:start", "a:end", "b:end"]
    );
    assert!(default_hooks().is_empty());
}


// ------------------------------------------------- defaults set after construction (review Y6)
#[test]
fn defaults_set_after_construction_still_apply() {
    let _registry = registry_isolation();
    clear_default_hooks();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // The agent is built before any default hook exists.
    let mut agent = agent();
    // ...and the defaults are installed only afterwards: compose_hooks reads
    // the registry at call time, so they must still fire.
    crate::hooks::add_default_hook(tag(&log, "late"));
    agent
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap();
    clear_default_hooks();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["late:start", "late:end"]
    );
}
