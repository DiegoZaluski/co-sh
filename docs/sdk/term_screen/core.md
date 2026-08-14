# `term_screen::core` — Terminal, Screen, configuration

The user-facing entry point of the module. (Ported from WezTerm — see the
[module overview](term_screen.md) for the attribution note; this page covers the API
only.)

## `Terminal` — the entry point

```rust,ignore
pub struct Terminal { /* TerminalState + escape-sequence Parser */ }
impl Deref<Target = TerminalState> for Terminal {}
impl DerefMut for Terminal {}
```

Because of the `Deref` impls, every `TerminalState` method is available
directly on a `Terminal` — `term.screen()`, `term.resize(...)`, etc.

```rust,ignore
pub fn new(
    size: TerminalSize,
    config: Arc<dyn TerminalConfiguration + Send + Sync>,
    term_program: &str,     // reported in response to ESC [ > q
    term_version: &str,     // (the terminal identification sequence)
    writer: Box<dyn std::io::Write + Send>,   // input goes to the PTY here
) -> Self

pub fn advance_bytes<B: AsRef<[u8]>>(&mut self, bytes: B)
// Feed output bytes; chunks need not be complete escape sequences.

pub fn perform_actions(&mut self, actions: Vec<escape_parser::Action>)
// Apply pre-parsed actions directly (bypasses the byte parser).
```

## `TerminalState` — the model

```rust,ignore
pub fn new(size, config, term_program, term_version, writer) -> Self
pub fn resize(&mut self, size: TerminalSize)
pub fn get_size(&self) -> TerminalSize
pub fn screen(&self) -> &Screen
pub fn screen_mut(&mut self) -> &mut Screen
pub fn set_config(&mut self, config: Arc<dyn TerminalConfiguration>)
pub fn get_config(&self) -> Arc<dyn TerminalConfiguration>
pub fn get_title(&self) -> &str
pub fn get_progress(&self) -> Progress
pub fn get_keyboard_encoding(&self) -> KeyboardEncoding
pub fn get_semantic_zones(&mut self) -> error::Result<Vec<SemanticZone>>
pub fn pen(&self) -> CellAttributes            // the current "pen" (pending attrs)
pub fn make_all_lines_dirty(&mut self)
```

Handlers and state:

```rust,ignore
pub fn set_clipboard(&mut self, clipboard: &Arc<dyn Clipboard>)
pub fn set_device_control_handler(&mut self, handler: Box<dyn DeviceControlHandler>)
pub fn set_notification_handler(&mut self, handler: Box<dyn AlertHandler>)
pub fn palette(&self) -> ColorPalette
pub fn palette_mut(&mut self) -> &mut ColorPalette
pub fn focus_changed(&mut self, focused: bool)
pub fn send_paste(&mut self, text: &str) -> error::Result<()>
pub fn erase_scrollback(&mut self)
pub fn erase_scrollback_and_viewport(&mut self)
pub fn activate_alt_screen(&mut self, seqno: SequenceNo)
pub fn activate_primary_screen(&mut self, seqno: SequenceNo)
pub fn full_reset(&mut self)
```

## `Screen` — reading what's on the terminal

```rust,ignore
pub struct Screen { pub physical_rows: usize, /* ... */ }

pub fn new(size: TerminalSize, config: &Arc<dyn TerminalConfiguration>,
           allow_scrollback: bool, seqno: SequenceNo, bidi_mode: BidiMode) -> Self
pub fn resize(&mut self, size: TerminalSize)
pub fn scrollback_rows(&self) -> usize
pub fn get_cell(&mut self, x: usize, y: VisibleRowIndex) -> Option<&Cell>
pub fn cell_mut(&mut self, x: usize, y: VisibleRowIndex) -> Option<&mut Cell>
pub fn line_mut(&mut self, idx: PhysRowIndex) -> &mut Line
pub fn dirty_line(&mut self, idx: VisibleRowIndex, seqno: SequenceNo)
pub fn erase_scrollback(&mut self)
```

Iteration (the idiomatic way to walk the screen — `visible_lines()`/`all_lines()` exist only in the test harness):

```rust,ignore
pub fn for_each_phys_line<F>(&self, f: F)                       // F: FnMut(PhysRowIndex, &Line)
pub fn for_each_phys_line_mut<F>(&mut self, f: F)               // F: FnMut(PhysRowIndex, &mut Line)
pub fn with_phys_lines<F>(&self, phys_range: Range<PhysRowIndex>, f: F)
pub fn lines_in_phys_range(&self, phys_range: Range<PhysRowIndex>) -> Vec<Line>
pub fn get_changed_stable_rows(...)
```

### Row-index types

WezTerm uses distinct signedness to catch index arithmetic mistakes at
compile time:

| Type | Meaning |
|---|---|
| `PhysRowIndex = usize` | Index into `screen.lines`; 0 = top of scrollback |
| `VisibleRowIndex = i64` | Index into the visible viewport; 0 = first visible row |
| `ScrollbackOrVisibleRowIndex = i32` | Signed — can index backwards into scrollback |
| `StableRowIndex = isize` | Logical line identity that survives scrolling/purge |

`StableRowIndex` is the one that stays put: a line keeps its `StableRowIndex`
even as scrollback purges shift its `PhysRowIndex`. Use `stable_range(...)`,
`stable_row_to_phys(...)`, and `visible_row_to_stable_row(...)` to convert.

## `TerminalSize`

```rust,ignore
pub struct TerminalSize {
    pub rows: usize,
    pub cols: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub dpi: u32,
}
// Default: 24 × 80, zero pixels/dpi.
```

## `TerminalConfiguration` — the config trait

```rust,ignore
pub trait TerminalConfiguration: Downcast + std::fmt::Debug + Send + Sync {
    fn generation(&self) -> usize { 0 }          // bump to flush caches
    fn scrollback_size(&self) -> usize { 3500 }
    fn enable_csi_u_key_encoding(&self) -> bool { false }
    fn color_palette(&self) -> ColorPalette;     // the ONLY required method
    fn canonicalize_pasted_newlines(&self) -> NewlineCanon { NewlineCanon::default() }
    fn alternate_buffer_wheel_scroll_speed(&self) -> u8 { 3 }
    fn enq_answerback(&self) -> String { String::new() }
}
```

Implement it with a struct (the cosh-tools `VisionConfig` implements just
`color_palette()` and `scrollback_size() -> 0`), wrap in `Arc`, and hand it
to `Terminal::new`.

## Handler traits

```rust,ignore
pub trait Clipboard: Send + Sync { /* set_clipboard(selection, text) */ }
pub enum ClipboardSelection { Clipboard, PrimarySelection }
pub trait AlertHandler: Send + Sync { /* alert(...) */ }
pub enum Alert { Bell, ... }
pub trait DeviceControlHandler: Send + Sync { /* ... */ }
```

---

Next: [surface — Line and Cell](surface.md).
