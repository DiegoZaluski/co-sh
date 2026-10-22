# `windows` — list and manage an application's windows

`windows` is the window-level tool: it answers "which windows does this app
have, and what state is each in" and performs the window-manager verbs on
ONE of them — activate, minimize, maximize, fullscreen, restore, close,
move, resize. Where [`snapshot`](snapshot.md) reads the tree and
[`act`](act.md) presses buttons INSIDE a window, `windows` acts on the
windows THEMSELVES — dialogs included.

```
Computer::windows(&self, input: &ComputerWindows) -> Result<WindowsResult, String>
windows(metadata, ComputerWindows) -> Result<WindowsResult, String>
```

---

## The model: one scope, one action, two shapes

The tool has two modes, selected by the presence of `action`:

1. **List** (no `action`) — every window (and native dialog) of the app,
   with role, title, geometry and state flags:

   ```
   { app: "Reports", pid: 4242, count: 2, windows: [
     { role: "window", name: "Quarterly report", bounds: {x: 80, y: 40, ...},
       active: true, minimized: false, maximized: null, fullscreen: null },
     { role: "dialog", name: "Save changes?", bounds: {…}, active: false, … } ] }
   ```

2. **Manage** (`action` set) — perform the action on one window (the
   `selector` target, default the app's first window) and RETURN the fresh
   post-action state:

   ```
   { action: "minimize", window: { role: "window", name: "Quarterly report",
     minimized: true, active: false, … } }
   ```

The output shape is an UNTAGGED enum (`WindowsResult`): the model receives
either the listing or the manage report, never a wrapper.

## The actions

| `action` | Payload | Effect |
|---|---|---|
| `activate` | — | System foreground + keyboard focus. |
| `minimize` / `maximize` | — | Native verbs — the window-manager job, never a simulated move. |
| `enter_fullscreen` | — | Native fullscreen when the platform has it. |
| `restore` | — | Out of minimized/maximized/fullscreen, previous geometry. |
| `close` | — | Removes the window — unsaved-change prompts are the APP's own dialogs; re-list to see them. The post-observation is skipped, so `window` is `null`. |
| `move` | `x`, `y` | Top-left corner to desktop pixels (may be negative on multi-monitor). |
| `resize` | `width`, `height` | NEW outer size in desktop pixels. |

`move` and `resize` validate their payload pair up front — a `move` with
only one of `x`/`y` (or a `resize` missing `width`/`height`) fails before
any platform call, never a silent `move_to(x, 0)`.

## Why "minimize" before "close" teaches

The manage report is the ACTUAL state after the action, re-read fresh — not
an echo of the request. A minimize shows `minimized: true` — a hint of what
the next step can target. Platform honesty applies: a flag the platform
does not report stays `null` (Linux AT-SPI never reports `maximized`), and
a verb the platform does not support renders as an ordinary
[`error`](errors.md) that names the window and the verb.

## Edges

- **Scope exactly like the other tools**: `name` or `pid` — exactly one;
  `selector` helps achieve granularity (`dialog[name='Erasing…']`).
- **Selectors narrow the action** to the matched window — the default is
  the app's FIRST window, which may be a dialog; when in doubt, list first
  and address by role + name.
- **Listing is read-only** — safe in all harness modes; the `action` form
  routes through the harness approval gate like any state-changing tool.
- **A closed window is gone** — `close` returns `window: null` by design
  (there is nothing to re-observe; re-list to see the successor state).

## See also

[`apps`](apps.md) for which applications run · [`snapshot`](snapshot.md)
for the tree INSIDE a window · `cargo test -p cosh-tools --
computer::test::windows` (the
fixture app models every verb on both platforms)
