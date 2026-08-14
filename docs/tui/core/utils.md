# `utils` — text attribute helpers

Small pure functions for packing and unpacking the `u32` text-attribute value
carried by [`TextChunk`](lib/styled_text.md) and renderables. The layout of
that `u32` is documented in [`types`](types.md): bits 0–7 are the standard
[`TextAttributes`](types.md#textattributes--bitflags-for-text-styling)
flags; bits 8–31 may carry a link id.

## Building attributes from options

```rust
pub struct TextAttributeOptions {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

pub const fn create_text_attributes(options: TextAttributeOptions) -> u32
```

`create_text_attributes` ORs the enabled flags into a `u32`. Use it instead of
hand-computing bit masks:

```rust,ignore
let attrs = create_text_attributes(TextAttributeOptions {
    bold: true,
    underline: true,
    ..Default::default()
});
// attrs == TextAttributes::BOLD.bits() | TextAttributes::UNDERLINE.bits()
```

## Link-id packing

Two helpers store and retrieve a 24-bit link id in bits 8–31 of the attribute
value:

```rust
pub const fn attributes_with_link(base_attributes: u32, link_id: u32) -> u32
pub const fn get_link_id(attributes: u32) -> u32
```

```rust,ignore
let attrs = attributes_with_link(create_text_attributes(TextAttributeOptions { ..Default::default() }), 42);
assert_eq!(get_link_id(attrs), 42);
```

`attributes_with_link` masks the base to the low 8 bits, shifts the link id
up by 8 (masked to 24 bits), and ORs them together.

Next: [lib/ — colors, borders, text, unicode](lib/primitives.md).
