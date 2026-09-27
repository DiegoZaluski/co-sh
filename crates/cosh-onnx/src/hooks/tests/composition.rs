//! Ported tests: the installed-vs-per-call, dynamic-registration and
//! composition/dispatch sections of `tests/test_hooks.py` and the
//! `hooks.rs` unit checks (aggregate_usage, compose_hooks, dispatch,
//! serialising lock).

use super::*;

// ------------------------------------------------- installed vs per-call (upstream ~157-176)
#[test]
fn installed_and_per_call_hooks_are_additive_and_ordered() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut agent = agent();
    agent.hooks = vec![tag(&log, "installed")];
    let log2 = Arc::clone(&log);
    let per_call = PerCall {
        on_predict_start: Some(Arc::new(move |_ctx: &mut PredictContext| {
            log2.lock().unwrap().push("percall".to_string());
            Ok(())
        })),
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    // The installed hook fires on start and end around the per-call hook.
    assert_eq!(
        *log.lock().unwrap(),
        vec!["installed:start", "percall", "installed:end"]
    );
}

/// A hook that appends one number per event (`counter` upstream).
struct Counter {
    seq: Arc<Mutex<Vec<u32>>>,
    n: u32,
}

impl Hook for Counter {
    fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
        self.seq.lock().unwrap().push(self.n);
        Ok(())
    }

    fn hook_name(&self) -> &str {
        "counter"
    }
}

#[test]
fn per_call_hook_sequences_run_in_order() {
    let _registry = registry_isolation();
    let seq: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let agent = agent();
    let per_call = PerCall {
        hooks: vec![
            Arc::new(Counter { seq: Arc::clone(&seq), n: 1 }) as SharedHook,
            Arc::new(Counter { seq: Arc::clone(&seq), n: 2 }) as SharedHook,
        ],
        ..PerCall::default()
    };
    agent
        .system_one(&json!("s0"), &questions(), None, None, None, &per_call)
        .unwrap();
    assert_eq!(*seq.lock().unwrap(), vec![1, 2]);
}


// ------------------------------------------------- dynamic registration (upstream ~349-390)
#[test]
fn add_hook_fires_and_remove_hook_removes_by_identity() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut agent = agent();
    let hook = tag(&log, "a");
    agent.add_hook(hook);
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
    assert_eq!(*log.lock().unwrap(), vec!["a:start", "a:end"]);

    let hook = agent.hooks[0].clone();
    assert!(agent.remove_hook(&hook));
    log.lock().unwrap().clear();
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
    assert!(log.lock().unwrap().is_empty());

    let unknown: SharedHook = Arc::new(Tag { log: Arc::new(Mutex::new(Vec::new())), tag: "x" });
    assert!(!agent.remove_hook(&unknown));
}

#[test]
fn hooks_installed_scopes_hooks_to_the_closure() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut agent = agent();
    agent.add_hook(tag(&log, "installed"));
    {
        let temp = vec![tag(&log, "temp")];
        agent.hooks_installed(temp, |agent| {
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
        });
    }
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
    // Inside the closure the temp hook is active; after it the temp hook is
    // gone and only the installed hook fires again.
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "installed:start",
            "temp:start",
            "installed:end",
            "temp:end",
            "installed:start",
            "installed:end"
        ]
    );
}


// ------------------------------------------------- hooks.rs units
#[test]
fn aggregate_usage_sums_the_per_state_blocks() {
    let _registry = registry_isolation();
    // Missing usage entries count as zero, like `(r.get("usage") or {}).get(key, 0) or 0`.
    let results = vec![
        json!({"usage": {"input_tokens": 5, "output_tokens": 0}}),
        json!({"usage": {"input_tokens": 7}}),
        json!({}),
        json!({"usage": null}),
    ];
    assert_eq!(
        aggregate_usage(&results),
        json!({"input_tokens": 12, "output_tokens": 0})
    );
}

#[test]
fn compose_hooks_orders_defaults_installed_and_per_call() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    crate::hooks::set_default_hooks(vec![tag(&log, "default")], None, None);
    let installed = vec![tag(&log, "installed")];
    let per_call = PerCall {
        hooks: vec![tag(&log, "percall")],
        ..PerCall::default()
    };
    let active = compose_hooks(&installed, &per_call);
    clear_default_hooks();
    let names: Vec<String> = active.iter().map(|h| h.hook_name().to_string()).collect();
    assert_eq!(names, vec!["default", "installed", "percall"]);
}

#[test]
fn dispatch_runs_each_event_on_the_hooks_that_implement_it() {
    let _registry = registry_isolation();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
    let hooks = vec![tag(&log, "t")];
    dispatch(&hooks, HookEvent::PredictStart, &mut ctx, true, None, None).unwrap();
    dispatch(&hooks, HookEvent::PredictEnd, &mut ctx, true, None, None).unwrap();
    // on_route/on_load/on_evict/on_error are no-ops on this hook.
    dispatch(&hooks, HookEvent::Route, &mut ctx, true, None, None).unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["t:start", "t:end"]);
}

#[test]
fn a_failing_dispatch_with_raise_disabled_returns_ok() {
    let _registry = registry_isolation();
    let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
    let hooks: Vec<SharedHook> = vec![Arc::new(BoomHook)];
    dispatch(&hooks, HookEvent::PredictStart, &mut ctx, false, None, None).unwrap();
    // With raise enabled the same hook propagates.
    let err = dispatch(&hooks, HookEvent::PredictStart, &mut ctx, true, None, None).unwrap_err();
    assert_eq!(err.to_string(), "hook boom");
}

#[test]
fn a_serialising_lock_runs_hooks_one_at_a_time() {
    let _registry = registry_isolation();
    // hooks_concurrent=False installs a lock; dispatch takes it around every
    // hook call. Mirrors the upstream concurrency check (max_active == 1
    // under 5 threads): a shared hook counts overlapping executions while two
    // threads dispatch against the same lock.
    struct ActiveProbe {
        active: std::sync::atomic::AtomicUsize,
        max_active: std::sync::atomic::AtomicUsize,
    }
    impl Hook for ActiveProbe {
        fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
            let now = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_active.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn hook_name(&self) -> &str {
            "active_probe"
        }
    }
    let probe = Arc::new(ActiveProbe {
        active: std::sync::atomic::AtomicUsize::new(0),
        max_active: std::sync::atomic::AtomicUsize::new(0),
    });
    let lock = Arc::new(Mutex::new(()));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let probe = Arc::clone(&probe);
        let lock = Arc::clone(&lock);
        handles.push(std::thread::spawn(move || {
            let mut ctx = PredictContext::new(vec![], Map::new(), None, None, None);
            dispatch(
                &[Arc::clone(&probe) as SharedHook],
                HookEvent::PredictStart,
                &mut ctx,
                true,
                Some(&lock),
                None,
            )
            .unwrap();
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(
        probe.max_active.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the lock must serialise hook executions"
    );
}

