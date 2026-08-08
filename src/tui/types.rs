use serde::{Deserialize, Serialize};

use cosh_tui::core::lib::rgba::RGBA;

use crate::theme::Theme;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    /// Unix timestamp in milliseconds when the session was created.
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<Part>,
    pub created_at: u64,
    pub agent: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Part {
    Text(TextPart),
    Tool(ToolPart),
    Reasoning(ReasoningPart),
    File(FilePart),
    /// A context-compaction status line (pipeline stopwatch / phase notice).
    Compaction(CompactionPart),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPart {
    pub text: String,
    pub synthetic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPart {
    pub tool: String,
    pub input: serde_json::Value,
    pub output: Option<String>,
    pub status: ToolStatus,
    pub tool_call_id: Option<String>,
    pub is_start: bool,
    pub is_streaming: bool,
    /// Cached line count for streaming tools (glob/grep) to avoid O(n) recounting
    #[serde(skip)]
    pub cached_line_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningPart {
    pub text: String,
    pub collapsed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePart {
    pub filename: String,
    pub mime: String,
}

/// A context-compaction status line in the chat. The pipeline line carries a
/// live stopwatch while it runs; the other phase lines are one-shot notices.
/// Persisted with the session like any other part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionPart {
    /// Which compaction phase this line reports.
    pub phase: CompactionPhase,
    /// Wall-clock unix millis when the phase started (persisted so a restored
    /// session can still show a coherent elapsed time).
    pub started_at: u64,
    /// Final elapsed millis when the phase finished; `None` while running.
    pub elapsed_ms: Option<u64>,
}

/// Which context-compaction phase a [`CompactionPart`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactionPhase {
    /// Phase 1 — the TF-IDF → LSA → MMR pipeline. The only phase with a
    /// stopwatch: it runs synchronously on the agent loop's thread and can
    /// block it for seconds.
    Pipeline,
    /// Phase 2 — gradual draft eviction.
    Drafts,
    /// Phase 3 — tool-chain eviction.
    Tools,
    /// Phase 4 — loop-closure trimming.
    Closures,
}

impl CompactionPart {
    /// A fresh running phase line (stopwatch active).
    pub fn running(phase: CompactionPhase) -> Self {
        Self {
            phase,
            started_at: now_ms(),
            elapsed_ms: None,
        }
    }

    /// A finished one-shot phase line (no stopwatch — phases 2-4 are too fast).
    pub fn done(phase: CompactionPhase) -> Self {
        Self {
            phase,
            started_at: now_ms(),
            elapsed_ms: Some(0),
        }
    }

    pub fn is_running(&self) -> bool {
        self.elapsed_ms.is_none()
    }
}

/// Current wall-clock time in unix milliseconds (used by the stopwatch).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Working,
    Retry {
        message: String,
        action: Option<serde_json::Value>,
    },
}

pub struct AgentColors {
    palette: Vec<RGBA>,
}

impl AgentColors {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            palette: vec![
                theme.secondary,
                theme.accent,
                theme.success,
                theme.warning,
                theme.primary,
                theme.error,
                theme.info,
            ],
        }
    }

    pub fn get(&self, name: &str, unique_agents: &[String]) -> RGBA {
        let index = unique_agents.iter().position(|a| a == name).unwrap_or(0);
        self.palette[index % self.palette.len()]
    }
}
