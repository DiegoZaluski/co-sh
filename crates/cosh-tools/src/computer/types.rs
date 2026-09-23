use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `computer_apps` should report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppsTarget {
    /// Every running application (default).
    #[default]
    All,
    /// The application that currently holds the system foreground, plus the
    /// element inside it holding keyboard focus — the cheap "where do my
    /// keystrokes go?" check before `computer_keyboard`.
    Focused,
}

/// Input for `computer_apps`.
///
/// `target: "all"` (default) lists every running application;
/// `target: "focused"` resolves the foreground application and its
/// keyboard-focused element instead.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerApps {
    /// What to report: `all` (default) = every running app with PIDs and the
    /// foreground flag; `focused` = the foreground app plus the element
    /// holding keyboard focus inside it.
    pub target: Option<AppsTarget>,
    /// How long to wait for a foreground application to exist, in
    /// milliseconds (`target: "focused"` only). Default 3000.
    pub timeout_ms: Option<u64>,
}

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
    /// Target an OS SHELL SURFACE instead of an application (mutually
    /// exclusive with `name`/`pid`): the menu bar, Dock/taskbar/panel, tray
    /// status items, the desktop, or a flyout open right now. `selector`
    /// then narrows within that surface's tree.
    pub surface: Option<SurfaceKind>,
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

/// The element holding keyboard focus inside the foreground application,
/// reported by `computer_apps` with `target: "focused"`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FocusedElement {
    /// Element role, snake_case (same vocabulary as selectors).
    pub role: String,
    /// Accessible name, when the element has one.
    pub name: Option<String>,
    /// Current value, when the element has one (e.g. the text in a focused
    /// field).
    pub value: Option<String>,
    /// Path of roles from the application root down to the focused element
    /// (inclusive), e.g. `["window", "group", "text_field"]` — context for
    /// locating the field without another snapshot.
    pub path: Vec<String>,
}

/// Output of `computer_apps` with `target: "focused"`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FocusedOutput {
    /// Name of the foreground application.
    pub app: String,
    /// Process ID of the foreground application.
    pub pid: Option<u32>,
    /// The element inside the app holding keyboard focus, when one does.
    /// `None` when the app has no focused element (e.g. focus is on the
    /// window itself or the platform does not report element focus).
    pub focused_element: Option<FocusedElement>,
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
    ///
    /// With `annotate: true` this changes meaning: it selects which elements
    /// get annotated boxes (default `"*"` — every element of the app), and
    /// the legend maps each tag to a round-trippable selector. Comma
    /// alternations (`"button, link"`) are rejected in both modes.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1). Not applicable to annotated captures — the legend
    /// covers every match; narrow `selector` instead.
    pub nth: Option<usize>,
    /// Draw a labeled box on every matching element and return a legend
    /// mapping each tag (`B7`) back to a selector (`*:nth(7)`) that
    /// `computer_act` accepts directly — visual grounding without pixel
    /// coordinates. Requires `app` or `pid` (annotation groups must be
    /// application-scoped) and is mutually exclusive with `region`.
    /// Default `false`.
    #[serde(default)]
    pub annotate: bool,
    /// Maximum delivered image width in pixels (default 1568). Larger
    /// captures are downscaled preserving aspect ratio; coordinates always
    /// refer to the delivered image.
    pub max_width: Option<u32>,
    /// How long to wait for the app to appear (element capture only), in
    /// milliseconds. Default 3000.
    pub timeout_ms: Option<u64>,
    /// Target an OS SHELL SURFACE instead of an application (mutually
    /// exclusive with `app`/`pid`; requires `selector`): shoot the pixels
    /// under the matched element of the menu bar, Dock/taskbar/panel, tray
    /// status items, the desktop, or a flyout open right now. With
    /// `annotate: true` the legend covers the surface's elements the same
    /// way it covers an app's.
    pub surface: Option<SurfaceKind>,
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
    /// Legend of the annotated capture: one entry per drawn box, mapping the
    /// tag drawn on the image (`B7`) to a selector that resolves the
    /// element — pass it to `computer_act` as-is. Empty for plain
    /// captures.
    pub legend: Vec<LegendEntryOutput>,
    /// Elements that matched the annotation selector but could not be
    /// drawn, each with the reason — the picture and the legend never
    /// disagree silently. Empty for plain captures.
    pub omitted: Vec<OmissionOutput>,
    /// How many matched elements were not described at all because the
    /// annotation cap (100) was reached. `0` when the cap did not bite;
    /// narrow the `selector` when it does.
    pub truncated: usize,
    /// Inline image payload (base64 PNG) for the multimodal channel.
    #[serde(skip)]
    pub images: Vec<cosh_sdk::connector::ImageBlock>,
}

