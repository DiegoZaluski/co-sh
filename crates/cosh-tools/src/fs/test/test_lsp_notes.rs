//! Passive LSP feedback: the `with_lsp` toggle, the per-call guards, and the
//! structured notes attached to mutating fs results — driven over the SDK's
//! in-memory fake servers (same harness as the `lsp` tool tests).

use std::sync::Arc;

use cosh_sdk::lsp::test_support::{auto_respond, spawn_fake_server};
use cosh_sdk::lsp::{ClientFactory, DiagnosticsEngine, LanguageServer, Manager, ManagerConfig, ServerSpec};
use serde_json::json;

use crate::fs::Fs;
use crate::fs::types::TargetFile;
use crate::lsp::Lsp;

fn fake_factory() -> ClientFactory {
    Arc::new(move |config| {
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(32 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            // The pull-diagnostics answer the fs passive feedback consumes.
            let report = json!({
                "kind": "full",
                "items": [
                    {
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                        "severity": 1,
                        "source": "fake",
                        "message": "boom"
                    },
                    {
                        "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                        "severity": 2,
                        "source": "fake",
                        "message": "meh"
                    }
                ]
            });
            let table = vec![
                ("initialize".to_owned(), json!({ "capabilities": {} })),
                ("textDocument/diagnostic".to_owned(), report),
                ("shutdown".to_owned(), serde_json::Value::Null),
            ];
            tokio::spawn(auto_respond(server, table));
            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(std::time::Duration::from_secs(5)).await?;
            Ok(client)
        })
    })
}

/// Workspace with a `.fake` file; the fake server answers diagnostics
/// pulls with one error (`boom`) and one warning (`meh`), so the fs
/// passive feedback populates the engine through the real pull path.
fn lsp_with_findings(dir: &std::path::Path, _file: &std::path::Path) -> Lsp {
    let mut config = ManagerConfig::new(dir.to_path_buf());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![ServerSpec {
            name: "fake",
            command: "unused",
            args: &[],
            extensions: &[".fake"],
            filenames: &[],
            root_markers: &[],
        }],
        fake_factory(),
    );

    Lsp::with_manager(Arc::new(manager), Arc::new(DiagnosticsEngine::new()))
}

fn target(path: &str) -> TargetFile {
    TargetFile {
        text: "fn target() {}\n".to_owned(),
        path: path.to_owned(),
        file_hash: None,
    }
}

#[tokio::test]
async fn write_attaches_error_notes_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.fake");
    std::fs::write(&file, "").unwrap();

    let lsp = Arc::new(lsp_with_findings(dir.path(), &file));
    let fs = Fs::new().cwd(dir.path()).with_lsp(Arc::clone(&lsp));

    let results = fs.write(vec![target("sample.fake")]).await.unwrap();
    assert_eq!(results.len(), 1);
    let notes = results[0].lsp_notes.as_ref().expect("notes attached");
    assert_eq!(notes.errors.len(), 1);
    assert_eq!(notes.errors[0].message, "boom");
    assert_eq!(notes.errors[0].path, "sample.fake");
    assert_eq!(notes.errors[0].line, 1);
    assert!(notes.warnings.is_empty(), "warnings need an explicit opt-in");
}

#[tokio::test]
async fn warnings_guard_opts_in_and_without_lsp_suppresses() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.fake");
    std::fs::write(&file, "").unwrap();

    let lsp = Arc::new(lsp_with_findings(dir.path(), &file));
    let fs = Fs::new().cwd(dir.path()).with_lsp(Arc::clone(&lsp));

    // Opt-in: errors AND warnings.
    let results = fs.warnings().write(vec![target("sample.fake")]).await.unwrap();
    let notes = results[0].lsp_notes.as_ref().unwrap();
    assert_eq!(notes.errors.len(), 1);
    assert_eq!(notes.warnings.len(), 1);
    assert_eq!(notes.warnings[0].message, "meh");

    // Suppression: the guard only affects the chained call...
    let results = fs.without_lsp().write(vec![target("sample.fake")]).await.unwrap();
    assert!(results[0].lsp_notes.is_none());

    // ...and the default policy is back on the next call.
    let results = fs.write(vec![target("sample.fake")]).await.unwrap();
    let notes = results[0].lsp_notes.as_ref().expect("toggle still on");
    assert_eq!(notes.errors.len(), 1);
    assert!(notes.warnings.is_empty());
}

#[tokio::test]
async fn without_a_handle_fs_behaves_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.txt"), "").unwrap();

    let fs = Fs::new().cwd(dir.path());
    let results = fs.write(vec![target("plain.txt")]).await.unwrap();
    assert!(results[0].lsp_notes.is_none());
}
