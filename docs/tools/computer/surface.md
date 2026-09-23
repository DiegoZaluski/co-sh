# `surface` — targeting the desktop itself

Applications are not the whole screen. The menu bar, the taskbar/panel/dock,
tray status items, the desktop underneath, and transient flyouts (notification
centers, shell context menus) all live OUTSIDE any application's tree.
`surface` makes them a first-class targeting root: pass `surface` instead of
`name`/`pid` to the addressing tools and their selectors resolve from the
shell's element tree.

```
surface::resolve(kind: SurfaceKind, timeout: Duration) -> Result<ShellSurface, String>
surface::resolve_with(provider, kind, timeout) -> Result<ShellSurface, String>   // injected
surface::surface_label(kind) -> &'static str                                     // human label
```

Consumers rarely call these directly — [`snapshot`](snapshot.md),
[`act`](act.md) and [`screenshot`](screenshot.md) accept `surface` as a
scope field and route through here.

---

## The model: the third root

| Root | Fields | Means |
|---|---|---|
| Application | `name`/`pid` | One app's accessibility tree. |
| **Shell surface** | `surface` | Part of the desktop chrome itself. |

The vocabulary — [`SurfaceKind`](types.md), snake_case on the wire, mirroring
xa11y's `ShellSurfaceKind` exactly:

| Kind | Wire | What lives there |
|---|---|---|
| `MenuBar` | `"menu_bar"` | The frontmost app's menu bar (macOS: File → Save As; per-window menus on Windows/Linux live in the app's own tree). |
| `StatusItems` | `"status_items"` | Tray icons / menu-bar extras. Windows: the overflow's content exists only while the overflow is open. |
| `Taskbar` | `"taskbar"` | The Windows taskbar. |
| `Panel` | `"panel"` | GNOME top bar, KDE panel, and friends. |
| `Dock` | `"dock"` | The macOS Dock. |
| `Desktop` | `"desktop"` | Icons and wallpaper-level widgets. |
| `Flyout` | `"flyout"` | Transient shell popups open RIGHT NOW. |
| `Unknown` | `"unknown"` | A surface the platform reported without a known kind. |

## Flyouts exist only while open

The one timing rule worth memorizing: a flyout (Notification Center, Quick
Settings, a shell context menu) has elements **only while it is open**. The
caller performs the press that opens it on a real element first, THEN
re-enumerates — which is also why a flyout snapshot can legitimately be
empty.

## Edges: platform honesty

Surface enumeration is only as rich as the backend. On macOS and
GNOME-family desktops the full vocabulary is reachable. On X11 window
managers like i3 the backend classifies only AT-SPI dock frames — a
legitimately empty result, not a bug. That honesty is built into the errors:
when enumeration finds nothing, the error carries a **live census** of what
the platform DID report, so "0 surfaces" is diagnosable from the message
alone (see [`errors`](errors.md)).

```rust,ignore
// The taskbar's tree, same outline as an app snapshot:
computer.snapshot(&ComputerSnapshot {
    surface: Some(SurfaceKind::Taskbar),
    ..Default::default()
}).await?;

// Press a taskbar icon by selector:
computer.act(&ComputerAct {
    surface: Some(SurfaceKind::Taskbar),
    selector: Some("button[name='Files']".into()),
    ..Default::default()
}).await?;
```

## See also

[`snapshot`](snapshot.md) / [`act`](act.md) for the tools that consume it ·
`cargo run --example computer-surface`
