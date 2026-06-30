use super::super::core::{Harness, ServerSession};
use super::super::namespace_cache::CacheData;
use rmcp::model::Tool;
use rmcp::service::{RoleClient, serve_directly};
use rmcp::transport::async_rw::AsyncRwTransport;
use std::collections::HashMap;
use std::sync::Arc;

// ── Helpers ──────────────────────────────────────────────────────

fn make_harness() -> Harness {
    Harness::new_test()
}

fn mock_tool(name: &str, desc: &str) -> Tool {
    let mut tool = Tool::default();
    tool.name = name.to_string().into();
    tool.description = Some(desc.to_string().into());
    tool.input_schema = Arc::new(serde_json::Map::new());
    tool
}

fn mock_session(server: &str, tools: Vec<Tool>) -> ServerSession {
    let (io, _) = tokio::io::duplex(64);
    let (r, w) = tokio::io::split(io);
    let transport = AsyncRwTransport::<RoleClient, _, _>::new(r, w);
    let client = serve_directly((), transport, None);
    ServerSession {
        name_server: server.into(),
        tools,
        client,
    }
}

// ── Construction & builders ──────────────────────────────────────

#[test]
fn new_creates_harness() {
    let _ = make_harness();
    // Construction succeeded — no panic.
}

#[tokio::test]
async fn with_system_prompt_accumulates() {
    let mut h = make_harness()
        .with_system_prompt("Role", "You are a bot")
        .with_system_prompt("Scope", "You test the harness");

    let header = h.format_header_context();
    assert!(header.contains("## System: Role"));
    assert!(header.contains("## System: Scope"));
    assert!(header.contains("You are a bot"));
    assert!(header.contains("You test the harness"));
}

#[tokio::test]
async fn with_system_prompt_single() {
    let mut h = make_harness().with_system_prompt("Role", "Code reviewer");

    let header = h.format_header_context();
    assert!(header.contains("## System: Role"));
    assert!(header.contains("Code reviewer"));
}

#[tokio::test]
async fn set_stream_and_protocol() {
    let mut h = make_harness();
    h.set_protocol(Some("stdio".to_string()));
    h.set_protocol(Some("http".to_string()));
    h.set_protocol(None::<String>);
    // All builder methods accept their expected types — no panic.
}

// ── format_header_context — content ──────────────────────────────

#[tokio::test]
async fn format_header_includes_internal_tools_section() {
    let mut h = make_harness();
    let header = h.format_header_context();

    assert!(header.contains("## Internal Tools"));
    assert!(header.contains("expand_namespace"));
    assert!(header.contains("## Connected Servers"));
}

#[tokio::test]
async fn format_header_shows_cached_namespace() {
    let mut h = make_harness();
    let mut cache = HashMap::new();
    let mut ns_map = HashMap::new();
    ns_map.insert(
        "filesystem".into(),
        CacheData {
            hash: 42,
            description: "File system operations".into(),
        },
    );
    cache.insert("server-a".into(), ns_map);
    h.set_live_cache(cache);

    let header = h.format_header_context();
    assert!(header.contains("server-a.filesystem"));
    assert!(header.contains("File system operations"));
}

#[tokio::test]
async fn format_header_expanded_namespace_shows_original_tools() {
    let mut h = make_harness();

    let tools = vec![
        mock_tool("filesystem.read", "Read a file"),
        mock_tool("filesystem.write", "Write to a file"),
        mock_tool("database.query", "Run a query"),
    ];
    let session = mock_session("server-a", tools);
    h.push_session(session);

    // Simulate the cache having a summary for filesystem
    let mut cache = HashMap::new();
    let mut ns_map = HashMap::new();
    ns_map.insert(
        "filesystem".into(),
        CacheData {
            hash: 1,
            description: "File tools".into(),
        },
    );
    ns_map.insert(
        "database".into(),
        CacheData {
            hash: 2,
            description: "DB tools".into(),
        },
    );
    cache.insert("server-a".into(), ns_map);
    h.set_live_cache(cache);

    h.expand_namespace("server-a", "filesystem");

    let header = h.format_header_context();
    // Expanded — shows individual tools directly (no namespace summary line)
    assert!(!header.contains("File tools"));
    assert!(header.contains("filesystem.read"));
    assert!(header.contains("Read a file"));
    assert!(header.contains("filesystem.write"));
    assert!(header.contains("Write to a file"));

    // database namespace was NOT expanded — shows only the cached summary
    assert!(header.contains("server-a.database"));
    assert!(header.contains("DB tools"));
}

#[tokio::test]
async fn format_header_expanded_namespace_still_shows_other_summaries() {
    let mut h = make_harness();

    let tools = vec![
        mock_tool("alpha.open", "Open alpha"),
        mock_tool("beta.close", "Close beta"),
    ];
    let session = mock_session("server-x", tools);
    h.push_session(session);

    let mut cache = HashMap::new();
    let mut ns_map = HashMap::new();
    ns_map.insert(
        "alpha".into(),
        CacheData {
            hash: 10,
            description: "Alpha namespace".into(),
        },
    );
    ns_map.insert(
        "beta".into(),
        CacheData {
            hash: 20,
            description: "Beta namespace".into(),
        },
    );
    cache.insert("server-x".into(), ns_map);
    h.set_live_cache(cache);

    h.expand_namespace("server-x", "alpha");

    let header = h.format_header_context();
    // Expanded
    assert!(header.contains("alpha.open"));
    assert!(header.contains("Open alpha"));
    // Not expanded — still shows summary
    assert!(header.contains("Beta namespace"));
}

#[tokio::test]
async fn format_header_empty_live_cache_shows_only_sections() {
    let mut h = make_harness();
    let header = h.format_header_context();

    assert!(header.contains("## Internal Tools"));
    assert!(header.contains("## Connected Servers"));
    // No namespaces listed
}

// ── build_cache_map ──────────────────────────────────────────────

#[tokio::test]
async fn build_cache_map_groups_tools_by_namespace() {
    let mut h = make_harness();

    let tools = vec![
        mock_tool("filesystem.read", "desc"),
        mock_tool("filesystem.write", "desc"),
        mock_tool("database.query", "desc"),
    ];
    let session = mock_session("server-a", tools);
    h.push_session(session);

    let map = h.build_cache_map();
    assert_eq!(map.len(), 2);

    assert!(map.contains_key(&("server-a".into(), "filesystem".into())));
    assert!(map.contains_key(&("server-a".into(), "database".into())));
}

#[tokio::test]
async fn build_cache_map_multi_server() {
    let mut h = make_harness();

    let session_a = mock_session(
        "server-a",
        vec![mock_tool("fs.read", "r"), mock_tool("fs.write", "w")],
    );
    let session_b = mock_session("server-b", vec![mock_tool("db.query", "q")]);
    h.push_session(session_a);
    h.push_session(session_b);

    let map = h.build_cache_map();
    assert_eq!(map.len(), 2);
    assert!(map.contains_key(&("server-a".into(), "fs".into())));
    assert!(map.contains_key(&("server-b".into(), "db".into())));
}
