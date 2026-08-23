//! Input and output types shared by all `lsp` tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for `lsp_diagnostics`.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct DiagnosticsInput {
    /// File to scope diagnostics to. Omitted: every tracked file in the
    /// workspace. The file is opened on its language server if needed.
    pub file_path: Option<String>,
    /// Minimum severity to report: `"errors"`, `"warnings"` (errors +
    /// warnings), or `"all"`. Default `"all"`.
    pub severity: Option<String>,
    /// Maximum number of formatted lines. Default 50.
    pub max_items: Option<u32>,
    /// Milliseconds to wait for diagnostics to settle after opening/refreshing
    /// the file. Default 5000.
    pub settle_ms: Option<u32>,
}

/// Result of `lsp_diagnostics`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DiagnosticsOutput {
    /// Model-ready rendering: one diagnostic per line, ordered by severity
    /// then position, deduplicated across servers, capped with an explicit
    /// `… and N more` marker.
    pub formatted: String,
    /// Number of rendered lines (excluding any truncation marker).
    pub count: usize,
    /// Whether the store was quiet before rendering (`true` = fresh data;
    /// `false` = the settle budget elapsed mid-churn and results may be stale).
    pub settled: bool,
}

/// One-based position inside a file, as the model counts lines.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub struct Position1 {
    /// 1-based line number.
    pub line: u32,
    /// 1-based character/column offset.
    pub character: u32,
}

/// Parameters shared by `lsp_definitions` and `lsp_references`.
///
/// Addressing is hybrid: give `position`, or just `symbol` and the engine
/// locates its first whole-word occurrence in the file.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct LocationInput {
    /// File to query.
    pub file_path: String,
    /// Target position (1-based). Provide this OR `symbol`.
    pub position: Option<Position1>,
    /// Symbol name to locate in the file (first whole-word match). Used when
    /// `position` is omitted.
    pub symbol: Option<String>,
}

/// Parameters for `lsp_definitions`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct DefinitionsInput {
    #[serde(flatten)]
    pub location: LocationInput,
}

/// Parameters for `lsp_references`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ReferencesInput {
    #[serde(flatten)]
    pub location: LocationInput,
    /// Include the declaration itself among the references. Default true.
    pub include_declaration: Option<bool>,
    /// Maximum number of references returned. Default 100.
    pub max_items: Option<u32>,
}

/// A resolved definition/reference target.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct LocationEntry {
    /// File path (absolute).
    pub path: String,
    /// 1-based line of the target start.
    pub line: u32,
    /// 1-based character of the target start.
    pub character: u32,
    /// The source line's text, trimmed (context aid).
    pub text: String,
}

/// Result of `lsp_definitions`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DefinitionsOutput {
    pub definitions: Vec<LocationEntry>,
    /// Model-ready rendering, one entry per line.
    pub formatted: String,
}

/// References grouped by file.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ReferenceFile {
    pub path: String,
    /// 1-based positions of each reference in this file.
    pub locations: Vec<(u32, u32)>,
}

/// Result of `lsp_references`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ReferencesOutput {
    pub total: usize,
    pub files: Vec<ReferenceFile>,
    /// Model-ready rendering grouped by file.
    pub formatted: String,
}

/// Parameters for `lsp_symbols`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SymbolsInput {
    /// File whose document symbols are listed.
    pub file_path: String,
    /// Substring filter on symbol names (case-insensitive). Omitted: return
    /// everything.
    pub query: Option<String>,
    /// Maximum number of symbols returned. Default 200.
    pub max_items: Option<u32>,
}

/// One document symbol.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SymbolEntry {
    pub name: String,
    /// LSP symbol kind rendered as text (`function`, `struct`, …).
    pub kind: String,
    /// 1-based definition position.
    pub line: u32,
    pub character: u32,
    /// Container name when the server reports one (e.g. enclosing impl).
    pub container: Option<String>,
}

/// Result of `lsp_symbols`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SymbolsOutput {
    pub symbols: Vec<SymbolEntry>,
    /// Model-ready indented tree preserving hierarchy.
    pub formatted: String,
}

/// Parameters for `lsp_restart`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct RestartInput {
    /// Restart only the server(s) serving this file. Omitted: restart every
    /// running server in the workspace.
    pub file_path: Option<String>,
}

/// Result of `lsp_restart`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct RestartOutput {
    /// Server names that were stopped (they respawn lazily on next touch;
    /// files touched during restart are re-opened immediately).
    pub restarted: Vec<String>,
}
