//! Read-only state snapshots for the TUI: LSP engine and MCP status.

use std::sync::Arc;

#[cfg(test)]
use cosh_sdk::connector::ClaudeThinkingBlock;
use cosh_tools::lsp::Lsp;

use super::Harness;

impl Harness {
    /// Catalog names of every language server currently alive (Starting or
    /// Ready) across both managers this harness drives — its own passive
    /// diagnostics manager and the one inside [`CoshTools`] used by the
    /// `lsp_*` tools. Deduplicated, order not guaranteed. An empty slice means
    /// no server is running right now.
    pub(super) fn active_lsp_server_names(&self) -> Vec<String> {
        use std::collections::HashSet;

        let mut servers = HashSet::new();
        let mut collect = |lsp: Option<&Arc<Lsp>>| {
            if let Some(lsp) = lsp {
                for (key, lifecycle) in lsp.manager().states() {
                    if matches!(
                        lifecycle,
                        cosh_sdk::lsp::ClientLifecycle::Starting
                            | cosh_sdk::lsp::ClientLifecycle::Ready
                    ) {
                        servers.insert(key.server);
                    }
                }
            }
        };
        collect(self.lsp.as_ref());
        collect(self.cosh_tools.as_ref().and_then(|c| c.lsp()));
        servers.into_iter().collect()
    }

    /// Whether LSP is enabled at all (`COSH_LSP` not `off`): true when either
    /// manager built a wrapper. Distinct from how many servers run right now.
    pub(super) fn lsp_available(&self) -> bool {
        self.lsp.is_some() || self.cosh_tools.as_ref().and_then(|c| c.lsp()).is_some()
    }

    /// Test-only access to the Claude thinking-block stash: the harness-test
    /// module is a sibling of `core`, so the private field is unreachable.
    #[cfg(test)]
    pub(crate) fn stash_thinking_blocks_for_test(&mut self, blocks: Vec<ClaudeThinkingBlock>) {
        self.pending_thinking_blocks = blocks;
    }

    #[cfg(test)]
    pub(crate) fn pending_thinking_blocks_count_for_test(&self) -> usize {
        self.pending_thinking_blocks.len()
    }

    #[cfg(test)]
    pub(crate) fn push_correction_for_test(&mut self, msg: &str) {
        self.correction_memory.push(msg);
    }

    /// Snapshot of the current LSP engine state for the TUI's footer/tags.
    pub(super) fn lsp_snapshot_event(&self) -> super::events::HarnessEvent {
        super::events::HarnessEvent::LspServers {
            available: self.lsp_available(),
            servers: self.active_lsp_server_names(),
        }
    }

    /// Current MCP snapshots for the TUI's prompt footer.
    pub fn mcp_snapshots(&self) -> Vec<crate::mcp::ServerSnapshot> {
        self.mcp.status_snapshots()
    }

    /// Snapshot of the current MCP state for the TUI's footer.
    pub(super) fn mcp_snapshot_event(&self) -> super::events::HarnessEvent {
        super::events::HarnessEvent::McpStatus {
            servers: self.mcp_snapshots(),
        }
    }
}
