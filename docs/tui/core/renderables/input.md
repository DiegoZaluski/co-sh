# `InputRenderable` — single-line input

A single-line text field with a cursor, placeholder, min/max length
enforcement, and programmatic editing. Focusable.

```rust
InputRenderable::new(value: Option<String>) -> Self
```

## Configuration

| Method | Default | Effect |
|---|---|---|
| `set_value(&str)` | `""` | Replace the value (strips `\n`/`\r`, truncates to `max_length`, cursor to end). |
| `set_placeholder(String)` | `""` | Ghost text shown when the field is empty. |
| `set_max_length(usize)` | `1000` | Hard cap; truncating the value on shrink. |
| `set_min_length(usize)` | `0` | **Panics** if `min > max_length`. |
| `set_text_color(Option<ColorInput>)` | `#FFFFFF` | Text color. |
| `set_background_color(Option<ColorInput>)` | transparent | Background. |

## Reading

```rust
value() -> &str
placeholder() -> &str
max_length() -> usize
min_length() -> usize
cursor_offset() -> usize      // byte offset into value
```

## Editing

| Method | Behavior | Returns |
|---|---|---|
| `insert_text(&str)` | Insert at the cursor; strips newlines, respects `max_length`. | — |
| `delete_char_backward()` | Delete the char before the cursor. | `false` if at start / empty |
| `delete_char()` | Delete the char at the cursor. | `false` if at end |
| `set_value(&str)` | Replace everything. | — |

`insert_text` ignores newlines and stops at `max_length`; the cursor
advances with inserted text. `delete_char_backward` respects char
boundaries (won't split a multi-byte char).

## Rendering

`render_self` draws either the value or — when empty — the placeholder,
left-to-right from the area's start, clipped to the area width. No border is
drawn; wrap this widget in a `BoxRenderable` for a framed field.

## Example

```rust,ignore
use cosh_tui::core::renderables::input::InputRenderable;

let mut input = InputRenderable::new(Some("hi".into()));
input.set_placeholder("Type a message…".into());
input.set_max_length(200);

input.insert_text(" there");      // "hi there"
input.delete_char_backward();     // "hi ther"
assert_eq!(input.value(), "hi ther");
```

For a multi-line editor, see [`TextareaRenderable`](textarea.md).