/// One legend entry of an annotated capture: a box on the image plus the
/// selector that reaches the element it labels.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LegendEntryOutput {
    /// What is drawn in the box — `"B7"`.
    pub tag: String,
    /// Selector usable as-is against the annotation scope —
    /// `"button[name='Export']:nth(7)"`. This round-trip is the point: read
    /// the tag off the image, act on the selector.
    pub selector: String,
    /// The element's role, snake_case as everywhere else.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// The box colour, RGB, for correlating a box with its entry by eye.
    pub color: [u8; 3],
}

/// An element the annotation selector matched but that could not be drawn.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OmissionOutput {
    /// The selector that would reach this element.
    pub selector: String,
    /// The element's role, snake_case.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// Why it could not be drawn: `no_bounds`, `zero_area` or
    /// `outside_capture`.
    pub reason: String,
}

/// Output of `computer_snapshot`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotOutput {
    /// Name of the application the snapshot was taken from.
    pub app: String,
    /// Process ID of the application, when known.
    pub pid: Option<u32>,
    /// Rendered snapshot: an indented outline (`tree`) or pretty-printed
    /// JSON (`json`), one element per node with role, name, value and states.
    pub snapshot: String,
    /// Number of elements in the snapshot.
    pub elements: usize,
}

/// Tri-state toggle value for a checkable control (`checked` in
/// [`ElementStates`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToggleState {
    /// Checkable and currently off.
    Off,
    /// Checkable and currently on.
    On,
    /// Tri-state checkbox in the indeterminate state.
    Mixed,
}

impl From<xa11y::Toggled> for ToggleState {
    fn from(t: xa11y::Toggled) -> Self {
        match t {
            xa11y::Toggled::Off => Self::Off,
            xa11y::Toggled::On => Self::On,
            xa11y::Toggled::Mixed => Self::Mixed,
        }
    }
}

/// The element states a snapshot reports, normalized from the platform's
/// [`xa11y::StateSet`].
///
/// The subset is chosen for action planning: these are the flags that change
/// whether an interaction will succeed (a `disabled` button fails
/// `computer_act`'s press; a `hidden` row needs `scroll_into_view` first).
/// Static attributes (focusable, modal, required) and window-level `active`
/// are left out to keep per-node cost low.
///
/// Default (`enabled` + `visible`, everything else off/absent) matches
/// [`xa11y::StateSet`]'s default, so "all defaults" renders as nothing in the
/// outline and as a predictable flat object in JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ElementStates {
    /// Whether the element accepts interaction (`false` = aria `disabled`).
    pub enabled: bool,
    /// Whether the element is on-screen (`false` = aria `hidden`).
    pub visible: bool,
    /// Whether the element currently holds keyboard focus.
    pub focused: bool,
    /// `None` = not checkable. `Some(Off)` is meaningful (an unchecked
    /// checkbox), hence `Option` rather than a plain bool.
    pub checked: Option<ToggleState>,
    /// Whether the element is the selected item of its container.
    pub selected: bool,
    /// `None` = not expandable. `Some(false)` = a collapsed disclosure.
    pub expanded: Option<bool>,
    /// Whether the element accepts text input.
    pub editable: bool,
    /// Whether an async operation is in progress on the element.
    pub busy: bool,
}

impl Default for ElementStates {
    fn default() -> Self {
        Self {
            enabled: true,
            visible: true,
            focused: false,
            checked: None,
            selected: false,
            expanded: None,
            editable: false,
            busy: false,
        }
    }
}

impl ElementStates {
    /// Normalize a platform [`xa11y::StateSet`] into the reported subset.
    #[must_use]
    pub fn from_state_set(states: &xa11y::StateSet) -> Self {
        Self {
            enabled: states.enabled,
            visible: states.visible,
            focused: states.focused,
            checked: states.checked.map(ToggleState::from),
            selected: states.selected,
            expanded: states.expanded,
            editable: states.editable,
            busy: states.busy,
        }
    }

