//! Router lifecycle: preload, attach and the thread-safety guarantees
//! (#95).

use super::*;

// ---------------------------------------------------------------- preload
// Exercise the real preload/load/LRU paths; only checkpoint construction is
// stubbed (upstream patches `laya.agent.Agent`).
#[test]
fn preload_builds_each_model_once_and_raises_capacity() {
    use super::PredictOptions;
    let rp = RecordingRouter::new(RouterOptions { preload: true, ..Default::default() });
    let mut loaded = rp.router.loaded();
    loaded.sort();
    assert_eq!(
        loaded,
        ["english", "multilingual", "typed-decisions"],
        "preload/all three stay resident"
    );
    assert_eq!(rp.router.max_loaded(), 3, "preload/max_loaded raised");
    assert_eq!(rp.builds().len(), 3, "preload/builds each model once");
    // `preload/returns router` and `preload/repeated call reuses models`
    assert!(std::ptr::eq(
        rp.router.preload(None).expect("preload"),
        &rp.router as *const Router
    ));
    assert_eq!(rp.builds().len(), 3, "preload/repeated call reuses models");

    let empty = RecordingRouter::new(RouterOptions::default());
    empty.router.preload(Some(&[])).expect("preload");
    assert!(empty.router.loaded().is_empty(), "preload/empty selection leaves models unloaded");
    assert_eq!(empty.builds().len(), 0, "preload/empty selection builds nothing");

    let rp2 = RecordingRouter::new(RouterOptions::default());
    rp2.router.preload(Some(&["english", "multilingual"])).expect("preload");
    let mut loaded = rp2.router.loaded();
    loaded.sort();
    assert_eq!(loaded, ["english", "multilingual"], "preload/subset stays resident");
    assert_eq!(rp2.router.max_loaded(), 2, "preload/subset capacity");
    rp2.router.load("english").expect("load");
    let mut loaded = rp2.router.loaded();
    loaded.sort();
    assert_eq!(loaded, ["english", "multilingual"], "preload/touch does not evict");
    assert_eq!(rp2.builds().len(), 2, "preload/touch does not rebuild");

    let incremental = RecordingRouter::new(RouterOptions::default());
    incremental.router.preload(Some(&["english"])).expect("preload");
    let english = incremental.router.load("english").expect("load");
    incremental.router.preload(Some(&["multilingual"])).expect("preload");
    assert_eq!(
        incremental.router.loaded(),
        ["english", "multilingual"],
        "preload/incremental keeps both models"
    );
    assert_eq!(incremental.router.max_loaded(), 2, "preload/incremental capacity");
    // `preload/incremental reuses original`: the same handle comes back.
    let again = incremental.router.load("english").expect("load");
    assert!(
        Arc::ptr_eq(&english, &again),
        "preload/incremental reuses original"
    );
    assert_eq!(incremental.builds().len(), 2, "preload/incremental avoids rebuilds");

    incremental.router.preload(Some(&["en", "english", "multi", "ml"])).expect("preload");
    assert_eq!(
        incremental.router.max_loaded(),
        2,
        "preload/aliases do not inflate capacity"
    );
    assert_eq!(incremental.builds().len(), 2, "preload/aliases reuse models");
    incremental.router.preload(Some(&["english", "typed-decisions"])).expect("preload");
    let mut loaded = incremental.router.loaded();
    loaded.sort();
    assert_eq!(
        loaded,
        ["english", "multilingual", "typed-decisions"],
        "preload/overlap preserves unrequested models"
    );
    assert_eq!(incremental.router.max_loaded(), 3, "preload/overlap capacity");
    for name in ["english", "multilingual", "typed-decisions"] {
        incremental
            .router
            .predict(
                &json!("hello"),
                &q_generic(),
                &PredictOptions { model: Some(name), ..Default::default() },
            )
            .expect("predict");
    }
    assert_eq!(incremental.builds().len(), 3, "preload/predictions never rebuild");

    let attached = RecordingRouter::new(RouterOptions::default());
    let registered = attached
        .router
        .attach("english", Box::new(StubAgent::new()))
        .expect("attach");
    attached.router.preload(Some(&["multilingual"])).expect("preload");
    // `preload/keeps attached model`: loading returns the attached agent
    // itself (upstream asserts `attached.load("english") is original`).
    let loaded_agent = attached.router.load("english").expect("load");
    assert!(
        Arc::ptr_eq(&registered, &loaded_agent),
        "preload/keeps attached model"
    );
    let mut loaded = attached.router.loaded();
    loaded.sort();
    assert_eq!(
        loaded,
        ["english", "multilingual"],
        "preload/attached and new stay resident"
    );
    assert_eq!(attached.router.max_loaded(), 2, "preload/attached capacity");
    assert_eq!(attached.builds().len(), 1, "preload/attached model not rebuilt");

    let roomy = RecordingRouter::new(RouterOptions { max_loaded: Some(5), ..Default::default() });
    roomy.router.preload(Some(&["en", "english", "multi"])).expect("preload");
    assert_eq!(roomy.router.max_loaded(), 5, "preload/larger capacity is preserved");
    assert_eq!(roomy.builds().len(), 2, "preload/duplicates build once");
}

