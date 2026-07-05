use bitflags::bitflags;

bitflags! {
    pub struct TextAttributes: u32 {
        const NONE = 0;
        const BOLD = 1 << 0; // 1
        const DIM = 1 << 1; // 2
        const ITALIC = 1 << 2; // 4
        const UNDERLINE = 1 << 3; // 8
        const BLINK = 1 << 4; // 16
        const INVERSE = 1 << 5; // 32
        const HIDDEN = 1 << 6; // 64
        const STRIKETHROUGH = 1 << 7; // 128
    }
}

// Constants for attribute bit packing
pub const ATTRIBUTE_BASE_BITS: u32 = 8;
pub const ATTRIBUTE_BASE_MASK: u32 = 0xff;

/// Extract the base 8 bits of attributes from a u32 attribute value.
/// Currently we only use the first 8 bits for standard text attributes.
#[must_use]
pub fn get_base_attributes(attr: u32) -> u32 {
    attr & ATTRIBUTE_BASE_MASK
}

pub type ThemeMode = &'static str;
pub const THEME_MODE_DARK: ThemeMode = "dark";
pub const THEME_MODE_LIGHT: ThemeMode = "light";

pub type CursorStyle = &'static str;
pub const CURSOR_STYLE_BLOCK: CursorStyle = "block";
pub const CURSOR_STYLE_LINE: CursorStyle = "line";
pub const CURSOR_STYLE_UNDERLINE: CursorStyle = "underline";
pub const CURSOR_STYLE_DEFAULT: CursorStyle = "default";

pub type MousePointerStyle = &'static str;
pub const MOUSE_POINTER_DEFAULT: MousePointerStyle = "default";
pub const MOUSE_POINTER_POINTER: MousePointerStyle = "pointer";
pub const MOUSE_POINTER_TEXT: MousePointerStyle = "text";
pub const MOUSE_POINTER_CROSSHAIR: MousePointerStyle = "crosshair";
pub const MOUSE_POINTER_MOVE: MousePointerStyle = "move";
pub const MOUSE_POINTER_NOT_ALLOWED: MousePointerStyle = "not-allowed";

pub use super::rgba::RGBA;

pub struct CursorStyleOptions {
    pub style: Option<CursorStyle>,
    pub blinking: Option<bool>,
    pub color: Option<RGBA>,
    pub cursor: Option<MousePointerStyle>,
}

pub enum DebugOverlayCorner {
    TopLeft = 0,
    TopRight = 1,
    BottomLeft = 2,
    BottomRight = 3,
}

pub enum TargetChannel {
    Fg = 1,
    Bg = 2,
    Both = 3,
}

pub type WidthMethod = &'static str;
pub const WIDTH_METHOD_WCWIDTH: WidthMethod = "wcwidth";
pub const WIDTH_METHOD_UNICODE: WidthMethod = "unicode";

pub type TerminalMultiplexer = &'static str;
pub const TERMINAL_MULTIPLEXER_NONE: TerminalMultiplexer = "none";
pub const TERMINAL_MULTIPLEXER_TMUX: TerminalMultiplexer = "tmux";
pub const TERMINAL_MULTIPLEXER_ZELLIJ: TerminalMultiplexer = "zellij";
pub const TERMINAL_MULTIPLEXER_SCREEN: TerminalMultiplexer = "screen";
pub const TERMINAL_MULTIPLEXER_UNKNOWN: TerminalMultiplexer = "unknown";

pub type TerminalCapabilityState = &'static str;
pub const TERMINAL_CAPABILITY_STATE_UNKNOWN: TerminalCapabilityState = "unknown";
pub const TERMINAL_CAPABILITY_STATE_SUPPORTED: TerminalCapabilityState = "supported";
pub const TERMINAL_CAPABILITY_STATE_UNSUPPORTED: TerminalCapabilityState = "unsupported";

pub struct TerminalInfo {
    pub name: String,
    pub version: String,
    pub from_xtversion: bool,
}

#[allow(clippy::struct_excessive_bools)]
pub struct TerminalCapabilities {
    pub kitty_keyboard: bool,
    pub kitty_graphics: bool,
    pub rgb: bool,
    pub ansi256: bool,
    pub unicode: WidthMethod,
    pub sgr_pixels: bool,
    pub color_scheme_updates: bool,
    pub explicit_width: bool,
    pub scaled_text: bool,
    pub sixel: bool,
    pub focus_tracking: bool,
    pub sync: bool,
    pub bracketed_paste: bool,
    pub hyperlinks: bool,
    pub osc52: bool,
    pub osc52_support: TerminalCapabilityState,
    pub notifications: bool,
    pub explicit_cursor_positioning: bool,
    pub remote: bool,
    pub multiplexer: TerminalMultiplexer,
    pub terminal: TerminalInfo,
}