    /// Whether every flag matches the default (enabled + visible, nothing
    /// else). The outline renders nothing for such elements.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// aria-vocabulary tokens for the non-default flags, in fixed order:
    /// `disabled`, `hidden`, `focused`, `checked`/`unchecked`/`mixed`,
    /// `selected`, `expanded`/`collapsed`, `editable`, `busy`. Fixed order
    /// keeps the outline predictable to parse.
    #[must_use]
    pub fn outline_tokens(&self) -> Vec<&'static str> {
        let mut tokens = Vec::new();
        if !self.enabled {
            tokens.push("disabled");
        }
        if !self.visible {
            tokens.push("hidden");
        }
        if self.focused {
            tokens.push("focused");
        }
        match self.checked {
            Some(ToggleState::On) => tokens.push("checked"),
            Some(ToggleState::Off) => tokens.push("unchecked"),
            Some(ToggleState::Mixed) => tokens.push("mixed"),
            None => {}
        }
        if self.selected {
            tokens.push("selected");
        }
        match self.expanded {
            Some(true) => tokens.push("expanded"),
            Some(false) => tokens.push("collapsed"),
            None => {}
        }
        if self.editable {
            tokens.push("editable");
        }
        if self.busy {
            tokens.push("busy");
        }
        tokens
    }
}

/// One element of the rendered snapshot: role, name, value, current states
/// and children.
///
/// This is the tool's own tree — xa11y's `TreeNode` carries no states, and
/// the builder below reads them from the same `ElementData` the node's
/// identity comes from, so building here adds zero extra provider round
/// trips per node.
#[derive(Debug, Clone, Serialize)]
pub struct StateNode {
    /// Element role, snake_case (same vocabulary as selectors).
    pub role: String,
    /// Accessible name, when the element has one.
    pub name: Option<String>,
    /// Current value, when the element has one.
    pub value: Option<String>,
    /// Current element states.
    pub states: ElementStates,
    /// Child elements.
    pub children: Vec<StateNode>,
}
/// Action to perform on the element matched by `selector`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActAction {
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

/// Input for `computer_act` — ONE step of a semantic pipeline: either an
/// accessibility action on the element matched by `selector`, a keyboard
/// action (`key`/`text`), or a `wait` pause — chained via `then` with
/// shell `&&` semantics.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerAct {
    /// Application name (exact match, e.g. 'Safari'). Provide `name` or `pid`.
    /// Semantic-action steps only.
    pub name: Option<String>,
    /// Application process ID. Provide `name` or `pid`. Semantic-action
    /// steps only.
    pub pid: Option<u32>,
    /// CSS-like selector for the target element, e.g. `button[name='OK']`.
    /// Required on a semantic-action step; absent on keyboard/wait steps.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1).
    pub nth: Option<usize>,
    /// Action to perform (default `press`). Semantic-action steps only.
    pub action: Option<ActAction>,
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
    /// Target an OS SHELL SURFACE instead of an application (mutually
    /// exclusive with `name`/`pid`): the menu bar, Dock/taskbar/panel, tray
    /// status items, the desktop, or a flyout open right now. Semantic
    /// steps only — a surface has no process to type into; keyboard steps
    /// in the same chain still need an element step to have set focus.
    pub surface: Option<SurfaceKind>,
    // ── Keyboard fields (shared engine with computer_control) ──
    /// Key to tap, by name: single characters (lowercase — for uppercase
    /// hold `shift`) or named keys (`enter`, `escape`, `tab`, `space`,
    /// `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`,
    /// `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`). Types into
    /// whatever holds keyboard focus NOW.
    pub key: Option<String>,
    /// Literal text to type into the focused element. Mutually exclusive
    /// with `key`.
    pub text: Option<String>,
    /// Modifier keys held while tapping `key` (`shift`, `ctrl`, `alt`,
    /// `meta`) — e.g. key `a` + `held: ["ctrl"]` = select all. `text`
    /// rejects `held`.
    pub held: Option<Vec<String>>,
    /// Milliseconds to WAIT as a STANDALONE step (no other fields): gives
    /// the app time to settle before the next step acts. Capped at 10 s.
    pub wait: Option<u64>,
    /// Optional NEXT step of the pipeline, executed only if this step
    /// succeeded. Same shape as this step, recursively.
    pub then: Option<Box<ComputerAct>>,
}

/// Output of `computer_act`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ActOutput {
    /// What was executed, per step: semantic actions as
    /// `<action> \`<selector>\` (app)`, keyboard steps as the tapped key +
    /// held modifiers or the typed text, waits as `waited N ms`.
    /// Pipeline steps join with ` → `.
    pub sent: String,
}

/// The condition `computer_wait` watches, mirroring xa11y's
/// [`xa11y::ElementState`] (the same vocabulary `computer_snapshot` reports
/// as state tokens).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WaitState {
    /// A matching element exists in the tree.
    Attached,
    /// No element matches the selector (a dialog closed, a spinner went
    /// away). Tolerates the element never having existed.
    Detached,
    /// A matching element exists and is visible (default).
    #[default]
    Visible,
    /// A matching element is hidden or no longer exists.
    Hidden,
    /// A matching element exists and is enabled.
    Enabled,
    /// A matching element exists but is disabled.
    Disabled,
    /// A matching element holds keyboard focus.
    Focused,
    /// No matching element holds keyboard focus.
    Unfocused,
}

