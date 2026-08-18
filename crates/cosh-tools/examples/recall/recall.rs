//! Demonstrate `recall`: the `Recall` wrapper, description customization,
//! and a live vector search against a temporary LanceDB database.
//!
//! The `recall` module is compiled only with the `embed` feature, so this
//! example requires it:
//!
//! ```bash
//! cargo run --example recall --features embed
//! ```

use cosh_tools::recall::{Recall, RecallSearchInput};

#[tokio::main]
async fn main() {
    // ── 1. The tool schema ----------------------------------------------------
    let recall = Recall::new();
    let schema = &recall.description_search["inputSchema"];
    println!("== 1. recall_search schema ==");
    println!(
        "  name: {}, required: {:?}",
        recall.description_search["name"].as_str().unwrap(),
        schema["required"].as_array().unwrap()
    );
    println!(
        "  limit default: {}\n",
        schema["properties"]["limit"]["default"]
    );

    // ── 2. Description customization -------------------------------------------
    println!("== 2. with_description / rebuild_description ==");
    let default = Recall::new().description_search["description"]
        .as_str()
        .unwrap()
        .to_string();
    let customized =
        Recall::new().with_description("Active databases: docs (semantic code search).");
    let custom = customized.description_search["description"]
        .as_str()
        .unwrap();
    println!(
        "  with_description appends after the default: {}",
        custom.starts_with(&default) && custom.contains("semantic code search")
    );
    let mut rebuilt = Recall::new();
    rebuilt.rebuild_description("This knowledge base covers Rust and WebAssembly.");
    println!(
        "  rebuild_description replaces the suffix: {}",
        rebuilt.description_search["description"]
            .as_str()
            .unwrap()
            .contains("Rust and WebAssembly")
    );
    rebuilt.rebuild_description("");
    println!(
        "  rebuild_description(\"\") reverts to default: {}\n",
        rebuilt.description_search == Recall::new().description_search
    );

    // ── 3. Live search against a temporary LanceDB database ---------------------
    let dir = std::env::temp_dir().join("cosh-recall-example");
    let _ = std::fs::remove_dir_all(&dir);

    // Create the table (dimension 3) and insert a few hand-embedded entries.
    let db = cosh_recall::embed::VecDb::connect(&dir.to_string_lossy(), "docs", 3)
        .await
        .expect("create temp db");
    db.post(
        "rust-systems",
        "Rust is a systems programming language focused on safety",
        vec![1.0, 0.0, 0.0],
    )
    .await
    .unwrap();
    db.post(
        "rust-async",
        "Rust's async runtime powers high-performance web servers",
        vec![0.9, 0.2, 0.0],
    )
    .await
    .unwrap();
    db.post(
        "python-web",
        "Python web frameworks like Django and Flask",
        vec![0.0, 1.0, 0.0],
    )
    .await
    .unwrap();

    // Search with a pre-computed query vector close to the Rust entries.
    let out = recall
        .search(&RecallSearchInput {
            db_uri: dir.to_string_lossy().into_owned(),
            table_name: "docs".into(),
            vector_dim: 3,
            query: "rust concurrency".into(),
            query_vector: vec![0.8, 0.15, 0.0],
            limit: Some(3),
        })
        .await
        .unwrap();
    println!("== 3. live search (query vector near the Rust entries) ==");
    println!("  query: {}", out.query);
    for entry in &out.results {
        println!("  - {}: {}", entry.id, entry.content);
    }
    println!();

    // ── 4. Error paths -----------------------------------------------------------
    let err = recall
        .search(&RecallSearchInput {
            db_uri: dir.to_string_lossy().into_owned(),
            table_name: "docs".into(),
            vector_dim: 5, // wrong dimension vs. the table schema (3)
            query: "x".into(),
            query_vector: vec![0.1, 0.2, 0.3, 0.4, 0.5],
            limit: None,
        })
        .await
        .unwrap_err();
    println!("== 4a. dimension mismatch ==");
    println!("  {err}\n");

    let err = recall
        .search(&RecallSearchInput {
            db_uri: dir.to_string_lossy().into_owned(),
            table_name: "nope".into(), // table does not exist
            vector_dim: 3,
            query: "x".into(),
            query_vector: vec![0.1, 0.2, 0.3],
            limit: None,
        })
        .await
        .unwrap_err();
    println!("== 4b. missing table ==");
    println!("  {err}\n");

    std::fs::remove_dir_all(&dir).ok();
}