// ------------------------------------------------------------------ attach
#[test]
fn attach_registers_and_raises_the_cap() {
    let ra = RecordingRouter::new(RouterOptions { max_loaded: Some(1), ..Default::default() });
    let sentinel: Box<dyn AgentLike> = Box::new(StubAgent::new());
    let registered = ra.router.attach("english", sentinel).expect("attach");
    // `attach/registers under the name`: the resident under `english` is the
    // attached one (identity through the shared handle).
    let resident = ra.router.load("english").expect("load");
    assert!(Arc::ptr_eq(&registered, &resident), "attach/registers under the name");
    assert!(
        ra.router.loaded().contains(&"english".to_string()),
        "attach/counts as resident"
    );
    // `max_loaded` starts at `max(1, max_loaded)`, so one attach to a cap-1
    // router cannot move it. The cap only rises on the attach that would not
    // otherwise fit.
    assert_eq!(ra.router.max_loaded(), 1, "attach/first attach leaves the cap alone");
    ra.router
        .attach("multilingual", Box::new(StubAgent::new()))
        .expect("attach");
    assert_eq!(ra.router.max_loaded(), 2, "attach/raises max_loaded to hold the extra one");
    ra.router.unload(Some("multilingual")).expect("unload");
    // attaching then loading another must not evict the attached one
    ra.router.load("multilingual").expect("load");
    let mut loaded = ra.router.loaded();
    loaded.sort();
    assert_eq!(loaded, ["english", "multilingual"], "attach/survives a later load");
    let still = ra.router.load("english").expect("load");
    assert!(Arc::ptr_eq(&registered, &still), "attach/still the same object");
    assert!(
        Router::configure(RouterOptions { max_loaded: Some(1), ..Default::default() })
            .expect("router")
            .attach("en", Box::new(StubAgent::new()))
            .is_ok(),
        "attach/accepts aliases"
    );
}

// ---------------------------------------------------- thread safety (#95)
#[test]
fn concurrent_loads_share_one_agent() {
    // Concurrent load() of the same checkpoint must build one Agent, shared
    // by all callers. The 0.05 s construction sleep widens the
    // check-then-build window.
    let constructions = Arc::new(Mutex::new(0usize));
    let recorded = Arc::clone(&constructions);
    let router = Router::configure(RouterOptions::default())
        .expect("router")
        .with_agent_factory(move |_repo, _subfolder, _revision| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            *recorded.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            Ok(Box::new(StubAgent::new()) as Box<dyn AgentLike>)
        });
    let got: std::sync::Mutex<Vec<super::SharedAgent>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let router = &router;
            let got = &got;
            scope.spawn(move || {
                got.lock().unwrap_or_else(|e| e.into_inner()).push(router.load("english").expect("load"));
            });
        }
    });
    let got = got.lock().unwrap_or_else(|e| e.into_inner());
    let unique: usize = {
        let mut ids: Vec<usize> = got.iter().map(|a| Arc::as_ptr(a) as *const () as usize).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    assert_eq!(unique, 1, "threads/8 concurrent loads share one Agent");
    assert_eq!(*constructions.lock().unwrap_or_else(|e| e.into_inner()), 1, "threads/Agent constructed exactly once");
    let order = router.loaded();
    assert_eq!(order.len(), 1, "threads/LRU views stay consistent (one entry)");
    assert_eq!(order, ["english"], "threads/LRU views stay consistent (name)");
}

#[test]
fn concurrent_hotpath_loads_stay_consistent() {
    // Concurrent hot-path loads of an already-cached model must keep the
    // LRU order and the agent map consistent.
    let router = RecordingRouter::new(RouterOptions { max_loaded: Some(3), ..Default::default() });
    router.router.load("english").expect("load"); // warm the cache
    std::thread::scope(|scope| {
        for _ in 0..20 {
            let router = &router.router;
            scope.spawn(move || {
                router.load("english").expect("load");
            });
        }
    });
    assert_eq!(router.router.loaded().len(), 1, "threads/hot-path loads keep one entry");
    let residents = {
        router.router.lock_lifecycle().agents.len()
    };
    assert_eq!(residents, 1, "threads/hot-path loads keep agents consistent");
    assert_eq!(router.router.loaded(), ["english"], "threads/hot-path order intact");
}

