//! Opt-in prediction hooks: observe or shape every decision without forking.
//!
//! A hook is a `Hook` trait object (any subset of the six lifecycle events;
//! missing events default to a no-op) or a plain closure wrapped for
//! `on_predict_start` / `on_predict_end`, the two convenience callables
//! upstream's `_StartAdapter` / `_EndAdapter` wrap. Hooks are configured on
//! the agent and can be overridden per call.
//!
//! Divergences from the Python original, all mechanical consequences of the
//! runtime stack:
//! - `normalise_hooks`' runtime `TypeError` checks (a class instead of an
//!   instance, a non-callable entry, a non-callable method) are compile-time
//!   errors in Rust and are not reproduced; the HOOK_EVENTS set and the
//!   context field layout are pinned by tests instead.
//! - `PredictContext` carries `model` but not `agent`/`router`: Python stores
//!   the runtime object on the context, which Rust's aliasing rules forbid
//!   while the same context is handed to hooks mutably. Hooks that need the
//!   runtime capture what they need in their closure instead.
//! - `AsyncHook` and `run_coroutine_sync` are not ported (no async runtime in
//!   this crate). A hook that would be `async def` upstream is a closure or
//!   trait impl here; the awaited-inline behaviour has no Rust counterpart.
//! - `hooks_timeout` runs the hook on a detached thread with a clone of the
//!   context and writes the result back when the hook finishes in time. An
//!   overrunning hook keeps running (Python cannot interrupt it either) but
//!   its post-timeout context mutations are discarded: Python's shared
//!   mutable context cannot be reproduced soundly across threads.
//! - Python's `__context__` chaining (a failing `on_error` hook must not hide
//!   the original failure) has no Rust equivalent: the original error is
//!   returned and the hook's own failure is logged through the `log` facade.
//! - `_SKIP_DEFAULTS` is a thread-local flag (upstream: a `contextvars`
//!   ContextVar); the Router port drives it through [`skip_default_hooks`].

use std::cell::Cell;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::pycompat::{py_g, py_repr_value};

/// The lifecycle events a hook can implement, in dispatch order.
pub const HOOK_EVENTS: [&str; 6] = [
    "on_predict_start",
    "on_predict_end",
    "on_route",
    "on_load",
    "on_evict",
    "on_error",
];

/// Mutable state passed to every hook for one call.
///
/// `states` / `questions` may be rewritten by `on_predict_start`; `results`
/// may be rewritten by `on_predict_end`. A start hook can call [`Self::skip`]
/// to short-circuit inference with a cached result.
///
/// Identity semantics (`eq=False` upstream): the type deliberately does not
/// implement `PartialEq`, so two contexts are never equal.
#[derive(Clone)]
pub struct PredictContext {
    pub states: Vec<Value>,
    pub questions: Map<String, Value>,
    /// Shared by every hook of one call (`uuid4().hex` upstream).
    pub run_id: String,
    pub results: Option<Vec<Value>>,
    /// Router: the `RouteDecision` dict (set with the Router port).
    pub decision: Option<Value>,
    /// Resolved checkpoint name.
    pub model: Option<String>,
    /// Per-call overrides; `None` = agent config. A start hook may set them
    /// to shape the token budget.
    pub max_len: Option<usize>,
    pub head_max_len: Option<usize>,
    /// Aggregated input/output tokens.
    pub usage: Option<Value>,
    pub started_at: Instant,
    pub elapsed_ms: Option<f64>,
    pub error: Option<Error>,
}