/// Input for `computer_wait` — block until a selector in a target app
/// reaches a state, or time out with a diagnosis of the last observed
/// state.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerWait {
    /// Application name (exact match, e.g. `"Safari"`). Provide `name` or
    /// `pid` — exactly one.
    pub name: Option<String>,
    /// Application process ID. Provide `name` or `pid` — exactly one.
    pub pid: Option<u32>,
    /// CSS-like selector for the element to watch, e.g.
    /// `progress_indicator[name='Exporting…']`.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1).
    pub nth: Option<usize>,
    /// The condition to wait for (default `visible`).
    pub state: Option<WaitState>,
    /// How long to wait for the condition, in milliseconds. Default 10 000;
    /// capped at 60 000 so a stuck call cannot pin the session for minutes.
    pub timeout_ms: Option<u64>,
}

/// Output of `computer_wait` when the condition was met in time.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WaitOutput {
    /// Whether the condition was met before the timeout (always `true` in
    /// the success response — a timeout is an error carrying the diagnosis).
    pub met: bool,
    /// How long the call actually waited, in milliseconds.
    pub elapsed_ms: u64,
    /// The state the element was in when the condition was met — a
    /// convenience sanity check for the model (e.g. a `detached` wait's
    /// element is gone; an `enabled` wait's element reports enabled).
    pub observed: WaitObservation,
}

/// What the watched element looked like when the wait resolved.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WaitObservation {
    /// Whether an element matched the selector at resolution time.
    pub attached: bool,
    /// Whether the matched element was visible (absent when detached).
    pub visible: Option<bool>,
    /// Whether the matched element was enabled (absent when detached).
    pub enabled: Option<bool>,
    /// Whether the matched element held keyboard focus (absent when
    /// detached).
    pub focused: Option<bool>,
}

/// Which OS shell surface to target instead of an application — the parts
/// of the desktop OUTSIDE any app: menu bar, Dock/taskbar/panel, tray
/// status items, the desktop itself, transient flyouts (Notification
/// Center, Quick Settings, shell context menus). Mirrors xa11y's
/// [`xa11y::ShellSurfaceKind`] exactly (same snake_case wire spelling).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    /// The frontmost application's menu bar (macOS: File → Save As, the
    /// Apple menu). Windows/Linux: per-window menu bars live in the app's
    /// own tree, not here.
    #[default]
    MenuBar,
    /// System status items / tray icons (macOS menu-bar extras, Windows
    /// tray overflow needs a press first — content only exists while the
    /// overflow is open).
    StatusItems,
    /// The Windows taskbar.
    Taskbar,
    /// Linux desktop panels (GNOME top bar, KDE panel, …).
    Panel,
    /// The macOS Dock.
    Dock,
    /// The desktop itself (icons, wallpaper-level widgets).
    Desktop,
    /// Transient shell flyouts open RIGHT NOW: Notification Center, Quick
    /// Settings, shell context menus. They exist only while open — the
    /// caller performs the press that opens them on a real element first,
    /// then re-enumerates.
    Flyout,
    /// A shell surface the platform reported without a known kind.
    Unknown,
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
    /// Press at the start point, interpolate movement to the end point
    /// (~60 Hz), release. Requires both endpoints — coordinates (`x`/`y` +
    /// `x2`/`y2`) or elements (`selector` + `to_selector`), not a mix.
    /// `duration_ms` paces the movement (default 150, min 50).
    Drag,
}

/// Where a pointer action lands inside the element's bounds.
///
/// The element form is tree-grounded and therefore allowed in Build mode;
/// the coordinate form is position-dependent and stays Yolo/Command only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerAnchor {
    /// Center of the bounds (default).
    #[default]
    Center,
    /// Top-left corner (half-open bounds: the last point inside, not the
    /// exclusive edge).
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
}

