# Type reference — computer wire types

Every computer tool takes a single input struct and returns a single output.
The inputs ARE the MCP wire payloads: each derives `Deserialize` with
snake_case keys, so the JSON an MCP client sends maps field-for-field onto
the struct below. All structs live in `cosh_tools::computer::types` and are
re-exported through `cosh_tools::computer`.

---

## Targeting (shared vocabulary)

### `SurfaceKind` — the shell-surface root

```rust,ignore
pub enum SurfaceKind { MenuBar, StatusItems, Taskbar, Panel, Dock,
                       Desktop, Flyout, Unknown }
```

`#[non_exhaustive]`; wire spelling is snake_case (`"menu_bar"`, …). The
full table lives on the [`surface`](surface.md) page.

### Selectors

Selectors are CSS-like strings parsed by xa11y: compound clauses of
`role[name='value']` joined by `>` (direct child) or space (descendant),
with a single trailing `:nth(k)` pseudo-class (1-based). Roles use xa11y's
normalized snake_case vocabulary (`button`, `text_field`, `progress_bar`,
`list_item`, …) — non-normalized names degrade to platform-role fallback
matching, so prefer the vocabulary [`snapshot`](snapshot.md) renders.

---

## `apps`

### Input — `ComputerApps`

| Field | Type | Meaning |
|---|---|---|
| `target` | `Option<AppsTarget>` | `All` (default) or `Focused`. |
| `timeout_ms` | `Option<u64>` | Bounds the focused-app resolution (default 3000). |

### Output — `AppsResult` (untagged enum)

- `AppsResult::All(AppsOutput { apps: Vec<AppInfo>, count: usize })`
- `AppsResult::Focused(FocusedOutput { app: String, pid: Option<u32>,
  focused_element: Option<FocusedElement> })`

