//! The `load` path contract: facade call shape and error forwarding, without
//! a network or a checkpoint.

use cosh_onnx::decision::model::{DecisionModel, LoadOptions, ModelKind};
use cosh_onnx::error::Error;
use cosh_onnx::{Router, load};

/// The load error for a missing local path carries the upstream text and
/// names the path — the same message `agent.load` produces.
#[test]
fn a_missing_local_path_fails_with_the_upstream_text() {
    let err = load(
        ModelKind::Custom {
            repo: "/definitely/not/a/checkpoint".to_string(),
            subfolder: None,
        },
        &LoadOptions::default(),
    )
    .err()
    .expect("missing local path");
    match &err {
        Error::Runtime(msg) => {
            assert_eq!(
                msg,
                "Local model path not found: '/definitely/not/a/checkpoint'."
            );
        }
        other => panic!("expected Error::Runtime, got {other:?}"),
    }
}

/// A relative path that does not exist and is not a repo id also fails with
/// the local-path error: `./x` and `../x` are local shapes upstream.
#[test]
fn relative_local_shapes_are_local() {
    for repo in ["./no/such/dir", "../no/such/dir"] {
        let err = load(
            ModelKind::Custom {
                repo: repo.to_string(),
                subfolder: None,
            },
            &LoadOptions::default(),
        )
        .err()
        .expect("missing relative path");
        match &err {
            Error::Runtime(msg) => {
                assert!(
                    msg.starts_with("Local model path not found:"),
                    "{repo}: {msg}"
                );
            }
            other => panic!("expected Error::Runtime for {repo}, got {other:?}"),
        }
    }
}

/// A subfolder mismatch on a local directory fails with the upstream
/// "Subfolder ... not found" error, not the Hub path.
#[test]
fn a_missing_subfolder_names_the_directory() {
    // Create a minimal local checkpoint directory (no files needed: the
    // subfolder check runs before the config is read).
    let dir = std::env::temp_dir().join(format!("cosh-onnx-contract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let err = load(
        ModelKind::Custom {
            repo: dir.to_string_lossy().to_string(),
            subfolder: Some("missing".to_string()),
        },
        &LoadOptions::default(),
    )
    .err()
    .expect("missing subfolder");
    match &err {
        Error::Runtime(msg) => {
            assert!(
                msg.starts_with("Subfolder 'missing' not found in '"),
                "{msg}"
            );
        }
        other => panic!("expected Error::Runtime, got {other:?}"),
    }
    std::fs::remove_dir(&dir).expect("remove scratch dir");
}

/// The facade `load` is generic over where the model came from; the error it
/// returns is the shared [`Error`], so a caller matches one enum.
#[test]
fn errors_are_the_shared_laya_error() {
    fn assert_laya_error(e: Error) -> Error {
        e
    }
    let err = load(
        ModelKind::Custom {
            repo: "/no/such/dir".to_string(),
            subfolder: None,
        },
        &LoadOptions::default(),
    )
    .err()
    .unwrap();
    let _ = assert_laya_error(err);
}

/// The Router stays reachable from the facade import surface: constructing
/// one is offline (no checkpoint load), so the constructor contract holds.
#[test]
fn the_router_constructs_offline() {
    let router = Router::new().expect("Router::new is offline");
    assert_eq!(router.max_loaded(), 2, "upstream default max_loaded=2");
    assert_eq!(router.loaded().len(), 0, "lazy: nothing resident");
}

/// A `DecisionModel` handle can be cloned and moved across threads — the
/// `Send + Sync` contract the facade documents.
#[test]
fn the_model_trait_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<std::sync::Arc<dyn DecisionModel>>();
    assert_send_sync::<LoadOptions>();
}
