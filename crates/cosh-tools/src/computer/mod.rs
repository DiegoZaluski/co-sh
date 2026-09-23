//! Desktop computer-control tools: observe native application UIs and
//! interact with them through the OS accessibility tree (xa11y), screen
//! captures, and synthetic pointer/keyboard input.
//!
//! The layout separates COMPONENTS from PIPELINES:
//!
//! - Components own one input KIND — validation and dispatch of a single
//!   step, reusable by any tool without carrying pipelines along:
//!   [`mouse`] (click/scroll/drag at coordinates or elements),
//!   [`keyboard`] (keys/text/wait + the shared input simulator) and
//!   [`touch`] (semantic accessibility actions on elements).
//! - Pipelines own one TOOL — chain walking (`then` with `&&` semantics),
//!   step classification and the wire schema, composing the components:
//!   [`control`] (pointer + keyboard at raw coordinates or elements) and
//!   [`act`] (semantic + keyboard on accessibility-tree elements).
//! - Observation tools are standalone: [`apps`], [`snapshot`],
//!   [`screenshot`].
//!
//! The two pipelines share the same schema skeleton on purpose (`then` →
//! `then` → `wait`): the model learns the shape once and only the step
//! kinds differ per tool.
//!
//! All xa11y calls are blocking, so every operation runs on tokio's
//! blocking pool and returns an owned result.

pub mod act;
pub mod apps;
pub mod control;
pub mod errors;
pub mod keyboard;
pub mod mouse;
pub mod screenshot;
pub mod snapshot;
pub mod surface;
pub mod touch;
pub mod types;
pub mod wait;

pub use act::act;
pub use apps::AppsResult;
pub use apps::apps;
pub use control::control;
pub use screenshot::screenshot;
pub use snapshot::snapshot;
pub use types::{
    ActAction, ActOutput, AppInfo, AppsOutput, AppsTarget, ControlOutput, ComputerAct,
    ComputerControl, ComputerApps, ComputerWait, FocusedElement, FocusedOutput, PointerAction,
    SnapshotFormat, SnapshotOutput, ScreenshotOutput, ComputerScreenshot, ComputerSnapshot,
    ElementStates, StateNode, PointerAnchor, ToggleState, WaitObservation, WaitOutput, WaitState,
};
pub use wait::wait;

use crate::ToolDescription;

/// Process-wide input simulator shared by every computer-control step.
///
/// The Wayland backend owns a `uinput` device created per `InputSim`;
/// rebuilding it per call would split a `down`/`up` (or drag) sequence
/// across devices and lose the held button. Sharing one instance keeps
/// button state continuous across tool calls.
pub(crate) fn shared_input_sim() -> Result<xa11y::InputSim, String> {
    static SIM: std::sync::OnceLock<Result<xa11y::InputSim, String>> = std::sync::OnceLock::new();
    SIM.get_or_init(|| {
        xa11y::input_sim()
            .map_err(|e| errors::render("computer", "initialize input backend", &e))
    })
    .clone()
}

