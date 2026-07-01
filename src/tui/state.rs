use crate::types::*;

pub struct AppState {
    pub sessions: Vec<Session>,
    pub current_session_id: Option<String>,
    pub status: SessionStatus,
    pub scroll_y: i32,
    pub content_height: i32,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            sessions: vec![],
            current_session_id: None,
            status: SessionStatus::Idle,
            scroll_y: 0,
            content_height: 0,
        }
    }

    pub fn current_session(&self) -> Option<&Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| s.id == *id))
    }

    pub fn max_scroll(&self) -> i32 {
        (self.content_height - 10).max(0)
    }

    pub fn unique_agents(&self) -> Vec<String> {
        let mut seen = Vec::new();
        if let Some(session) = self.current_session() {
            for msg in &session.messages {
                if let Some(agent) = &msg.agent
                    && !seen.contains(agent) {
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
                        text: "Hi! Can you help me with a Rust borrow checker issue?".to_string(),
                        synthetic: false,
                    })],
                    created_at: 1000,
                },
                Message {
                    id: "msg-2".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![Part::Text(TextPart {
                        text: "Of course! I'd be happy to help with your Rust borrow checker problem. What's the specific issue you're running into?".to_string(),
                        synthetic: false,
                    })],
                    created_at: 2000,
                },
                Message {
                    id: "msg-3".to_string(),
                    role: MessageRole::User,
                    agent: Some("build".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "I have a struct that holds a reference and I can't make the compiler happy with the lifetimes.".to_string(),
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
                        Part::Text(TextPart {
                            text: "Let me help you with that! Borrow checker issues with references usually need explicit lifetime annotations. Here's an example of how to properly structure it:".to_string(),
                            synthetic: false,
                        }),
                        Part::Text(TextPart {
                            text: "struct Container<'a> {\n    data: &'a str,\n}\n\nimpl<'a> Container<'a> {\n    fn new(data: &'a str) -> Self {\n        Container { data }\n    }\n}\n\nfn main() {\n    let s = String::from(\"hello\");\n    let c = Container::new(&s);\n    println!(\"{}\", c.data);\n}".to_string(),
                            synthetic: false,
                        }),
                    ],
                    created_at: 4000,
                },
                Message {
                    id: "msg-5".to_string(),
                    role: MessageRole::User,
                    agent: Some("build".to_string()),
                    model: None,
                    parts: vec![Part::Text(TextPart {
                        text: "That works! But what if I want to store a trait object instead of a direct reference?".to_string(),
                        synthetic: false,
                    })],
                    created_at: 5000,
                },
                Message {
                    id: "msg-6".to_string(),
                    role: MessageRole::Assistant,
                    agent: None,
                    model: Some("claude-3.5-sonnet".to_string()),
                    parts: vec![Part::Text(TextPart {
                        text: "For trait objects with references, use &dyn Trait with a lifetime:\n\ntrait Animal {\n    fn make_sound(&self) -> String;\n}\n\nstruct Zoo<'a> {\n    animals: Vec<&'a dyn Animal>,\n}\n\nOr use Box<dyn Trait> if you need owned trait objects.".to_string(),
                        synthetic: false,
                    })],
                    created_at: 6000,
                },
            ],
        };
        self.sessions.push(session);
        self.current_session_id = Some("demo-1".to_string());
    }
}
