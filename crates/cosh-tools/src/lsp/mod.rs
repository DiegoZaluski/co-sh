//! Language-server tools built on the `cosh-sdk` LSP engine.
//!
//! - [`diagnostics::run_diagnostics`]: settle and render diagnostics. Not
//!   exposed to the agent as a tool; the fs wrapper consumes it directly to
//!   attach passive findings to write/edit/rollback results.
//! - [`definitions::run_definitions`]: go to definition (hybrid addressing).
//! - [`references::run_references`]: find references grouped by file.
//! - [`symbols::run_symbols`]: document symbols with hierarchy.
//! - [`restart::run_restart`]: targeted or workspace-wide server restart.
//! - [`rename::run_rename`]: two-phase rename (plan → confirm) applying
//!   WorkspaceEdit to disk with negotiated-encoding offsets.
//! - [`hover::run_hover`]: type/signature documentation at a position.
//! - [`workspace_symbols::run_workspace_symbols`]: project-wide symbol search.
//! - [`call_hierarchy::run_call_hierarchy`]: incoming/outgoing calls.
//! - [`code_actions::run_code_actions`]: quickfixes at a diagnostic position.
//!
//! [`Lsp`] bundles the [`Manager`] + [`DiagnosticsEngine`] pair every engine
//! shares, plus the path guard applied to model-supplied paths. Server
//! binaries must be on `PATH`; discovery is lazy — the first tool call that
//! touches a file starts its server.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::lsp::Lsp;
//!
//! let lsp = Lsp::new("/project");
//! lsp.diagnostics(&DiagnosticsInput {
//!     file_path: Some("src/main.rs".into()),
//!     ..Default::default()
//! }).await;
//! ```

pub mod call_hierarchy;
pub mod code_actions;
pub mod definitions;
pub mod diagnostics;
pub mod hover;
pub mod references;
pub mod rename;
pub mod restart;
pub mod support;
pub mod symbols;
pub mod types;
pub mod workspace_symbols;
pub use types::{DefinitionsInput, DiagnosticsInput, ReferencesInput, RestartInput, SymbolsInput};

#[cfg(test)]
mod test;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{ToolDescription, util::path_guard::PathGuard};
use cosh_sdk::lsp::{ClientKey, DiagnosticsEngine, Manager};

/// Shared-state wrapper for language-server tool operations.
///
/// The manager and diagnostics store live behind `Arc`s; session layers that
/// need the same state elsewhere (e.g. an event pump) clone those handles
/// directly via [`Lsp::manager`] / [`Lsp::diagnostics_engine`].
pub struct Lsp {
    manager: Arc<Manager>,
    diagnostics: Arc<DiagnosticsEngine>,
    guard: PathGuard,
    request_timeout: Duration,
    settle_cap: Duration,

    /// MCP Tool description for `lsp_definitions`.
    pub description_definitions: ToolDescription,
    /// MCP Tool description for `lsp_references`.
    pub description_references: ToolDescription,
    /// MCP Tool description for `lsp_symbols`.
    pub description_symbols: ToolDescription,
    /// MCP Tool description for `lsp_restart`.
    pub description_restart: ToolDescription,
    /// MCP Tool description for `lsp_rename`.
    pub description_rename: ToolDescription,
    /// MCP Tool description for `lsp_hover`.
    pub description_hover: ToolDescription,
    /// MCP Tool description for `lsp_workspace_symbols`.
    pub description_workspace_symbols: ToolDescription,
    /// MCP Tool description for `lsp_call_hierarchy`.
    pub description_call_hierarchy: ToolDescription,
    /// MCP Tool description for `lsp_code_actions`.
    pub description_code_actions: ToolDescription,
}

