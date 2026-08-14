# `types` — attributes, mouse events, selection

The shared types that widgets, the renderer, and applications pass around:
text attributes, mouse events, selection state, terminal capabilities, and
the `RenderContext` trait.

---

## `TextAttributes` — bitflags for text styling

```rust
bitflags! {
    pub struct TextAttributes: u32 {
        const NONE          = 0;
        const BOLD          = 1 << 0; // 1
        const DIM           = 1 << 1; // 2
        const ITALIC        = 1 << 2; // 4
        const UNDERLINE     = 1 << 3; // 8
        const BLINK         = 1 << 4; // 16
        const INVERSE       = 1 << 5; // 32
        const HIDDEN        = 1 << 6; // 64
        const STRIKETHROUGH = 1 << 7; // 128
    }
}
```

Attributes are stored as a plain `u32` on text chunks and renderables. The
low 8 bits are the standard text attributes above; bits 8–31 can carry extra
payloads such as a link id (see [`utils::attributes_with_link`](utils.md)).

- `get_base_attributes(attr: u32) -> u32` — mask off everything but the low 8
  attribute bits (`attr & 0xff`).
- `ATTRIBUTE_BASE_BITS = 8`, `ATTRIBUTE_BASE_MASK = 0xff` — constants for the
  bit layout.

Use [`create_text_attributes`](utils.md) to build a `u32` from named boolean
options instead of hand-computing bits.

## Mode and style string constants

Several `&'static str` type aliases describe terminal state:

| Alias | Values |
|---|---|
| `ThemeMode` | `"dark"`, `"light"` |
| `CursorStyle` | `"block"`, `"line"`, `"underline"`, `"default"` |
| `MousePointerStyle` | `"default"`, `"pointer"`, `"text"`, `"crosshair"`, `"move"`, `"not-allowed"` |
| `WidthMethod` | `"wcwidth"`, `"unicode"` |
| `TerminalMultiplexer` | `"none"`, `"tmux"`, `"zellij"`, `"screen"`, `"unknown"` |
| `TerminalCapabilityState` | `"unknown"`, `"supported"`, `"unsupported"` |

Each alias comes with matching `const` values (`THEME_MODE_DARK`,
`CURSOR_STYLE_BLOCK`, `MOUSE_POINTER_POINTER`, `WIDTH_METHOD_WCWIDTH`, …).

## `CursorStyleOptions`

Carries the requested cursor appearance:

```rust
pub struct CursorStyleOptions {
    pub style: Option<CursorStyle>,       // "block" | "line" | "underline" | "default"
    pub blinking: Option<bool>,
    pub color: Option<RGBA>,
    pub cursor: Option<MousePointerStyle>, // shape of the mouse pointer
}
```

## Terminal info and capabilities

```rust
pub struct TerminalInfo {
    pub name: String,
    pub version: String,
    pub from_xtversion: bool,
}
```

`TerminalCapabilities` is a large struct of booleans describing what the
terminal supports: `kitty_keyboard`, `kitty_graphics`, `rgb`, `ansi256`,
`sgr_pixels`, `sixel`, `focus_tracking`, `sync`, `bracketed_paste`,
`hyperlinks`, `osc52`, `notifications`, and more, plus `unicode: WidthMethod`,
`multiplexer: TerminalMultiplexer`, and `terminal: TerminalInfo`.

`RenderContext::capabilities()` exposes an optional snapshot to widgets that
need to adapt (e.g. fall back to ANSI-256 when truecolor is unsupported).

## `Selection` — text selection state

Tracks a drag selection with an anchor (start) and focus (current end):

```rust
pub struct Selection {
    pub anchor_x: i32, pub anchor_y: i32,   // where the selection started
    pub focus_x:  i32, pub focus_y:  i32,   // current endpoint
    pub is_dragging: bool,
    pub is_active: bool,
}
```

