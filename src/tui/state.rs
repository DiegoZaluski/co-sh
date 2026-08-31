use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use lru::LruCache;

use crate::routes::session::right_panel::types::RightPanelState;
use crate::session_store::SessionSummary;
use crate::types::{
    FilePart, Message, MessageRole, Part, ReasoningPart, Session, SessionStatus, TextPart,
    ToolPart, ToolStatus,
};
use cosh::harness::Mode;

const SESSION_CACHE_SIZE: usize = 10;

/// In-memory pending user messages for one session, waiting to be delivered.
///
/// - [`Self::next_request`] messages enter the NEXT REQUEST of the currently
///   running agent loop (the "next request" queue).
/// - [`Self::next_loop`] messages wait for the current loop to end and then
///   start a fresh loop (the "next agent loop" queue).
///
/// Deliberately kept out of the persisted session model — this is transient
/// UI state that lives for the app's lifetime only.
#[derive(Debug, Clone, Default)]
pub struct PendingQueues {
    /// FIFO of messages for the next request of the running loop.
    pub next_request: VecDeque<String>,
    /// FIFO of messages for the next agent loop.
    pub next_loop: VecDeque<String>,
}

impl PendingQueues {
    /// Total number of queued messages across both queues.
    pub fn queued_count(&self) -> usize {
        self.next_request.len() + self.next_loop.len()
    }

    /// All queued messages in FIFO order: next_request first, then next_loop.
    pub fn queued_list(&self) -> impl Iterator<Item = &str> {
        self.next_request
            .iter()
            .chain(self.next_loop.iter())
            .map(String::as_str)
    }

    /// Clear all queued messages from both queues.
    pub fn clear(&mut self) {
        self.next_request.clear();
        self.next_loop.clear();
    }
}

