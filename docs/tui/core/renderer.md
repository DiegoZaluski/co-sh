# `renderer` — driving the frame loop

`Renderer` owns the terminal and the renderable tree, and pushes one frame to
the screen per call. It is the "managed" render path: you give it a
[`RootRenderable`](renderable.md#rootrenderable), a [`RendererConfig`](#rendererconfig),
and a ratatui `Terminal`, and it handles layout, drawing, and resizing.

```rust
Renderer::new(terminal: Terminal<B>, config: RendererConfig) -> Self   // B: ratatui Backend
```

---

## Configuration

`RendererConfig` is a plain struct with a `Default` — start from
`RendererConfig::default()` and override what you need:

| Field | Default | Meaning |
|---|---|---|
| `alternate_screen` | `true` | Use the terminal's alternate screen buffer. |
| `width` / `height` | `80` / `24` | Fallback size (used before the terminal reports its real size). |
| `target_fps` / `max_fps` | `30` / `60` | Frame-rate targets. |
| `exit_on_ctrl_c` | `true` | Terminate the loop on Ctrl+C. |
| `clear_on_shutdown` | `true` | Restore the screen on shutdown. |
| `enable_mouse_movement` | `true` | Report mouse moves (not just clicks). |
| `use_mouse` | `true` | Enable mouse capture. |
| `auto_focus` | `true` | Automatically focus the first focusable widget. |
| `background_color` | `None` | Global background override. |
| `screen_mode` | `AlternateScreen` | See below. |
| `external_output_mode` | `Passthrough` | See below. |
| `console_mode` | `Disabled` | See below. |
| `debounce_delay` | `100ms` | Input debounce. |
| `memory_snapshot_interval` | `0s` | Off by default; interval for heap snapshots. |
| `gather_stats` | `false` | Collect frame-time stats. |
| `max_stat_samples` | `300` | Ring buffer size for stats. |

The three mode enums:

```rust
pub enum ScreenMode { AlternateScreen, MainScreen, SplitFooter { footer_height: u16 } }
pub enum ExternalOutputMode { CaptureStdout, Passthrough }
pub enum ConsoleMode { ConsoleOverlay, Disabled }
```

## Construction and teardown

```rust
let mut renderer = Renderer::new(terminal, RendererConfig::default());
renderer.root_mut().add_child(Box::new(my_widget));
```

| Method | Purpose |
|---|---|
| `root() -> &RootRenderable` | Read the root node (never `None`). |
| `root_mut() -> &mut RootRenderable` | Build the tree: add/remove children here. |
| `config() -> &RendererConfig` | Inspect the current config. |
| `frame_count() -> u64` | Number of frames pushed so far. |
| `layout(width: f32, height: f32)` | Recompute layout for the whole tree. |
| `render_frame(delta_time: f64) -> Result<(), B::Error>` | **Layout + draw one frame.** |
| `resize(width: u16, height: u16)` | Resize the terminal buffer. |
| `destroy()` / `is_destroyed()` | Tear down / query teardown state. |

## The frame cycle

`render_frame` does three things:

1. **Measure** — read the terminal's current size.
2. **Layout** — rebuild a `LayoutTree`, walk the renderable tree calling
   `build_style()` on every node, solve layout at the measured size, and
   hand each node its computed `taffy::Layout` via `apply_layout`.
3. **Draw** — call `terminal.draw`, rendering the root into the frame's
   `Buffer` via `render_self`.

## Typical app loop

```rust,ignore
let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
let terminal = ratatui::Terminal::new(backend)?;
let mut renderer = Renderer::new(terminal, RendererConfig::default());

// Build the UI tree once.
renderer.root_mut().add_child(Box::new(panel));

// One frame per loop iteration.
loop {
    renderer.render_frame(0.0)?;   // delta_time in seconds for animations
    // … handle input; update widgets; break on quit …
}
renderer.destroy();
```

## Stats

When `gather_stats` is on, `RendererStats` holds `fps`, `frame_count`,
`frame_times`, and `average/min/max_frame_time`. `RendererFrameEvent` carries
`frame_id` for per-frame hooks.

Next: [syntax_style — the style registry](syntax_style.md).
