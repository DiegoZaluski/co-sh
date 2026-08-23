//! Integration tests for the lsp tools, driven over in-memory fake servers
//! from the SDK's `lsp-test-support` harness.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cosh_sdk::lsp::lsp_types::{Diagnostic as LspDiagnostic, PublishDiagnosticsParams};
use cosh_sdk::lsp::test_support::{auto_respond, spawn_fake_server};
use cosh_sdk::lsp::{
    ClientFactory, DiagnosticsEngine, LanguageServer, Manager, ManagerConfig, ServerSpec,
};
use serde_json::json;

use crate::lsp::Lsp;
use crate::lsp::types::*;

// Fixtures

/// Factory standing up an autonomous fake server per spawn; `replies` are the
/// canned results for named requests and `capabilities` the raw JSON object
/// advertised during the initialize handshake.
fn factory_with(
    counter: Arc<AtomicUsize>,
    capabilities: &'static str,
    replies: Arc<Vec<(&'static str, serde_json::Value)>>,
) -> ClientFactory {
    let owned: Vec<(String, serde_json::Value)> = replies
        .iter()
        .map(|(m, v)| ((*m).to_owned(), v.clone()))
        .collect();
    let owned = Arc::new(owned);

    Arc::new(move |config| {
        counter.fetch_add(1, Ordering::SeqCst);
        let owned = Arc::clone(&owned);
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(32 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            // The handshake answer plus the caller's canned results.
            let caps: serde_json::Value =
                serde_json::from_str(capabilities).expect("capability json");
            let mut table = vec![
                ("initialize".to_owned(), json!({ "capabilities": caps })),
                ("shutdown".to_owned(), serde_json::Value::Null),
            ];
            for (method, result) in owned.iter() {
                table.push((method.clone(), result.clone()));
            }
            tokio::spawn(auto_respond(server, table));

            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(Duration::from_secs(5)).await?;
            Ok(client)
        })
    })
}

fn one_spec_manager(dir: &std::path::Path, factory: ClientFactory) -> Manager {
    let mut config = ManagerConfig::new(dir.to_path_buf());
    config.resolves_binaries = false;
    Manager::build(
        config,
        vec![ServerSpec {
            name: "fake",
            command: "unused",
            args: &[],
            extensions: &[".fake"],
            root_markers: &["marker.txt"],
        }],
        factory,
    )
}

/// Workspace with a marker + one `.fake` file containing a `target` symbol.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "m").unwrap();
    let file = dir.path().join("sample.fake");
    std::fs::write(&file, "fn target() {}\nlet x = target;\n").unwrap();
    (dir, file)
}

fn lsp_for(
    dir: &std::path::Path,
    capabilities: &'static str,
    replies: Vec<(&'static str, serde_json::Value)>,
) -> (Lsp, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = Arc::new(one_spec_manager(
        dir,
        factory_with(Arc::clone(&counter), capabilities, Arc::new(replies)),
    ));
    (
        Lsp::with_manager(manager, Arc::new(DiagnosticsEngine::new())),
        counter,
    )
}

fn location_entry(uri_str: &str, line: u32, character: u32) -> serde_json::Value {
    json!({
        "uri": uri_str,
        "range": {
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + 6 }
        }
    })
}

// Tests

#[tokio::test]
async fn diagnostics_reports_empty_when_clean_and_settled() {
    let (dir, file) = workspace();
    let (lsp, _counter) = lsp_for(dir.path(), "{}", vec![]);

    let out = lsp
        .diagnostics(&DiagnosticsInput {
            file_path: Some(file.display().to_string()),
            severity: None,
            max_items: None,
            settle_ms: Some(300),
        })
        .await
        .unwrap();

    assert_eq!(out.formatted, "");
    assert_eq!(out.count, 0);
    assert!(out.settled);
}

