use std::sync::OnceLock;

use super::rag::RagError;
use super::vec_db::VecDbError;

fn rt() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("failed to create tokio runtime"))
}

// ---------------------------------------------------------------------------
// RagError behaviour
// ---------------------------------------------------------------------------

#[test]
fn rag_error_display_database() {
    let err = RagError::Database(VecDbError::NotFound("x".into()));
    assert_eq!(err.to_string(), "database error: Entry not found: x");
}

#[test]
fn rag_error_display_embedding() {
    let err = RagError::Embedding("boom".into());
    assert_eq!(err.to_string(), "embedding error: boom");
}

#[test]
fn rag_error_display_lock_poisoned() {
    let err = RagError::LockPoisoned;
    assert_eq!(err.to_string(), "internal lock poisoned");
}

#[test]
fn rag_error_from_vec_db_error() {
    let vdb_err = VecDbError::InvalidId("bad".into());
    let rag_err: RagError = vdb_err.into();
    assert!(matches!(rag_err, RagError::Database(_)));
}

// ---------------------------------------------------------------------------
// Embedder — local (ONNX via fastembed)
// ---------------------------------------------------------------------------

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "requires downloading the model (~23 MB)"]
fn local_embedder_create_default_model() {
    let embedder = super::Embedder::try_new_local(Default::default()).unwrap();
    assert_eq!(embedder.dim(), 384);
}

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "requires downloading the model (~23 MB)"]
fn local_embedder_embed_single_text() {
    let embedder = super::Embedder::try_new_local(Default::default()).unwrap();
    let vecs = rt().block_on(embedder.embed(&["hello world"])).unwrap();
    assert_eq!(vecs.len(), 1);
    assert_eq!(vecs[0].len(), embedder.dim());
}

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "requires downloading the model (~23 MB)"]
fn local_embedder_embed_batch() {
    let embedder = super::Embedder::try_new_local(Default::default()).unwrap();
    let texts = &["first document", "second document", "third document"];
    let vecs = rt().block_on(embedder.embed(texts)).unwrap();
    assert_eq!(vecs.len(), 3);
    for v in &vecs {
        assert_eq!(v.len(), embedder.dim());
    }
}

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "requires downloading the model (~23 MB)"]
fn local_embedder_dim_for_all_minilm() {
    let embedder =
        super::Embedder::try_new_local(fastembed::EmbeddingModel::AllMiniLML6V2).unwrap();
    assert_eq!(embedder.dim(), 384);
}

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "requires downloading the model (~23 MB)"]
fn local_embedder_different_models_have_different_dims() {
    use fastembed::EmbeddingModel;
    let small = super::Embedder::try_new_local(EmbeddingModel::BGESmallENV15).unwrap();
    let base = super::Embedder::try_new_local(EmbeddingModel::BGEBaseENV15).unwrap();
    assert!(base.dim() > small.dim());
}

// ---------------------------------------------------------------------------
// Embedder — cloud (remote via cosh-sdk connector)
// ---------------------------------------------------------------------------

#[cfg(feature = "cloud")]
#[test]
fn cloud_embedder_create() {
    let connector = cosh_sdk::connector::Connector::new("openai").unwrap();
    let embedder = super::Embedder::new_cloud(connector, 1536);
    assert_eq!(embedder.dim(), 1536);
}

#[cfg(feature = "cloud")]
#[test]
fn cloud_embedder_dim_can_be_any_value() {
    let connector = cosh_sdk::connector::Connector::new("openai").unwrap();
    let embedder = super::Embedder::new_cloud(connector, 42);
    assert_eq!(embedder.dim(), 42);
}