impl PredictContext {
    pub fn new(
        states: Vec<Value>,
        questions: Map<String, Value>,
        model: Option<String>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Self {
        Self {
            states,
            questions,
            run_id: uuid::Uuid::new_v4().simple().to_string(),
            results: None,
            decision: None,
            model,
            max_len,
            head_max_len,
            usage: None,
            started_at: Instant::now(),
            elapsed_ms: None,
            error: None,
        }
    }

    /// Set cached results from a start hook; inference is skipped, end hooks
    /// still run.
    pub fn skip(&mut self, results: Vec<Value>) {
        self.results = Some(results);
    }
}

/// A hook: implement any subset of the lifecycle events; the rest are no-ops
/// (upstream's `Hook` protocol + `BaseHook` no-op base class collapse into one
/// trait with default bodies).
pub trait Hook: Send + Sync {
    fn on_predict_start(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    fn on_predict_end(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    fn on_route(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    fn on_load(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    fn on_evict(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    fn on_error(&self, _ctx: &mut PredictContext) -> Result<()> {
        Ok(())
    }

    /// `_hook_name`: the name the timeout message reports. Defaults to the
    /// Rust type path; closures report the event they wrap.
    fn hook_name(&self) -> &str {
        "hook"
    }
}

/// A plain `on_predict_start` / `on_predict_end` callable
/// (`PredictHook` upstream: `Callable[[PredictContext], None]`). A hook
/// "raises" by returning `Err`. Shared (`Arc`) so a `PerCall` argument can be
/// composed into several hook lists.
pub type PredictHook = std::sync::Arc<dyn Fn(&mut PredictContext) -> Result<()> + Send + Sync>;

/// A shared hook object as stored on an agent.
pub type SharedHook = Arc<dyn Hook + Send + Sync>;

struct StartAdapter {
    f: PredictHook,
}

impl Hook for StartAdapter {
    fn on_predict_start(&self, ctx: &mut PredictContext) -> Result<()> {
        (self.f)(ctx)
    }

    fn hook_name(&self) -> &str {
        "on_predict_start"
    }
}

struct EndAdapter {
    f: PredictHook,
}

impl Hook for EndAdapter {
    fn on_predict_end(&self, ctx: &mut PredictContext) -> Result<()> {
        (self.f)(ctx)
    }

    fn hook_name(&self) -> &str {
        "on_predict_end"
    }
}

/// `normalise_hooks`: flatten the hook list and the two convenience callables
/// into one ordered hook list. Installed hooks are normalised once at
/// construction; per-call arguments are appended after them, so per-call hooks
/// always run last.
///
/// The upstream `TypeError` guards are compile-time checks in Rust (a hook is
/// a `Hook` impl or a callable; there is no "class passed instead of
/// instance" runtime failure to reproduce).
pub fn normalise_hooks(
    hooks: Vec<SharedHook>,
    on_predict_start: Option<PredictHook>,
    on_predict_end: Option<PredictHook>,
) -> Vec<SharedHook> {
    let mut result = hooks;
    if let Some(f) = on_predict_start {
        result.push(Arc::new(StartAdapter { f }));
    }
    if let Some(f) = on_predict_end {
        result.push(Arc::new(EndAdapter { f }));
    }
    result
}

static DEFAULT_HOOKS: OnceLock<Mutex<Vec<SharedHook>>> = OnceLock::new();

fn default_hooks_cell() -> &'static Mutex<Vec<SharedHook>> {
    DEFAULT_HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Lock the default-hook registry, tolerating a poisoned mutex (a panicking
/// test/hook must not break the process-wide registry permanently).
fn lock_registry() -> std::sync::MutexGuard<'static, Vec<SharedHook>> {
    default_hooks_cell().lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    static SKIP_DEFAULTS: Cell<bool> = const { Cell::new(false) };
}

/// `_SKIP_DEFAULTS`: suppress the process-wide defaults for the current thread
/// (the Router sets it around its own re-dispatch). Returns a guard that
/// restores the previous value on drop.
pub(crate) fn skip_default_hooks_guard() -> SkipDefaultHooksGuard {
    let previous = SKIP_DEFAULTS.with(Cell::get);
    SKIP_DEFAULTS.with(|cell| cell.set(true));
    SkipDefaultHooksGuard { previous }
}

/// Guard restoring `_SKIP_DEFAULTS` on drop.
pub(crate) struct SkipDefaultHooksGuard {
    previous: bool,
}

impl Drop for SkipDefaultHooksGuard {
    fn drop(&mut self) {
        SKIP_DEFAULTS.with(|cell| cell.set(self.previous));
    }
}

pub(crate) fn skip_default_hooks() -> bool {
    SKIP_DEFAULTS.with(Cell::get)
}

/// Whether a call would run any hook at all — the fast-path probe for the
/// no-hooks prediction path. False only when every source is empty: the
/// process-wide defaults (unless suppressed on this thread), the installed
/// hooks and the per-call hooks. Reading the default registry takes its
/// mutex, so call this once per prediction, not per question.
pub(crate) fn no_hooks_active(installed: &[SharedHook], per_call: &PerCall) -> bool {
    per_call.hooks.is_empty()
        && per_call.on_predict_start.is_none()
        && per_call.on_predict_end.is_none()
        && installed.is_empty()
        && (skip_default_hooks() || lock_registry().is_empty())
}

/// The process-wide hooks, a copy, in order. Empty unless set via
/// [`set_default_hooks`].
pub fn default_hooks() -> Vec<SharedHook> {
    lock_registry().clone()
}

/// Replace the process-wide default hooks.
///
/// Defaults run before installed and per-call hooks for every agent and router
/// in the process, so a tracer or metrics hook does not have to be threaded
/// through every construction. Accepts the same arguments as the `hooks=`
/// parameter of [`normalise_hooks`].
pub fn set_default_hooks(
    hooks: Vec<SharedHook>,
    on_predict_start: Option<PredictHook>,
    on_predict_end: Option<PredictHook>,
) {
    let normalised = normalise_hooks(hooks, on_predict_start, on_predict_end);
    *lock_registry() = normalised;
}

/// Append one hook to the process-wide defaults (`add_default_hook`).
pub fn add_default_hook(hook: SharedHook) {
    lock_registry().push(hook);
}

/// Remove every process-wide default hook (`clear_default_hooks`).
pub fn clear_default_hooks() {
    lock_registry().clear();
}

/// `compose_hooks`: effective hook list for one call — defaults, then
/// installed, then per-call hooks. Reads the process-wide defaults at call
/// time, so hooks set after construction still apply.
pub fn compose_hooks(installed: &[SharedHook], per_call: &PerCall) -> Vec<SharedHook> {
    let mut active = Vec::new();
    if !skip_default_hooks() {
        active.extend(default_hooks());
    }
    active.extend(installed.iter().cloned());
    active.extend(normalise_hooks(
        per_call.hooks.clone(),
        per_call.on_predict_start.clone(),
        per_call.on_predict_end.clone(),
    ));
    active
}

/// The per-call hook arguments (`hooks=`, `on_predict_start=`,
/// `on_predict_end=`, `hooks_raise=`, `hooks_timeout=`), named exactly as the
/// upstream keyword parameters.
#[derive(Default)]
pub struct PerCall {
    pub hooks: Vec<SharedHook>,
    pub on_predict_start: Option<PredictHook>,
    pub on_predict_end: Option<PredictHook>,
    pub hooks_raise: Option<bool>,
    pub hooks_timeout: Option<f64>,
}

/// `validate_timeout`: return the value as a positive float, or `None` for no
/// limit.
///
/// A non-positive timeout is rejected here rather than left to the join:
/// `join(0)` and `join(-1)` return before the hook has started, so the outcome
/// of a fast hook with such a value is a race.
///
/// Message note: Python formats the original value with `%r`; the Rust API
/// receives `f64`, so an integer `0` renders as `0.0` where Python would show
/// `0` (the value the caller passed is a float here by construction).
pub fn validate_timeout(value: Option<f64>) -> Result<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_nan() {
        // Python accepts NaN here (NaN <= 0 is False) and then crashes inside
        // thread.join; the closest safe behaviour is the same validation
        // error, rendered like repr(float('nan')).
        return Err(Error::Value(format!(
            "hooks_timeout must be a positive number or None; got {}",
            py_g(value).to_ascii_lowercase()
        )));
    }
    if value <= 0.0 {
        return Err(Error::Value(format!(
            "hooks_timeout must be a positive number or None; got {}",
            py_repr_value(&json!(value))
        )));
    }
    if value.is_infinite() || value >= 1.0e18 {
        // A positive infinity — or any wait beyond what `Duration::secs_f64`
        // can represent — is an unlimited wait, which is exactly what `None`
        // means (upstream `thread.join(inf)` is a legal infinite wait); this
        // also keeps the downstream duration conversion from panicking.
        return Ok(None);
    }
    Ok(Some(value))
}

/// One lifecycle event, dispatched by name upstream (`getattr(hook, event)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookEvent {
    PredictStart,
    PredictEnd,
    Route,
    Load,
    Evict,
    Error,
}

impl HookEvent {
    fn call(self, hook: &dyn Hook, ctx: &mut PredictContext) -> Result<()> {
        match self {
            Self::PredictStart => hook.on_predict_start(ctx),
            Self::PredictEnd => hook.on_predict_end(ctx),
            Self::Route => hook.on_route(ctx),
            Self::Load => hook.on_load(ctx),
            Self::Evict => hook.on_evict(ctx),
            Self::Error => hook.on_error(ctx),
        }
    }

    /// The upstream event name (`on_predict_start`, ...), used in log lines.
    pub fn name(self) -> &'static str {
        match self {
            Self::PredictStart => "on_predict_start",
            Self::PredictEnd => "on_predict_end",
            Self::Route => "on_route",
            Self::Load => "on_load",
            Self::Evict => "on_evict",
            Self::Error => "on_error",
        }
    }
}

/// `_call_hook`: call one hook method under an optional timeout.
///
/// Without a timeout the hook runs inline on the calling thread. With one, the
/// hook runs on a detached thread operating on a clone of the context; a
/// timely completion writes the mutated context back, an overrun abandons the
/// clone (see the module docs) and raises `TimeoutError` — [`Error::Timeout`]
/// here — with the upstream message.
fn call_hook(
    hook: &SharedHook,
    event: HookEvent,
    ctx: &mut PredictContext,
    timeout: Option<f64>,
) -> Result<()> {
    let Some(timeout) = timeout else {
        // The untimed path catches panics the same way the timed worker does:
        // a hook failure is an error result (upstream: the hook's exception
        // propagates), never a process crash.
        let name = hook.hook_name().to_string();
        return run_hook_catching(hook, event, ctx, &name);
    };
    let mut timed_ctx = ctx.clone();
    let name = hook.hook_name().to_string();
    let worker_name = name.clone();
    let hook = Arc::clone(hook);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cosh-onnx-hook-timeout".to_string())
        .spawn(move || {
            // A panicking hook must not be misreported as a timeout: the
            // panic is caught and shipped through the channel as the hook's
            // own failure (upstream re-raises the boxed exception on the
            // caller thread).
            let outcome = run_hook_catching(&hook, event, &mut timed_ctx, &worker_name);
            // A dropped receiver (the caller already timed out) makes this a
            // no-op: the detached hook simply finishes and its context is
            // discarded.
            let _ = tx.send((outcome, timed_ctx));
        })
        .map_err(|e| Error::Runtime(format!("cosh-onnx: could not start the hook thread: {}", e)))?;
    match rx.recv_timeout(std::time::Duration::from_secs_f64(timeout)) {
        Ok((outcome, timed_ctx)) => {
            *ctx = timed_ctx;
            outcome
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(crate::error::Error::hook_timeout(&name, timeout))
        }
        // The worker exited without sending: it could only abort or be
        // cancelled before the send, which is neither a hook failure nor a
        // timeout.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(Error::Runtime(format!(
            "cosh-onnx: hook {} stopped without returning a result",
            name
        ))),
    }
}

