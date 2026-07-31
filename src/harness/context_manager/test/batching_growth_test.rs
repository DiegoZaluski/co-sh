//! Test edge cases where batching in deterministic_compression could
//! cause the output to be larger than the input.
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
#[test]
fn batching_with_disjoint_vocabulary_can_exceed_expected_compression() {
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
    
    // Compress all fresh chunks
    let last_checkpoint = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last_checkpoint);
    
    assert!(result.is_ok(), "Compression should succeed: {:?}", result);
    
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
    
    // Verify this actually triggered batching by checking the compressed result
    // If batching was triggered, we should have a compressed entry in the queue
    assert_eq!(cm.queue.len(), 1, "Should have one compressed entry");
    assert!(cm.queue[0].tokens < before_tokens, "Compressed entry should be smaller than input");
}

/// Test another edge case: very short sentences that don't compress well individually
/// 
/// Scenario: Content consisting of many very short, unique sentences.
/// - Each sentence is 3-5 words but completely unique
/// - TF-IDF sees each word as rare (not a stop word)
/// - MMR struggles to find redundancy when there's little content per chunk
/// - When batched, each batch keeps 40% of its minimal content
/// - Concatenation may not achieve meaningful compression
#[test]
fn batching_with_short_unique_sentences_ineffective_compression() {
    let mut cm = ContextManager::new(test_connector(), 3);
    
    let mut text = String::new();
    
    // Generate 400+ short, unique sentences
    // Each sentence is structurally similar but with unique words
    let subjects = ["User", "Admin", "System", "Client", "Server", "Agent", "Bot", "Proxy"];
    let verbs = ["created", "updated", "deleted", "accessed", "modified", "requested", "processed", "validated"];
    let objects = ["record", "file", "document", "resource", "endpoint", "session", "token", "configuration"];
    let contexts = ["successfully", "with errors", "after retry", "immediately", "later", "manually", "automatically", "via API"];
    
    for i in 0..500 {
        let subj = subjects[i % subjects.len()];
        let verb = verbs[i % verbs.len()];
        let obj = objects[i % objects.len()];
        let ctx = contexts[i % contexts.len()];
        // Add unique identifier to make each sentence truly unique
        text.push_str(&format!("{} {} {} {} [ID:{}] ", subj, verb, obj, ctx, i));
    }
    
    let input_tokens = crate::util::token_counter::estimate_tokens(&text);
    assert!(input_tokens > 5_000, "Input should be substantial (got {} tokens)", input_tokens);
    
    // Add as multiple fresh chunks
    let chunk_size = text.len() / 3;
    for i in 0..3 {
        let start = i * chunk_size;
        let end = if i == 2 { text.len() } else { (i + 1) * chunk_size };
        cm.add_buffer_context(Role::assistant(&text[start..end]));
    }
    
    let before_tokens = cm.display_info().total_tokens;
    
    let last_checkpoint = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last_checkpoint);
    
    assert!(result.is_ok(), "Compression should succeed: {:?}", result);
    
    let after_tokens = cm.display_info().total_tokens;
    
    // Even with poor compression characteristics, should never grow
    assert!(
        after_tokens <= before_tokens,
        "CRITICAL BUG: Even with poor compression, output should never exceed input. \
         Input: {} tokens, Output: {} tokens",
        before_tokens, after_tokens
    );
    
    println!("Short sentences test: {} tokens → {} tokens (ratio: {:.2})", 
             before_tokens, after_tokens, after_tokens as f64 / before_tokens as f64);
}

/// Test boundary condition: exactly at the batching threshold
#[test]
fn batching_at_threshold_boundary() {
    let mut cm = ContextManager::new(test_connector(), 3);
    
    // Create content that will result in exactly ~200 chunks (the threshold)
    // TextSplitter uses 200 character chunks, so we need ~40,000 characters
    let text = "A ".repeat(200 * 200); // 40,000 chars = ~200 chunks
    
    let _input_tokens = crate::util::token_counter::estimate_tokens(&text);
    
    cm.add_buffer_context(Role::assistant(&text));
    
    let before_tokens = cm.display_info().total_tokens;
    
    let last_checkpoint = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last_checkpoint);
    
    assert!(result.is_ok(), "Compression should succeed: {:?}", result);
    
    let after_tokens = cm.display_info().total_tokens;
    
    assert!(
        after_tokens <= before_tokens,
        "Even at threshold boundary, should not grow: {} → {} tokens",
        before_tokens, after_tokens
    );
    
    println!("Threshold boundary test: {} tokens → {} tokens", before_tokens, after_tokens);
}

