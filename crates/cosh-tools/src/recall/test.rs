//! Unit tests for the recall module.
//!
//! Tests focus on:
//! - Tool description shape and content
//! - Input/output type serialization
//! - Edge cases (empty query, zero limit, dimension mismatch)

use super::{Recall, RecallEntry, RecallOutput, RecallSearchInput};

// Tool description

#[test]
fn recall_new_sets_search_description_name() {
    let recall = Recall::new();
    let desc = &recall.description_search;
    assert_eq!(desc["name"], "recall_search");
}

#[test]
fn recall_search_description_has_input_schema() {
    let recall = Recall::new();
    let schema = &recall.description_search["inputSchema"];
    assert!(schema.is_object());
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"].is_object());
}

#[test]
fn recall_search_description_requires_fields() {
    let recall = Recall::new();
    let required = recall.description_search["inputSchema"]["required"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"db_uri"));
    assert!(names.contains(&"table_name"));
    assert!(names.contains(&"vector_dim"));
    assert!(names.contains(&"query"));
    assert!(names.contains(&"query_vector"));
}

#[test]
fn recall_search_description_has_limit_as_optional() {
    let recall = Recall::new();
    let required = recall.description_search["inputSchema"]["required"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(!names.contains(&"limit"));
}

#[test]
fn recall_default_equals_new() {
    let a = Recall::new();
    let b: Recall = Default::default();
    assert_eq!(a.description_search, b.description_search);
}

#[test]
fn recall_with_description_appends_suffix() {
    let default = Recall::new();
    let customized =
        Recall::new().with_description("This knowledge base covers Rust and WebAssembly.");

    let default_desc = default.description_search["description"].as_str().unwrap();
    let custom_desc = customized.description_search["description"]
        .as_str()
        .unwrap();

    assert!(custom_desc.starts_with(default_desc));
    assert!(custom_desc.contains("Rust and WebAssembly"));
    assert!(custom_desc.len() > default_desc.len());
}

#[test]
fn recall_with_description_empty_suffix_is_noop() {
    let a = Recall::new();
    let b = Recall::new().with_description("");
    assert_eq!(a.description_search, b.description_search);
}

#[test]
fn recall_with_description_preserves_name_and_schema() {
    let customized = Recall::new().with_description("Custom context for testing.");

    assert_eq!(customized.description_search["name"], "recall_search");

    let required = customized.description_search["inputSchema"]["required"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"db_uri"));
}

// Input validation — structural (search is async, integration test only)
//
#[test]
fn recall_search_input_serde_roundtrip() {
    let json = serde_json::json!({
        "db_uri": "/tmp/test_db",
        "table_name": "docs",
        "vector_dim": 384,
        "query": "test query",
        "query_vector": [0.1, 0.2, 0.3],
        "limit": 10
    });

    let input: RecallSearchInput = serde_json::from_value(json).unwrap();
    assert_eq!(input.db_uri, "/tmp/test_db");
    assert_eq!(input.table_name, "docs");
    assert_eq!(input.vector_dim, 384);
    assert_eq!(input.query, "test query");
    assert_eq!(input.query_vector, vec![0.1_f32, 0.2, 0.3]);
    assert_eq!(input.limit, Some(10));
}

#[test]
fn recall_search_input_limit_defaults_to_none() {
    let json = serde_json::json!({
        "db_uri": "/tmp/test_db",
        "table_name": "docs",
        "vector_dim": 384,
        "query": "test",
        "query_vector": [0.1, 0.2, 0.3]
    });

    let input: RecallSearchInput = serde_json::from_value(json).unwrap();
    assert!(input.limit.is_none());
}

#[test]
fn recall_output_serialization() {
    let output = RecallOutput {
        query: "test".into(),
        results: vec![],
        total: 0,
    };

    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(json["query"], "test");
    assert!(json["results"].as_array().unwrap().is_empty());
    assert_eq!(json["total"], 0);
}

#[test]
fn recall_output_with_results() {
    let output = RecallOutput {
        query: "rust".into(),
        results: vec![RecallEntry {
            id: "doc-1".into(),
            content: "Rust is a systems language".into(),
        }],
        total: 1,
    };

    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(json["results"][0]["id"], "doc-1");
    assert_eq!(json["results"][0]["content"], "Rust is a systems language");
}
