//! Vector search on a `LanceDB` table via [`cosh_recall::embed::VecDb`].

use super::types::{RecallEntry, RecallOutput, RecallSearchInput};

/// Search a `LanceDB` vector store for entries semantically similar to the
/// given query vector.
///
/// This function:
/// 1. Connects to the `LanceDB` database at the specified URI.
/// 2. Opens the named table (read-only).
/// 3. Performs an ANN (approximate nearest neighbour) search using the
///    pre-computed query vector.
/// 4. Returns the top `limit` entries with their ids and content.
///
/// # Errors
///
/// Returns `Err` if:
/// - The `LanceDB` connection fails.
/// - The query vector dimension does not match the table schema.
/// - The search itself fails.
pub async fn search(input: &RecallSearchInput) -> Result<RecallOutput, String> {
    let limit = input.limit.unwrap_or(5);

    let db = cosh_recall::embed::VecDb::connect_readonly(&input.db_uri, &input.table_name)
        .await
        .map_err(|e| format!("failed to connect to vector db: {e}"))?;

    // Validate that the provided vector_dim matches the schema.
    // The actual dimension is inferred from the table schema; the input
    // value is metadata for the caller (e.g. to know what model to use).
    if input.vector_dim != db.vector_dim() {
        return Err(format!(
            "vector dimension mismatch: input has {}, but table schema has {}",
            input.vector_dim,
            db.vector_dim()
        ));
    }

    let entries = db
        .get(&input.query_vector, limit)
        .await
        .map_err(|e| format!("vector search failed: {e}"))?;

    let results: Vec<RecallEntry> = entries
        .into_iter()
        .map(|e| RecallEntry {
            id: e.id,
            content: e.content,
        })
        .collect();

    Ok(RecallOutput {
        query: input.query.clone(),
        results,
    })
}
