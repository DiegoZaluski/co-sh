//! Desktop computer-control tools: observe native application UIs and
//! interact with them through the OS accessibility tree (xa11y), screen
//! captures, and synthetic pointer/keyboard input.
//!
//! The tools are split so each schema stays small and unambiguous, and the
//! names follow the vocabulary of Anthropic's computer-use toolset:
//!
//! - [`Computer::apps`] — `computer_apps`: list running apps and their PIDs.
//! - [`Computer::snapshot`] — `computer_snapshot`: read an app's accessibility
//!   tree (structure, exact names/roles, values).
//! - [`Computer::screenshot`] — `computer_screenshot`: capture the screen, a
//!   region, or one element and receive the PNG inline.
//! - [`Computer::touch`] — `computer_touch`: semantic actions (press, toggle,
//!   set_value, ...) on accessibility-tree elements by selector, with
//!   auto-wait.
//! - [`Computer::pointer`] — `computer_pointer`: coordinate-based pointer
//!   input (click, scroll, drag) at desktop pixels.
//! - [`Computer::keyboard`] — `computer_keyboard`: synthetic keystrokes and
//!   literal text into the focused element.
//!
//! All xa11y calls are blocking, so every operation runs on tokio's
//! blocking pool and returns an owned result.

pub mod apps;
pub mod keyboard;
pub mod pointer;
pub mod screenshot;
pub mod snapshot;
pub mod touch;
pub mod types;

pub use apps::apps;
pub use keyboard::keyboard;
pub use pointer::pointer;
pub use screenshot::screenshot;
pub use snapshot::snapshot;
pub use touch::touch;
pub use types::{
    AppInfo, AppsOutput, KeyboardOutput, PointerAction, PointerOutput, SnapshotFormat,
    SnapshotOutput, ScreenshotOutput, TouchAction, TouchOutput, ComputerApps, ComputerKeyboard, ComputerPointer,
    ComputerScreenshot, ComputerSnapshot, ComputerTouch,
};

use crate::ToolDescription;

/// Shared-state wrapper for computer-control tool operations.
///
/// Carries the MCP tool descriptions so the harness can register the
/// tools; the operation methods are stateless.
pub struct Computer {
    /// MCP Tool description for `computer_apps`.
    pub description_apps: ToolDescription,
    /// MCP Tool description for `computer_snapshot`.
    pub description_snapshot: ToolDescription,
    /// MCP Tool description for `computer_screenshot`.
    pub description_screenshot: ToolDescription,
    /// MCP Tool description for `computer_touch` (accessibility-tree actions).
    pub description_touch: ToolDescription,
    /// MCP Tool description for `computer_pointer` (coordinate-based pointer).
    pub description_pointer: ToolDescription,
    /// MCP Tool description for `computer_keyboard` (synthetic keystrokes).
    pub description_keyboard: ToolDescription,
}

impl Default for Computer {
    fn default() -> Self {
        Self::new()
    }
}

