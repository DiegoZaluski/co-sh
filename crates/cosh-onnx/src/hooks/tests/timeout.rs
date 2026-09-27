//! Ported tests: the hooks_timeout sections of `tests/test_hooks.py` —
//! overrunning hooks, raise-disabled continuation, per-call overrides, the
//! mutation write-back — plus the review edges (infinity, panics, timeout
//! under lock).

use super::*;

// ------------------------------------------------- timeout (upstream ~1096-1136)
struct SlowHook(Duration);

impl Hook for SlowHook {
    fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
        std::thread::sleep(self.0);
        Ok(())
    }

    fn hook_name(&self) -> &str {
        "slow_hook"
    }
}

#[test]
fn an_overrunning_hook_raises_timeout_error() {
    let _registry = registry_isolation();
    let mut agent = agent();
    agent.hooks = vec![Arc::new(SlowHook(Duration::from_millis(300)))];
    let per_call = PerCall {
        hooks_timeout: Some(0.05),
        ..PerCall::default()
    };
    let err = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap_err();
    assert!(matches!(err, Error::Timeout(ref m) if m.contains("exceeded")), "{}", err);
    assert!(
        err.to_string().contains("cosh-onnx: hook slow_hook exceeded 0.05s"),
        "{}",
        err
    );
}

#[test]
fn an_overrunning_hook_with_raise_disabled_continues() {
    let _registry = registry_isolation();
    let mut agent = agent();
    agent.hooks = vec![Arc::new(SlowHook(Duration::from_millis(300)))];
    let per_call = PerCall {
        hooks_timeout: Some(0.05),
        hooks_raise: Some(false),
        ..PerCall::default()
    };
    let out = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(out["model"], json!("laya-rl-agent-onnx"));
}

#[test]
fn instance_level_timeout_applies_and_per_call_overrides_it() {
    let _registry = registry_isolation();
    let mut agent = agent();
    agent.hooks = vec![Arc::new(SlowHook(Duration::from_millis(300)))];
    agent.hooks_timeout = Some(0.05);
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
    assert!(matches!(err, Error::Timeout(_)));
    // A per-call override wins over the instance default.
    let per_call = PerCall {
        hooks_timeout: Some(5.0),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
}

#[test]
fn a_fast_hook_is_unaffected_by_a_timeout() {
    let _registry = registry_isolation();
    let mut agent = agent();
    let per_call = PerCall {
        on_predict_end: Some(Arc::new(|_ctx: &mut PredictContext| Ok(()))),
        hooks_timeout: Some(1.0),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
}

#[test]
fn a_timed_hook_writes_its_mutations_back() {
    let _registry = registry_isolation();
    // The timed hook runs on a clone of the context; a timely completion is
    // written back (upstream: one shared mutable context). Observed through
    // the tokenizer: the start hook's state rewrite must reach `encode`,
    // exactly like the untimed rewrite test.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut agent = recorded_agent(Arc::clone(&seen));
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(|ctx: &mut PredictContext| {
            ctx.states = vec![json!("timed-rewrite")];
            Ok(())
        })),
        hooks_timeout: Some(1.0),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("orig"), &questions(), None, None, None, &per_call)
        .unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|text| text.contains("timed-rewrite")),
        "timed hook mutation must be written back: {:?}",
        *seen
    );
}

// ------------------------------------------------- validate_timeout (upstream ~1140-1150)
#[test]
fn validate_timeout_rejects_non_positive_values() {
    let _registry = registry_isolation();
    assert_eq!(crate::hooks::validate_timeout(None).unwrap(), None);
    assert_eq!(
        crate::hooks::validate_timeout(Some(1.5)).unwrap(),
        Some(1.5)
    );
    let err = crate::hooks::validate_timeout(Some(0.0)).unwrap_err();
    assert!(
        err.to_string()
            .contains("hooks_timeout must be a positive number or None"),
        "{}",
        err
    );
    let err = crate::hooks::validate_timeout(Some(-0.5)).unwrap_err();
    assert!(
        err.to_string()
            .contains("hooks_timeout must be a positive number or None"),
        "{}",
        err
    );
    // A zero timeout passed per call is rejected before any hook runs.
    let mut agent = agent();
    let per_call = PerCall {
        hooks_timeout: Some(0.0),
        ..PerCall::default()
    };
    let err = agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap_err();
    assert!(err.to_string().contains("hooks_timeout must be a positive"));
}


