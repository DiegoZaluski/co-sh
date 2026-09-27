//! Ported tests: the portable subset of `tests/test_hooks.py` from the laya
//! repository, plus the structural checks of `tests/test_hooks_api.py` that
//! translate to Rust.
//!
//! Dropped sections, with the reason:
//! - The `make_fake()` Agent sections (order/context/mutation on
//!   `predict_batch`, batching, Router sections): `predict_batch` and the
//!   Router are torch-only upstream and out of this crate's scope; the
//!   ONNXAgent-equivalent checks run through `system_one` below.
//! - Async hooks (`AsyncHook`, `run_coroutine_sync`, contextvars): not ported
//!   (no async runtime; see the module docs in `hooks.rs`).
//! - `test_hooks_api.py` signature inspection (`inspect.signature`): a
//!   Python-introspection guard with no Rust counterpart; the structural
//!   parts (event set, context fields, BaseHook no-ops, class defaults) are
//!   pinned here and by the type system.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::runtime::batch::CollatedBatch;
use crate::runtime::tokenizer::Tokenizer;
use crate::hooks::{
    aggregate_usage, clear_default_hooks, compose_hooks, default_hooks, dispatch, Hook, HookEvent,
    PerCall, PredictContext, SharedHook, HOOK_EVENTS,
};
use crate::laya::agent::{bare_agent, FakeTok, OnnxAgent, StubSession};
use crate::runtime::session::{SessionOutput, SessionRunner};

use std::collections::HashMap;

/// The process-wide default-hook registry is global state; cargo runs tests
/// on parallel threads, so every test that touches it takes this lock and
/// clears the registry first.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// Take the registry lock and clear any leaked default hooks. Every hook test
/// calls this first: `compose_hooks` reads the global registry on every
/// `system_one`, so an unlocked test would fire another test's leaked
/// defaults. Poison is tolerated — a panicked neighbour must not fail the
/// rest of the suite.
fn registry_isolation() -> std::sync::MutexGuard<'static, ()> {
    let guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_default_hooks();
    guard
}

/// Upstream `QUESTIONS` from `test_hooks.py`.
fn questions() -> Map<String, Value> {
    json!({
        "a": {"type": "noul", "instructions": "?"},
        "b": {"type": "noul", "instructions": "?"}
    })
    .as_object()
    .unwrap()
    .clone()
}

fn agent() -> OnnxAgent {
    bare_agent(
        json!({"max_len": 64, "head_max_len": 32}).as_object().unwrap().clone(),
        Box::new(FakeTok),
        Box::new(StubSession { logits: vec![], act_logits: vec![] }),
        [1.0, 1.0, 1.0],
        HashMap::new(),
        HashMap::new(),
    )
}

/// A hook that records one label per fired event (`Tag` upstream).
struct Tag {
    log: Arc<Mutex<Vec<String>>>,
    tag: &'static str,
}

impl Hook for Tag {
    fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
        self.log.lock().unwrap().push(format!("{}:start", self.tag));
        Ok(())
    }

    fn on_predict_end(&self, _ctx: &mut PredictContext) -> Result<()> {
        self.log.lock().unwrap().push(format!("{}:end", self.tag));
        Ok(())
    }

    fn hook_name(&self) -> &str {
        self.tag
    }
}

fn tag(log: &Arc<Mutex<Vec<String>>>, tag: &'static str) -> SharedHook {
    Arc::new(Tag { log: Arc::clone(log), tag })
}

/// A hook whose start event raises (`boom` upstream).
struct BoomHook;

impl Hook for BoomHook {
    fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
        Err(Error::Value("hook boom".to_string()))
    }

    fn hook_name(&self) -> &str {
        "BoomHook"
    }
}

/// Records `ctx.error` on `on_error` (`ErrHook` upstream).
struct ErrHook {
    errors: Mutex<Vec<String>>,
}

impl Hook for ErrHook {
    fn on_error(&self, ctx: &mut PredictContext) -> Result<()> {
        self.errors
            .lock()
            .unwrap()
            .push(ctx.error.as_ref().map(|e| e.to_string()).unwrap_or_default());
        Ok(())
    }

    fn hook_name(&self) -> &str {
        "ErrHook"
    }
}

/// `on_error` that itself raises (`BadErrHook` upstream).
struct BadErrHook;

impl Hook for BadErrHook {
    fn on_error(&self, _ctx: &mut PredictContext) -> Result<()> {
        Err(Error::Value("hook error".to_string()))
    }

    fn hook_name(&self) -> &str {
        "BadErrHook"
    }
}

/// A session whose run always fails (`bad_forward` upstream).
struct FailingSession;

impl SessionRunner for FailingSession {
    fn run(&mut self, _batch: &CollatedBatch) -> Result<Vec<SessionOutput>> {
        Err(Error::Runtime("infer boom".to_string()))
    }
}

/// A tokenizer that records the state text it is asked to encode, so a start
/// hook's `ctx.states` rewrite is observable (`fake._encode_states` upstream).
struct RecTok {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Tokenizer for RecTok {
    fn encode(&self, text: &str, _truncation: bool, _max_length: Option<usize>) -> Vec<u32> {
        self.seen.lock().unwrap().push(text.to_string());
        text.bytes().map(|c| 10 + (c as u32 % 40)).collect()
    }
    fn mask_token(&self) -> &str {
        "[M]"
    }
    fn mask_token_id(&self) -> u32 {
        3
    }
    fn cls_token_id(&self) -> u32 {
        1
    }
    fn sep_token_id(&self) -> u32 {
        2
    }
    fn pad_token_id(&self) -> u32 {
        0
    }
}

fn recorded_agent(seen: Arc<Mutex<Vec<String>>>) -> OnnxAgent {
    let mut agent = agent();
    agent.tok = Box::new(RecTok { seen });
    agent
}

mod api;
mod composition;
mod defaults;
mod errors;
mod lifecycle;
mod timeout;
