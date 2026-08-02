//! Tests that batching in deterministic_compression never causes the
//! output to be larger than the input.
//!
//! The concern is that when processing large inputs in batches,
//! each batch is compressed independently (40% MMR selection per batch),
//! but when the results are concatenated, the total might exceed the
//! expected compression ratio or even grow larger than the original.

use crate::harness::context_manager::{ContextManager, Role};
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

/// Test a realistic edge case where batching could cause growth:
///
/// Scenario: Technical content with highly unique vocabulary per batch.
/// - Each batch contains distinct terminology (e.g., different API endpoints,
///   different error codes, different variable names)
/// - TF-IDF won't filter these as "stop words" since they're batch-local unique
/// - MMR selects 40% of each batch independently
/// - When batches are concatenated, the total may retain >40% of original content
/// - In worst case, if batches have completely disjoint vocabularies,
///   the concatenated result could approach 100% of original (no compression)
///
/// The compression is exercised through `run()` — the same per-iteration
/// context tick the agent loop calls — which drains the over-budget fresh
/// buffer through the batched deterministic pipeline.
#[tokio::test]
async fn batching_with_disjoint_vocabulary_can_exceed_expected_compression() {
    let mut cm = ContextManager::new(test_connector(), 3);

    // Create content where each batch has completely different vocabulary
    // Simulating different sections of a technical document:
    // Batch 1: Database terms (SQL, tables, indexes)
    // Batch 2: API terms (endpoints, HTTP, JSON)
    // Batch 3: Frontend terms (React, components, state)

    let mut text = String::new();

    // Batch 1: Database terminology (400+ unique terms)
    let db_terms = [
        "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "TABLE",
        "INDEX", "PRIMARY", "FOREIGN", "KEY", "CONSTRAINT", "TRIGGER", "VIEW", "PROCEDURE",
        "FUNCTION", "TRANSACTION", "COMMIT", "ROLLBACK", "GRANT", "REVOKE", "SCHEMA",
        "DATABASE", "COLUMN", "ROW", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER",
        "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET", "UNION", "INTERSECT",
        "EXCEPT", "DISTINCT", "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "NULL",
        "ISNULL", "NOTNULL", "DEFAULT", "CHECK", "UNIQUE", "REFERENCES", "CASCADE",
        "RESTRICT", "SET", "NULL", "AUTO_INCREMENT", "SERIAL", "IDENTITY", "BIGINT",
        "INTEGER", "SMALLINT", "TINYINT", "DECIMAL", "NUMERIC", "FLOAT", "DOUBLE",
        "VARCHAR", "CHAR", "TEXT", "BLOB", "DATE", "TIME", "TIMESTAMP", "DATETIME",
        "BOOLEAN", "ENUM", "SET", "GEOMETRY", "POINT", "LINESTRING", "POLYGON",
        "MULTIPOINT", "MULTILINESTRING", "MULTIPOLYGON", "GEOMETRYCOLLECTION",
        "SPATIAL", "INDEX", "FULLTEXT", "HASH", "BTREE", "RTREE", "CLUSTERED",
        "NONCLUSTERED", "PARTITION", "RANGE", "LIST", "HASH", "KEY", "SUBPARTITION",
    ];

    for i in 0..300 {
        let term = db_terms[i % db_terms.len()];
        text.push_str(&format!("{} operation on table_{} with index_{} ", term, i, i % 20));
    }

    // Batch 2: API/HTTP terminology (400+ unique terms)
    let api_terms = [
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
        "HTTP", "HTTPS", "HTTP/1.1", "HTTP/2", "WebSocket", "REST", "GraphQL", "SOAP",
        "JSON", "XML", "YAML", "CSV", "FormData", "Multipart", "Base64", "URL-encoded",
        "Authorization", "Bearer", "Basic", "OAuth", "JWT", "Session", "Cookie", "Header",
        "Content-Type", "Accept", "User-Agent", "Referer", "Origin", "Host", "Connection",
        "Cache-Control", "Expires", "ETag", "Last-Modified", "If-Modified-Since",
        "If-None-Match", "If-Match", "If-Unmodified-Since", "Range", "Content-Range",
        "Content-Length", "Content-Encoding", "Transfer-Encoding", "TE", "Trailer",
        "Upgrade", "Proxy-Authorization", "Proxy-Connection", "Max-Forwards", "Via",
        "Warning", "WWW-Authenticate", "Proxy-Authenticate", "Retry-After", "Server",
        "Allow", "Location", "Content-Disposition", "Status", "StatusCode", "ReasonPhrase",
        "Request", "Response", "Endpoint", "Route", "Middleware", "Controller", "Service",
        "Repository", "DTO", "DAO", "VO", "POJO", "Model", "View", "ViewModel", "Binding",
    ];

    for i in 0..300 {
        let term = api_terms[i % api_terms.len()];
        text.push_str(&format!("{} request to /api/v{}/resource_{} with {} ", term, i % 5, i,
            if i % 2 == 0 { "JSON" } else { "XML" }));
    }

    // Batch 3: Frontend/JavaScript terminology (400+ unique terms)
    let js_terms = [
        "const", "let", "var", "function", "arrow", "async", "await", "Promise", "callback",
        "then", "catch", "finally", "throw", "try", "catch", "class", "extends", "constructor",
        "this", "super", "static", "prototype", "instanceof", "typeof", "instanceof", "void",
        "null", "undefined", "true", "false", "NaN", "Infinity", "Object", "Array", "String",
        "Number", "Boolean", "Date", "RegExp", "Map", "Set", "WeakMap", "WeakSet", "Symbol",
        "BigInt", "Proxy", "Reflect", "JSON", "Math", "console", "window", "document", "DOM",
        "HTML", "CSS", "XPath", "localStorage", "sessionStorage", "IndexedDB", "Cache",
        "ServiceWorker", "WebWorker", "WebSocket", "EventSource", "Fetch", "XHR", "AJAX",
        "React", "Vue", "Angular", "Svelte", "Component", "Props", "State", "Hook", "Effect",
        "Context", "Reducer", "Action", "Dispatch", "Selector", "Memo", "Callback", "Ref",
    ];

    for i in 0..300 {
        let term = js_terms[i % js_terms.len()];
        text.push_str(&format!("{} in component_{} with hook_{} and state_{} ", term, i, i % 15, i % 10));
    }

    let input_tokens = crate::util::token_counter::estimate_tokens(&text);

    // Ensure we're above the batching threshold (200 chunks * ~200 chars = ~40k chars)
    assert!(input_tokens > 10_000,
        "Input must be large enough to trigger batching (got {} tokens)", input_tokens);

    // Split into fresh chunks to simulate real usage
    let chunk_size = text.len() / 5;
    for i in 0..5 {
        let start = i * chunk_size;
        let end = if i == 4 { text.len() } else { (i + 1) * chunk_size };
        cm.add_buffer_context(Role::assistant(&text[start..end]));
    }

    let before_tokens = cm.display_info().total_tokens;

    // The agent loop calls run() after every tool dispatch; an over-budget
    // fresh buffer is drained through the batched deterministic pipeline.
    cm.run().await;

    let after_tokens = cm.display_info().total_tokens;

    // CRITICAL ASSERTION: The compressed result should NEVER be larger than the input
    // This is the core invariant of compression
    assert!(
        after_tokens <= before_tokens,
        "CRITICAL BUG: Batching caused content growth! Input: {} tokens, Output: {} tokens. \
         This proves that independent batch processing with .collect() can create results \
         larger than the original input.",
        before_tokens, after_tokens
    );

    // Additional sanity check: we should achieve meaningful compression
    // With 40% MMR ratio, we expect at least some reduction
    let compression_ratio = after_tokens as f64 / before_tokens as f64;
    assert!(
        compression_ratio < 0.9,  // Allow some tolerance, but should compress significantly
        "Compression ratio {} is too high (should be < 0.9). Batching may be ineffective.",
        compression_ratio
    );

    println!("Batching test: {} tokens → {} tokens (ratio: {:.2})",
             before_tokens, after_tokens, compression_ratio);

    // The drain must produce exactly one compressed queue entry
    assert_eq!(cm.queue.len(), 1, "Should have one compressed entry");
    assert!(cm.queue[0].tokens < before_tokens, "Compressed entry should be smaller than input");
}