impl Computer {
    /// Create a new `Computer` with tool descriptions pre-configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            description_apps: serde_json::json!({
                "name": "computer_apps",
                "description": concat!(
                    "List running desktop applications visible to the OS accessibility ",
                    "tree, with their PIDs and which one is in the foreground. Call ",
                    "this first to find the `name` or `pid` to pass to computer_snapshot ",
                    "and computer_touch."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }),
            description_snapshot: serde_json::json!({
                "name": "computer_snapshot",
                "description": concat!(
                    "Capture the accessibility tree of a desktop application as a ",
                    "compact outline (role, name and value of every element). Prefer ",
                    "this over a screenshot when you need structure: names and roles ",
                    "are exact and can be used directly as selectors for computer_touch. ",
                    "Optionally snapshot only the subtree matching a CSS-like selector.",
                    "\n\nSelector patterns: `button`, `button[name='OK']`, ",
                    "`text_field[name^='Search']`, `text_field[name*='email']`, ",
                    "`group > button` (direct child), `window button` (descendant), ",
                    "`button:nth(2)`."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Application name (exact match, e.g. 'Safari'). Provide `name` or `pid` — exactly one."
                        },
                        "pid": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Application process ID. Provide `name` or `pid` — exactly one."
                        },
                        "selector": {
                            "type": "string",
                            "description": "Optional CSS-like selector to snapshot only the matching subtree, e.g. \"window[name='Main'] > group\". Resolution is fail-fast (no auto-wait); if the element may not exist yet, re-call after the app's interface settles."
                        },
                        "nth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "1-based match index when `selector` matches multiple elements (default 1). Requires `selector`."
                        },
                        "max_depth": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 40,
                            "description": "Maximum snapshot depth (default 12, max 40)"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["tree", "json"],
                            "description": "'tree' = compact indented outline (default); 'json' = structured JSON"
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How long to wait for the app to appear, in milliseconds (default 3000). Applies only to the app lookup."
                        }
                    },
                    "anyOf": [
                        { "required": ["name"] },
                        { "required": ["pid"] }
                    ],
                    "allOf": [
                        {
                            "if": { "required": ["name"] },
                            "then": { "not": { "required": ["pid"] } }
                        },
                        {
                            "if": { "required": ["nth"] },
                            "then": { "required": ["selector"] }
                        }
                    ]
                }
            }),
            description_screenshot: serde_json::json!({
                "name": "computer_screenshot",
                "description": concat!(
                    "Capture the screen (full display, a region, or the bounds of one ",
                    "element) and return it as an image you can see. Use when visual ",
                    "layout matters; use computer_snapshot when you need exact element ",
                    "structure. The output reports the DELIVERED image's dimensions, ",
                    "its desktop-space origin, and the desktop scale — map an image ",
                    "coordinate back to the screen as ",
                    "desktop = desktop_origin + image_coord * desktop_scale.",
                    "\n\nThree capture modes (exactly one): full display (omit every ",
                    "argument); `region` = [x, y, width, height] in desktop pixels; ",
                    "element capture = `app` or `pid` together with a CSS-like ",
                    "`selector` (pixels under the element's current bounds; occluded ",
                    "elements are NOT raised first — `app`/`pid` without `selector` is ",
                    "rejected instead of falling back to a full-screen capture). ",
                    "Oversized captures are downscaled to `max_width` (default 1568)."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "region": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "minItems": 4,
                            "maxItems": 4,
                            "description": "Capture only this region: [x, y, width, height] in desktop pixels. Mutually exclusive with element capture (`app`/`pid` + `selector`)."
                        },
                        "app": {
                            "type": "string",
                            "description": "Application name (exact match) for element capture. Requires `selector`; without `selector` the call is rejected."
                        },
                        "pid": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Application process ID for element capture. Requires `selector`; without `selector` the call is rejected."
                        },
                        "selector": {
                            "type": "string",
                            "description": "CSS-like selector for element capture, e.g. \"window[name='Main'] > group\". Requires `app` or `pid`."
                        },
                        "nth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "1-based match index when `selector` matches multiple elements (default 1)."
                        },
                        "max_width": {
                            "type": "integer",
                            "minimum": 256,
                            "description": "Maximum delivered image width in pixels (default 1568). Coordinates refer to the delivered image; use desktop_origin/desktop_scale from the output to map back."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How long to wait for the app to appear (element capture only), in milliseconds (default 3000)."
                        }
                    },
                    "allOf": [
                        {
                            "if": { "required": ["region"] },
                            "then": {
                                "not": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["selector"] }] }
                            }
                        },
                        {
                            "if": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }] },
                            "then": { "allOf": [{ "required": ["selector"] }, { "not": { "required": ["region"] } }] }
                        },
                        {
                            "if": { "required": ["app"] },
                            "then": { "not": { "required": ["pid"] } }
                        },
                        {
                            "if": { "required": ["selector"] },
                            "then": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }] }
                        },
                        {
                            "if": { "required": ["nth"] },
                            "then": { "required": ["selector"] }
                        }
                    ],
                    "required": []
                }
            }),
            description_touch: serde_json::json!({
                "name": "computer_touch",
                "description": concat!(
                    "Perform an action on an element found via the accessibility ",
                    "tree — no screen coordinates involved. PREFERRED over ",
                    "computer_pointer whenever the target exists in computer_snapshot output: ",
                    "actions are semantic (press/toggle/select/set_value) and ",
                    "auto-wait for the element to become visible and enabled. Use ",
                    "computer_pointer only for canvas/custom widgets without ",
                    "accessibility nodes.",
                    "\n\nTypical flow: computer_apps → computer_snapshot (find role+name) → ",
                    "computer_touch. Value actions need payloads: `set_value`/`type_text`/",
                    "`perform_action` need `value`, `set_numeric_value` needs ",
                    "`numeric_value`, `select_text` needs `range: [start, end]` ",
                    "(0-based). `type_text` inserts at the caret; `set_value` ",
                    "replaces the whole value. Keyboard typing into the focused ",
                    "element (arrows, chords) is computer_keyboard's job."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Application name (exact match, e.g. 'Safari'). Provide `name` or `pid` — exactly one."
                        },
                        "pid": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Application process ID. Provide `name` or `pid` — exactly one."
                        },
                        "selector": {
                            "type": "string",
                            "minLength": 1,
                            "description": "CSS-like selector for the target element, e.g. \"button[name='OK']\"."
                        },
                        "nth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "1-based match index when `selector` matches multiple elements (default 1)."
                        },
                        "action": {
                            "type": "string",
                            "enum": [
                                "press", "focus", "blur", "toggle", "select", "expand",
                                "collapse", "show_menu", "increment", "decrement",
                                "scroll_into_view", "set_value", "set_numeric_value",
                                "select_text", "type_text", "perform_action"
                            ],
                            "description": "Action to perform (default `press`)."
                        },
                        "value": {
                            "type": "string",
                            "description": "Text for `set_value`, `type_text` or `perform_action` (action name for the latter, e.g. \"raise\")."
                        },
                        "numeric_value": {
                            "type": "number",
                            "description": "Number for `set_numeric_value` (slider, spinner)."
                        },
                        "range": {
                            "type": "array",
                            "items": { "type": "integer", "minimum": 0 },
                            "minItems": 2,
                            "maxItems": 2,
                            "description": "[start, end] (0-based; `end` EXCLUSIVE — [0, 3] selects three characters) for `select_text`."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How long to wait for the app AND a visible+enabled element match, in milliseconds (default 3000). Applies to all actions except scroll_into_view, which only waits for the element to exist."
                        }
                    },
                    "anyOf": [
                        { "required": ["name"] },
                        { "required": ["pid"] }
                    ],
                    "allOf": [
                        {
                            "if": { "required": ["name"] },
                            "then": { "not": { "required": ["pid"] } }
                        },
                        {
                            "if": { "properties": { "action": { "enum": ["set_value", "type_text", "perform_action"] } }, "required": ["action"] },
                            "then": { "required": ["value"] }
                        },
                        {
                            "if": { "properties": { "action": { "const": "set_numeric_value" } }, "required": ["action"] },
                            "then": { "required": ["numeric_value"] }
                        },
                        {
                            "if": { "properties": { "action": { "const": "select_text" } }, "required": ["action"] },
                            "then": { "required": ["range"] }
                        }
                    ],
                    "required": ["selector"]
                }
            }),
            description_pointer: serde_json::json!({
                "name": "computer_pointer",
                "description": concat!(
                    "Click, scroll or move the pointer at raw desktop pixel ",
                    "coordinates. Use ONLY when the target has no accessibility ",
                    "node (canvas, custom widget); coordinates should come from a ",
                    "computer_screenshot image mapped to the desktop as ",
                    "desktop = desktop_origin + image_coord * desktop_scale. If the ",
                    "target appears in computer_snapshot, prefer computer_touch instead — it is ",
                    "semantic and auto-waits."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": {
                            "type": "integer",
                            "description": "X coordinate in desktop pixels (required unless action is `up`)."
                        },
                        "y": {
                            "type": "integer",
                            "description": "Y coordinate in desktop pixels (required unless action is `up`)."
                        },
                        "action": {
                            "type": "string",
                            "enum": ["click", "double_click", "right_click", "move", "down", "up", "scroll"],
                            "description": "Pointer action (default `click`). `down`/`up` allow custom drags."
                        },
                        "button": {
                            "type": "string",
                            "enum": ["left", "right", "middle"],
                            "description": "Button for `down`/`up` (default `left`). Clicks are always left."
                        },
                        "dx": {
                            "type": "integer",
                            "description": "Horizontal wheel steps for `scroll`."
                        },
                        "dy": {
                            "type": "integer",
                            "description": "Vertical wheel steps for `scroll`; positive scrolls down."
                        }
                    },
                    "allOf": [
                        {
                            "if": { "properties": { "action": { "const": "scroll" } }, "required": ["action"] },
                            "then": { "anyOf": [{ "required": ["dx"] }, { "required": ["dy"] }] }
                        },
                        {
                            "if": { "properties": { "action": { "enum": ["click", "double_click", "right_click", "move", "down", "scroll"] } }, "required": ["action"] },
                            "then": { "required": ["x", "y"] }
                        },
                        {
                            "if": { "not": { "required": ["action"] } },
                            "then": { "required": ["x", "y"] }
                        },
                        {
                            "if": { "properties": { "action": { "const": "up" } }, "required": ["action"] },
                            "then": { "properties": { "x": { "not": {} }, "y": { "not": {} } } }
                        }
                    ],
                    "required": []
                }
            }),
            description_keyboard: serde_json::json!({
                "name": "computer_keyboard",
                "description": concat!(
                    "Send synthetic keystrokes to the currently FOCUSED element — ",
                    "focus a field first with computer_touch action `focus`. Two modes ",
                    "(exactly one): `key` + optional `held` modifiers (e.g. key ",
                    "`a` + held [\"ctrl\"] = select all; keys are lowercase — for ",
                    "uppercase hold `shift`), or `text` to type a literal string ",
                    "(any characters, including uppercase; `held` is rejected)."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "Key to tap: single lowercase character (`a`, `5`, `.`), `enter`, `escape`, `esc`, `tab`, `space`, `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, or `f1`..`f12`."
                        },
                        "held": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "meta"] },
                            "description": "Modifier keys held while tapping `key`."
                        },
                        "text": {
                            "type": "string",
                            "description": "Literal text to type (handles uppercase/shift itself). Mutually exclusive with `key`."
                        }
                    },
                    "allOf": [
                        {
                            "if": { "required": ["text"] },
                            "then": { "properties": { "key": { "not": {} }, "held": { "not": {} } } }
                        },
                        {
                            "if": { "required": ["key"] },
                            "then": { "properties": { "text": { "not": {} } } }
                        }
                    ],
                    "anyOf": [
                        { "required": ["key"] },
                        { "required": ["text"] }
                    ]
                }
            }),
        }
    }

    /// List running desktop applications with their PIDs.
    ///
    /// See [`apps`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the platform accessibility API is unreachable.
    pub async fn apps(&self) -> Result<AppsOutput, String> {
        apps::apps().await
    }

    /// Capture an application's accessibility tree.
    ///
    /// See [`snapshot`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for invalid input, an app/selector that never matches,
    /// or an unreachable platform accessibility API.
    pub async fn snapshot(&self, input: &ComputerSnapshot) -> Result<SnapshotOutput, String> {
        snapshot::snapshot(input).await
    }

    /// Capture the screen (full display, a region, or one element) and get
    /// the PNG inline as base64.
    ///
    /// See [`screenshot`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for invalid input, an app/selector that never matches,
    /// a failed capture (e.g. missing screen-recording permission), or an
    /// encode failure.
    pub async fn screenshot(
        &self,
        input: &ComputerScreenshot,
    ) -> Result<ScreenshotOutput, String> {
        screenshot::screenshot(input).await
    }

    /// Perform a semantic action on an accessibility-tree element by
    /// selector (auto-waits for actionability).
    ///
    /// See [`touch`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for invalid input or a missing payload, when the app or
    /// a visible+enabled element match does not appear within the timeout,
    /// or when the platform rejects the action.
    pub async fn touch(&self, input: &ComputerTouch) -> Result<TouchOutput, String> {
        touch::touch(input).await
    }

    /// Click, scroll or move the pointer at desktop pixel coordinates.
    ///
    /// See [`pointer`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for missing coordinates/scroll delta, an unknown button
    /// name, or an unavailable input backend.
    pub async fn pointer(&self, input: &ComputerPointer) -> Result<PointerOutput, String> {
        pointer::pointer(input).await
    }

    /// Tap a key (optionally with modifiers) or type literal text into the
    /// focused element.
    ///
    /// See [`keyboard`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for invalid key/text input or an unavailable input
    /// backend.
    pub async fn keyboard(&self, input: &ComputerKeyboard) -> Result<KeyboardOutput, String> {
        keyboard::keyboard(input).await
    }
}
