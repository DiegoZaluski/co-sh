use crate::namespace_cache::{CacheData, NamespaceCache, Verification};
use crate::summarizer;

use cosh_sdk::connector::Connector;
use rmcp::ServiceExt;
use rmcp::model::Tool;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

const CACHE_FILE: &str = "cache-namespace-tools.toml";

pub struct NamespaceTool {
    pub name: String,
    pub description: Vec<String>,
    pub tools: Vec<Tool>,
}

pub struct ServerSession {
    pub name_server: String,
    pub tools: Vec<Tool>,
    pub client: RunningService<RoleClient, ()>,
}
pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct InternalTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub struct Harness {
    connector: Connector,
    sessions: Vec<ServerSession>,
    live_cache: HashMap<String, HashMap<String, CacheData>>,
    protocol: Option<String>, // add fallback ?
    header_context: String,
    system_prompts: Vec<PromptSystem>,
    expanded_namespaces: HashSet<(String, String)>,
    internal_tools: Vec<InternalTool>,

    #[allow(dead_code)]
    /// To stop the agent loop.
    stop: bool,
}

impl Harness {
    #[must_use]
    pub fn new(connector: Connector) -> Self {
        // --- Tool System ---
        let expand_namespace = InternalTool {
            name: "expand_namespace".into(),
            description: "Expand a namespace to see every available MCP tool inside it with their original descriptions and schemas. Call this when the summary alone is not enough to understand what the namespace offers.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Server name containing the namespace"
                    },
                    "namespace": {
                        "type": "string",
                        "description": "Namespace to expand"
                    }
                },
                "required": ["server", "namespace"]
            }),
        };

        // --- Harness Setup ---
        Self {
            connector,
            sessions: Vec::new(),
            live_cache: HashMap::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            expanded_namespaces: HashSet::new(),
            internal_tools: vec![expand_namespace],
            stop: false,
        }
    }

    pub async fn connect(
        &mut self,
        server: &str,
        protocol: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match protocol {
            "stdio" => {
                let mut cmd = tokio::process::Command::new("npx");
                cmd.arg("-y").arg(server);
                let transport = TokioChildProcess::new(cmd)?;
                ().serve(transport).await?
            }

            "http" => {
                let transport = StreamableHttpClientTransport::from_uri(server);
                ().serve(transport).await?
            }

            other => return Err(format!("unsupported protocol: {other}").into()),
        };

        let name_server = client
            .peer_info()
            .map(|i| i.server_info.name.clone())
            .unwrap_or_default();

        let tools = client.list_all_tools().await?;

        self.sessions.push(ServerSession {
            name_server,
            tools,
            client,
        });
        Ok(())
    }

    // Builds the full content for each (server, namespace) by concatenating
    // every tool's description + input_schema. The string is used as the hash
    // seed — any change to any tool in the group invalidates the namespace.
    pub fn build_cache_map(&self) -> HashMap<(String, String), String> {
        let mut out = HashMap::new();
        for session in &self.sessions {
            // Group tool parts by namespace within this server
            let mut ns_parts: HashMap<String, Vec<String>> = HashMap::new();
            for tool in &session.tools {
                let ns = tool.name.split('.').next().unwrap().to_string(); // ATTENTION HERE
                let desc = tool.description.as_deref().unwrap_or_default();
                // input_schema is Arc<JsonObject> — serialize to include in hash
                let schema = serde_json::to_string(&*tool.input_schema).unwrap_or_default();
                ns_parts
                    .entry(ns)
                    .or_default()
                    .push(format!("{desc}\n{schema}"));
            }

            // Flatten each namespace group into one content blob
            for (ns, parts) in ns_parts {
                out.insert((session.name_server.clone(), ns), parts.join("\n---\n"));
            }
        }
        out
    }

    pub fn resolve_cache(&mut self) -> &HashMap<String, HashMap<String, CacheData>> {
        let cache_map = self.build_cache_map();
        let mut cache = NamespaceCache::new().with_cache_file(CACHE_FILE);

        for ((server, ns), content) in &cache_map {
            cache.set_context(server, ns, content);
            if cache.verify() == Verification::Modified {
                // send content to LLM to summarize
                let summary = summarizer::summarize_namespace(&self.connector, server, ns, content);
                cache.mark_modified(summary);
            }
        }

        cache.flush().expect("failed to flush cache");
        self.live_cache = cache.cached().clone();
        &self.live_cache
    }

    pub fn set_protocol(&mut self, protocol: impl Into<Option<String>>) -> &mut Self {
        self.protocol = protocol.into();
        self
    }

    #[must_use]
    pub fn with_system_prompt(mut self, title: impl Into<String>, text: impl Into<String>) -> Self {
        self.system_prompts.push(PromptSystem {
            title: title.into(),
            text: text.into(),
        });
        self
    }

    /// Builds the system header for the LLM.
    ///
    /// Concatenates system prompts, internal harness tools (always expanded),
    /// and MCP namespace summaries from the live cache. Namespaces that were
    /// previously marked as expanded via `expand_namespace` show the original
    /// tool descriptions from the MCP sessions instead of the cached summary.
    ///
    /// Clears the expanded set after formatting so the next loop starts fresh.
    ///
    /// # Panics
    ///
    /// Panics if a tool name does not contain a `.` — the harness assumes
    /// MCP tools use a `namespace.name` convention internally.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
        }

        let _ = write!(out, "## Internal Tools\n\n");
        for tool in &self.internal_tools {
            let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
            let _ = write!(
                out,
                "### {}\n{}\nSchema: {}\n\n",
                tool.name, tool.description, schema,
            );
        }

        // Index tools by (server, namespace) for O(1) expanded lookup
        let mut tools_by_ns: HashMap<(&str, &str), Vec<&Tool>> = HashMap::new();
        for session in &self.sessions {
            for tool in &session.tools {
                let ns = tool.name.split('.').next().unwrap();
                tools_by_ns
                    .entry((session.name_server.as_str(), ns))
                    .or_default() // ???!
                    .push(tool);
            }
        }

        let _ = write!(out, "## Connected Servers\n\n");
        for (server, namespaces) in &self.live_cache {
            for (ns, data) in namespaces {
                if self
                    .expanded_namespaces
                    .contains(&(server.clone(), ns.clone()))
                {
                    if let Some(tools) = tools_by_ns.get(&(server.as_str(), ns.as_str())) {
                        for tool in tools {
                            let desc = tool.description.as_deref().unwrap_or_default();
                            let schema = serde_json::to_string_pretty(&tool.input_schema) // *
                                .unwrap_or_default();
                            let _ = write!(
                                out,
                                "- **{}**: {}\n  Schema: {}\n",
                                tool.name, desc, schema,
                            );
                        }
                    }
                } else {
                    let _ = writeln!(out, "- **{}.{}**: {}", server, ns, data.description);
                }
            }
        }

        // Intentional: expanded state is consumed after render.
        //
        // Caller must call expand_namespace() again for next format.
        self.expanded_namespaces.clear();
        self.header_context = out;
        &self.header_context
    }

    /// Send a chat completion and return the full response as a single string.
    ///
    /// Use this when streaming is not enabled — the model's reply is collected
    /// entirely and returned as `Result<String, String>`.
    pub async fn chat(&mut self, input: &str) -> Result<String, String> {
        self.connector
            .chat_with_system(input, &self.header_context)
            .await
            .map_err(|e| e.to_string())
            .map(|out| out.message().to_string())
    }

    /// Stream a chat completion, calling `on_token` with each text delta.
    ///
    /// Use this when streaming is enabled — tokens are delivered in real time
    /// via the callback. Returns `Ok("done".into())` when the stream finishes.
    pub async fn stream_chat(
        &mut self,
        input: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<String, String> {
        let mut stream = self
            .connector
            .stream_chat_with_system(input, &self.header_context)
            .await
            .map_err(|e| e.to_string())?;

        use tokio_stream::StreamExt;
        while let Some(content) = stream.next().await {
            let chunk = content.map_err(|err| err.to_string())?;
            on_token(chunk.token());
        }

        Ok("done".into())
    }
}

#[cfg(test)]
impl Harness {
    pub(crate) fn push_session(&mut self, session: ServerSession) {
        self.sessions.push(session);
    }

    pub(crate) fn set_live_cache(&mut self, cache: HashMap<String, HashMap<String, CacheData>>) {
        self.live_cache = cache;
    }

    pub(crate) fn expand_namespace(&mut self, server: &str, ns: &str) {
        self.expanded_namespaces
            .insert((server.to_string(), ns.to_string()));
    }
}
