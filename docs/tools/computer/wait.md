# `wait` — block on an element state

`wait` solves the oldest problem in desktop automation: **the model cannot
sleep**. Without it, checking whether a dialog appeared means re-calling
[`snapshot`](snapshot.md) in a tight loop or — worse — acting on a stale
picture. With it, the call *blocks* until the condition you name is true, or
times out with a diagnosis of what it actually saw.

```
Computer::wait(&self, input: &ComputerWait) -> Result<WaitOutput, String>
wait(metadata, ComputerWait) -> Result<WaitOutput, String>
```

---

## The model: target → condition → bounded patience

1. **Target** the app to watch: `name` or `pid` — exactly one — plus the
   `selector` of the element to watch.
2. **Name the condition.** `state` is one of the eight `WaitState` values,
   the same tokens [`snapshot`](snapshot.md) renders as flags:
   `attached`, `detached`, `visible` (default), `hidden`, `enabled`,
   `disabled`, `focused`, `unfocused`.
3. **Bound the patience.** `timeout_ms` defaults to 10 000 and is capped at
   60 000 — a stuck call can never pin the session for minutes. Chain calls
   for longer waits.

On success it returns `WaitOutput { met: true, elapsed_ms, observed }` —
`observed` is what the element looked like at resolution time (attached?
visible? enabled? focused?), a free sanity check: a `detached` wait's
element is really gone.

## The condition you probably want is `detached`

Waiting for a busy indicator to disappear tolerates the indicator **never
having existed**: a spinner that already finished is a satisfied wait, not
an error. `detached` and `hidden` have that "tolerates absence" semantics —
`visible`, `enabled` and `focused` do not (they can only be met by an
element that exists).

```rust,ignore
computer.wait(&ComputerWait {
    name: Some("Reports".into()),
    selector: Some("progress_bar[name='Exporting…']".into()),
    state: Some(WaitState::Detached),   // done exporting = spinner gone
    timeout_ms: Some(30_000),
    ..Default::default()
}).await?;
```

## Timeouts teach

A timeout is an **error carrying the Diagnosis**: the condition waited for,
the selector, and the last observed state — including near-miss candidates.
`"matched button \"OK\" (enabled=false)"` means the dialog IS there but
still disabled; `"selector never matched"` means the flow never opened it.
The model re-plans from that difference instead of retrying blind — see
[`errors`](errors.md) for how it renders.

## Edges

- **Validated up front**: scope-less calls, double scopes and over-cap
  timeouts fail before any platform call.
- **Waits are pauses, not sleeps**: there is no bare `wait_ms` tool call —
  inside a pipeline, [`control`](control.md) and [`act`](act.md) accept
  inline `wait` steps for settle-pauses between actions.

## See also

[`snapshot`](snapshot.md) for the state-flag vocabulary ·
`cargo run --example computer-wait`