#[cfg(feature = "cloud")]
#[test]
fn cloud_embedder_returns_error_without_api_key() {
    let connector = cosh_sdk::connector::Connector::new("openai").unwrap();
    let embedder = super::Embedder::new_cloud(connector, 1536);
    let result = rt().block_on(embedder.embed(&["test"]));
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// VecDb — LanceDB-backed vector store
// ---------------------------------------------------------------------------

#[cfg(feature = "lancedb")]
fn test_vec_db() -> (super::vec_db::VecDb, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = rt()
        .block_on(super::vec_db::VecDb::connect(
            dir.path().to_str().unwrap(),
            "test_table",
            4,
        ))
        .unwrap();
    (db, dir)
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_connect_creates_new_table() {
    let (db, _dir) = test_vec_db();
    let count = rt().block_on(db.entry_count()).unwrap();
    assert_eq!(count, 0);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_post_and_get_roundtrip() {
    let (db, _dir) = test_vec_db();
    let id = rt()
        .block_on(db.post("a", "hello", vec![1.0, 0.0, 0.0, 0.0]))
        .unwrap();
    assert_eq!(id, "a");

    let results = rt().block_on(db.get(&[1.0, 0.0, 0.0, 0.0], 10)).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "a");
    assert_eq!(results[0].content, "hello");
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_post_dedup_by_content() {
    let (db, _dir) = test_vec_db();
    let id1 = rt()
        .block_on(db.post("a", "same content", vec![1.0; 4]))
        .unwrap();
    let id2 = rt()
        .block_on(db.post("b", "same content", vec![2.0; 4]))
        .unwrap();
    assert_eq!(id1, id2);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_get_empty_when_no_match() {
    let (db, _dir) = test_vec_db();
    let results = rt().block_on(db.get(&[1.0, 0.0, 0.0, 0.0], 10)).unwrap();
    assert!(results.is_empty());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_get_respects_limit() {
    let (db, _dir) = test_vec_db();
    for i in 0..5 {
        let v = vec![i as f32; 4];
        rt().block_on(db.post(&i.to_string(), &i.to_string(), v))
            .unwrap();
    }
    let results = rt().block_on(db.get(&[1.0; 4], 3)).unwrap();
    assert_eq!(results.len(), 3);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_get_returns_empty_on_dim_mismatch() {
    let (db, _dir) = test_vec_db();
    let results = rt().block_on(db.get(&[1.0, 2.0, 3.0], 10)).unwrap();
    assert!(results.is_empty());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_delete_existing_entry() {
    let (db, _dir) = test_vec_db();
    rt().block_on(db.post("a", "hello", vec![1.0; 4])).unwrap();
    rt().block_on(db.delete("a")).unwrap();
    let count = rt().block_on(db.entry_count()).unwrap();
    assert_eq!(count, 0);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_delete_nonexistent_returns_error() {
    let (db, _dir) = test_vec_db();
    let result = rt().block_on(db.delete("missing"));
    assert!(result.is_err());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_entry_count_after_inserts() {
    let (db, _dir) = test_vec_db();
    assert_eq!(rt().block_on(db.entry_count()).unwrap(), 0);
    rt().block_on(db.post("a", "x", vec![1.0; 4])).unwrap();
    assert_eq!(rt().block_on(db.entry_count()).unwrap(), 1);
    rt().block_on(db.post("b", "y", vec![1.0; 4])).unwrap();
    assert_eq!(rt().block_on(db.entry_count()).unwrap(), 2);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_reject_invalid_id_characters() {
    let (db, _dir) = test_vec_db();
    let r = rt().block_on(db.post("bad;id", "x", vec![1.0; 4]));
    assert!(r.is_err());
    let r = rt().block_on(db.post("bad'id", "x", vec![1.0; 4]));
    assert!(r.is_err());
    let r = rt().block_on(db.post("bad\"id", "x", vec![1.0; 4]));
    assert!(r.is_err());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_reject_empty_id() {
    let (db, _dir) = test_vec_db();
    let r = rt().block_on(db.post("", "x", vec![1.0; 4]));
    assert!(r.is_err());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_reject_overlong_id() {
    let (db, _dir) = test_vec_db();
    let long_id = "a".repeat(101);
    let r = rt().block_on(db.post(&long_id, "x", vec![1.0; 4]));
    assert!(r.is_err());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_reject_dimension_mismatch() {
    let (db, _dir) = test_vec_db();
    let r = rt().block_on(db.post("a", "x", vec![1.0, 2.0, 3.0]));
    assert!(r.is_err());
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_patch_updates_entry() {
    let (db, _dir) = test_vec_db();
    rt().block_on(db.post("a", "original", vec![1.0; 4]))
        .unwrap();
    rt().block_on(db.patch("a", "updated", vec![2.0; 4]))
        .unwrap();
    let results = rt().block_on(db.get(&[2.0; 4], 10)).unwrap();
    assert_eq!(results[0].content, "updated");
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_exists_by_content_true() {
    let (db, _dir) = test_vec_db();
    rt().block_on(db.post("a", "unique text", vec![1.0; 4]))
        .unwrap();
    let exists = rt().block_on(db.exists_by_content("unique text")).unwrap();
    assert!(exists);
}

#[cfg(feature = "lancedb")]
#[test]
fn vec_db_exists_by_content_false() {
    let (db, _dir) = test_vec_db();
    let exists = rt().block_on(db.exists_by_content("nothing")).unwrap();
    assert!(!exists);
}

// ---------------------------------------------------------------------------
// Rag — integration (embedder + LanceDB)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
fn test_rag() -> (super::Rag, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let embedder = super::Embedder::try_new_local(Default::default()).unwrap();
    let rag = rt()
        .block_on(super::Rag::connect(
            dir.path().to_str().unwrap(),
            "rag_test",
            embedder,
        ))
        .unwrap();
    (rag, dir)
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_connect_creates_empty_index() {
    let (rag, _dir) = test_rag();
    let count = rt().block_on(rag.entry_count()).unwrap();
    assert_eq!(count, 0);
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_ingest_and_search_roundtrip() {
    let (rag, _dir) = test_rag();
    let id = rt()
        .block_on(rag.ingest("doc-1", "Rust is a systems programming language"))
        .unwrap();
    assert_eq!(id, "doc-1");

    let results = rt().block_on(rag.search("programming", 5)).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "doc-1");
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_ingest_returns_content_based_dedup() {
    let (rag, _dir) = test_rag();
    let id1 = rt()
        .block_on(rag.ingest("doc-1", "duplicate content"))
        .unwrap();
    let id2 = rt()
        .block_on(rag.ingest("doc-2", "duplicate content"))
        .unwrap();
    assert_eq!(id1, id2);
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_ingest_batch() {
    let (rag, _dir) = test_rag();
    let docs = vec![
        ("a".into(), "First document".into()),
        ("b".into(), "Second document".into()),
        ("c".into(), "Third document".into()),
    ];
    let ids = rt().block_on(rag.ingest_batch(&docs)).unwrap();
    assert_eq!(ids.len(), 3);

    let count = rt().block_on(rag.entry_count()).unwrap();
    assert_eq!(count, 3);
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_search_returns_relevant_results() {
    let (rag, _dir) = test_rag();
    rt().block_on(rag.ingest("cat", "Cats are furry animals that purr"))
        .unwrap();
    rt().block_on(rag.ingest("car", "Cars have four wheels and an engine"))
        .unwrap();

    let results = rt().block_on(rag.search("automobile", 5)).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "car");
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_search_respects_limit() {
    let (rag, _dir) = test_rag();
    for i in 0..10 {
        let id = format!("doc-{}", i);
        rt().block_on(rag.ingest(&id, &format!("Document number {}", i)))
            .unwrap();
    }
    let results = rt().block_on(rag.search("document", 3)).unwrap();
    assert_eq!(results.len(), 3);
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_delete_removes_entry() {
    let (rag, _dir) = test_rag();
    rt().block_on(rag.ingest("doc-1", "Something to delete"))
        .unwrap();
    rt().block_on(rag.delete("doc-1")).unwrap();
    let count = rt().block_on(rag.entry_count()).unwrap();
    assert_eq!(count, 0);
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_delete_nonexistent_returns_error() {
    let (rag, _dir) = test_rag();
    let result = rt().block_on(rag.delete("missing"));
    assert!(result.is_err());
}

#[cfg(all(feature = "lancedb", feature = "fastembed"))]
#[test]
#[ignore = "requires downloading the embedding model (~23 MB)"]
fn rag_entry_count_tracks_inserts_and_deletes() {
    let (rag, _dir) = test_rag();
    assert_eq!(rt().block_on(rag.entry_count()).unwrap(), 0);
    rt().block_on(rag.ingest("a", "One")).unwrap();
    assert_eq!(rt().block_on(rag.entry_count()).unwrap(), 1);
    rt().block_on(rag.ingest("b", "Two")).unwrap();
    assert_eq!(rt().block_on(rag.entry_count()).unwrap(), 2);
    rt().block_on(rag.delete("a")).unwrap();
    assert_eq!(rt().block_on(rag.entry_count()).unwrap(), 1);
}
