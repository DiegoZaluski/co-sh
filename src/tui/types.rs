use cosh_tui::core::lib::rgba::RGBA;

use crate::theme::Theme;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<Part>,
    pub created_at: u64,
    pub agent: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub enum Part {
    Text(TextPart),
    Tool(ToolPart),
    Reasoning(ReasoningPart),
    File(FilePart),
}

#[derive(Debug, Clone)]
pub struct TextPart {
    pub text: String,
    pub synthetic: bool,
}

#[derive(Debug, Clone)]
pub struct ToolPart {
    pub tool: String,
    pub input: serde_json::Value,
    pub output: Option<String>,
    pub status: ToolStatus,
    pub tool_call_id: Option<String>,
    pub is_start: bool,
    pub is_streaming: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ReasoningPart {
    pub text: String,
    pub collapsed: bool,
}

#[derive(Debug, Clone)]
pub struct FilePart {
    pub filename: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq)]
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
