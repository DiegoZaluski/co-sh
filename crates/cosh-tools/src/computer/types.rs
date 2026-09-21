use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for `computer_apps`.
///
/// Intentionally empty — listing takes no parameters — but kept as an object
/// so the tool call shape matches every other tool (`{}`).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerApps {}

/// Output format for `computer_snapshot`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotFormat {
    /// Compact indented outline (one line per element). Default.
    #[default]
    Tree,
    /// Structured JSON (`role`/`name`/`value`/`children` per node).
    Json,
}

/// Input for `computer_snapshot`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerSnapshot {
    /// Application name (exact match, e.g. `"Safari"`). Provide `name` or
    /// `pid` — exactly one.
    pub name: Option<String>,
    /// Application process ID. Provide `name` or `pid` — exactly one.
    pub pid: Option<u32>,
    /// Optional CSS-like selector to snapshot only the matching subtree
    /// instead of the whole application, e.g. `"window[name='Main'] > group"`.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1).
    pub nth: Option<usize>,
    /// Maximum snapshot depth. Default 12; hard cap 40.
    pub max_depth: Option<u32>,
    /// Output format. Default `tree`.
    pub format: Option<SnapshotFormat>,
    /// How long to wait for the application to surface in the accessibility
    /// tree, in milliseconds. Default 3000.
    pub timeout_ms: Option<u64>,
}

/// One running application, as reported by `computer_apps`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AppInfo {
    /// Application name.
    pub name: String,
    /// Process ID, when the platform reports one.
    pub pid: Option<u32>,
    /// Whether this app currently holds the system foreground.
    pub foreground: bool,
}

/// Output of `computer_apps`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AppsOutput {
    /// Running applications (unsorted, platform enumeration order).
    pub apps: Vec<AppInfo>,
    /// Convenience count of [`AppsOutput::apps`].
    pub count: usize,
}

/// Input for `computer_screenshot`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerScreenshot {
    /// Capture only this region of the display, as `[x, y, width, height]`
    /// in display pixels (origin top-left). Mutually exclusive with element
    /// capture (`app`/`pid` + `selector`).
    pub region: Option<Vec<i32>>,
    /// Application name (exact match) for element capture: shoot only the
    /// pixels under the element matched by `selector`. Requires `selector`.
    pub app: Option<String>,
    /// Application process ID for element capture. Requires `selector`.
    pub pid: Option<u32>,
    /// Optional CSS-like selector for element capture, e.g.
    /// `"window[name='Main'] > group"`. Requires `app` or `pid`.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1).
    pub nth: Option<usize>,
    /// Maximum delivered image width in pixels (default 1568). Larger
    /// captures are downscaled preserving aspect ratio; coordinates always
    /// refer to the delivered image.
    pub max_width: Option<u32>,
    /// How long to wait for the app to appear (element capture only), in
    /// milliseconds. Default 3000.
    pub timeout_ms: Option<u64>,
}

/// Output of `computer_screenshot`.
///
/// The PNG travels inline via [`ScreenshotOutput::images`] (base64
/// `ImageBlock`s) — no file references.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScreenshotOutput {
    /// Width of the DELIVERED image, in pixels (after any downscale).
    /// Coordinates for touch tools are expressed in this space.
    pub width: u32,
    /// Height of the DELIVERED image, in pixels.
    pub height: u32,
    /// Desktop-coordinate origin of the image: the desktop point under the
    /// delivered image's pixel (0, 0) — `(x, y)`.
    pub desktop_origin: (i32, i32),
    /// Desktop pixels per delivered image pixel — `(dx, dy)`. Map an image
    /// coordinate back to the screen as
    /// `desktop = desktop_origin + image_coord * desktop_scale`.
    pub desktop_scale: (f64, f64),
    /// Size of the encoded PNG in bytes.
    pub bytes: usize,
    /// Inline image payload (base64 PNG) for the multimodal channel.
    #[serde(skip)]
    pub images: Vec<cosh_sdk::connector::ImageBlock>,
}

/// Output of `computer_snapshot`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotOutput {
    /// Name of the application the snapshot was taken from.
    pub app: String,
    /// Process ID of the application, when known.
    pub pid: Option<u32>,
    /// Rendered snapshot: an indented outline (`tree`) or pretty-printed
    /// JSON (`json`), one element per node with role, name and value.
    pub snapshot: String,
    /// Number of elements in the snapshot.
    pub elements: usize,
}
/// Action to perform on the element matched by `selector`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TouchAction {
    /// Click / invoke the element via the accessibility action layer
    /// (buttons, links, menu items).
    #[default]
    Press,
    /// Set keyboard focus.
    Focus,
    /// Remove keyboard focus.
    Blur,
    /// Toggle a two- or three-state control (checkbox, switch).
    Toggle,
    /// Select the element (list item, tab, row).
    Select,
    /// Expand a disclosure, menu, combo box or tree item.
    Expand,
    /// Collapse an expanded element.
    Collapse,
    /// Open the element's context menu or dropdown.
    ShowMenu,
    /// Increment a stepper/slider.
    Increment,
    /// Decrement a stepper/slider.
    Decrement,
    /// Scroll the element into view.
    ScrollIntoView,
    /// Replace the element's text value entirely (text fields). Requires
    /// `value`.
    SetValue,
    /// Set a numeric value (slider, spinner). Requires `numeric_value`.
    SetNumericValue,
    /// Select a text range `start..end` inside the element. Requires `range`.
    SelectText,
    /// Type `value` at the element's caret (focuses it first, inserts rather
    /// than replacing). Requires `value`.
    TypeText,
    /// Platform-specific action by name (e.g. `"raise"`). Requires `value`.
    PerformAction,
}

