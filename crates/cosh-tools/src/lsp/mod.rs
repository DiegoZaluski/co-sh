//! Language-server tools built on the `cosh-sdk` LSP engine.
//!
//! - [`diagnostics::run_diagnostics`]: settle and render diagnostics.
//! - [`definitions::run_definitions`]: go to definition (hybrid addressing).
//! - [`references::run_references`]: find references grouped by file.
//! - [`symbols::run_symbols`]: document symbols with hierarchy.
//! - [`restart::run_restart`]: targeted or workspace-wide server restart.
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

pub mod definitions;
pub mod diagnostics;
pub mod references;
pub mod restart;
pub mod support;
pub mod symbols;
pub mod types;

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

    /// MCP Tool description for `lsp_diagnostics`.
    pub description_diagnostics: ToolDescription,
    /// MCP Tool description for `lsp_definitions`.
    pub description_definitions: ToolDescription,
    /// MCP Tool description for `lsp_references`.
    pub description_references: ToolDescription,
    /// MCP Tool description for `lsp_symbols`.
    pub description_symbols: ToolDescription,
    /// MCP Tool description for `lsp_restart`.
    pub description_restart: ToolDescription,
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
            description_diagnostics: json_description(
                "lsp_diagnostics",
                concat!(
                    "List compiler/language-server diagnostics for one file or ",
                    "the whole workspace. Opens the file on its language server ",
                    "and waits for results to settle before rendering.\n\n",
                    "Use after edits to confirm code is clean, before committing, ",
                    "or whenever you suspect type/borrow/import errors you cannot ",
                    "see from source alone. Prefer this over compiling manually — ",
                    "the language server is faster and incremental."
                ),
                &serde_json::json!({
                    "file_path": {
                        "type": "string",
                        "description": "File to scope diagnostics to (workspace-relative or absolute). Omitted: report every tracked file in the workspace."
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["errors", "warnings", "all"],
                        "description": "Minimum severity to report. Default \"all\"."
                    },
                    "max_items": { "type": "integer", "minimum": 1, "description": "Maximum formatted lines. Default 50." },
                    "settle_ms": { "type": "integer", "minimum": 0, "description": "How long to wait for diagnostics to settle after opening the file. Default 5000." }
                }),
            ),
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
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties["properties"].clone(),
        }
    })
}
