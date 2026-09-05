use serde::{Deserialize, Serialize};

/// Lifecycle state of one registered MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

/// Point-in-time view of one server for the TUI footer (`mcp ready/failed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSnapshot {
    pub name: String,
    pub status: ServerStatus,
    pub tool_count: usize,
    pub last_error: Option<String>,
}

impl ServerSnapshot {
    pub fn disabled(name: String) -> Self {
        Self {
            name,
            status: ServerStatus::Disabled,
            tool_count: 0,
            last_error: None,
        }
    }

    pub fn connecting(name: String) -> Self {
        Self {
            name,
            status: ServerStatus::Connecting,
            tool_count: 0,
            last_error: None,
        }
    }

    pub fn ready(name: String, tool_count: usize) -> Self {
        Self {
            name,
            status: ServerStatus::Ready,
            tool_count,
            last_error: None,
        }
    }

    pub fn failed(name: String, error: String) -> Self {
        Self {
            name,
            status: ServerStatus::Failed,
            tool_count: 0,
            last_error: Some(error),
        }
    }

    pub const fn is_ready(&self) -> bool {
        matches!(self.status, ServerStatus::Ready)
    }
}

/// Number of servers currently serving tools. `Connecting` and `Disabled`
/// servers are excluded: the footer counts settled states only.
pub fn ready_count(snapshots: &[ServerSnapshot]) -> usize {
    snapshots.iter().filter(|s| s.is_ready()).count()
}

/// Number of servers in a failed state. `Connecting` and `Disabled` servers
/// are excluded for the same reason as in [`ready_count`].
pub fn failed_count(snapshots: &[ServerSnapshot]) -> usize {
    snapshots
        .iter()
        .filter(|s| matches!(s.status, ServerStatus::Failed))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_split_ready_and_failed() {
        let snapshots = vec![
            ServerSnapshot::ready("a".to_string(), 2),
            ServerSnapshot::failed("b".to_string(), "boom".to_string()),
            ServerSnapshot {
                name: "c".to_string(),
                status: ServerStatus::Disabled,
                tool_count: 0,
                last_error: None,
            },
        ];
        assert_eq!(ready_count(&snapshots), 1);
        assert_eq!(failed_count(&snapshots), 1);
    }
}