/// Best-effort payload extraction from a caught panic, mirroring the string
/// content Python would put in the traceback.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Call one hook, turning a panic into the hook's own failure instead of a
/// process crash (upstream: the hook's exception propagates to the caller).
fn run_hook_catching(
    hook: &SharedHook,
    event: HookEvent,
    ctx: &mut PredictContext,
    name: &str,
) -> Result<()> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event.call(hook.as_ref(), ctx)
    })) {
        Ok(outcome) => outcome,
        Err(payload) => Err(Error::Runtime(format!(
            "cosh-onnx: hook {} panicked: {}",
            name,
            panic_message(&payload)
        ))),
    }
}

/// `dispatch`: call `event` on every hook.
///
/// `raise_errors=false` warns and continues, for hooks (telemetry) that must
/// not fail a request. `lock` serialises dispatch for hooks that are not safe
/// to run concurrently. `timeout` bounds each hook call in seconds; an
/// overrunning hook raises [`Error::Timeout`], or warns when
/// `raise_errors` is false. An overrunning hook keeps running in the
/// background: the timeout protects the request, not the process.
pub fn dispatch(
    hooks: &[SharedHook],
    event: HookEvent,
    ctx: &mut PredictContext,
    raise_errors: bool,
    lock: Option<&Mutex<()>>,
    timeout: Option<f64>,
) -> Result<()> {
    for hook in hooks {
        let outcome = match lock {
            Some(lock) => {
                // Poison tolerance: a panicking hook must not permanently
                // break the agent (upstream `with lock:` releases cleanly).
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                call_hook(hook, event, ctx, timeout)
            }
            None => call_hook(hook, event, ctx, timeout),
        };
        if let Err(exc) = outcome {
            if raise_errors {
                return Err(exc);
            }
            log::warn!(
                "cosh-onnx: hook {}.{} failed: {}",
                hook.hook_name(),
                event.name(),
                exc
            );
        }
    }
    Ok(())
}

/// `aggregate_usage`: sum the per-state usage blocks so a hook sees one total
/// for the call.
///
/// `int((r.get("usage") or {}).get(key, 0) or 0)`: a missing or falsy entry
/// counts as zero.
pub fn aggregate_usage(results: &[Value]) -> Value {
    let total = |key: &str| -> i64 {
        results
            .iter()
            .map(|r| {
                r.get("usage")
                    .and_then(Value::as_object)
                    .and_then(|u| u.get(key))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            })
            .sum()
    };
    json!({
        "input_tokens": total("input_tokens"),
        "output_tokens": total("output_tokens"),
    })
}

#[cfg(test)]
mod tests;