pub struct MemorySnapshot {
    pub heap_used: u64,
    pub heap_total: u64,
    pub array_buffers: u64,
}

pub struct Selection;

// ── Mouse event types ──────────────────────────────────────────────────────────

/// Mouse buttons, matching crossterm's convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
}

/// Types of mouse event that can be dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventType {
    Down,
    Up,
    Drag,
    Move,
    ScrollDown,
    ScrollUp,
}

/// Modifier keys held during a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl MouseModifiers {
    #[must_use]
    pub fn none() -> Self {
        MouseModifiers { shift: false, alt: false, ctrl: false }
    }
}

/// A mouse event dispatched by the renderer or application.
#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub event_type: MouseEventType,
    pub button: MouseButton,
    pub x: u16,
    pub y: u16,
    pub modifiers: MouseModifiers,
    pub(crate) propagation_stopped: bool,
    pub(crate) default_prevented: bool,
}

impl MouseEvent {
    #[must_use]
    pub fn new(
        event_type: MouseEventType,
        button: MouseButton,
        x: u16,
        y: u16,
        modifiers: MouseModifiers,
    ) -> Self {
        MouseEvent {
            event_type,
            button,
            x,
            y,
            modifiers,
            propagation_stopped: false,
            default_prevented: false,
        }
    }

    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }

    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    #[must_use]
    pub fn is_left_click(&self) -> bool {
        self.button == MouseButton::Left && self.event_type == MouseEventType::Up
    }
}

pub trait RenderContext<TRenderable> {
    fn add_to_hit_grid(&mut self, x: i32, y: i32, width: i32, height: i32, id: i32);
    fn push_hit_grid_scissor_rect(&mut self, x: i32, y: i32, width: i32, height: i32);
    fn pop_hit_grid_scissor_rect(&mut self);
    fn clear_hit_grid_scissor_rects(&mut self);
    fn width(&self) -> i32;
    fn height(&self) -> i32;
    /// Monotonic, bumped once per `loop()` iteration. Lets renderables dedupe per-frame work.
    fn frame_id(&self) -> u64;
    fn request_render(&mut self);
    fn set_cursor_position(&mut self, x: i32, y: i32, visible: bool);
    fn set_cursor_style(&mut self, options: CursorStyleOptions);
    fn set_cursor_color(&mut self, color: RGBA);
    fn set_mouse_pointer(&mut self, shape: MousePointerStyle);
    fn width_method(&self) -> WidthMethod;
    fn capabilities(&self) -> Option<&TerminalCapabilities>;
    fn request_live(&mut self);
    fn drop_live(&mut self);
    fn has_selection(&self) -> bool;
    fn get_selection(&self) -> Option<&Selection>;
    fn request_selection_update(&mut self);
    fn current_focused_renderable(&self) -> Option<&TRenderable>;
    fn focus_renderable(&mut self, renderable: &TRenderable);
    fn blur_renderable(&mut self, renderable: &TRenderable);
    fn claim_first_line_offset(&mut self, renderable: Option<&TRenderable>) -> i32;
    fn register_lifecycle_pass(&mut self, renderable: &TRenderable);
    fn unregister_lifecycle_pass(&mut self, renderable: &TRenderable);
    fn clear_selection(&mut self);
    fn start_selection(&mut self, renderable: &TRenderable, x: i32, y: i32);
    fn update_selection(
        &mut self,
        current_renderable: Option<&TRenderable>,
        x: i32,
        y: i32,
        options: Option<UpdateSelectionOptions>,
    );
}

pub struct UpdateSelectionOptions {
    pub finish_dragging: Option<bool>,
}

pub struct ViewportBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub style_id: i32,
    pub priority: Option<i32>,
    pub hl_ref: Option<i32>,
}

pub struct LineInfo {
    /// Display-column offset for each visual line start.
    pub line_start_cols: Vec<i32>,
    /// Display-column width for each visual line.
    pub line_width_cols: Vec<i32>,
    /// Maximum display-column width across the reported lines.
    pub line_width_cols_max: i32,
    /// Source logical line index for each visual line.
    pub line_sources: Vec<i32>,
    /// Wrap index within each source logical line.
    pub line_wraps: Vec<i32>,
}

pub trait LineInfoProvider {
    fn line_info(&self) -> &LineInfo;
    fn line_count(&self) -> i32;
    fn virtual_line_count(&self) -> i32;
    fn scroll_y(&self) -> i32;
}

pub struct CapturedSpan {
    pub text: String,
    pub fg: RGBA,
    pub bg: RGBA,
    pub attributes: u32,
    pub width: i32,
}

pub struct CapturedLine {
    pub spans: Vec<CapturedSpan>,
}

pub struct CapturedFrame {
    pub cols: i32,
    pub rows: i32,
    pub cursor: (i32, i32),
    pub lines: Vec<CapturedLine>,
}
