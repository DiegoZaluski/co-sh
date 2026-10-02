# Prediction hooks: observing a decision lifecycle

Hooks are for applications that need telemetry, policy or a small adaptation
around inference. They are additive: defaults, installed hooks and per-call
hooks are composed in that order.

## The context is the shared notebook

Each call receives one [`PredictContext`](https://docs.rs/cosh-onnx/latest/cosh_onnx/struct.PredictContext.html).
It carries the states and questions, a `run_id`, the model and routing
decision, token limits, usage, elapsed time and any error. A start hook can
rewrite state or questions. It can also call `skip` with cached results; end
hooks still run after a skip.

```rust,ignore
use cosh_onnx::hooks::{Hook, PredictContext};

struct Audit;

impl Hook for Audit {
    fn on_predict_start(&self, ctx: &mut PredictContext) -> cosh_onnx::Result<()> {
        eprintln!("starting {}", ctx.run_id);
        Ok(())
    }

    fn on_predict_end(&self, ctx: &mut PredictContext) -> cosh_onnx::Result<()> {
        eprintln!("finished {} in {:?} ms", ctx.run_id, ctx.elapsed_ms);
        Ok(())
    }
}
```

The same trait can implement `on_route`, `on_load`, `on_evict` and `on_error`.
Unimplemented methods are no-ops, so a hook only needs to name the event it
cares about.

## Errors and timeouts

`hooks_raise` decides whether a hook error becomes the prediction error. When
it is false, the crate logs the hook failure and continues; the original model
error is never replaced by a later failing error hook. `hooks_timeout` applies
to each hook call and must be positive; `None` means no limit.

`hooks_concurrent` controls whether hook calls may overlap. Keep it enabled
for independent telemetry. Disable it when a hook writes to a non-thread-safe
sink or depends on strict ordering.

## Global defaults and local composition

The module exposes `default_hooks`, `set_default_hooks`,
`add_default_hook`, `clear_default_hooks` and `compose_hooks`. For most
applications, passing hooks through `LoadOptions` or `RouterOptions` is easier
to reason about than changing process-wide defaults. Use the defaults only
when every model in the process should share the same policy.

## Lifecycle order

For a successful routed prediction, the useful mental model is:

```text
route → on_route → load (if cold) → on_load → on_predict_start
       → inference → on_predict_end
```

On failure, `on_error` runs before the end hooks. A hook may rewrite a route or
the final results, but it should preserve the result shape expected by the
caller.

## Summary

- Hooks are lifecycle observers with controlled mutation points.
- `PredictContext::skip` supplies cached results without running inference.
- Hook failures can be strict or fail-open.
- Timeouts and concurrency are explicit options.
- Defaults, installed hooks and per-call hooks compose predictably.