/// A diagnostic pushed into the store shows up in the tool output with the
/// severity filter applied.
#[tokio::test]
async fn diagnostics_render_pushed_entries_with_filter() {
    use cosh_sdk::lsp::{SeverityFilter, format_for_model};

    let (dir, file) = workspace();
    let (lsp, _counter) = lsp_for(dir.path(), "{}", vec![]);

    // Simulate a push arriving through the relay.
    let params = PublishDiagnosticsParams::new(
        cosh_sdk::lsp::uri_from_path(&file).unwrap(),
        vec![LspDiagnostic {
            range: cosh_sdk::lsp::lsp_types::Range {
                start: cosh_sdk::lsp::lsp_types::Position {
                    line: 0,
                    character: 3,
                },
                end: cosh_sdk::lsp::lsp_types::Position {
                    line: 0,
                    character: 9,
                },
            },
            severity: Some(cosh_sdk::lsp::lsp_types::DiagnosticSeverity::ERROR),
            code: Some(cosh_sdk::lsp::lsp_types::NumberOrString::Number(234)),
            code_description: None,
            source: Some("fake".into()),
            message: "borrow of moved value".into(),
            related_information: None,
            tags: None,
            data: None,
        }],
        None,
    );
    lsp.diagnostics_engine().ingest("fake", &params);

    let out = lsp
        .diagnostics(&DiagnosticsInput {
            file_path: Some(file.display().to_string()),
            severity: Some("errors".into()),
            max_items: None,
            settle_ms: Some(100),
        })
        .await
        .unwrap();

    assert_eq!(out.count, 1);
    assert!(out.formatted.contains("[error 1:4]"), "{}", out.formatted);
    assert!(out.formatted.contains("(234)"));
    assert!(out.formatted.contains("borrow of moved value"));

    // ErrorsOnly filter drops hints entirely.
    assert_eq!(
        format_for_model(&params.diagnostics, SeverityFilter::ErrorsOnly, 10),
        out.formatted
    );
}

#[tokio::test]
async fn definitions_resolve_via_symbol_addressing() {
    let (dir, file) = workspace();
    let definition_uri = format!("file://{}", file.display());
    const CAPS: &str = r#"{"definitionProvider":true}"#;
    let replies = vec![(
        "textDocument/definition",
        json!([location_entry(&definition_uri, 0, 3)]),
    )];
    let (lsp, _counter) = lsp_for(dir.path(), CAPS, replies);

    let out = lsp
        .definitions(&DefinitionsInput {
            location: LocationInput {
                file_path: file.display().to_string(),
                position: None,
                symbol: Some("target".into()),
            },
        })
        .await
        .unwrap();

    assert_eq!(out.definitions.len(), 1);
    assert_eq!(out.definitions[0].path, file.display().to_string());
    assert_eq!(
        out.definitions[0].line, 1,
        "wire lines are 0-based; output is 1-based"
    );
    assert_eq!(out.definitions[0].text, "fn target() {}");
    assert!(out.formatted.contains(":1:4"));
}

