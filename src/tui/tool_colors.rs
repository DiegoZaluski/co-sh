//! Tool colour palette — RGB colours for spinner highlights.
//!
//! Each tool that has a visual spinner gets a distinctive colour
//! so the user can quickly identify which tool is running.
//!
//! Excluded tools (no dedicated colour, return `None`):
//!   - bash
//!   - write / edit
//!   - question
//!   - todo / plan_*
//!   - task / subagent
//!
//! Usage
//! -----
//! The caller should first map the raw tool name through [`tool_display`](crate::util::tool_render::tool_display)
//! and then pass the result to [`tool_color`].

use ratatui::style::Color;

/// Return the highlight colour for a *display* tool name.
///
/// Tools without a dedicated colour (bash, write, edit, question, todo,
/// task) always return `None` so the caller can fall back to a default.
///
/// # Examples
///
/// ```
/// assert_eq!(tool_color("glob"), Some(Color::Rgb(74, 158, 255)));
/// assert_eq!(tool_color("bash"), None);
/// ```
pub fn tool_color(display: &str) -> Option<Color> {
    let (r, g, b) = match display {
        "glob" => (74, 158, 255),          // file search  — bright blue
        "read" => (76, 175, 80),           // file read    — green
        "grep" => (171, 71, 188),          // text search  — purple
        "webfetch" => (38, 198, 218),      // HTTP fetch   — cyan
        "websearch" => (26, 188, 156),     // web search   — teal
        "skill" => (233, 30, 99),          // skill system — pink
        "recall_search" => (255, 167, 38), // recall/RAG   — amber
        "generic" => (158, 158, 158),      // fallback     — grey
        _ => return None,
    };
    Some(Color::Rgb(r, g, b))
}