pub struct AppState {
    /// Header-only session summaries (always in RAM, sidebar uses these).
    pub session_summaries: Vec<SessionSummary>,
    /// Full session cache (LRU eviction, only recently accessed sessions stored).
    pub session_cache: LruCache<String, Session>,
    /// Pending queued messages per session id (in-memory only).
    pub pending_queues: HashMap<String, PendingQueues>,
    pub current_session_id: Option<String>,
    pub status: SessionStatus,
    pub scroll_y: i32,
    pub content_height: i32,
    pub working_directory: String,
    pub mode: Mode,
    pub connected: bool,
    pub lsp_count: usize,
    pub mcp_count: usize,
    pub mcp_errors: usize,
    pub permission_count: usize,
    /// Right panel state (TODOs, PTY sessions).
    pub right_panel: RightPanelState,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session_summaries: vec![],
            session_cache: LruCache::new(NonZeroUsize::new(SESSION_CACHE_SIZE).unwrap()),
            pending_queues: HashMap::new(),
            current_session_id: None,
            status: SessionStatus::Idle,
            scroll_y: 0,
            content_height: 0,
            working_directory: String::new(),
            mode: Mode::Build,
            connected: false,
            lsp_count: 0,
            mcp_count: 0,
            mcp_errors: 0,
            permission_count: 0,
            right_panel: RightPanelState::new(),
        }
    }

    /// Pending queues for the current session (if one is selected).
    pub fn current_pending_queues(&self) -> Option<&PendingQueues> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.pending_queues.get(id))
    }

    /// Mutable pending queues for the current session (creating an empty
    /// entry when missing). `None` when no session is selected.
    pub fn current_pending_queues_mut(&mut self) -> Option<&mut PendingQueues> {
        let id = self.current_session_id.clone()?;
        Some(self.pending_queues.entry(id).or_default())
    }

    /// Remove a session by ID. Clears `current_session_id` if it matches.
    pub fn remove_session(&mut self, session_id: &str) {
        self.session_summaries
            .retain(|s| s.session_id != session_id);
        self.session_cache.pop(session_id);
        self.pending_queues.remove(session_id);
        if self
            .current_session_id
            .as_deref()
            .is_some_and(|id| id == session_id)
        {
            self.current_session_id = None;
        }
    }

    /// Add a full session (with its messages) and a matching summary.
    pub fn add_session(&mut self, session: Session) {
        let summary = SessionSummary {
            session_id: session.id.clone(),
            title: session.title.clone(),
            created_at: session.created_at,
            message_count: session.messages.len(),
            cwd: self.working_directory.clone(),
            model: session.model.clone(),
            title_generated: session.title_generated,
        };
        self.session_summaries.push(summary);
        self.pending_queues.entry(session.id.clone()).or_default();
        self.session_cache.put(session.id.clone(), session);
    }

    /// Add a placeholder empty session to the cache. No summary is created yet —
    /// the session only appears in the sidebar once it has been persisted (i.e.,
    /// it has valid content and has been saved to disk).
    pub fn add_empty_session(&mut self, id: String, title: String, created_at: u64) {
        self.pending_queues.entry(id.clone()).or_default();
        self.session_cache.put(
            id.clone(),
            Session {
                id: id.clone(),
                title,
                created_at,
                messages: vec![],
                title_generated: false,
                provider: None,
                model: None,
                reasoning: None,
                ctx_ids: Default::default(),
            },
        );
    }

    /// Ensure a summary exists in `session_summaries` for the session with the
    /// given id. If it already exists, this is a no-op.
    pub fn ensure_session_summary(&mut self, session_id: &str) {
        if self
            .session_summaries
            .iter()
            .any(|s| s.session_id == session_id)
        {
            return;
        }
        if let Some(session) = self.session_cache.peek(session_id) {
            let summary = SessionSummary {
                session_id: session.id.clone(),
                title: session.title.clone(),
                created_at: session.created_at,
                message_count: session.messages.len(),
                cwd: self.working_directory.clone(),
                model: session.model.clone(),
                title_generated: session.title_generated,
            };
            self.session_summaries.push(summary);
        }
    }

    /// Ensure a session is in the cache. If not, attempts to load from the
    /// provided store. Returns `true` if the session is available after the call.
    pub fn ensure_session_cached(
        &mut self,
        session_id: &str,
        store: &crate::session_store::SessionStore,
    ) -> bool {
        if self.session_cache.contains(session_id) {
            return true;
        }
        if let Some(session) = store.load_session(session_id) {
            self.session_cache.put(session_id.to_string(), session);
            true
        } else {
            false
        }
    }

    /// Returns the current session if one is selected and in cache.
    /// Uses `peek` to avoid mutating LRU order on reads — the current session
    /// is kept alive by `current_session_mut()` and `switch_to_session()`.
    pub fn current_session(&self) -> Option<&Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.session_cache.peek(id.as_str()))
    }

    /// Returns a mutable reference to the current session if one is selected
    /// and in cache.
    pub fn current_session_mut(&mut self) -> Option<&mut Session> {
        let id = self.current_session_id.clone()?;
        self.session_cache.get_mut(&id)
    }

    /// Swap the current session: save one, load another. The old session is
    /// upserted into the cache (so subsequent switches are fast).
    pub fn switch_to_session(
        &mut self,
        session_id: String,
        store: &crate::session_store::SessionStore,
    ) {
        // Save current session to disk only if it has real content
        let old_id = self.current_session_id.clone();
        if let Some(ref oid) = old_id {
            let should_save = self
                .session_cache
                .get(oid)
                .is_some_and(crate::session_store::is_valid_session);
            if should_save {
                // Reborrow to avoid borrow conflict with ensure_session_summary
                if let Some(session) = self.session_cache.get(oid) {
                    store.save_session_async(session);
                }
                self.ensure_session_summary(oid);
            }
        }

        // Ensure target is cached
        self.ensure_session_cached(&session_id, store);
        self.current_session_id = Some(session_id);
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

    /// Update the message count in the summary for the current session.
    /// Called after a new message is appended during streaming.
    pub fn update_summary_msg_count(&mut self) {
        if let Some(id) = self.current_session_id.as_ref()
            && let Some(session) = self.session_cache.get(id.as_str())
        {
            let count = session.messages.len();
            if let Some(summary) = self
                .session_summaries
                .iter_mut()
                .find(|s| s.session_id == *id)
            {
                summary.message_count = count;
            }
        }
    }

    pub fn add_demo_data(&mut self) {
        let session = Session {
            id: "demo-1".to_string(),
            title: "Demo Session".to_string(),
            created_at: 1000,
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            ctx_ids: Default::default(),
            messages: vec![
                Message {
                    id: "msg-1".to_string(),
                    role: MessageRole::User,
                    agent: Some("build".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "Hi! Can you help me write a Rust CLI tool that processes JSON files?"
                            .to_string(),
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
                            text: "I'd be happy to help! Let me first **look at what files** you have in the project."
                                .to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "glob".to_string(),
                            input: serde_json::json!({"pattern": "**/*.rs", "path": "."}),
                            output: Some(
                                "src/main.rs\nsrc/lib.rs\nsrc/config.rs\nsrc/processor.rs"
                                    .to_string(),
                            ),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("glob-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
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
                        text: "Let me read the current main.rs to understand the structure."
                            .to_string(),
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
                            text: "The user wants a CLI tool for JSON processing. I should check what dependencies are available and look at the current code structure before suggesting changes."
                                .to_string(),
                            collapsed: true,
                        }),
                        Part::Tool(ToolPart {
                            tool: "read".to_string(),
                            input: serde_json::json!({"filePath": "src/main.rs"}),
                            output: Some(
                                "fn main() {\n    println!(\"Hello, world!\");\n}".to_string(),
                            ),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("read-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
                        }),
                        Part::Tool(ToolPart {
                            tool: "grep".to_string(),
                            input: serde_json::json!({"pattern": "serde|clap|json", "path": "Cargo.toml"}),
                            output: Some(String::new()),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("grep-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
                        }),
                        Part::Text(TextPart {
                            text: "I can see you have a basic Rust project. Let me add **clap** for CLI argument parsing and **serde_json** for JSON processing."
                                .to_string(),
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
                            output: Some(
                                "fn main() {\n    println!(\"Hello, world!\");\n}".to_string(),
                            ),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("write-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
                        }),
                        Part::Text(TextPart {
                            text: "Now let me also add the Cargo.toml changes:".to_string(),
                            synthetic: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "edit".to_string(),
                            input: serde_json::json!({"filePath": "Cargo.toml", "oldString": "[dependencies]", "newString": "[dependencies]\nclap = { version = \"4\", features = [\"derive\"] }\nserde_json = \"1\""}),
                            output: Some(
                                "--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,2 +1,4 @@\n [dependencies]\n+clap = { version = \"4\", features = [\"derive\"] }\n+serde_json = \"1\""
                                    .to_string(),
                            ),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("edit-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
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
                        text: "That looks great! Can you also add error handling with proper messages?"
                            .to_string(),
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
                            text: "The user wants better error handling. I should use anyhow or create proper error messages with context. Let me update the main.rs to use expect() with meaningful messages."
                                .to_string(),
                            collapsed: false,
                        }),
                        Part::Tool(ToolPart {
                            tool: "shell".to_string(),
                            input: serde_json::json!({"command": "cargo check 2>&1"}),
                            output: Some(
                                "    Checking json-processor v0.1.0\n    Finished dev profile [unoptimized + debuginfo] target(s) in 0.32s"
                                    .to_string(),
                            ),
                            status: ToolStatus::Completed,
                            tool_call_id: Some("shell-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
                        }),
                        Part::Text(TextPart {
                            text: "The project compiles! Let me now add proper error handling:"
                                .to_string(),
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
                            cached_line_count: None,
                        }),
                        Part::Tool(ToolPart {
                            tool: "websearch".to_string(),
                            input: serde_json::json!({"query": "rust clap CLI tutorial 2025", "provider": "parallel"}),
                            output: None,
                            status: ToolStatus::Completed,
                            tool_call_id: Some("web-1".to_string()),
                            is_start: false,
                            is_streaming: false,
                            cached_line_count: None,
                        }),
                    ],
                    created_at: 7000,
                },
            ],
        };
        self.add_session(session);
        self.current_session_id = Some("demo-1".to_string());
        self.working_directory = "~/cosh".to_string();
        self.connected = true;
        self.lsp_count = 2;
        self.mcp_count = 3;
        self.mcp_errors = 0;
        self.permission_count = 1;
    }
}
