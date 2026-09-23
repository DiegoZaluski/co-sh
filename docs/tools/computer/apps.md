# `apps` — which applications are running, and who has focus

`apps` is the observation tool you reach for **before** anything else: it
answers "what is running?" and — with one query — "where do my keystrokes go
right now?". It never synthesizes input, so it is safe in every permission
mode.

```
Computer::apps(&self, input: &ComputerApps) -> Result<AppsResult, String>
apps(metadata, ComputerApps) -> Result<AppsResult, String>          // free function
```

---

## The two queries

1. **`target: "all"`** (default) — a point-in-time enumeration of running
   applications: name, PID (when the platform reports one), and which entry
   holds the system foreground. Enumeration order is the platform's; the
   `foreground` flag is the stable fact.
2. **`target: "focused"`** — resolves the foreground application **and** the
   element inside it holding keyboard focus. This is the cheap ground truth
   the input tools previously guessed at: before typing, CHECK the focused
   element instead of assuming the last click landed. `timeout_ms` bounds
   the wait for a foreground app to exist at all.

## Reading the result

The result is the untagged enum [`AppsResult`](types.md#appsresult) — the
variant follows the query:

- `AppsResult::All(AppsOutput)` — `apps: Vec<AppInfo>` + `count`;
- `AppsResult::Focused(FocusedOutput)` — the foreground app's `name`/`pid`
  plus `focused_element: Option<FocusedElement>`. The **`Option` matters**:
  focus may sit on the window itself (no element), or the platform may not
  report element focus. Treat `None` as "keystrokes go to the window, not a
  named field".

## Edges

- **Focus is a moment.** Between the `focused` query and your next call the
  user may click elsewhere. For input that must land in a specific field,
  prefer a real click ([`control`](control.md)) over assuming focus persists.
- **PIDs can be absent** on some backends; never build logic that requires
  them. Scope other tools by `name` when possible.

## See also

[`snapshot`](snapshot.md) for the tree below the focused element ·
[runnable example](https://github.com/): `cargo run --example computer-apps`
