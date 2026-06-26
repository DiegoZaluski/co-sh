use crate::harness::{Harness, ServerSession};
use crate::namespace_cache::CacheData;
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

// ── Tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn namespaced_tools_are_summarized_dotless_tools_are_inline() {
    let mut h = make_harness();

    // Mix of namespaced and dotless tools on the same server
    let tools = vec![
        mock_tool("fs.read", "Read a file"),
        mock_tool("fs.write", "Write to a file"),
        mock_tool("fs.edit", "Edit a file"),
        mock_tool("read", "Read raw data"),
        mock_tool("write", "Write raw data"),
        mock_tool("edit", "Edit raw data"),
    ];
    let session = mock_session("server-mcp", tools);
    h.push_session(session);

    // Set a cached summary for the "fs" namespace only
    let mut cache = HashMap::new();
    let mut ns_map = HashMap::new();
    ns_map.insert(
        "fs".into(),
        CacheData {
            hash: 7,
            description: "File system read/write/edit operations".into(),
        },
    );
    cache.insert("server-mcp".into(), ns_map);
    h.set_live_cache(cache);

    let header = h.format_header_context();

    // ── Namespaced tools appear as a cached summary ──
    assert!(
        header.contains("server-mcp.fs"),
        "cache key should appear"
    );
    assert!(
        header.contains("File system read/write/edit operations"),
        "cached summary should appear"
    );

    // ── Dotless tools appear in ## Other Tools ──
    assert!(header.contains("## Other Tools"), "dotless section should exist");

    // Each dotless tool should be listed individually with its description
    assert!(header.contains("read"));
    assert!(header.contains("Read raw data"));
    assert!(header.contains("write"));
    assert!(header.contains("Write raw data"));
    assert!(header.contains("edit"));
    assert!(header.contains("Edit raw data"));

    // ── Dotless tools are NOT in the Connected Servers section ──
    // (they render after it, in Other Tools)
    let connected_pos = header.find("## Connected Servers").unwrap();
    let other_pos = header.find("## Other Tools").unwrap();
    assert!(
        other_pos > connected_pos,
        "Other Tools should come after Connected Servers"
    );

    // The fs namespace tools should NOT appear individually in Other Tools
    assert!(!header.contains("fs.read"));
    assert!(!header.contains("fs.write"));
    assert!(!header.contains("fs.edit"));
}

#[tokio::test]
async fn build_cache_map_skips_dotless_tools() {
    let mut h = make_harness();

    let tools = vec![
        mock_tool("fs.read", "desc"),
        mock_tool("read", "desc"),
        mock_tool("write", "desc"),
    ];
    let session = mock_session("srv", tools);
    h.push_session(session);

    let map = h.build_cache_map();

    // Only the namespaced tool has an entry
    assert_eq!(map.len(), 1, "dotless tools must be skipped");
    assert!(map.contains_key(&("srv".into(), "fs".into())));
}

#[tokio::test]
async fn only_dotless_tools_with_no_live_cache_still_shows_in_other_tools() {
    // Even with an empty live cache, dotless tools should render.
    let mut h = make_harness();

    let tools = vec![
        mock_tool("status", "Server status check"),
        mock_tool("ping", "Ping the server"),
    ];
    let session = mock_session("monitor", tools);
    h.push_session(session);

    let header = h.format_header_context();

    assert!(header.contains("## Other Tools"));
    assert!(header.contains("status"));
    assert!(header.contains("Server status check"));
    assert!(header.contains("ping"));
    assert!(header.contains("Ping the server"));
}

#[tokio::test]
async fn mixed_namespaces_and_dotless_across_multiple_servers() {
    let mut h = make_harness();

    let srv_a = mock_session(
        "srv-a",
        vec![
            mock_tool("db.query", "Query database"),
            mock_tool("log", "Print log"),
        ],
    );
    let srv_b = mock_session(
        "srv-b",
        vec![
            mock_tool("fs.read", "Read file"),
            mock_tool("alert", "Send alert"),
        ],
    );
    h.push_session(srv_a);
    h.push_session(srv_b);

    let mut cache = HashMap::new();
    let mut ns_a = HashMap::new();
    ns_a.insert(
        "db".into(),
        CacheData {
            hash: 1,
            description: "Database queries".into(),
        },
    );
    cache.insert("srv-a".into(), ns_a);

    let mut ns_b = HashMap::new();
    ns_b.insert(
        "fs".into(),
        CacheData {
            hash: 2,
            description: "File system tools".into(),
        },
    );
    cache.insert("srv-b".into(), ns_b);
    h.set_live_cache(cache);

    let header = h.format_header_context();

    // Namespaced tools summarized
    assert!(header.contains("srv-a.db"));
    assert!(header.contains("Database queries"));
    assert!(header.contains("srv-b.fs"));
    assert!(header.contains("File system tools"));

    // Dotless tools in Other Tools with their server context
    assert!(header.contains("## Other Tools"));
    assert!(header.contains("log"));
    assert!(header.contains("Print log"));
    assert!(header.contains("alert"));
    assert!(header.contains("Send alert"));
}