/// Input for `computer_control` — one step of a desktop-control pipeline:
/// EITHER a pointer action (click/scroll/move/drag — `key`/`text` absent)
/// OR a keyboard action (`key` or `text`), at raw coordinates or on an
/// accessibility-tree element.
///
/// A step may chain the NEXT step via `then`: the steps run sequentially in
/// a single call, each only if the previous succeeded (shell `&&` semantics)
/// — the first failing step aborts the chain with the point of failure.
/// Targeting is PER STEP: the canonical pattern is `click` an element (the
/// real click moves OS keyboard focus — the one mechanism that works on
/// every toolkit) and `then` type into the focused field. `up` needs no
/// target.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComputerControl {
    // ── Pointer fields ──
    /// X coordinate in desktop pixels (coordinate form; also the drag START).
    pub x: Option<i32>,
    /// Y coordinate in desktop pixels (coordinate form; also the drag START).
    pub y: Option<i32>,
    /// Pointer action (default `click`). Ignored — and rejected together
    /// with any other pointer field — when the step is a keyboard step
    /// (`key`/`text`).
    pub action: Option<PointerAction>,
    /// Button for `down`/`up` (`left` default, `right`, `middle`).
    pub button: Option<String>,
    /// Horizontal scroll delta (wheel steps) for `scroll`.
    pub dx: Option<i32>,
    /// Vertical scroll delta (wheel steps) for `scroll`; positive scrolls down.
    pub dy: Option<i32>,
    /// Targeting PER STEP: application name (exact match) scoping
    /// `selector`. Provide `app` or `pid` — exactly one, only together with
    /// `selector`.
    pub app: Option<String>,
    /// Application process ID for the element form. Per step.
    pub pid: Option<u32>,
    /// Target an OS SHELL SURFACE instead of an application (mutually
    /// exclusive with `app`/`pid`; only together with `selector`): the
    /// menu bar, Dock/taskbar/panel, tray status items, the desktop, or a
    /// flyout open right now. Pointer steps act on the element matched
    /// under the surface's root the same way they do under an app-scoped
    /// step.
    pub surface: Option<SurfaceKind>,
    /// CSS-like selector of the element this step acts on, e.g.
    /// `"button[name='OK']"` — the point resolves from the element's CURRENT
    /// bounds when the step runs, never stale. A `click` on an element moves
    /// OS keyboard focus to it, which is how the next step's typing finds
    /// the right field. Per step.
    pub selector: Option<String>,
    /// 1-based match index when `selector` matches multiple elements
    /// (default 1). Requires `selector`. Per step.
    pub nth: Option<usize>,
    /// Where inside the element's bounds the point lands (default `center`).
    pub anchor: Option<PointerAnchor>,
    /// Number of consecutive clicks for click actions (1 default, 2 =
    /// double-click, 3 = triple). Independent of `action`: `click` +
    /// `count: 2` is a double-click.
    pub count: Option<u32>,
    /// Modifier keys for THIS step: held during a click or drag, or held
    /// while tapping `key` (`shift`, `ctrl`, `alt`, `meta`) — e.g. `click` +
    /// `held: ["ctrl"]` = ctrl+click; key `a` + `held: ["ctrl"]` = select
    /// all. `text` rejects `held`.
    pub held: Option<Vec<String>>,
    /// End X for `drag` (with `y2`). Mutually exclusive with `to_selector`.
    pub x2: Option<i32>,
    /// End Y for `drag`.
    pub y2: Option<i32>,
    /// End element selector for `drag` (CSS-like selector on the same app) —
    /// the element form for the drag's end point. Mutually exclusive with
    /// `x2`/`y2`.
    pub to_selector: Option<String>,
    /// Total duration of a `drag`'s movement, in milliseconds (default 150,
    /// min 50). Backends interpolate the pointer path across this time.
    pub duration_ms: Option<u64>,
    // ── Keyboard fields ──
    /// Key to tap, by name: single characters (`a`, `5`, `.` — lowercase;
    /// for uppercase hold `shift`), or `enter`, `escape`, `tab`, `space`,
    /// `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`,
    /// `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`. Typing goes into
    /// the element focused by an earlier step's element click (or whatever
    /// holds focus).
    pub key: Option<String>,
    /// Literal text to type into the focused element (any characters,
    /// including uppercase). Mutually exclusive with `key`.
    pub text: Option<String>,
    /// Milliseconds to WAIT as a STANDALONE step (no other fields): gives
    /// the app time to open a dialog or create an ephemeral field before
    /// the next step acts — e.g. click the menu item that opens a rename
    /// box, then `{"wait": 600}`, then type into it. Capped at 10 s.
    pub wait: Option<u64>,
    /// Optional NEXT step of the pipeline, executed only if this step
    /// succeeded (e.g. click `text_field` → then text `"olá"` → then key
    /// `enter`). Same shape as this step, recursively.
    pub then: Option<Box<ComputerControl>>,
}

/// Output of `computer_control`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ControlOutput {
    /// What was executed, per step: pointer actions as
    /// `click <what>@x,y`, keyboard steps as the tapped key + held modifiers
    /// or the typed text. Pipeline steps join with ` → `.
    pub sent: String,
}