#[tokio::test]
async fn definitions_accept_explicit_position() {
    let (dir, file) = workspace();
    let replies = vec![("textDocument/definition", json!(null))];
    let (lsp, _counter) = lsp_for(dir.path(), r#"{"definitionProvider":true}"#, replies);

    // Position addressing on a word that exists; server answers null → empty.
    let out = lsp
        .definitions(&DefinitionsInput {
            location: LocationInput {
                file_path: file.display().to_string(),
                position: Some(Position1 {
                    line: 1,
                    character: 9,
                }),
                symbol: None,
            },
        })
        .await
        .unwrap();

    assert!(out.definitions.is_empty());
    assert_eq!(out.formatted, "");
}

#[tokio::test]
async fn references_group_by_file() {
    let (dir, file) = workspace();
    const CAPS: &str = r#"{"referencesProvider":true}"#;
    let replies = vec![(
        "textDocument/references",
        json!([
            location_entry("file:///w/a.fake", 1, 0),
            location_entry("file:///w/b.fake", 4, 2),
            location_entry("file:///w/b.fake", 8, 2),
        ]),
    )];
    let (lsp, _counter) = lsp_for(dir.path(), CAPS, replies);

    let out = lsp
        .references(&ReferencesInput {
            location: LocationInput {
                file_path: file.display().to_string(),
                position: Some(Position1 {
                    line: 2,
                    character: 4,
                }),
                symbol: None,
            },
            include_declaration: Some(false),
            max_items: None,
        })
        .await
        .unwrap();

    assert_eq!(out.total, 3);
    assert_eq!(out.files.len(), 2);
    assert_eq!(out.files[0].path, "/w/a.fake");
    assert_eq!(out.files[1].locations.len(), 2);
    assert!(out.formatted.contains("3 reference(s):"));
}

#[tokio::test]
async fn symbols_flatten_hierarchy_and_filter() {
    let (dir, file) = workspace();
    let tree = json!([
        {
            "name": "Impl",
            "kind": 11, // Interface-ish container in this fake
            "range": { "start": {"line":0,"character":0}, "end": {"line":9,"character":0} },
            "selectionRange": { "start": {"line":0,"character":0}, "end": {"line":0,"character":3} },
            "children": [
                {
                    "name": "run",
                    "kind": 6, // method
                    "range": { "start": {"line":1,"character":4}, "end": {"line":2,"character":0} },
                    "selectionRange": { "start": {"line":1,"character":4}, "end": {"line":1,"character":7} },
                    "children": null
                },
                {
                    "name": "other",
                    "kind": 6,
                    "range": { "start": {"line":3,"character":4}, "end": {"line":4,"character":0} },
                    "selectionRange": { "start": {"line":3,"character":4}, "end": {"line":3,"character":9} },
                    "children": null
                }
            ]
        }
    ]);
    let replies = vec![("textDocument/documentSymbol", tree)];
    let (lsp, _counter) = lsp_for(dir.path(), r#"{"documentSymbolProvider":true}"#, replies);

    let all = lsp
        .symbols(&SymbolsInput {
            file_path: file.display().to_string(),
            query: None,
            max_items: None,
        })
        .await
        .unwrap();
    assert_eq!(all.symbols.len(), 3);
    assert_eq!(all.symbols[0].name, "Impl");
    assert_eq!(
        all.symbols[0].kind, "interface",
        "kind 11 maps to interface"
    );

    let filtered = lsp
        .symbols(&SymbolsInput {
            file_path: file.display().to_string(),
            query: Some("RUN".into()),
            max_items: None,
        })
        .await
        .unwrap();
    assert_eq!(filtered.symbols.len(), 1);
    assert_eq!(filtered.symbols[0].name, "run");
    assert_eq!(filtered.symbols[0].container.as_deref(), Some("Impl"));
    assert!(filtered.formatted.contains("Impl::method run"));
}

#[tokio::test]
async fn restart_scoped_respawns_and_keeps_state_consistent() {
    let (dir, file) = workspace();
    let (lsp, counter) = lsp_for(dir.path(), "{}", vec![]);

    // Warm up.
    lsp.diagnostics(&DiagnosticsInput {
        file_path: Some(file.display().to_string()),
        severity: None,
        max_items: None,
        settle_ms: Some(100),
    })
    .await
    .unwrap();
    let spawns_after_first = counter.load(Ordering::SeqCst);
    assert_eq!(spawns_after_first, 1);

    let out = lsp
        .restart(&RestartInput {
            file_path: Some(file.display().to_string()),
        })
        .await
        .unwrap();
    assert_eq!(out.restarted, vec!["fake"]);

    // The warm re-spawn inside restart must have created a fresh client.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        spawns_after_first + 1,
        "scoped restart warms a fresh client"
    );
    assert!(!lsp.manager().states().is_empty());
}

#[tokio::test]
async fn unsupported_files_report_no_server_without_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "m").unwrap();
    let file = dir.path().join("nope.unknownext");
    std::fs::write(&file, "").unwrap();

    let (lsp, _counter) = lsp_for(dir.path(), "{}", vec![]);
    let out = lsp
        .diagnostics(&DiagnosticsInput {
            file_path: Some(file.display().to_string()),
            severity: None,
            max_items: None,
            settle_ms: None,
        })
        .await
        .unwrap_err();

    assert!(out.contains("no language server"), "{out}");
}
