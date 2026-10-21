# `wait` — block on a state, a value, or an event

`wait` solves the oldest problem in desktop automation: **the model cannot
sleep**. Without it, checking whether a dialog appeared means re-calling
[`snapshot`](snapshot.md) in a tight loop or — worse — acting on a stale
picture. With it, the call *blocks* until the condition you name is true,
or times out with a diagnosis of what it actually saw.

```
Computer::wait(&self, input: &ComputerWait) -> Result<WaitResult, String>
wait(metadata, ComputerWait) -> Result<WaitResult, String>
```

---

## The model: three modes, one budget

Pick exactly ONE mode — they are mutually exclusive and validated up front:

1. **Element state** — `selector` + `state`: the watched element reaches a
   boolean condition, one of the eight `WaitState` values (the same tokens
   [`snapshot`](snapshot.md) renders as flags): `attached`, `detached`,
   `visible` (default), `hidden`, `enabled`, `disabled`, `focused`,
   `unfocused`.
2. **Value holds** — `selector` + `value` ("equals") or `value_contains`
   ("substring"): the answer to "wait until the export progress bar says
   100%" — as a `state` this is undecidable, as a VALUE it is one call.
3. **Graphical event** — `event` + app scope (exactly one of `name`/`pid`,
   NO selector): subscribe to the platform's event stream and wake on the
   first matching event — `window_opened`, `window_closed`, `focus_changed`,
   `value_changed`, `menu_opened`, `announcement`, … (the full list lives
   in [`types.md`](types.md#wait)). This is the app-scoped answer to
   "wait until SOMETHING happens" without guessing which element to watch.

**Bound the patience.** `timeout_ms` defaults to 10 000 and is capped at
60 000 — a stuck call can never pin the session for minutes. Chain calls
for longer waits.

## What comes back — one shape per mode

- State / value modes return `WaitOutput { met: true, elapsed_ms,
  observed }` — `observed` is what the element looked like at resolution
  time (attached? visible? enabled? focused?), a free sanity check: a
  `detached` wait's element is really gone.
- The event mode returns `WaitedEvent { kind, app, pid, element }` — the
  kind in the same snake_case vocabulary as the request, and the element
  the event was about (role/name/value at arrival: window_opened names the
  NEW window, focus_changed points at what gained focus). The app lookup
  and the rest of the call SHARE the timeout budget — wiring up takes
  milliseconds out of your 10 s, not a second budget.

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
    value: Some("100%".into()),        // VALUE mode: the number lands
    timeout_ms: Some(30_000),
    ..Default::default()
}).await?;

computer.wait(&ComputerWait {
    name: Some("Reports".into()),
    event: Some(WaitEvent::WindowOpened), // EVENT mode: the dialog appears
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

- **Validated up front**: scope-less calls, double scopes, state+value
  pairings (`state` excludes `value`/`value_contains`, and the two value
  fields exclude each other) and over-cap timeouts fail before any
  platform call. Event mode is app-scoped ONLY — it takes no
  `selector`/`nth`/`state`/`value`.
- **Waits are pauses, not sleeps**: there is no bare `wait_ms` tool call —
  inside a pipeline, [`control`](control.md) and [`act`](act.md) accept
  inline `wait` steps for settle-pauses between actions.
- **Platform honesty**: `window_activated`/`window_deactivated` are not
  emitted on Windows (UIA has no first-class activation event) — on that
  platform, wait `focus_changed` or poll an element state instead.

## See also

[`snapshot`](snapshot.md) for the state-flag vocabulary · [`windows`](windows.md)
for the window verbs events pair with · `cargo run --example computer-wait`
