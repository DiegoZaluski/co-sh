# `TextareaRenderable` — multi-line editor

A full multi-line text editor widget: cursor movement, selection, line
operations, undo/redo, configurable key bindings, and a submit callback.
Focusable. This is the widget behind multi-line prompt inputs.

```rust
TextareaRenderable::new(options: TextareaOptions) -> Self
```

## `TextareaOptions`

| Field | Default | Meaning |
|---|---|---|
| `initial_value` | `None` | Starting content. |
| `background_color` / `text_color` | transparent / white | Base colors. |
| `focused_background_color` / `focused_text_color` | `#1a1a1a` / white | Colors while focused. |
| `placeholder` | `None` | Ghost text when empty. |
| `placeholder_color` | muted | Placeholder color. |
| `key_bindings` | `None` (defaults) | [`TextareaKeyBinding`](#textareaaction--key-bindings) list. |
| `wrap` | `true` | Word-wrap long lines. |
| `min_height` / `max_height` | — | Editor height bounds. |

```rust
pub struct TextareaKeyBinding {
    pub name: String,
    pub ctrl: bool, pub meta: bool, pub shift: bool, pub super_key: bool,
    pub action: TextareaAction,
}
```

## `TextareaAction`

The editor commands a key binding can trigger:

- **Movement**: `MoveLeft`, `MoveRight`, `MoveUp`, `MoveDown`,
  `LineHome`, `LineEnd`, `VisualLineHome`, `VisualLineEnd`, `BufferHome`,
  `BufferEnd`, `WordForward`, `WordBackward`.
- **Selection**: `SelectLeft` … `SelectDown`, `SelectLineHome/End`,
  `SelectVisualLineHome/End`, `SelectBufferHome/End`, `SelectWordForward`,
  `SelectWordBackward`, `SelectAll`.
- **Editing**: `Backspace`, `Delete`, `DeleteLine`, `DeleteToLineEnd`,
  `DeleteToLineStart`, `DeleteWordForward`, `DeleteWordBackward`, `Newline`.
- **History**: `Undo`, `Redo`.
- **Submit**: `Submit` (fires the `on_submit` callback).

## Callbacks and focus

```rust
pub type SubmitEvent = ();
pub type OnSubmitCallback = Box<dyn FnMut()>;

pub fn set_on_submit(&mut self, handler: Option<OnSubmitCallback>)
pub const fn focus(&mut self)
pub const fn blur(&mut self)
pub const fn is_focused(&self) -> bool
```

`set_on_submit` installs a callback fired when the user submits; pass `None`
to clear it.

## Reading and writing content

```rust
value() -> String              // current content
set_value(&str)                // replace content
placeholder() -> Option<&str>
set_placeholder(Option<String>)
```

## Editing commands

All return `bool` — `true` when the command changed state:

| Group | Methods |
|---|---|
| Insert | `insert_text(&str)`, `new_line()`, `submit()` |
| Delete | `delete_char_backward()`, `delete_char()`, `delete_line()`, `delete_to_line_end()`, `delete_to_line_start()` |
| Move | `move_cursor_left/right/up/down()`, `goto_buffer_home/end()`, `goto_line_home/end()` |
| History | `undo()`, `redo()` |
| Select | `select_all()` |

Example — a small "submit on Enter" loop:

```rust,ignore
use cosh_tui::core::renderables::textarea::{TextareaRenderable, TextareaOptions};

let mut editor = TextareaRenderable::new(TextareaOptions {
    initial_value: Some("line one".into()),
    ..Default::default()
});

editor.insert_text("\nline two");    // becomes multi-line
editor.move_cursor_up();             // cursor back to line one
assert_eq!(editor.value(), "line one\nline two");
```

Styling: `set_focused_background_color` / `set_focused_text_color` take
`Option<ColorInput>`.

Next: [select — vertical option list](select.md).
