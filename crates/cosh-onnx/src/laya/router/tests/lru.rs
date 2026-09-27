//! Resident-cache behaviour: the temperature anchors beside the LRU
//! section, LRU bookkeeping, the default cap (#172) and the memory-release
//! test.

use super::*;

// ------------------------------------------------------- temperature clamp
// The clamp table is ported in `decision/tests/confidence.rs` (the same
// upstream table appears in `test_common.py`); only the anchors upstream keeps beside the
// LRU section are repeated here, so a routing change cannot move them.
#[test]
fn temperature_anchors_beside_the_lru_section() {
    use crate::decision::confidence::{clamp_temperature_default, temp_bucket};
    use crate::decision::question::qtype_code;
    assert_eq!(clamp_temperature_default(&json!(0.1006)), 0.5, "clamp/pathological sharpening");
    assert_eq!(
        clamp_temperature_default(&json!(0.10058280825614929)),
        TEMP_MIN,
        "clamp/shipped choice:11+ is rejected"
    );
    assert_eq!(clamp_temperature_default(&json!(1.0)), 1.0, "clamp/neutral untouched");
    assert_eq!(clamp_temperature_default(&json!(9.0)), TEMP_MAX, "clamp/upper bound");
    // 13 options is the bucket the reported skill-router landed in
    assert_eq!(temp_bucket(qtype_code("choice").unwrap(), 13), "choice:11+");
}


#[test]
fn lru_bookkeeping() {
    let rr = RecordingRouter::new(RouterOptions { max_loaded: Some(1), ..Default::default() });
    rr.router.load("english").expect("load");
    rr.router.load("multilingual").expect("load");
    assert_eq!(
        rr.router.loaded(),
        ["multilingual"],
        "lru/cap 1 keeps newest"
    );
    // `lru/cap 1 agents match order`: the two views must agree.
    let residents: Vec<String> = {
        rr.router
            .lock_lifecycle()
            .agents
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    };
    let mut sorted = residents.clone();
    sorted.sort();
    assert_eq!(sorted, ["multilingual"], "lru/cap 1 agents match order");
    assert_eq!(rr.builds(), ["english", "multilingual"]);

    let rr = RecordingRouter::new(RouterOptions { max_loaded: Some(2), ..Default::default() });
    rr.router.load("english").expect("load");
    rr.router.load("multilingual").expect("load");
    rr.router.load("typed-decisions").expect("load");
    assert_eq!(
        rr.router.loaded(),
        ["multilingual", "typed-decisions"],
        "lru/cap 2 evicts oldest"
    );

    let rr = RecordingRouter::new(RouterOptions { max_loaded: Some(2), ..Default::default() });
    rr.router.load("english").expect("load");
    rr.router.load("multilingual").expect("load");
    rr.router.load("english").expect("load"); // touch english
    rr.router.load("typed-decisions").expect("load");
    let mut loaded = rr.router.loaded();
    loaded.sort();
    assert_eq!(loaded, ["english", "typed-decisions"], "lru/touch protects");

    rr.router.unload(Some("english")).expect("unload");
    assert!(
        !rr.router.loaded().contains(&"english".to_string()),
        "lru/unload one"
    );
    rr.router.unload(None).expect("unload");
    assert!(rr.router.loaded().is_empty(), "lru/unload all");
}

// ------------------------------------------------- default cap (#172)
// A workload that alternates languages at 20-23 s per request on CPU
// (reloading a checkpoint every request) against 49-136 ms with both
// resident. Automatic routing only ever chooses between `english` and
// `multilingual`, so the default holds both.
#[test]
fn the_default_cap_matches_the_alternating_workload() {
    assert_eq!(Router::new().expect("router").max_loaded(), 2, "lru/default is two");

    let en = json!({"body": "I was charged twice for invoice 4411, please refund."});
    let ml = json!({"body": "Der Kunde wurde zweimal belastet und moechte eine Rueckerstattung"});
    let generic = q_generic();

    for (cap, want_built) in [(1usize, 20usize), (2usize, 2usize)] {
        let cr = RecordingRouter::new(RouterOptions { max_loaded: Some(cap), ..Default::default() });
        for _ in 0..10 {
            // the reported alternating workload
            cr.router
                .predict(&en, &generic, &PredictOptions::default())
                .expect("predict");
            cr.router
                .predict(&ml, &generic, &PredictOptions::default())
                .expect("predict");
        }
        assert_eq!(
            cr.builds().len(),
            want_built,
            "lru/alternating traffic, cap={cap} builds"
        );
    }

    // and the default costs a single-language deployment nothing at all
    let cr = RecordingRouter::new(RouterOptions::default());
    for _ in 0..5 {
        cr.router
            .predict(&en, &generic, &PredictOptions::default())
            .expect("predict");
    }
    assert_eq!(
        cr.builds(),
        ["english"],
        "lru/single-language traffic builds one checkpoint"
    );
}


// ------------------------------------------------------------- memory test
// `tests/test_router_memory.py`: attach two mock agents, unload one, unload
// all.
#[test]
fn evict_and_unload_memory_release() {
    struct MockAgent;
    impl AgentLike for MockAgent {
        fn system_one(
            &self,
            _state: &Value,
            _questions: &Map<String, Value>,
            _lang: Option<&str>,
            _max_len: Option<usize>,
            _head_max_len: Option<usize>,
        ) -> Result<Value> {
            Ok(json!({}))
        }
    }

    let r = Router::configure(RouterOptions { max_loaded: Some(1), ..Default::default() })
        .expect("router");
    r.attach("english", Box::new(MockAgent)).expect("attach");
    r.attach("multilingual", Box::new(MockAgent)).expect("attach");
    assert_eq!(r.loaded().len(), 2, "attach/both attached stay resident");

    // Trigger unload of specific model
    r.unload(Some("english")).expect("unload");
    assert!(!r.loaded().contains(&"english".to_string()), "unload/english gone");
    assert!(r.loaded().contains(&"multilingual".to_string()), "unload/multilingual kept");

    // Trigger unload all
    r.unload(None).expect("unload");
    assert!(r.loaded().is_empty(), "unload/all released");
}

// =====================================================================
// tests/test_router_batch.py — deterministic heterogeneous batch coverage
