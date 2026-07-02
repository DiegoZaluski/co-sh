use crate::types::{
    FilePart, Message, MessageRole, Part, ReasoningPart, Session, SessionStatus, TextPart,
    ToolPart, ToolStatus,
};

pub struct AppState {
    pub sessions: Vec<Session>,
    pub current_session_id: Option<String>,
    pub status: SessionStatus,
    pub scroll_y: i32,
    pub content_height: i32,
    pub working_directory: String,
    pub connected: bool,
    pub lsp_count: usize,
    pub mcp_count: usize,
    pub mcp_errors: usize,
    pub permission_count: usize,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            sessions: vec![],
            current_session_id: None,
            status: SessionStatus::Idle,
            scroll_y: 0,
            content_height: 0,
            working_directory: String::new(),
            connected: false,
            lsp_count: 0,
            mcp_count: 0,
            mcp_errors: 0,
            permission_count: 0,
        }
    }

    pub fn current_session(&self) -> Option<&Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| s.id == *id))
    }

    pub fn current_session_mut(&mut self) -> Option<&mut Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.iter_mut().find(|s| s.id == *id))
    }

    pub fn max_scroll(&self) -> i32 {
        (self.content_height - 10).max(0)
    }

    pub fn unique_agents(&self) -> Vec<String> {
        let mut seen = Vec::new();
        if let Some(session) = self.current_session() {
            for msg in &session.messages {
                if let Some(agent) = &msg.agent
                    && !seen.contains(agent)
                {
                    seen.push(agent.clone());
                }
            }
        }
        seen
    }

    pub fn add_demo_data(&mut self) {
        let session = Session {
            id: "demo-1".to_string(),
            title: "Demo Session".to_string(),
            messages: vec![
                Message {
                    id: "msg-1".to_string(),
                    role: MessageRole::User,
                    agent: Some("build".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "Hi! Can you help me write a Rust CLI tool that processes JSON files?".to_string(),
                        synthetic: false,
                    })],
                    created_at: 1000,
                },
                Message {
                    id: "msg-2".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![
                        Part::Text(TextPart {
                            text: "I'd be happy to help! Let me first **look at what files** you have in the project.".to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "glob".to_string(),
                            input: serde_json::json!({"pattern": "**/*.rs", "path": "."}),
                            output: Some("src/main.rs\nsrc/lib.rs\nsrc/config.rs\nsrc/processor.rs".to_string()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("glob-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::File(FilePart {
                            filename: "src/main.rs".to_string(),
                            mime: "text/x-rust".to_string(),
                        }),
                        Part::File(FilePart {
                            filename: "src/lib.rs".to_string(),
                            mime: "text/x-rust".to_string(),
                        }),
                    ],
                    created_at: 2000,
                },
                Message {
                    id: "msg-3".to_string(),
                    role: MessageRole::User,
                    agent: Some("editor".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "Let me read the current main.rs to understand the structure.".to_string(),
                        synthetic: false,
                    })],
                    created_at: 3000,
                },
                Message {
                    id: "msg-4".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![
                        Part::Reasoning(ReasoningPart {
                            text: "The user wants a CLI tool for JSON processing. I should check what dependencies are available and look at the current code structure before suggesting changes.".to_string(),
                            collapsed: true,
                        }),
                        Part::Tool(ToolPart {
                            tool: "read".to_string(),
                            input: serde_json::json!({"filePath": "src/main.rs"}),
                            output: Some("fn main() {\n    println!(\"Hello, world!\");\n}".to_string()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("read-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "grep".to_string(),
                            input: serde_json::json!({"pattern": "serde|clap|json", "path": "Cargo.toml"}),
                            output: Some(String::new()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("grep-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::Text(TextPart {
                            text: "I can see you have a basic Rust project. Let me add **clap** for CLI argument parsing and **serde_json** for JSON processing.".to_string(),
                            synthetic: false,
                        }),
                    ],
                    created_at: 4000,
                },
                Message {
                    id: "msg-5".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![
                        Part::Text(TextPart {
                            text: "First, let me add the dependencies to Cargo.toml:".to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "write".to_string(),
                            input: serde_json::json!({"filePath": "src/main.rs", "content": "use clap::Parser;\nuse serde_json::Value;\n\n#[derive(Parser)]\nstruct Args {\n    file: String,\n    #[arg(short, long)]\n    pretty: bool,\n}\n\nfn main() {\n    let args = Args::parse();\n    let content = std::fs::read_to_string(&args.file).unwrap();\n    let json: Value = serde_json::from_str(&content).unwrap();\n    if args.pretty {\n        println!(\"{}\", serde_json::to_string_pretty(&json).unwrap());\n    } else {\n        println!(\"{}\", content);\n    }\n}".to_string()}),
                            output: Some("fn main() {\n    println!(\"Hello, world!\");\n}".to_string()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("write-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::Text(TextPart {
                            text: "Now let me also add the Cargo.toml changes:".to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "edit".to_string(),
                            input: serde_json::json!({"filePath": "Cargo.toml", "oldString": "[dependencies]", "newString": "[dependencies]\nclap = { version = \"4\", features = [\"derive\"] }\nserde_json = \"1\""}),
                            output: Some("--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,2 +1,4 @@\n [dependencies]\n+clap = { version = \"4\", features = [\"derive\"] }\n+serde_json = \"1\"".to_string()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("edit-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                    ],
                    created_at: 5000,
                },
                Message {
                    id: "msg-6".to_string(),
                    role: MessageRole::User,
                    agent: Some("build".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "That looks great! Can you also add error handling with proper messages?".to_string(),
                        synthetic: false,
                    })],
                    created_at: 6000,
                },
                Message {
                    id: "msg-7".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![
                        Part::Reasoning(ReasoningPart {
                            text: "The user wants better error handling. I should use anyhow or create proper error messages with context. Let me update the main.rs to use expect() with meaningful messages.".to_string(),
                            collapsed: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "shell".to_string(),
                            input: serde_json::json!({"command": "cargo check 2>&1"}),
                            output: Some("    Checking json-processor v0.1.0\n    Finished dev profile [unoptimized + debuginfo] target(s) in 0.32s".to_string()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("shell-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::Text(TextPart {
                            text: "The project compiles! Let me now add proper error handling:".to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "edit".to_string(),
                            input: serde_json::json!({"filePath": "src/main.rs", "oldString": "fn main() {\n    let args = Args::parse();\n    let content = std::fs::read_to_string(&args.file).unwrap();\n    let json: Value = serde_json::from_str(&content).unwrap();\n    if args.pretty {\n        println!(\"{}\", serde_json::to_string_pretty(&json).unwrap());\n    } else {\n        println!(\"{}\", content);\n    }\n}", "newString": "fn main() {\n    let args = Args::parse();\n    let content = std::fs::read_to_string(&args.file)\n        .expect(\"failed to read input file\");\n    let json: Value = serde_json::from_str(&content)\n        .expect(\"input is not valid JSON\");\n    if args.pretty {\n        println!(\"{}\", serde_json::to_string_pretty(&json).unwrap());\n    } else {\n        println!(\"{}\", content);\n    }\n}"}),
                            output: Some(String::new()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("edit-2".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "websearch".to_string(),
                            input: serde_json::json!({"query": "rust clap CLI tutorial 2025", "provider": "parallel"}),
                            output: None,
                            status: ToolStatus::Completed,
                            tool_call_id: Some("web-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                        }),
                    ],
                    created_at: 7000,
                },
            ],
        };
        self.sessions.push(session);
        self.current_session_id = Some("demo-1".to_string());
        self.working_directory = "~/cosh".to_string();
        self.connected = true;
        self.lsp_count = 2;
        self.mcp_count = 3;
        self.mcp_errors = 0;
        self.permission_count = 1;
    }
}