/// Test to verify that the batching path is actually being exercised
/// by calling deterministic_compression directly with a large input
#[test]
fn verify_batching_path_is_exercised() {
    // Create content large enough to trigger batching (>10,000 tokens)
    let large_text = "A ".repeat(50_000); // ~50,000 chars
    
    let input_tokens = crate::util::token_counter::estimate_tokens(&large_text);
    assert!(input_tokens > 10_000, 
        "Input must be large enough to trigger batching (got {} tokens)", input_tokens);
    
    // Call deterministic_compression directly to test the batching logic
    // We need to make it accessible for testing, or test through compress_fresh_up_to
    
    // For now, test through the public interface
    let mut cm = ContextManager::new(test_connector(), 3);
    cm.add_buffer_context(Role::assistant(&large_text));
    
    let before_tokens = cm.display_info().total_tokens;
    
    let last_checkpoint = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last_checkpoint);
    
    assert!(result.is_ok(), "Compression should succeed: {:?}", result);
    
    let after_tokens = cm.display_info().total_tokens;
    
    // The key assertion: should never grow
    assert!(
        after_tokens <= before_tokens,
        "CRITICAL: Batching path caused growth! {} → {} tokens",
        before_tokens, after_tokens
    );
    
    // With 40% MMR ratio, we expect significant compression
    let compression_ratio = after_tokens as f64 / before_tokens as f64;
    println!("Direct batching test: {} tokens → {} tokens (ratio: {:.2})", 
             before_tokens, after_tokens, compression_ratio);
    
    // The ratio should be reasonably close to 0.4 (allowing for overhead)
    assert!(
        compression_ratio < 0.6,
        "Compression ratio {} is too high (expected ~0.4 with MMR_RATIO)",
        compression_ratio
    );
}

/// Extreme test: many small batches to test the .collect() worst case
/// 
/// If we have many batches and each keeps 40%, the total could theoretically
/// approach 100% if batches have completely disjoint content.
#[test]
fn extreme_many_batches_still_compressed() {
    let mut cm = ContextManager::new(test_connector(), 3);
    
    // Create content that will create many batches (>5 batches)
    // Each batch is 200 chunks, so we need >1000 chunks
    // TextSplitter uses 200 chars per chunk, so we need >200,000 chars
    let huge_text = "A ".repeat(250_000); // ~250,000 chars = ~1250 chunks = ~6+ batches
    
    let input_tokens = crate::util::token_counter::estimate_tokens(&huge_text);
    assert!(input_tokens > 50_000, 
        "Input must be huge to trigger multiple batches (got {} tokens)", input_tokens);
    
    cm.add_buffer_context(Role::assistant(&huge_text));
    
    let before_tokens = cm.display_info().total_tokens;
    
    let last_checkpoint = cm.buffer_chunks.last().map(|c| c.checkpoints).unwrap_or(0);
    let result = cm.compress_fresh_up_to(last_checkpoint);
    
    assert!(result.is_ok(), "Compression should succeed: {:?}", result);
    
    let after_tokens = cm.display_info().total_tokens;
    
    // Even with many batches, should never grow
    assert!(
        after_tokens <= before_tokens,
        "CRITICAL: Multiple batches caused growth! {} → {} tokens",
        before_tokens, after_tokens
    );
    
    let compression_ratio = after_tokens as f64 / before_tokens as f64;
    println!("Extreme multi-batch test: {} tokens → {} tokens (ratio: {:.2})", 
             before_tokens, after_tokens, compression_ratio);
    
    // Even with repeated content, should still achieve some compression
    assert!(
        compression_ratio < 0.8,
        "Even with extreme batching, ratio {} should be < 0.8",
        compression_ratio
    );
}