/// Input for `computer_touch` — perform an action on an element found via the
/// accessibility tree (no screen coordinates involved).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerTouch {
    /// Application name (exact match, e.g. 'Safari'). Provide `name` or `pid`.
    pub name: Option<String>,
    /// Application process ID. Provide `name` or `pid`.
    pub pid: Option<u32>,
    /// CSS-like selector for the target element, e.g. `button[name='OK']`.
    pub selector: String,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1).
    pub nth: Option<usize>,
    /// Action to perform (default `press`).
    pub action: Option<TouchAction>,
    /// Text for `set_value`, `type_text` and `perform_action`.
    pub value: Option<String>,
    /// Number for `set_numeric_value`.
    pub numeric_value: Option<f64>,
    /// `[start, end]` (0-based; `end` EXCLUSIVE — `[0, 3]` selects three
    /// characters, matching the AT-SPI convention) for `select_text`.
    pub range: Option<Vec<u32>>,
    /// How long to wait for the app AND a visible+enabled element match, in
    /// milliseconds (default 3000). Unlike computer_snapshot/computer_screenshot,
    /// actions auto-wait for actionability.
    pub timeout_ms: Option<u64>,
}

/// Output of `computer_touch`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TouchOutput {
    /// Name of the application the action was performed on.
    pub app: String,
    /// Process ID of the application.
    pub pid: Option<u32>,
    /// The action that was performed.
    pub action: TouchAction,
}

/// Pointer action to perform at desktop coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerAction {
    /// Left-click at (`x`, `y`).
    #[default]
    Click,
    /// Double left-click at (`x`, `y`).
    DoubleClick,
    /// Right-click at (`x`, `y`).
    RightClick,
    /// Move the cursor to (`x`, `y`) without clicking.
    Move,
    /// Press a mouse button down at the current position (`button`, default
    /// left) — pair with `up` for custom drags.
    Down,
    /// Release a previously pressed mouse button.
    Up,
    /// Scroll by (`dx`, `dy`) wheel steps at (`x`, `y`). Requires `dx`/`dy`.
    Scroll,
}

/// Input for `computer_pointer` — click/scroll/move at raw coordinates (usually
/// taken from a `computer_screenshot` image, mapped to the desktop via the
/// screenshot's `desktop_origin`/`desktop_scale`).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerPointer {
    /// X coordinate in desktop pixels.
    pub x: Option<i32>,
    /// Y coordinate in desktop pixels.
    pub y: Option<i32>,
    /// Action to perform (default `click`).
    pub action: Option<PointerAction>,
    /// Button for `down`/`up` (`left` default, `right`, `middle`).
    pub button: Option<String>,
    /// Horizontal scroll delta (wheel steps) for `scroll`.
    pub dx: Option<i32>,
    /// Vertical scroll delta (wheel steps) for `scroll`; positive scrolls down.
    pub dy: Option<i32>,
}

/// Output of `computer_pointer`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PointerOutput {
    /// The action that was performed.
    pub action: PointerAction,
    /// Effective desktop x coordinate (null for `up`, which uses the
    /// current cursor position).
    pub x: Option<i32>,
    /// Effective desktop y coordinate (null for `up`).
    pub y: Option<i32>,
}

/// Input for `computer_keyboard` — synthetic keystrokes into the FOCUSED element
/// (focus it first with computer_touch `focus`).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerKeyboard {
    /// Key to tap, by name: single characters (`a`, `5`, `.` — lowercase;
    /// for uppercase hold `shift`), or `enter`, `escape`, `tab`, `space`,
    /// `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`,
    /// `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`.
    pub key: Option<String>,
    /// Modifier keys held while tapping `key` (`shift`, `ctrl`, `alt`,
    /// `meta`) — e.g. key `a` + held `["ctrl"]` = select all.
    pub held: Option<Vec<String>>,
    /// Literal text to type into the focused element (any characters,
    /// including uppercase). Mutually exclusive with `key`.
    pub text: Option<String>,
}

/// Output of `computer_keyboard`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct KeyboardOutput {
    /// What was sent: the tapped key + held modifiers, or the typed text.
    pub sent: String,
}
