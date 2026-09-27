//! Ported Router tests: `tests/test_router.py`, `tests/test_router_batch.py`,
//! `tests/test_router_memory.py`, `tests/test_lang_guess.py` and
//! `tests/test_blank_lang_routing.py`.
//!
//! Upstream patches `laya.agent.Agent` (`unittest.mock.patch` /
//! `pytest.monkeypatch`); here the same seam is [`Router::with_agent_factory`],
//! which the stubs below install. Upstream's `rr.load = ...` replacements in
//! `stubbed_router` / `counting_router` map to the same factory, which
//! exercises the real load/touch/evict path instead of replacing it.
//!
//! Structural omissions, with no Rust analogue:
//! - `route_batch` rejecting a non-sequence `requests` (`None`, `{}`,
//!   `"text"` with `TypeError "requests must be a sequence"`): the parameter
//!   is a slice here, so a non-sequence cannot be expressed; the type
//!   mismatch is a compile error rather than a `TypeError`.
//! - `analyse` over a Python `bytes` value and over mapping types that are
//!   not builtin dicts (`UserDict`, `MappingProxyType`): a JSON [`Value`]
//!   has no such variants, so those #384 cases carry no equivalent (the
//!   plain-`dict` ones are kept).
//! - `unittest.mock.patch` call-count bookkeeping: the factory below records
//!   the repos it built, which is the same observable.
//!
//! The detector-level tables upstream shares between `test_lang.py` and
//! `test_router.py` (SCRIPTS, is_english, the mixed-states table, the
//! 200,000-character cap checks) are ported once in `lang/tests.rs`; only
//! the assertions that consult the Router (`.route(...).model`, reasons,
//! decision payloads) appear here, as upstream has them.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::{
    default_models, english_from_code, match_typed_decisions_workflow, normalise_name, repo_str,
    standalone_models, LangGuess, LangGuessFn, ModelSpec, PredictOptions, RouteDecision,
    RouteOptions, Router, AgentLike, RouterOptions, SharedAgent, BUNDLE_REPO,
};
use crate::decision::confidence::{TEMP_MAX, TEMP_MIN};
use crate::error::{Error, Result};
use crate::lang::{analyse, detect_script, guess_latin_language, is_english};

/// `Q_GENERIC` and `Q_TD`, upstream's two question schemas.
fn q_generic() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(
        "dept".to_string(),
        json!({"type": "choice", "instructions": "Which team?",
               "criteria": {"billing": null, "tech": null}}),
    );
    map
}

/// `Q_TD`: the customer_service signature typed as `noul`.
fn q_td() -> Map<String, Value> {
    [
        "action",
        "category",
        "churn_risk",
        "needs_human",
        "urgency",
    ]
    .iter()
    .map(|id| (id.to_string(), json!({"type": "noul", "instructions": "x"})))
    .collect()
}

fn empty_questions() -> Map<String, Value> {
    Map::new()
}

fn route(state: &Value, questions: &Map<String, Value>, opts: &RouteOptions<'_>) -> RouteDecision {
    Router::new().expect("router").route(state, Some(questions), opts).expect("route")
}

fn default_route(state: &Value, questions: &Map<String, Value>) -> RouteDecision {
    route(state, questions, &RouteOptions::default())
}

fn route_text(text: &str) -> RouteDecision {
    default_route(&json!(text), &empty_questions())
}


// ------------------------------------------------------- LRU bookkeeping
// `_Stub` and the stubbed loaders: upstream replaces `rr.load`; here the
// same recording happens in the agent factory, which exercises the real
// load/touch/evict path.
struct RecordingRouter {
    router: Router,
    built: Arc<Mutex<Vec<String>>>,
}

impl RecordingRouter {
    /// `Router(max_loaded=...)` whose factory records and stubs every build.
    ///
    /// A `preload: true` option is deferred until after the factory is
    /// installed: upstream patches `Agent` before constructing
    /// `Router(preload=True)`, so the constructor pass must never reach the
    /// real ONNX loader here.
    fn new(opts: RouterOptions) -> Self {
        let built: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&built);
        let preload = opts.preload;
        let router = Router::configure(RouterOptions { preload: false, ..opts })
            .expect("router")
            .with_agent_factory(move |_repo, subfolder, _revision| {
                let checkpoint = subfolder.unwrap_or("english").to_string();
                recorded.lock().unwrap_or_else(|e| e.into_inner()).push(checkpoint);
                Ok(Box::new(StubAgent::new()) as Box<dyn AgentLike>)
            });
        if preload {
            router.preload(None).expect("preload");
        }
        Self { router, built }
    }

    fn builds(&self) -> Vec<String> {
        self.built.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// `_Stub`: a duck-typed agent whose `system_one` answers
/// `{"model": <name>, "answers": {}, "usage": {}}`.
struct StubAgent {
    name: String,
}

impl StubAgent {
    fn new() -> Self {
        Self { name: "fake".to_string() }
    }
}

impl AgentLike for StubAgent {
    fn system_one(
        &mut self,
        _state: &Value,
        _questions: &Map<String, Value>,
        _lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Value> {
        Ok(json!({"model": self.name, "answers": {}, "usage": {}}))
    }
}


/// `Q`, the batch suite's question schema.
fn q_batch() -> Map<String, Value> {
    json!({"intent": {"type": "noul", "instructions": "Relevant?"}})
        .as_object()
        .unwrap()
        .clone()
}

/// The auto-detection question set of the mixed-groups test (`typed`): the
/// agent_trace_observability signature with `instructions: "?"`.
fn q_td_auto() -> Map<String, Value> {
    ["action", "needs_review", "outcome", "risk", "urgency"]
        .iter()
        .map(|id| (id.to_string(), json!({"type": "noul", "instructions": "?"})))
        .collect()
}

/// `request(state, **overrides)`.
fn request_with(state: &str, extra: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    m.insert("state".to_string(), json!(state));
    m.insert("questions".to_string(), Value::Object(q_batch()));
    for (key, value) in extra {
        m.insert(key.to_string(), value.clone());
    }
    Value::Object(m)
}

fn request(state: &str) -> Value {
    request_with(state, &[])
}

mod batches;
mod lang_guess;
mod lifecycle;
mod lru;
mod multilingual;
mod routing;
mod specs;