The variant follows the query — [`apps`](apps.md#reading-the-result).

| Struct | Fields |
|---|---|
| `AppInfo` | `name: String`, `pid: Option<u32>`, `foreground: bool` |
| `FocusedElement` | `role: String`, `name: Option<String>`, `value: Option<String>` |

`focused_element` is `None` when focus sits on the window itself or the
platform reports no element focus.

---

## `snapshot`

### Input — `ComputerSnapshot`

| Field | Type | Meaning |
|---|---|---|
| `name` / `pid` | `Option<String>` / `Option<u32>` | The app to read — exactly one. |
| `selector` | `Option<String>` | Cut the traversal to the matched subtree. |
| `nth` | `Option<usize>` | 1-based match index (default 1). |
| `max_depth` | `Option<usize>` | Traversal budget (default 12, hard cap 40). |
| `format` | `Option<SnapshotFormat>` | `Tree` (default) or `Json`. |
| `timeout_ms` | `Option<u64>` | App-resolution timeout (default 3000). |
| `surface` | `Option<SurfaceKind>` | Read a shell surface instead of an app. |

### Output — `SnapshotOutput`

`app: String`, `pid: Option<u32>`, `snapshot: String` (the rendered
outline/JSON), `elements: usize` (rendered count — tells "small tree" from
"truncated by `max_depth`").

---

## `screenshot`

### Input — `ComputerScreenshot`

| Field | Type | Meaning |
|---|---|---|
| `region` | `Option<Vec<i32>>` | `[x, y, w, h]` display pixels — mutually exclusive with element capture and annotation. |
| `app` / `pid` | `Option<…>` | Element capture / annotation scope. |
| `selector` | `Option<String>` | Capture target; with `annotate: true`, WHICH elements get boxes (default `"*"`). |
| `nth` | `Option<usize>` | Ignored when annotating (the legend covers every match). |
| `annotate` | `bool` | Default `false`. Requires an app scope; mutually exclusive with `region`. |
| `max_width` | `Option<u32>` | Delivery budget (default 1568) — the mapping below absorbs the downscale. |
| `timeout_ms` | `Option<u64>` | App-resolution timeout (default 3000). |
| `surface` | `Option<SurfaceKind>` | Capture a shell surface's region. |

### Output — `ScreenshotOutput`

`width`, `height` (delivered image), `desktop_origin: (i32, i32)`,
`desktop_scale: (f64, f64)`, `bytes: usize` (encoded PNG size),
`legend: Vec<LegendEntryOutput>` (annotation boxes only),
`omitted: Vec<OmissionOutput>` (matched but undrawable, with the reason),
`truncated: usize` (matches dropped by the annotation cap of 100 — narrow
`selector` when it bites), and `images: Vec<ImageBlock>` (the inline base64
PNG for the multimodal channel). Mapping:

```text
desktop = desktop_origin + image_coord × desktop_scale
```

| Struct | Fields |
|---|---|
| `LegendEntryOutput` | `tag: String` (`"B7"`), `selector: String` (copy-ready), `role: String`, `name: Option<String>`, `color: [u8; 3]` (correlate box ↔ entry by eye) |
| `OmissionOutput` | `selector: String`, `role: String`, `name: Option<String>`, `reason: String` (`no_bounds`, `zero_area` or `outside_capture`) |

---

## `wait`

### Input — `ComputerWait`

| Field | Type | Meaning |
|---|---|---|
| `name` / `pid` | `Option<…>` | The app to watch — exactly one. |
| `selector` | `Option<String>` | The element to watch. |
| `nth` | `Option<usize>` | 1-based match index (default 1). |
| `state` | `Option<WaitState>` | The condition (default `visible`). |
| `timeout_ms` | `Option<u64>` | Default 10 000, hard cap 60 000. |
| `surface` | `Option<SurfaceKind>` | Watch a shell surface's tree. |

`WaitState` — the eight conditions: `Attached`, `Detached`, `Visible`,
`Hidden`, `Enabled`, `Disabled`, `Focused`, `Unfocused` (same tokens as the
[snapshot](snapshot.md) state flags). `detached`/`hidden` tolerate absence;
the others can only be met by an element that exists.

### Output — `WaitOutput`

`met: bool`, `elapsed_ms: u64`, `observed: String` (the element's state at
resolution time). On timeout the error carries the platform's
`Diagnosis` — see [`errors`](errors.md).

---

## `act`

### Input — `ComputerAct` (ONE step; chain via `then`)

| Field | Type | Meaning |
|---|---|---|
| `name` / `pid` | `Option<…>` | Semantic target scope — or `surface`. |
| `selector` | `Option<String>` | Required for semantic steps. |
| `nth` | `Option<usize>` | 1-based (default 1). |
| `action` | `Option<ActAction>` | Default `press` — full list below. |
| `value` | `Option<String>` | Payload of `set_value` / `type_text` / `perform_action`. |
| `numeric_value` | `Option<f64>` | Payload of `set_numeric_value`. |
| `range` | `Option<[u32; 2]>` | `select_text` — 0-based, end EXCLUSIVE. |
| `timeout_ms` | `Option<u64>` | Auto-wait for actionability (default 3000). |
| `surface` | `Option<SurfaceKind>` | Shell-surface scope. |
| `key` / `held` / `text` / `wait` | keyboard fields | Keyboard/wait steps — no `selector`; see [keyboard](keyboard.md). |
| `then` | `Option<Box<ComputerAct>>` | Next step, run only if this one succeeded. |

`ActAction` — `press`, `focus`, `blur`, `toggle`, `select`, `expand`,
`collapse`, `show_menu`, `increment`, `decrement`, `scroll_into_view`,
`set_value`, `set_numeric_value`, `select_text`, `type_text`,
`perform_action`.

### Output — `ActOutput`

`sent: String` — the joined per-step report
(`set_value \`entry\` (app) → waited 400 ms → pressed \`button\``).

---

## `control`

### Input — `ComputerControl` (ONE step; chain via `then`)

| Field | Type | Meaning |
|---|---|---|
| `x`, `y` | `Option<i32>` | Coordinate form — desktop pixels. |
| `action` | `Option<PointerAction>` | `click` (default), `double_click`, `right_click`, `move`, `down`, `up`, `scroll`, `drag`. |
| `button` | `Option<String>` | Pointer button for `down`/`up` (default `"left"`). |
| `count` | `Option<u32>` | Click count (`2` = double-click). |
| `dx`, `dy` | `Option<i32>` | `scroll` wheel steps (required for scroll). |
| `app` / `pid` / `surface` | `Option<…>` | Element-form scope (exactly one). |
| `selector` / `to_selector` | `Option<String>` | Element form; `to_selector` = drag's end. |
| `nth` | `Option<usize>` | 1-based match index. |
| `anchor` | `Option<PointerAnchor>` | Where in the bounds the point lands — `center` (default), `top_left`, `top_right`, `bottom_left`, `bottom_right`. |
| `x2`, `y2` | `Option<i32>` | Drag's coordinate end. |
| `duration_ms` | `Option<u64>` | Drag pacing (default 150, floor 50). |
| `held` | `Option<Vec<String>>` | Modifier chord. |
| `key` | `Option<String>` | Keyboard step — no pointer fields. |
| `text` | `Option<String>` | Literal typing — rejects `held`. |
| `wait` | `Option<u64>` | Inline pause, ms (cap 10 000). |
| `then` | `Option<Box<ComputerControl>>` | Next step (shell `&&` semantics). |

### Output — `ControlOutput`

`sent: String` — the joined per-step report
(`click \`text_field[name='Filename']\` → typed "quarterly-report" →
waited 600 ms → pressed enter`).

---

## Error surfaces

Tool failures are `String` renders produced by `computer::errors::render`
from xa11y's `Error`/`Diagnosis` — the variant table and reading habit live
on the [`errors`](errors.md) page. Argument validation errors state that no
platform call was made.