// ------------------------------------------------- timeout edge cases (review R1)
#[test]
fn validate_timeout_accepts_infinity_as_no_limit() {
    let _registry = registry_isolation();
    // Upstream accepts any positive float; an infinite wait is what None
    // means (and avoids the Duration conversion panic).
    assert_eq!(crate::hooks::validate_timeout(Some(f64::INFINITY)).unwrap(), None);
    // A huge finite timeout is also just "no limit" for our purposes.
    assert_eq!(crate::hooks::validate_timeout(Some(1.0e300)).unwrap(), None);
    // NaN is rejected with the same validation error, rendered like repr().
    let err = crate::hooks::validate_timeout(Some(f64::NAN)).unwrap_err();
    assert!(
        err.to_string()
            .contains("hooks_timeout must be a positive number or None; got nan"),
        "{}",
        err
    );
}

#[test]
fn a_panicking_hook_reports_the_panic_not_a_timeout() {
    let _registry = registry_isolation();
    struct PanickingHook;
    impl Hook for PanickingHook {
        fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
            panic!("hook exploded");
        }

        fn hook_name(&self) -> &str {
            "panicker"
        }
    }
    // With a timeout: the panic must surface as the hook's own failure, not
    // as a Timeout error.
    let mut timed = agent();
    timed.hooks = vec![Arc::new(PanickingHook)];
    let per_call = PerCall {
        hooks_timeout: Some(5.0),
        ..PerCall::default()
    };
    let err = timed
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap_err();
    assert!(
        err.to_string().contains("hook panicker panicked"),
        "{}",
        err
    );
    // Without a timeout the same hook fails with its own error (caught the
    // same way the timed worker catches it).
    let mut untimed = agent();
    untimed.hooks = vec![Arc::new(PanickingHook)];
    let err = untimed
        .system_one(
            &json!("s0"),
            &questions(),
            None,
            None,
            None,
            &PerCall::default(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("hook panicker panicked"),
        "{}",
        err
    );
}

// ------------------------------------------------- timeout + lock combination (review Y6)
#[test]
fn a_timeout_while_another_thread_holds_the_lock_times_out_cleanly() {
    let _registry = registry_isolation();
    // The slow hook holds the lock well past the timeout; the second dispatch
    // waits on the lock inside the timeout window and must report a Timeout,
    // leaving the lock usable afterwards.
    let lock = Arc::new(Mutex::new(()));
    let slow: SharedHook = Arc::new(SlowHook(Duration::from_millis(300)));
    let holder_lock = Arc::clone(&lock);
    let holder: SharedHook = Arc::clone(&slow);
    let holder = std::thread::spawn(move || {
        let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
        dispatch(&[holder], HookEvent::PredictStart, &mut ctx, true, Some(&holder_lock), None)
    });
    std::thread::sleep(Duration::from_millis(50));
    let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
    let err = dispatch(
        &[Arc::clone(&slow)],
        HookEvent::PredictStart,
        &mut ctx,
        true,
        Some(&lock),
        Some(0.05),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::Timeout(ref m) if m.contains("exceeded")),
        "{}",
        err
    );
    holder.join().unwrap().unwrap();
    // The lock is still usable after the timeout window.
    let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
    dispatch(&[Arc::clone(&slow)], HookEvent::PredictStart, &mut ctx, true, Some(&lock), None)
        .unwrap();
}

