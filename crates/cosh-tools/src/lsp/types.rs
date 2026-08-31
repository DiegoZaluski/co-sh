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
    /// Enclosing symbol from the hierarchy walk (e.g. the impl or parent fn).
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

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

/// Parameters for `lsp_rename`.
///
/// Two-phase by default: without `confirm` the tool returns the rename PLAN
/// (files + edit counts + preview) and changes nothing. Re-call with
/// `confirm: true` to apply.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct RenameInput {
    /// File containing the symbol to rename.
    pub file_path: String,
    /// Target position (1-based). Provide this OR `symbol`.
    pub position: Option<Position1>,
    /// Symbol name to locate in the file (first whole-word match).
    pub symbol: Option<String>,
    /// The new name.
    pub new_name: String,
    /// Apply the plan. Default false (dry-run).
    pub confirm: Option<bool>,
}

/// One file touched by a rename plan.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct RenameFilePlan {
    pub path: String,
    /// Number of edits inside this file.
    pub edits: usize,
    /// First replacements (`L{line} → {new}`), capped at 3. Empty on applied
    /// output.
    pub preview: Vec<String>,
}

/// Result of `lsp_rename`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RenameOutput {
    /// `true` when edits were written to disk.
    pub applied: bool,
    pub files: Vec<RenameFilePlan>,
    pub total_edits: usize,
    pub formatted: String,
}

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

/// Parameters for `lsp_hover`. Same hybrid addressing as definitions.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct HoverInput {
    pub file_path: String,
    pub position: Option<Position1>,
    pub symbol: Option<String>,
}

/// Result of `lsp_hover`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HoverOutput {
    /// Rendered hover contents (markdown when the server provides it).
    pub formatted: String,
}

// ---------------------------------------------------------------------------
// Workspace symbols
// ---------------------------------------------------------------------------

/// Parameters for `lsp_workspace_symbols`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct WorkspaceSymbolsInput {
    /// Substring query matched against symbol names. Empty = all symbols.
    pub query: Option<String>,
    /// Maximum symbols returned. Default 100.
    pub max_items: Option<u32>,
}

/// One workspace-wide symbol hit.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceSymbolEntry {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
    pub character: u32,
}

/// Result of `lsp_workspace_symbols`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceSymbolsOutput {
    pub symbols: Vec<WorkspaceSymbolEntry>,
    pub total: usize,
    pub formatted: String,
}

// ---------------------------------------------------------------------------
// Call hierarchy
// ---------------------------------------------------------------------------

/// Parameters for `lsp_call_hierarchy`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct CallHierarchyInput {
    pub file_path: String,
    pub position: Option<Position1>,
    pub symbol: Option<String>,
    /// `"outgoing"` (what this calls) or `"incoming"` (what calls this).
    /// Default `"outgoing"`.
    pub direction: Option<String>,
    /// Maximum calls returned. Default 50.
    pub max_items: Option<u32>,
}

/// One call relationship.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CallEntry {
    /// Callee name for outgoing; caller name for incoming.
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
    pub character: u32,
    /// Call-site positions inside the caller (incoming only).
    pub sites: Vec<(u32, u32)>,
}

/// Result of `lsp_call_hierarchy`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CallHierarchyOutput {
    pub direction: String,
    pub calls: Vec<CallEntry>,
    pub formatted: String,
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

/// Parameters for `lsp_code_actions`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct CodeActionsInput {
    /// File containing the diagnostic/error.
    pub file_path: String,
    /// 1-based line where the problem is.
    pub line: u32,
    /// Apply the Nth action's edit instead of listing. Default: list only.
    pub apply_index: Option<usize>,
}

/// One available code action.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeActionEntry {
    pub index: usize,
    /// Human-readable title from the server (e.g. "Import `HashMap`").
    pub title: String,
    /// Action kind (`quickfix`, `refactor`, `source`…).
    pub kind: Option<String>,
    /// Whether this action carries file edits (vs just a command).
    pub has_edit: bool,
    /// Files the edit touches.
    pub files: Vec<String>,
}

/// Result of `lsp_code_actions`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeActionsOutput {
    /// `true` when an action was applied to disk.
    pub applied: bool,
    pub actions: Vec<CodeActionEntry>,
    pub formatted: String,
}
