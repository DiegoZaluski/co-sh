use crate::namespace_cache::{CacheData, NamespaceCache, Verification};
use crate::summarizer;

use cosh_sdk::connector::Connector;
use rmcp::ServiceExt;
use rmcp::model::Tool;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::HashMap;

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

pub struct Harness {
    #[allow(dead_code)]
    provider: Connector,
    sessions: Vec<ServerSession>,
    live_cache: HashMap<String, HashMap<String, CacheData>>,
    stream: bool,
    protocol: Option<String>, // add fallback
    #[allow(dead_code)]
    header_context: String,
}

impl Harness {
    #[must_use]
    pub fn new(provider: Connector) -> Self {
        Self {
            provider,
            sessions: Vec::new(),
            live_cache: HashMap::new(),
            stream: false,
            protocol: None,
            header_context: String::new(),
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
                let summary = summarizer::summarize_namespace(server, ns, content);
                cache.mark_modified(summary);
            }
        }

        cache.flush().expect("failed to flush cache");
        self.live_cache = cache.cached().clone();
        &self.live_cache
    }

    pub fn set_stream(&mut self, stream: bool) -> &mut Self {
        self.stream = stream;
        self
    }

    pub fn set_protocol(&mut self, protocol: impl Into<Option<String>>) -> &mut Self {
        self.protocol = protocol.into();
        self
    }
}
