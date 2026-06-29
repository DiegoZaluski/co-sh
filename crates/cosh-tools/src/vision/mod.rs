//! Terminal screen inspection for TUI construction.
//!
//! Provides a virtual terminal emulator that processes escape sequences
//! and exposes screen state. Use this when constructing Ratatui interfaces to
//! preview terminal output positioning without guessing.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::vision::terminal;
//!
//! let screen = terminal(&TerminalInput {
//!     output: "hello\nworld".into(),
//!     ..Default::default()
//! })?;
//! ```

use crate::ToolDescription;

mod terminal;

pub use terminal::{CellInfo, ColorInfo, HyperlinkInfo, TerminalInput, TerminalOutput, terminal};

/// MCP Tool bindings for terminal inspection.
pub struct Vision {
    /// MCP Tool description for `terminal`.
    pub description_terminal: ToolDescription,
}

impl Default for Vision {
    fn default() -> Self {
        Self::new()
    }
}

impl Vision {
    /// Create a new `Vision` with the tool description pre-configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            description_terminal: serde_json::json!({
                "name": "vision_terminal",
                "description": concat!(
                    "Process terminal output through a WezTerm-based virtual terminal emulator ",
                    "and return the screen state. By default returns JSON with full metadata ",
                    "(every visible cell with position, width, styling, structured RGB/palette colors, ",
                    "OSC 8 hyperlinks, cursor position/shape). ",
                    "Set 'plain: true' for plain text only."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "output": {
                            "type": "string",
                            "description": "Raw terminal output bytes (may include escape sequences) to process"
                        },
                        "rows": {
                            "type": "number",
                            "description": "Number of terminal rows (default: 24)",
                            "default": 24
                        },
                        "cols": {
                            "type": "number",
                            "description": "Number of terminal columns (default: 80)",
                            "default": 80
                        },
                        "plain": {
                            "type": "boolean",
                            "description": "When true, returns plain text; when false (default), returns JSON with full metadata",
                            "default": false
                        }
                    },
                    "required": ["output"]
                }
            }),
        }
    }

    /// Process terminal output through a virtual terminal emulator.
    ///
    /// The input is fed through a WezTerm-based virtual terminal emulator
    /// which interprets escape sequences for cursor positioning, colors, etc.
    /// Returns the visible screen as JSON with full metadata by default,
    /// or plain text if `input.plain` is set to true.
    ///
    /// # Errors
    ///
    /// Returns an error if processing fails.
    ///
    /// See [`terminal`] for details.
    pub fn terminal(&self, input: &TerminalInput) -> Result<String, String> {
        terminal(input)
    }
}

#[cfg(test)]
mod test;
