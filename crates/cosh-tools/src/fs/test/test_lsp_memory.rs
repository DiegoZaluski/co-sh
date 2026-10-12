//! Diagnóstico fs+LSP: o que fixa memória no servidor a cada operação.
//!
//! * **opencode**: abre sob demanda no toque explícito; sem `didSave`
//!   automático (sem `cargo check` implícito a cada edição).
//! * **cosh**: `Fs::read` aquece (`touch_file` → didOpen), o `global_lsp` não
//!   sobe servidores no startup (lazy) e o `Fs::write`/`edit` passivo nunca
//!   manda `didSave` — sem flycheck/`cargo check` implícito (subprocesso +
//!   GBs de RAM + lock em `target/`).
//!
//! Fakes em memória, sem binário real.

use std::sync::Arc;

use cosh_sdk::lsp::test_support::{auto_respond, spawn_fake_server};
use cosh_sdk::lsp::{
    ClientFactory, DiagnosticsEngine, LanguageServer, Manager, ManagerConfig, ServerSpec,
};
use serde_json::json;

use crate::fs::Fs;
use crate::fs::types::{Target, TargetFile};
use crate::lsp::Lsp;

fn fake_factory() -> ClientFactory {
    Arc::new(move |config| {
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(64 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            let report = json!({ "kind": "full", "items": [] });
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

fn lsp_for(dir: &std::path::Path) -> Lsp {
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
            workspace_aware: false,
        }],
        fake_factory(),
    );
    Lsp::with_manager(Arc::new(manager), Arc::new(DiagnosticsEngine::new()))
}

/// Um `read` passivo já fixa o documento no servidor.
///
/// O agente lê dezenas de arquivos explorando o repo; cada leitura vira um
/// `didOpen` com o texto integral. No rust-analyzer real, cada doc aberto
/// expande a análise em memória — aqui o fake prova o acoplamento.
#[tokio::test]
async fn read_warms_lsp_and_pins_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.fake"), "hello\n").unwrap();

    let lsp = Arc::new(lsp_for(dir.path()));
    let fs = Fs::new().cwd(dir.path()).with_lsp(Arc::clone(&lsp));

    let results = fs
        .read(vec![Target {
            path: "a.fake".to_owned(),
            line: None,
            symbol: None,
            line_range: None,
            offset: None,
            limit: None,
        }])
        .await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].content.is_empty());

    // O servidor agora tem o documento aberto — custo de memória fixado por
    // uma leitura que no opencode não abriria nada sozinha.
    let clients = lsp.manager().running_clients();
    assert_eq!(clients.len(), 1, "servidor subiu no read (warm)");
    assert_eq!(
        clients[0].open_documents().len(),
        1,
        "diagnóstico: read fixou 1 documento no servidor"
    );
}

/// Um `write` fixa o documento (touch) e NUNCA dispara `didSave`.
///
/// O caminho passivo observa diagnósticos via pull/push sobre
/// `didOpen`/`didChange` — sem flycheck/`cargo check` implícito a cada
/// edição, como no opencode.
#[tokio::test]
async fn write_pins_document_without_save() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("b.fake"), "").unwrap();

    let lsp = Arc::new(lsp_for(dir.path()));
    let fs = Fs::new().cwd(dir.path()).with_lsp(Arc::clone(&lsp));

    fs.write(vec![TargetFile {
        text: "hello\n".to_owned(),
        path: "b.fake".to_owned(),
        file_hash: None,
    }])
    .await
    .unwrap();

    let clients = lsp.manager().running_clients();
    assert_eq!(clients.len(), 1);
    let path = dir.path().join("b.fake");
    assert!(
        clients[0].open_documents().contains(&path),
        "diagnóstico: write fixou o documento no servidor"
    );
}
