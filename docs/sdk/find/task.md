# `find::task` — cancellation

`task` provides **cooperative cancellation** for the blocking search
operations. It is the mechanism behind every `timeout_ms` option in the
`find` module — and it is public so you can build your own long-running work
with the same guarantees, or cancel a search from another thread.

```rust,ignore
pub struct CancelToken { /* clone, cheap */ }
pub struct AbortToken { /* clone, cheap */ }
pub enum AbortReason { Timeout, Signal, User }
pub struct CancelledError { pub reason: AbortReason }
```

## The two sides

| Token | Who holds it | What it does |
|---|---|---|
| `CancelToken` | the running operation | checks `heartbeat()` between chunks of work; reads the deadline |
| `AbortToken` | the external caller | requests cancellation via `abort(reason)` |

A single `CancelToken::new(timeout_ms)` creates both: hand the `CancelToken`
to the search (the functions do this internally) and keep the `AbortToken`
(or clone the `CancelToken`) for cancellation from elsewhere.

## The API

```rust,ignore
pub fn new(timeout_ms: Option<u32>) -> CancelToken   // None = no deadline
pub fn heartbeat(&self) -> Result<(), String>        // Err(reason.to_string()) when cancelled
pub fn heartbeat_reason(&self) -> Result<(), AbortReason>  // typed variant
pub async fn wait(&self) -> AbortReason              // await cancellation or timeout
pub fn aborted(&self) -> bool                        // non-blocking check
pub fn abort_token(&self) -> AbortToken

impl AbortToken {
    pub fn abort(&self, reason: AbortReason)         // request cancellation
}
```

## How the search engines use it

- `CancelToken::new(options.timeout_ms)` is created at the top of `glob` /
  `grep`.
- The parallel walkers call `heartbeat_reason()` every 128 entries and stop
  the walk on cancellation.
- **Timeout** is reported as `AbortReason::Timeout`; the walkers keep the
  partial results and return them with `timed_out: true` (see the
  [timeout contract](find.md#the-timeout-contract-read-this-once)).
- **Non-timeout aborts** (`User`, `Signal`) always surface as errors — there
  is no partial-salvage for explicit cancellation.
- A deadline that already elapsed *before* the first `heartbeat()` is a hard
  error: nothing was collected, so there is nothing to salvage.

## Writing your own cancellable work

```rust,ignore
use cosh_sdk::find::{task::{CancelToken, AbortReason}, glob, GlobOptions};

// A long-running loop that respects a deadline:
fn my_loop(ct: &CancelToken) -> Result<(), String> {
    loop {
        ct.heartbeat()?;          // Err("Timeout") once the deadline passes
        // … one chunk of work …
    }
}

// Cancel a running glob from another thread:
let ct = CancelToken::new(Some(30_000));
let abort = ct.abort_token();
std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_millis(100));
    abort.abort(AbortReason::User);           // glob now fails with "User"
});
let result = glob(GlobOptions { /* … */ timeout_ms: Some(30_000), ..Default::default() });
```

`AbortReason` and `CancelledError` both `Display` as `"Timeout"`, `"Signal"`,
or `"User"` — the exact strings the engines surface in their `Err` values.

---

Back to the [module overview](find.md).
