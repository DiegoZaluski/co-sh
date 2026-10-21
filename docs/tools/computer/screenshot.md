# `screenshot` — capture the desktop, ground the selectors

`screenshot` is how the model **sees**. The accessibility tree
([`snapshot`](snapshot.md)) is the structured truth; the screenshot is what
the user actually perceives — layout, colors, a canvas nobody labeled. With
annotation on, the capture bridges seeing and addressing: boxes are drawn on
the image and a **legend** hands back selectors the other tools accept.

```
Computer::screenshot(&self, input: &ComputerScreenshot) -> Result<ScreenshotOutput, String>
screenshot(metadata, ComputerScreenshot) -> Result<ScreenshotOutput, String>
```

---

## The model: pick the region → capture → (optionally) annotate

**Region first** — mutually exclusive choices:

| Wire shape | Captures |
|---|---|
| `{}` | The full desktop. |
| `{"region": [x, y, w, h]}` | One display region, display pixels, origin top-left. |
| `{"app": "Reports", "selector": "…"}` | The pixels under the matched element (element capture REQUIRES `selector`). |
| `{"surface": "taskbar", "selector": "…"}` | The pixels under a shell surface element — [`surface`](surface.md) — (requires `selector`). |

**Annotation** is the second axis, and it is *gated*: pass
`"annotate": true` (requires an app scope) and the capture draws a labeled
box on every matching element — `selector` then chooses WHICH elements get
boxes (default `"*"`), not which pixels are shot. The legend maps each
drawn tag back to a selector:

```
B7  button[name='Export']  button 'Export'
```

That selector is copy-ready for [`act`](act.md) / [`control`](control.md) —
visual grounding without ever doing pixel math.

## Keep the pixels: `save_path`

Any capture form can ALSO write the PNG to disk — pass `save_path`
(relative paths resolve against cosh's working directory; the file is
overwritten, parent directories are created):

```
ScreenshotOutput.saved_to: "shot.png"     // echoed as provided
```

The output names the file in `saved_to` (the JSON result the harness
plate shows) and the image STILL returns inline — saving is for the
user's report/pipes, never a reason the model loses sight of the pixels.
Outside any project root the harness asks for approval before the write
(inside-root saves are free).

## The coordinate mapping (when you DO need pixels)

The delivered image may be **downscaled** (`max_width`, default 1568) to
stay inside a tool-result budget, so image pixels are not desktop pixels.
`ScreenshotOutput` reports the mapping:

```text
desktop = desktop_origin + image_coord × desktop_scale
```

Suppose the model spots a button at image pixel (410, 252) on an 800×600
delivery with `desktop_origin = (0, 0)` and `desktop_scale = (1.0, 1.0)` —
the desktop point is (410, 252). On a Retina-class scale of (2.0, 2.0) it
would be (820, 504). Prefer the element form — coordinates are the last
resort for node-less canvas targets (see [`control`](control.md)).

## Edges

- **Build mode allows only the annotated form.** Plain captures (full
  display / `region`) hand the model pixel coordinates, so they are denied
  in Build mode; `annotate: true` is selector-grounded and allowed. See
  [permissions](computer.md#permissions).
- **`nth` is ignored when annotating** — the legend covers every match;
  narrow `selector` instead.
- **Comma alternations are rejected up front** (`"button, link"`): `:nth(k)`
  would bind to the last clause only, so the legend would label a box with a
  selector that resolves elsewhere.
- **`save_path` is an echo, not a resolution** — the returned `saved_to`
  carries the path exactly as the call issued it (only the filesystem
  operations trim surrounding whitespace); nothing is canonicalized or
  absolutized behind your back.
- Coordinates in tool calls always refer to the **delivered** image — never
  the original screen size.

## See also

[`snapshot`](snapshot.md) for the tree form of the same app ·
`cargo run --example computer-screenshot`