/// Shared-state wrapper for computer-control tool operations.
///
/// Carries the MCP tool descriptions so the harness can register the
/// tools; the operation methods are stateless.
pub struct Computer {
    /// MCP Tool description for `computer_apps`.
    pub description_apps: ToolDescription,
    /// MCP Tool description for `computer_snapshot`.
    pub description_snapshot: ToolDescription,
    /// MCP Tool description for `computer_wait` (blocking state wait).
    pub description_wait: ToolDescription,
    /// MCP Tool description for `computer_screenshot`.
    pub description_screenshot: ToolDescription,
    /// MCP Tool description for `computer_act` (semantic pipeline on a11y
    /// elements).
    pub description_act: ToolDescription,
    /// MCP Tool description for `computer_control` (pointer + keyboard
    /// pipeline).
    pub description_control: ToolDescription,
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
                    "and computer_act.",
                    "\n\nWith target \"focused\" (default \"all\") it instead reports the ",
                    "FOREGROUND application and the element inside it that currently ",
                    "holds keyboard focus (role, name, current value, role path) — the ",
                    "cheap 'where will my keystrokes go?' check before computer_keyboard, ",
                    "which types into the focused element."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "enum": ["all", "focused"],
                            "description": "\"all\" (default) = every running app with PIDs and the foreground flag; \"focused\" = the foreground app plus the element holding keyboard focus inside it."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How long to wait for a foreground application to exist, in milliseconds (default 3000). Applies only to target \"focused\"."
                        }
                    },
                    "required": []
                }
            }),
            description_snapshot: serde_json::json!({
                "name": "computer_snapshot",
                "description": concat!(
                    "Capture the accessibility tree of a desktop application as a ",
                    "compact outline (role, name, value and state flags of every ",
                    "element). Prefer ",
                    "this over a screenshot when you need structure: names and roles ",
                    "are exact and can be used directly as selectors for computer_act. ",
                    "Each line carries the element's current state as plain tokens — ",
                    "disabled, hidden, focused, checked/unchecked/mixed, selected, ",
                    "expanded/collapsed, editable, busy — so a disabled or hidden ",
                    "control is visible BEFORE you try to act on it. Optionally ",
                    "snapshot only the subtree matching a CSS-like selector.",
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
                        },
                        "surface": {
                            "type": "string",
                            "enum": ["menu_bar", "status_items", "taskbar", "panel", "dock", "desktop", "flyout", "unknown"],
                            "description": "Target an OS SHELL SURFACE instead of an application — exactly one of `name`/`pid`/`surface`: menu_bar (macOS: File, Apple menu), status_items (tray/menu-bar extras), taskbar (Windows), panel (Linux), dock (macOS), desktop, flyout (Notification Center / Quick Settings / shell menus open RIGHT NOW — they exist only while open). Read-only: enumeration never opens, closes or focuses anything."
                        }
                    },
                    "anyOf": [
                        { "required": ["name"] },
                        { "required": ["pid"] },
                        { "required": ["surface"] }
                    ],
                    "allOf": [
                        {
                            "if": { "required": ["name"] },
                            "then": { "not": { "anyOf": [{ "required": ["pid"] }, { "required": ["surface"] }] } }
                        },
                        {
                            "if": { "required": ["pid"] },
                            "then": { "not": { "required": ["surface"] } }
                        },
                        {
                            "if": { "required": ["nth"] },
                            "then": { "required": ["selector"] }
                        }
                    ]
                }
            }),
            description_wait: serde_json::json!({
                "name": "computer_wait",
                "description": concat!(
                    "Block until an element of a desktop application reaches a state — ",
                    "or time out with a diagnosis of the LAST OBSERVED state. Replaces ",
                    "poll-loops (computer_snapshot → check → computer_snapshot …, each a ",
                    "full round trip): 'wait for the export to finish, then snapshot' is ",
                    "ONE call before the snapshot instead of N.",
                    "\n\ncomputer_act already auto-waits an element to be visible+enabled ",
                    "before acting — use computer_wait for the OTHER directions: waiting ",
                    "for things to DISAPPEAR (state \"detached\" — a dialog closed, a ",
                    "spinner went away; tolerates the element never having existed), to ",
                    "ENABLE later (\"enabled\"), to CLOSE (\"hidden\"), or to gain/lose ",
                    "keyboard focus (\"focused\"/\"unfocused\").",
                    "\n\nRead-only: no input is sent and nothing moves, so this tool is ",
                    "allowed in every mode. On timeout the error names the condition and ",
                    "the last observed state — decide the next move from that instead of ",
                    "probing again."
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
                            "description": "CSS-like selector for the element to watch, e.g. \"progress_indicator[name='Exporting…']\"."
                        },
                        "nth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "1-based match index when `selector` matches multiple elements (default 1)."
                        },
                        "state": {
                            "type": "string",
                            "enum": ["attached", "detached", "visible", "hidden", "enabled", "disabled", "focused", "unfocused"],
                            "description": "The condition to wait for (default \"visible\"): \"attached\" = exists; \"detached\" = gone (or never existed); \"visible\" = exists and is visible; \"hidden\" = hidden or gone; \"enabled\"/\"disabled\"; \"focused\"/\"unfocused\"."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 60000,
                            "description": "How long to wait for the condition, in milliseconds (default 10000, max 60000). A timeout is an ERROR carrying the last observed state — not a quiet false."
                        }
                    },
                    "required": ["selector"],
                    "anyOf": [
                        { "required": ["name"] },
                        { "required": ["pid"] }
                    ],
                    "allOf": [
                        {
                            "if": { "required": ["name"] },
                            "then": { "not": { "required": ["pid"] } }
                        }
                    ]
                }
            }),
            description_screenshot: serde_json::json!({
                "name": "computer_screenshot",
                "description": concat!(
                    "Capture the screen and return it as an image you can see. Use when visual ",
                    "layout matters; use computer_snapshot when you need exact element ",
                    "structure.",
                    "\n\nThree capture forms (exactly one): full display (omit every ",
                    "argument); `region` = [x, y, width, height] in desktop pixels; ",
                    "element capture = a ROOT together with a CSS-like `selector` ",
                    "(pixels under the element's current bounds; occluded elements ",
                    "are NOT raised first) — the root is `app`, `pid` or `surface` ",
                    "(an OS shell surface: menu_bar, status_items, taskbar, panel, ",
                    "dock, desktop, or a flyout open right now). Oversized captures ",
                    "are downscaled ",
                    "to `max_width` (default 1568); the output reports the DELIVERED ",
                    "image's dimensions, its desktop-space origin, and the desktop scale ",
                    "— map an image coordinate back to the screen as ",
                    "desktop = desktop_origin + image_coord * desktop_scale.",
                    "\n\nWith `annotate: true` the capture becomes SELECTOR-GROUNDED: every ",
                    "element of the app matching `selector` (default \"*\" = all) gets a ",
                    "labeled box drawn on the image, and the result carries a legend ",
                    "mapping each tag (`B7`) back to a round-trippable selector ",
                    "(`*:nth(7)`) you pass straight to computer_act — visual grounding ",
                    "without pixel coordinates. Requires `app` or `pid` (annotation groups ",
                    "must be application-scoped) and captures the full display; omissions ",
                    "and truncation (>100 matches) are reported — narrow the selector when ",
                    "truncated. Selectors must be single clauses: comma alternations ",
                    "(`\"button, link\"`) are rejected — pass one selector per intent."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "annotate": {
                            "type": "boolean",
                            "description": "Draw a labeled box on every matching element and return a legend mapping each tag to a selector computer_act accepts. Requires `app` or `pid`; mutually exclusive with `region`. Default false."
                        },
                        "region": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "minItems": 4,
                            "maxItems": 4,
                            "description": "Capture only this region: [x, y, width, height] in desktop pixels. Mutually exclusive with element capture and with `annotate`."
                        },
                        "app": {
                            "type": "string",
                            "description": "Application name (exact match). Element capture: requires `selector` (or pair with `annotate` to box all elements). Annotated capture: the annotation scope."
                        },
                        "pid": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Application process ID. Element capture: requires `selector` (or pair with `annotate` to box all elements). Annotated capture: the annotation scope."
                        },
                        "selector": {
                            "type": "string",
                            "description": "CSS-like selector, e.g. \"window[name='Main'] > group\". Element capture: which element to shoot. Annotated capture: which elements get boxes (default \"*\" = every element of the app). Single clause only — comma alternations are rejected."
                        },
                        "nth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "1-based match index when `selector` matches multiple elements (default 1). Element capture only — annotated captures cover every match."
                        },
                        "max_width": {
                            "type": "integer",
                            "minimum": 256,
                            "description": "Maximum delivered image width in pixels (default 1568). Coordinates refer to the delivered image; use desktop_origin/desktop_scale from the output to map back."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How long to wait for the app to appear (element/annotated capture), in milliseconds (default 3000)."
                        },
                        "surface": {
                            "type": "string",
                            "enum": ["menu_bar", "status_items", "taskbar", "panel", "dock", "desktop", "flyout", "unknown"],
                            "description": "Target an OS SHELL SURFACE instead of an application (element capture: requires `selector`; with `annotate: true` the legend covers the surface's elements the same way it covers an app's). Exactly one of `app`/`pid`/`surface`: menu_bar (macOS: File, Apple menu), status_items (tray/menu-bar extras), taskbar (Windows), panel (Linux), dock (macOS), desktop, flyout (open RIGHT NOW)."
                        }
                    },
                    "allOf": [
                        {
                            "if": { "required": ["region"] },
                            "then": {
                                "not": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }, { "required": ["selector"] }] }
                            }
                        },
                        {
                            "if": {
                                "allOf": [
                                    { "required": ["annotate"] },
                                    { "const": true, "properties": { "annotate": true } }
                                ]
                            },
                            "then": {
                                "allOf": [
                                    { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] },
                                    { "not": { "required": ["region"] } },
                                    { "not": { "required": ["nth"] } }
                                ]
                            }
                        },
                        {
                            "if": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] },
                            "then": {
                                "anyOf": [
                                    { "required": ["selector"] },
                                    {
                                        "allOf": [
                                            { "required": ["annotate"] },
                                            { "const": true, "properties": { "annotate": true } }
                                        ]
                                    }
                                ]
                            }
                        },
                        {
                            "if": { "required": ["app"] },
                            "then": { "not": { "anyOf": [{ "required": ["pid"] }, { "required": ["surface"] }] } }
                        },
                        {
                            "if": { "required": ["pid"] },
                            "then": { "not": { "required": ["surface"] } }
                        },
                        {
                            "if": { "required": ["selector"] },
                            "then": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] }
                        },
                        {
                            "if": { "required": ["nth"] },
                            "then": { "required": ["selector"] }
                        }
                    ],
                    "required": []
                }
            }),
            description_act: serde_json::json!({
                "name": "computer_act",
                "description": concat!(
                    "Act on a desktop element AND type as ONE pipeline. A call is a ",
                    "STEP — either a SEMANTIC action on the element matched by ",
                    "`selector` (press/toggle/select/set_value/..., auto-waiting for ",
                    "the element to become visible and enabled), a keyboard action ",
                    "(`key` or `text`), or a `wait` pause — and a step may chain the ",
                    "NEXT step via `then`: all steps run sequentially in this ONE call ",
                    "with shell `&&` semantics, each only if the previous succeeded, ",
                    "and the first failure aborts reporting the exact failing step ",
                    "plus everything that completed (max 8 steps).",
                    "\n\nThe pipeline skeleton (`then` → `then` → `wait`) is IDENTICAL ",
                    "to computer_control's — the model learns the shape once; only the ",
                    "step kinds differ (semantic actions here, pointer actions there). ",
                    "Typical flow: computer_apps → computer_snapshot (find role+name) → ",
                    "computer_act. Value actions need payloads: `set_value`/",
                    "`type_text`/`perform_action` need `value`, `set_numeric_value` ",
                    "needs `numeric_value`, `select_text` needs `range: [start, end]` ",
                    "(0-based). A `wait` step (standalone, millisecond cap 10 s) lets ",
                    "the app settle between steps. A semantic `press` focuses the ",
                    "target on many toolkits but NOT reliably on every one — the one ",
                    "mechanism that always moves OS keyboard focus is a real click, ",
                    "which is computer_control's job: if chained typing keeps missing ",
                    "the field, click the field element there instead. In Build mode ",
                    "chains containing an element step ask for approval; keyboard ",
                    "steps with no element step in the chain are denied (the approval ",
                    "dialog hands focus to the TUI)."
                ),
                "inputSchema": {
                    "type": "object",
                    "$defs": {
                        "step": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "Application name (exact match, e.g. 'Safari'). Provide `name` or `pid` — exactly one. Semantic-action steps only."
                                },
                                "pid": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Application process ID. Provide `name` or `pid` — exactly one. Semantic-action steps only."
                                },
                                "selector": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "CSS-like selector for the target element, e.g. \"button[name='OK']\". Required on a semantic-action step; absent on keyboard/wait steps."
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
                                    "description": "Semantic action to perform (default `press`). Semantic-action steps only."
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
                                    "description": "How long to wait for the app AND a visible+enabled element match, in milliseconds (default 3000). Semantic-action steps only."
                                },
                                "surface": {
                                    "type": "string",
                                    "enum": ["menu_bar", "status_items", "taskbar", "panel", "dock", "desktop", "flyout", "unknown"],
                                    "description": "Target an OS SHELL SURFACE instead of an application (semantic-action steps only, with `selector`): the menu bar, Dock/taskbar/panel, tray status items, the desktop, or a flyout open right now. Exactly one of `name`/`pid`/`surface` per step."
                                },
                                "key": {
                                    "type": "string",
                                    "description": "Key to tap: single lowercase character (`a`, `5`, `.`), `enter`, `escape`, `tab`, `space`, `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`. Types into whatever holds keyboard focus NOW."
                                },
                                "text": {
                                    "type": "string",
                                    "description": "Literal text to type into the focused element (any characters, including uppercase). Mutually exclusive with `key`."
                                },
                                "held": {
                                    "type": "array",
                                    "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "meta"] },
                                    "description": "Modifier keys held while tapping `key` (chord). Rejected with `text`."
                                },
                                "wait": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "maximum": 10000,
                                    "description": "Milliseconds to WAIT as a STANDALONE step (no other fields): gives the app time to settle before the next step acts."
                                },
                                "then": {
                                    "$ref": "#/$defs/step",
                                    "description": "Optional NEXT step of the pipeline, executed only if this step succeeded. Same shape as this step, recursively (max 8 steps)."
                                }
                            },
                            "allOf": [
                                {
                                    "anyOf": [
                                        { "required": ["selector"] },
                                        { "required": ["key"] },
                                        { "required": ["text"] },
                                        { "required": ["wait"] }
                                    ]
                                },
                                {
                                    "if": { "anyOf": [{ "required": ["key"] }, { "required": ["text"] }] },
                                    "then": {
                                        "not": { "anyOf": [{ "required": ["name"] }, { "required": ["pid"] }, { "required": ["surface"] }, { "required": ["selector"] }, { "required": ["nth"] }, { "required": ["action"] }, { "required": ["value"] }, { "required": ["numeric_value"] }, { "required": ["range"] }, { "required": ["timeout_ms"] }] }
                                    }
                                },
                                {
                                    "if": { "required": ["held"] },
                                    "then": { "required": ["key"] }
                                },
                                {
                                    "if": { "required": ["text"] },
                                    "then": { "properties": { "key": { "not": {} }, "held": { "not": {} } } }
                                },
                                {
                                    "if": { "required": ["key"] },
                                    "then": { "properties": { "text": { "not": {} } } }
                                },
                                {
                                    "if": { "required": ["wait"] },
                                    "then": { "not": { "anyOf": [{ "required": ["name"] }, { "required": ["pid"] }, { "required": ["surface"] }, { "required": ["selector"] }, { "required": ["nth"] }, { "required": ["action"] }, { "required": ["value"] }, { "required": ["numeric_value"] }, { "required": ["range"] }, { "required": ["timeout_ms"] }, { "required": ["key"] }, { "required": ["text"] }, { "required": ["held"] }] } }
                                },
                                {
                                    "if": { "anyOf": [{ "required": ["action"] }, { "required": ["value"] }, { "required": ["numeric_value"] }, { "required": ["range"] }, { "required": ["timeout_ms"] }] },
                                    "then": { "required": ["selector"] }
                                },
                                {
                                    "if": { "required": ["selector"] },
                                    "then": { "anyOf": [{ "required": ["name"] }, { "required": ["pid"] }, { "required": ["surface"] }] }
                                },
                                {
                                    "if": { "required": ["name"] },
                                    "then": { "not": { "anyOf": [{ "required": ["pid"] }, { "required": ["surface"] }] } }
                                },
                                {
                                    "if": { "required": ["pid"] },
                                    "then": { "not": { "required": ["surface"] } }
                                },
                                {
                                    "if": { "required": ["nth"] },
                                    "then": { "required": ["selector"] }
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
                            ]
                        }
                    },
                    "allOf": [ { "$ref": "#/$defs/step" } ]
                }
            }),
            description_control: serde_json::json!({
                "name": "computer_control",
                "description": concat!(
                    "Control the desktop: pointer AND keyboard as ONE pipeline. A call ",
                    "is a STEP — either a pointer action (`action`, default `click`, with ",
                    "a target) or a keyboard action (`key` or `text`) — and a step may ",
                    "chain the NEXT step via `then`: all steps run sequentially in this ",
                    "ONE call with shell `&&` semantics, each only if the previous ",
                    "succeeded, and the first failure aborts reporting the exact failing ",
                    "step plus everything that completed (max 8 steps).",
                    "\n\nTargeting is PER STEP (every step may carry `app`/`pid`/`surface` + ",
                    "`selector`). Two pointer target forms: ELEMENT form (preferred) — ",
                    "the point resolves from the element's CURRENT bounds at dispatch ",
                    "time, never stale; COORDINATE form — x/y from a computer_screenshot ",
                    "mapped as desktop = desktop_origin + image_coord * desktop_scale, ",
                    "ONLY when the target has no accessibility node. A real click on an ",
                    "element MOVES OS KEYBOARD FOCUS to it — the one mechanism that ",
                    "works on every toolkit — so the canonical pattern for typing into a ",
                    "field is: click the field element, `then` `text`, `then` `key` ",
                    "enter. Keyboard steps (`key`/`text`) always type into whatever ",
                    "holds focus NOW and reject pointer fields.",
                    "\n\nClick actions accept `count` (2 = double-click) and `held` ",
                    "modifiers (ctrl+click = held [\"ctrl\"] + click); key + held = ",
                    "chord (key `a` + held [\"ctrl\"] = select all; keys are lowercase — ",
                    "for uppercase hold `shift`); `text` types any literal string and ",
                    "rejects `held`. A `wait` step (millisecond cap 10 s, standalone — no ",
                    "other fields) gives the app time to catch up: click the menu item ",
                    "that opens a rename box, then {\"wait\": 600}, then type into the ",
                    "field it created. `action: drag` moves the pressed button from the ",
                    "start to the end point over `duration_ms` (default 150, min 50) — ",
                    "endpoints must share the form: x/y + x2/y2, or selector + ",
                    "to_selector. `action: up` needs no target. If the target appears ",
                    "in computer_snapshot, prefer computer_act — it is semantic and ",
                    "auto-waits. In Build mode the element form asks for approval; the ",
                    "coordinate form is denied (coordinates go stale when the approval ",
                    "dialog moves the pointer)."
                ),
                "inputSchema": {
                    "type": "object",
                    "$defs": {
                        "step": {
                            "type": "object",
                            "properties": {
                                "x": {
                                    "type": "integer",
                                    "description": "X coordinate in desktop pixels (coordinate form; also the drag START)."
                                },
                                "y": {
                                    "type": "integer",
                                    "description": "Y coordinate in desktop pixels (coordinate form; also the drag START)."
                                },
                                "action": {
                                    "type": "string",
                                    "enum": ["click", "double_click", "right_click", "move", "down", "up", "scroll", "drag"],
                                    "description": "Pointer action (default `click`). `down`/`up` allow custom drags; `drag` is the native interpolated drag. Ignored — and rejected with any other pointer field — on a keyboard step (`key`/`text`)."
                                },
                                "button": {
                                    "type": "string",
                                    "enum": ["left", "right", "middle"],
                                    "description": "Button for `down`/`up` and the button held during `drag` (default `left`)."
                                },
                                "dx": {
                                    "type": "integer",
                                    "description": "Horizontal wheel steps for `scroll`."
                                },
                                "dy": {
                                    "type": "integer",
                                    "description": "Vertical wheel steps for `scroll`; positive scrolls down."
                                },
                                "app": {
                                    "type": "string",
                                    "description": "Application name (exact match) for the element form. Exactly one of `app`/`pid`, only together with `selector`. Per step."
                                },
                                "pid": {
                                    "type": "integer",
                                    "description": "Application process ID for the element form. Per step."
                                },
                                "surface": {
                                    "type": "string",
                                    "enum": ["menu_bar", "status_items", "taskbar", "panel", "dock", "desktop", "flyout", "unknown"],
                                    "description": "Target an OS SHELL SURFACE instead of an application (element form: requires `selector`): the menu bar, Dock/taskbar/panel, tray status items, the desktop, or a flyout open right now. Exactly one of `app`/`pid`/`surface` per step."
                                },
                                "selector": {
                                    "type": "string",
                                    "description": "CSS-like selector for the element form, e.g. \"button[name='OK']\". A click on it moves OS keyboard focus to the element — how the next step's typing finds the field. For `drag`, the START element (end is `to_selector`)."
                                },
                                "nth": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "1-based match index when `selector` matches multiple elements (default 1)."
                                },
                                "anchor": {
                                    "type": "string",
                                    "enum": ["center", "top_left", "top_right", "bottom_left", "bottom_right"],
                                    "description": "Where inside the element's bounds the point lands (default `center`)."
                                },
                                "count": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Consecutive clicks for click actions (default 1; 2 = double-click)."
                                },
                                "held": {
                                    "type": "array",
                                    "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "meta"] },
                                    "description": "Modifier keys for THIS step: held during a click or drag, or held while tapping `key` (chord). Rejected with `text`."
                                },
                                "x2": {
                                    "type": "integer",
                                    "description": "End X for `drag` (with `y2`); mutually exclusive with `to_selector`."
                                },
                                "y2": {
                                    "type": "integer",
                                    "description": "End Y for `drag`."
                                },
                                "to_selector": {
                                    "type": "string",
                                    "description": "End element selector for `drag` (element form; same root as the start — app or surface)."
                                },
                                "duration_ms": {
                                    "type": "integer",
                                    "minimum": 50,
                                    "description": "Total duration of a `drag` movement in milliseconds (default 150)."
                                },
                                "key": {
                                    "type": "string",
                                    "description": "Key to tap: single lowercase character (`a`, `5`, `.`), `enter`, `escape`, `tab`, `space`, `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`. Types into the element an earlier step's click focused."
                                },
                                "text": {
                                    "type": "string",
                                    "description": "Literal text to type into the focused element (any characters, including uppercase). Mutually exclusive with `key`."
                                },
                                "wait": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "maximum": 10000,
                                    "description": "Milliseconds to WAIT as a STANDALONE step (no other fields): gives the app time to open a dialog or create an ephemeral field before the next step acts — e.g. click the menu item, then {\"wait\": 600}, then type into the rename box."
                                },
                                "then": {
                                    "$ref": "#/$defs/step",
                                    "description": "Optional NEXT step of the pipeline, executed only if this step succeeded. Same shape as this step, recursively (max 8 steps)."
                                }
                            },
                            "allOf": [
                                {
                                    "if": { "anyOf": [{ "required": ["key"] }, { "required": ["text"] }] },
                                    "then": {
                                        "not": { "anyOf": [{ "required": ["action"] }, { "required": ["x"] }, { "required": ["y"] }, { "required": ["button"] }, { "required": ["dx"] }, { "required": ["dy"] }, { "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }, { "required": ["selector"] }, { "required": ["nth"] }, { "required": ["anchor"] }, { "required": ["count"] }, { "required": ["x2"] }, { "required": ["y2"] }, { "required": ["to_selector"] }, { "required": ["duration_ms"] }] }
                                    }
                                },
                                {
                                    "if": { "required": ["text"] },
                                    "then": { "properties": { "key": { "not": {} }, "held": { "not": {} } } }
                                },
                                {
                                    "if": { "required": ["key"] },
                                    "then": { "properties": { "text": { "not": {} } } }
                                },
                                {
                                    "if": { "properties": { "action": { "const": "scroll" } }, "required": ["action"] },
                                    "then": { "anyOf": [{ "required": ["dx"] }, { "required": ["dy"] }] }
                                },
                                {
                                    "if": { "not": { "properties": { "action": { "const": "up" } }, "required": ["action"] } },
                                    "then": { "anyOf": [{ "allOf": [{ "required": ["key"] }, { "not": { "required": ["text"] } }] }, { "required": ["text"] }, { "allOf": [{ "required": ["x"] }, { "required": ["y"] }] }, { "required": ["selector"] }, { "required": ["wait"] }] }
                                },
                                {
                                    "if": { "required": ["wait"] },
                                    "then": { "not": { "anyOf": [{ "required": ["action"] }, { "required": ["x"] }, { "required": ["y"] }, { "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }, { "required": ["selector"] }, { "required": ["nth"] }, { "required": ["anchor"] }, { "required": ["count"] }, { "required": ["held"] }, { "required": ["x2"] }, { "required": ["y2"] }, { "required": ["to_selector"] }, { "required": ["duration_ms"] }, { "required": ["key"] }, { "required": ["text"] }, { "required": ["dx"] }, { "required": ["dy"] }, { "required": ["button"] }] } }
                                },
                                {
                                    "if": { "anyOf": [{ "required": ["x"] }, { "required": ["y"] }] },
                                    "then": { "allOf": [
                                        { "required": ["y"] },
                                        { "required": ["x"] },
                                        { "not": { "anyOf": [{ "required": ["selector"] }, { "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] } }
                                    ] }
                                },
                                {
                                    "if": { "required": ["selector"] },
                                    "then": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] }
                                },
                                {
                                    "if": { "anyOf": [{ "required": ["app"] }, { "required": ["pid"] }, { "required": ["surface"] }] },
                                    "then": { "required": ["selector"] }
                                },
                                {
                                    "if": { "required": ["app"] },
                                    "then": { "not": { "anyOf": [{ "required": ["pid"] }, { "required": ["surface"] }] } }
                                },
                                {
                                    "if": { "required": ["pid"] },
                                    "then": { "not": { "required": ["surface"] } }
                                },
                                {
                                    "if": { "anyOf": [{ "required": ["nth"] }, { "required": ["anchor"] }] },
                                    "then": { "required": ["selector"] }
                                },
                                {
                                    "if": { "properties": { "action": { "const": "drag" } }, "required": ["action"] },
                                    "then": {
                                        "anyOf": [
                                            { "allOf": [{ "required": ["x2", "y2"] }, { "not": { "required": ["to_selector"] } }] },
                                            { "allOf": [{ "required": ["to_selector"] }, { "not": { "anyOf": [{ "required": ["x2"] }, { "required": ["y2"] }] } }] }
                                        ]
                                    }
                                },
                                {
                                    "if": { "required": ["duration_ms"] },
                                    "then": { "properties": { "action": { "const": "drag" } }, "required": ["action"] }
                                },
                                {
                                    "if": {
                                        "anyOf": [
                                            { "required": ["x2"] },
                                            { "required": ["y2"] },
                                            { "required": ["to_selector"] }
                                        ]
                                    },
                                    "then": { "properties": { "action": { "const": "drag" } }, "required": ["action"] }
                                },
                                {
                                    "if": {
                                        "properties": { "action": { "enum": ["move", "down", "up", "scroll"] } },
                                        "required": ["action"]
                                    },
                                    "then": {
                                        "not": { "anyOf": [{ "required": ["count"] }, { "required": ["held"] }] }
                                    }
                                }
                            ]
                        }
                    },
                    "allOf": [ { "$ref": "#/$defs/step" } ]
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
    /// Returns `Err` when the platform accessibility API is unreachable, or —
    /// for `target: "focused"` — when no application holds the foreground
    /// within the timeout.
    pub async fn apps(&self, input: &ComputerApps) -> Result<apps::AppsResult, String> {
        apps::apps(input).await
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

    /// Block until an element reaches a state, or time out with a
    /// diagnosis of the last observed state.
    ///
    /// See [`wait`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for invalid input, an app that never surfaces, a
    /// selector that never matches, or a condition not met within the
    /// timeout (the error embeds xa11y's `Diagnosis`).
    pub async fn wait(&self, input: &ComputerWait) -> Result<WaitOutput, String> {
        wait::wait(input).await
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

    /// Run a semantic pipeline on an accessibility-tree element: actions,
    /// keyboard steps and waits chained via `then`, in ONE call.
    ///
    /// See [`act`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an invalid step anywhere in the chain (validated
    /// before any action), a missing `value`/`numeric_value`/`range`
    /// payload, when the app or a visible+enabled element match does not
    /// appear within the timeout, or when the platform rejects the action.
    pub async fn act(&self, input: &ComputerAct) -> Result<ActOutput, String> {
        act::act(input).await
    }

    /// Run a desktop-control pipeline: pointer and keyboard steps chained
    /// via `then`, executed sequentially in ONE call.
    ///
    /// See [`control`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an invalid step anywhere in the chain (validated
    /// before any event), an app/selector that does not resolve, or an
    /// unavailable input backend.
    pub async fn control(&self, input: &ComputerControl) -> Result<ControlOutput, String> {
        control::control(input).await
    }
}
