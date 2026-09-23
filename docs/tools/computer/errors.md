# `errors` — failures that teach

Every failure from a computer tool is rendered through ONE function,
`computer::errors::render` — not because the tools share a code path for
aesthetics, but because a failure that only says *something failed* forces
the model to guess. A failure that says *what was attempted, what was last
observed, and what usually fixes it* lets the model self-correct without a
round trip.

```
errors::render(tool: &str, ctx: &str, err: &xa11y::Error) -> String
```

`tool` is the calling tool's name (so a shared failure reads as coming from
the tool the model actually called), `ctx` names the operation (`"press"`,
`"type text"`, `"resolve app"`), and `err` is the xa11y error the platform
returned.

---

## The model: variant → guidance, Diagnosis verbatim

The renderer maps every xa11y error variant to operation-specific guidance —
with a non_exhaustive wildcard, so a future xa11y release degrades to a
clear message instead of breaking the build. Two properties matter more
than the individual mappings:

1. **The Diagnosis survives.** For wait-and-match failures the platform
   attaches a Diagnosis — the condition waited for, the selector, the last
   observed state, near-miss candidates. `render` preserves it VERBATIM
   after the guidance:

   ```text
   computer_control: press: no element matched `button[name='Export']` —
   re-capture with computer_snapshot and match an element that exists NOW
   (role[name='…'] is stable; a bare :nth index goes stale after any UI
   change) ; waiting for: press target actionable (visible && enabled);
   selector: button[name='Export']; last observed: matched button "Export"
   (visible=false, enabled=true)
   ```

   Read that closely: the element EXISTS, is visible, but disabled — the
   right move is a [`wait`](wait.md) for `enabled`, not a blind retry or a
   fresh selector hunt.

2. **Argument errors state that no platform call was made.** A bad selector
   or a missing payload fails at validation — the message says so, because
   "nothing happened" is itself diagnostic information.

## The variants worth knowing

| Variant | Means | The guidance points at |
|---|---|---|
| `PermissionDenied` | The OS consent toggle is off | The platform's own instructions (a Settings toggle, not a code change); a granted consent applies to the whole session. |
| `AccessibilityNotEnabled` | The app's bridge is off (Chromium/Electron) | The launch flag that fixes it (`--force-renderer-accessibility`). |
| `SelectorNotMatched` | Nothing matched (possibly "not YET") | Re-capture and match an element that exists NOW; stable `role[name='…']` over bare `:nth`. |
| `ElementStale` | The node's handle died mid-operation | The tree changed — fresh [`snapshot`](snapshot.md), then re-address. |
| `ActionNotSupported` | The element has no such verb | Role-appropriate alternatives. |
| `Timeout` | The wait outlived its budget | Raise `timeout_ms` or add a `wait` step; the Diagnosis says what was last seen. |
| `InvalidSelector` / `InvalidActionData` | Malformed input — no platform call | The grammar/payload shape. |
| `Unsupported` | This backend cannot do that at all | A different verb or tool (e.g. pointer warp on Wayland without a portal grant). |
| `Platform` | Platform-specific failure | Operation-dependent guidance with a bounded retry (capture/mapping failures vs. backend faults differ). |

## The message-reading habit

The rendered line is a small structured document: **tool, operation,
failure, guidance, diagnosis**. Reading it in that order answers the model's
actual question — "what do I do next?" — which is the whole point: the
error IS documentation, delivered at the moment of maximum relevance.

## See also

[`wait`](wait.md) for what populates the Diagnosis ·
`cargo run --example computer-errors` (renders every variant, fully offline)