| Method | Behavior |
|---|---|
| `new(x, y)` | Start a selection; anchor = focus = (x, y), `is_dragging = true`. |
| `update(x, y)` | Move the focus point during a drag; activates the selection. |
| `finish()` | End the drag (mouse up); still active. |
| `clear()` | Deactivate and stop dragging. |
| `bounds()` | `(min_x, min_y, max_x, max_y)` top-left / bottom-right in screen coords. |
| `has_non_zero_area()` | Active and anchor ≠ focus. |

## Mouse events

```rust
pub enum MouseButton { Left = 0, Middle = 1, Right = 2 }

pub enum MouseEventType { Down, Up, Drag, Move, ScrollDown, ScrollUp }

pub struct MouseModifiers { pub shift: bool, pub alt: bool, pub ctrl: bool }
// MouseModifiers::none() — all false

pub struct MouseEvent {
    pub event_type: MouseEventType,
    pub button: MouseButton,
    pub x: u16,
    pub y: u16,
    pub modifiers: MouseModifiers,
    // (crate-private) propagation_stopped, default_prevented
}
```

`MouseEvent::new(event_type, button, x, y, modifiers)` builds one;
`is_left_click()` is a convenience (`button == Left && event_type == Up`).
Two mutation methods mirror DOM semantics:

- `stop_propagation()` — the event won't bubble further up the tree.
- `prevent_default()` — the default handling is skipped.

Widgets receive mouse events through
[`Renderable::process_mouse_event`](renderable.md).

## `RenderContext<TRenderable>`

The interface between a renderable and the outside world during a frame. It
lets a widget:

- **Hit testing**: `add_to_hit_grid(x, y, w, h, id)`, scissor rects
  (`push_hit_grid_scissor_rect` / `pop_hit_grid_scissor_rect` /
  `clear_hit_grid_scissor_rects`).
- **Frame info**: `width()`, `height()`, `frame_id()` (monotonic per frame —
  lets renderables dedupe per-frame work).
- **Rendering control**: `request_render()`, `request_live()` /
  `drop_live()`.
- **Cursor & pointer**: `set_cursor_position(x, y, visible)`,
  `set_cursor_style(CursorStyleOptions)`, `set_cursor_color(RGBA)`,
  `set_mouse_pointer(MousePointerStyle)`.
- **Capabilities**: `width_method()`, `capabilities()`.
- **Selection**: `has_selection()`, `get_selection()`,
  `request_selection_update()`, `start_selection(renderable, x, y)`,
  `update_selection(...)`, `clear_selection()`.
- **Focus**: `current_focused_renderable()`, `focus_renderable(r)`,
  `blur_renderable(r)`.
- **Lifecycle**: `register_lifecycle_pass(r)`, `unregister_lifecycle_pass(r)`,
  `claim_first_line_offset(r)`.

`UpdateSelectionOptions { finish_dragging: Option<bool> }` tunes how
`update_selection` ends the drag.

## Text layout info

```rust
pub struct LineInfo {
    pub line_start_cols: Vec<i32>,    // display column of each visual line start
    pub line_width_cols: Vec<i32>,    // display width of each visual line
    pub line_width_cols_max: i32,
    pub line_sources: Vec<i32>,       // source logical line per visual line
    pub line_wraps: Vec<i32>,         // wrap index within each source line
}

pub trait LineInfoProvider {
    fn line_info(&self) -> &LineInfo;
    fn line_count(&self) -> i32;
    fn virtual_line_count(&self) -> i32;
    fn scroll_y(&self) -> i32;
}
```

## Captured output

```rust
pub struct CapturedSpan { pub text: String, pub fg: RGBA, pub bg: RGBA, pub attributes: u32, pub width: i32 }
pub struct CapturedLine { pub spans: Vec<CapturedSpan> }
pub struct CapturedFrame { pub cols: i32, pub rows: i32, pub cursor: (i32, i32), pub lines: Vec<CapturedLine> }
```

Used when a frame is captured for inspection or external output rather than
drawn to the terminal.

Other small types: `DebugOverlayCorner`, `TargetChannel`, `MemorySnapshot`,
`ViewportBounds`, `Highlight`, `UpdateSelectionOptions`.

Next: [layout — automatic layout with LayoutTree](layout.md).
