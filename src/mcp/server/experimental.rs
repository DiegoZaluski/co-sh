//! Minimal MCP server exposing only the `vision_terminal` tool.
//!
//! Run with: `cargo run --bin cosh`

use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, tool, tool_router,
    ErrorData as McpError,
};

/// Parameters for the vision_terminal tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct VisionTerminalParams {
    /// Raw terminal output to process (supports ANSI/escape sequences).
    pub output: String,
    /// Number of terminal rows (default: 24).
    #[schemars(default = "default_rows")]
    pub rows: usize,
    /// Number of terminal columns (default: 80).
    #[schemars(default = "default_cols")]
    pub cols: usize,
    /// When true returns plain text only; default false returns full JSON metadata.
    #[schemars(default)]
    pub plain: bool,
}

fn default_rows() -> usize {
    24
}

fn default_cols() -> usize {
    80
}

#[derive(Clone)]
pub struct VisionServer;

#[tool_router(server_handler)]
impl VisionServer {
    #[tool(
        description = "Process terminal output through a WezTerm-based virtual terminal emulator and return the screen state. By default returns JSON with full metadata (every visible cell with position, width, styling, structured RGB/palette colors, OSC 8 hyperlinks, cursor position/shape). Set 'plain: true' for plain text only."
    )]
    fn vision_terminal(
        &self,
        Parameters(params): Parameters<VisionTerminalParams>,
    ) -> Result<CallToolResult, McpError> {
        let input = cosh_tools::vision::TerminalInput {
            output: params.output,
            rows: params.rows,
            cols: params.cols,
            plain: params.plain,
        };
        let result = cosh_tools::vision::terminal(&input)
            .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, format!("{e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}