impl Lsp {
    /// Wrapper bound to a workspace root with default budgets (10 s per
    /// query, 5 s settle cap).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_manager(
            Arc::new(Manager::new(root)),
            Arc::new(DiagnosticsEngine::new()),
        )
    }

    /// Wrapper over an existing manager/engine pair — the wiring used by
    /// session layers that already pump events into the diagnostics store.
    #[must_use]
    pub fn with_manager(manager: Arc<Manager>, diagnostics: Arc<DiagnosticsEngine>) -> Self {
        let root = manager.root().to_path_buf();
        Self {
            manager,
            diagnostics,
            guard: PathGuard::new(&root, None, None),
            request_timeout: Duration::from_secs(10),
            settle_cap: Duration::from_secs(5),
            description_definitions: json_description(
                "lsp_definitions",
                concat!(
                    "Go to the definition of the symbol at a given position. ",
                    "Addressing is flexible: pass `position` (1-based line and ",
                    "character), OR just `symbol` and the first whole-word match ",
                    "in the file is used.\n\n",
                    "Returns absolute paths with 1-based positions plus the ",
                    "source line text, so you can jump straight to a read/edit ",
                    "call afterwards."
                ),
                &location_schema(),
            ),
            description_references: json_description(
                "lsp_references",
                concat!(
                    "Find all references to the symbol at a given position, ",
                    "grouped by file. Same hybrid addressing as definitions: ",
                    "`position` or bare `symbol`.\n\n",
                    "Use to gauge blast radius before changing a signature or to ",
                    "find every caller worth updating. Declaration inclusion can ",
                    "be toggled."
                ),
                &{
                    let mut schema = location_schema();
                    schema["properties"]["include_declaration"] = serde_json::json!({
                        "type": "boolean",
                        "description": "Include the declaration itself among references. Default true."
                    });
                    schema["properties"]["max_items"] = serde_json::json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum references returned. Default 100."
                    });
                    schema
                },
            ),
            description_symbols: json_description(
                "lsp_symbols",
                concat!(
                    "List document symbols (functions, structs, methods…) with ",
                    "their kinds and positions, preserving hierarchy. Optional ",
                    "substring `query` filters names.\n\n",
                    "Cheaper than reading a whole file when you need its shape: ",
                    "call this first, then read only the ranges you care about."
                ),
                &serde_json::json!({
                    "type": "object",
                    "required": ["file_path"],
                    "properties": {
                        "file_path": { "type": "string", "description": "File whose symbols are listed." },
                        "query": { "type": "string", "description": "Case-insensitive substring filter on symbol names." },
                        "max_items": { "type": "integer", "minimum": 1, "description": "Maximum symbols returned. Default 200." }
                    }
                }),
            ),
            description_restart: json_description(
                "lsp_restart",
                concat!(
                    "Restart one language server (scoped to a file) or all of ",
                    "them. Use when servers look wedged: diagnostics frozen, ",
                    "queries timing out, or stale completions after config ",
                    "changes.\n\n",
                    "Scoped restarts re-open the touched file immediately; ",
                    "workspace-wide restarts respawn lazily on next touch."
                ),
                &serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Restart only the server(s) serving this file. Omitted: restart everything running." }
                    }
                }),
            ),
            description_rename: json_description(
                "lsp_rename",
                concat!(
                    "Rename a symbol across the workspace via the language ",
                    "server. TWO-PHASE by design: the first call (confirm ",
                    "omitted/false) returns the PLAN — files, edit counts, ",
                    "preview lines — and changes nothing. Review it, then ",
                    "re-call with confirm=true to write.\n\n",
                    "Addressing matches definitions: position OR bare symbol ",
                    "name. LSP errors on the touched files are reported ",
                    "automatically after each fs write/edit, so rely on that ",
                    "passive feedback to catch fallout."
                ),
                &{
                    let mut schema = location_schema();
                    schema["properties"]["new_name"] = serde_json::json!({
                        "type": "string",
                        "description": "The new name for the symbol."
                    });
                    schema["properties"]["confirm"] = serde_json::json!({
                        "type": "boolean",
                        "description": "Apply the rename. Default false: returns the plan only."
                    });
                    schema
                },
            ),
            description_hover: json_description(
                "lsp_hover",
                concat!(
                    "Show type signature and documentation for the symbol at a ",
                    "position. Same hybrid addressing as definitions (position ",
                    "OR symbol).\n\n",
                    "Use when you need the exact type or doc comment without ",
                    "navigating away."
                ),
                &location_schema(),
            ),
            description_workspace_symbols: json_description(
                "lsp_workspace_symbols",
                concat!(
                    "Search symbols across the whole workspace (functions, ",
                    "structs, methods…) using the language server's index. ",
                    "Substring query on names.\n\n",
                    "Requires at least one server already running — touch any ",
                    "project file first if servers have not started."
                ),
                &serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Substring filter on symbol names. Empty: return all." },
                        "max_items": { "type": "integer", "minimum": 1, "description": "Maximum symbols returned. Default 100." }
                    }
                }),
            ),
            description_call_hierarchy: json_description(
                "lsp_call_hierarchy",
                concat!(
                    "Incoming or outgoing calls around a symbol: what it calls ",
                    "(outgoing) or what calls it (incoming). Same hybrid ",
                    "addressing as definitions.\n\n",
                    "Use to trace call chains before refactoring or to map a ",
                    "code path end-to-end."
                ),
                &{
                    let mut schema = location_schema();
                    schema["properties"]["direction"] = serde_json::json!({
                        "type": "string",
                        "enum": ["incoming", "outgoing"],
                        "description": "Direction of the hierarchy. Default outgoing."
                    });
                    schema["properties"]["max_items"] = serde_json::json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum calls returned. Default 50."
                    });
                    schema
                },
            ),
            description_code_actions: json_description(
                "lsp_code_actions",
                concat!(
                    "Quickfixes and refactoring suggestions for the error at ",
                    "the given line. The language server knows how to fix its ",
                    "own diagnostics — missing imports, wrong types, unused ",
                    "variables.\n\n",
                    "Two-phase: first call lists available actions. Re-call ",
                    "with apply_index to apply one to disk."
                ),
                &serde_json::json!({
                    "type": "object",
                    "required": ["file_path", "line"],
                    "properties": {
                        "file_path": { "type": "string", "description": "File containing the error." },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line where the problem is." },
                        "apply_index": { "type": "integer", "minimum": 0, "description": "Apply the Nth action's edit instead of listing." }
                    }
                }),
            ),
        }
    }

    /// Workspace root this wrapper serves.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.guard.root()
    }

    /// The shared diagnostics store (for event pumps feeding it).
    #[must_use]
    pub fn diagnostics_engine(&self) -> &Arc<DiagnosticsEngine> {
        &self.diagnostics
    }

    /// The underlying manager (session layers start/stop through it too).
    #[must_use]
    pub fn manager(&self) -> &Arc<Manager> {
        &self.manager
    }

    /// Per-request deadline used by the query engines.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Settle budget handed to the diagnostics engine after touches.
    #[must_use]
    pub fn settle_cap(&self) -> Duration {
        self.settle_cap
    }

    /// Override the per-request query deadline.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override the settle budget.
    #[must_use]
    pub fn with_settle_cap(mut self, cap: Duration) -> Self {
        self.settle_cap = cap;
        self
    }

    /// Narrow the path guard's allowlist (same semantics as other tools).
    #[must_use]
    pub fn allowlist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        let list: Vec<PathBuf> = paths.into_iter().map(Into::into).collect();
        self.guard = PathGuard::new(self.guard.root(), Some(&list), self.guard.blocklist());
        self
    }

    fn deps(&self) -> support::Deps<'_> {
        support::Deps {
            manager: &self.manager,
            diagnostics: &self.diagnostics,
            request_timeout: self.request_timeout,
            settle_cap: self.settle_cap,
        }
    }

    fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        self.guard.resolve(raw)
    }

    /// `lsp_diagnostics` entry point.
    pub async fn diagnostics(
        &self,
        input: &types::DiagnosticsInput,
    ) -> Result<types::DiagnosticsOutput, String> {
        let mut scoped = input.clone();
        if let Some(file_path) = &input.file_path {
            scoped.file_path = Some(self.resolve(file_path)?.display().to_string());
        }
        diagnostics::run_diagnostics(&self.deps(), &scoped).await
    }

    /// `lsp_definitions` entry point.
    pub async fn definitions(
        &self,
        input: &types::DefinitionsInput,
    ) -> Result<types::DefinitionsOutput, String> {
        let mut scoped = input.clone();
        scoped.location.file_path = self
            .resolve(&input.location.file_path)?
            .display()
            .to_string();
        definitions::run_definitions(&self.deps(), &scoped).await
    }

    /// `lsp_references` entry point.
    pub async fn references(
        &self,
        input: &types::ReferencesInput,
    ) -> Result<types::ReferencesOutput, String> {
        let mut scoped = input.clone();
        scoped.location.file_path = self
            .resolve(&input.location.file_path)?
            .display()
            .to_string();
        references::run_references(&self.deps(), &scoped).await
    }

    /// `lsp_symbols` entry point.
    pub async fn symbols(
        &self,
        input: &types::SymbolsInput,
    ) -> Result<types::SymbolsOutput, String> {
        let mut scoped = input.clone();
        scoped.file_path = self.resolve(&input.file_path)?.display().to_string();
        symbols::run_symbols(&self.deps(), &scoped).await
    }

    /// `lsp_restart` entry point.
    pub async fn restart(
        &self,
        input: &types::RestartInput,
    ) -> Result<types::RestartOutput, String> {
        let mut scoped = input.clone();
        if let Some(file_path) = &input.file_path {
            scoped.file_path = Some(self.resolve(file_path)?.display().to_string());
        }
        restart::run_restart(&self.deps(), &scoped).await
    }

    /// `lsp_rename` entry point.
    pub async fn rename(&self, input: &types::RenameInput) -> Result<types::RenameOutput, String> {
        let mut scoped = input.clone();
        scoped.file_path = self.resolve(&input.file_path)?.display().to_string();
        rename::run_rename(&self.deps(), &scoped).await
    }

    /// `lsp_hover` entry point.
    pub async fn hover(&self, input: &types::HoverInput) -> Result<types::HoverOutput, String> {
        let mut scoped = input.clone();
        scoped.file_path = self.resolve(&input.file_path)?.display().to_string();
        hover::run_hover(&self.deps(), &scoped).await
    }

    /// `lsp_workspace_symbols` entry point.
    pub async fn workspace_symbols(
        &self,
        input: &types::WorkspaceSymbolsInput,
    ) -> Result<types::WorkspaceSymbolsOutput, String> {
        workspace_symbols::run_workspace_symbols(&self.deps(), input).await
    }

    /// `lsp_call_hierarchy` entry point.
    pub async fn call_hierarchy(
        &self,
        input: &types::CallHierarchyInput,
    ) -> Result<types::CallHierarchyOutput, String> {
        let mut scoped = input.clone();
        scoped.file_path = self.resolve(&input.file_path)?.display().to_string();
        call_hierarchy::run_call_hierarchy(&self.deps(), &scoped).await
    }

    /// `lsp_code_actions` entry point.
    pub async fn code_actions(
        &self,
        input: &types::CodeActionsInput,
    ) -> Result<types::CodeActionsOutput, String> {
        let mut scoped = input.clone();
        scoped.file_path = self.resolve(&input.file_path)?.display().to_string();
        code_actions::run_code_actions(&self.deps(), &scoped).await
    }

    /// Key lookup helper for session layers driving `stop_client` directly.
    #[must_use]
    pub fn keys_for_file(&self, raw: &str) -> Vec<ClientKey> {
        match self.guard.resolve(raw) {
            Ok(path) => self
                .manager
                .matches_for_file(&path)
                .into_iter()
                .map(|(key, _)| key)
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn location_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["file_path"],
        "properties": {
            "file_path": { "type": "string", "description": "File to query." },
            "position": {
                "type": "object",
                "description": "Target position (1-based). Provide this OR `symbol`.",
                "required": ["line", "character"],
                "properties": {
                    "line": { "type": "integer", "minimum": 1 },
                    "character": { "type": "integer", "minimum": 1 }
                }
            },
            "symbol": {
                "type": "string",
                "description": "Symbol name; its first whole-word occurrence in the file is queried. Used when `position` is omitted."
            }
        }
    })
}

fn json_description(
    name: &'static str,
    description: &'static str,
    properties: &serde_json::Value,
) -> ToolDescription {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": properties["properties"].clone(),
    });
    if let Some(required) = properties.get("required") {
        schema["required"] = required.clone();
    }
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}
