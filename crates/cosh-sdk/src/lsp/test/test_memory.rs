//! Diagnóstico de memória LSP: por que o cosh com LSP gasta muito mais RAM
//! que o opencode com LSP.
//!
//! Estes testes documentam (e quantificam sem precisar de `rust-analyzer`
//! real) os três drivers de custo que separam os dois designs:
//!
//! * **opencode**: LSP desabilitado por omissão (`lsp` omitido → off),
//!   servidores sobem de forma preguiçosa no primeiro toque de arquivo por
//!   extensão, sem `didSave` automático (sem flycheck/`cargo check`
//!   implícito).
//! * **cosh (antes do fix)**: LSP ligado por omissão, `global_lsp` dispara
//!   `ensure_for_root` ansioso no startup (N processos antes de qualquer
//!   toque), cada `read` abre o documento no servidor (`touch_file`) e cada
//!   `write`/`edit` ainda manda `didSave` (gatilho do flycheck do
//!   rust-analyzer → subprocesso `cargo check` + reanálise do workspace
//!   inteiro), tudo com `initializationOptions` nulas (sem `cargo.targetDir`,
//!   sem limites).
//!
//! Cada teste quantifica sem precisar de `rust-analyzer` real (fakes em
//! memória — nenhum binário é gerado). O primeiro documenta o custo do modo
//! ansioso antigo; os demais travam o perfil enxuto pós-fix.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use crate::lsp::manager::{ClientFactory, Manager, ManagerConfig};
use crate::lsp::test_support::{auto_respond, spawn_fake_server};
use crate::lsp::{CATALOG, LanguageServer, LanguageServerConfig};
use serde_json::json;

/// Quantos servidores o startup ansioso subiria num workspace Rust típico.
///
/// Com `Cargo.toml` na raiz, o catálogo real casa **rust-analyzer + taplo**
/// (ambos listam `Cargo.toml` como root marker). O `global_lsp` antigo
/// chamava `ensure_for_root` no startup → 2 processos pesados antes de
/// qualquer arquivo ser tocado. O opencode subiria **0** aqui (lazy + off
/// por omissão).
#[tokio::test]
async fn rust_workspace_root_matches_multiple_servers_eager() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    let manager = Manager::new(dir.path().to_path_buf());
    let matched = manager.matches_for_root(dir.path());
    let names: Vec<&str> = matched.iter().map(|k| k.server.as_str()).collect();

    assert!(
        names.contains(&"rust-analyzer"),
        "esperava rust-analyzer no match de um workspace Rust, vi: {names:?}"
    );
    // Documenta o custo ansioso: mais de um servidor casa a mesma raiz.
    assert!(
        matched.len() >= 2,
        "diagnóstico: startup ansioso sobe {} servidores ({names:?}) onde o \
         opencode (lazy) subiria 0 — cada um é um processo pesado (rust-analyzer \
         indexa o workspace inteiro)",
        matched.len()
    );
    // Sanidade: o catálogo real não passa nenhum arg de contenção.
    let ra = CATALOG.iter().find(|s| s.name == "rust-analyzer").unwrap();
    assert!(
        ra.args.is_empty(),
        "rust-analyzer sem args de contenção no catálogo"
    );
}

/// A config de spawn carrega contenção de memória para rust-analyzer.
///
/// Regressão do perfil enxuto (estilo opencode): o manager injeta
/// `cargo.targetDir=true` nos dois canais de configuração
/// (`initializationOptions` + `settings`, pois cada servidor lê só um deles)
/// para o rust-analyzer não dividir `target/` com `cargo build`/`check`
/// (lock contention + árvore de artefatos duplicada em RAM, opencode#17377).
/// Outros servidores seguem com config nula — sem payload inventado.
#[tokio::test]
async fn spawn_config_carries_memory_mitigations_for_rust_analyzer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("main.rs");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let captured: Arc<Mutex<Vec<LanguageServerConfig>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    let factory: ClientFactory = Arc::new(move |config: LanguageServerConfig| {
        captured_clone
            .lock()
            .expect("capture lock")
            .push(config.clone());
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(32 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            tokio::spawn(auto_respond(
                server,
                vec![
                    ("initialize".to_owned(), json!({ "capabilities": {} })),
                    ("shutdown".to_owned(), serde_json::Value::Null),
                ],
            ));
            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(Duration::from_secs(5)).await?;
            Ok(client)
        })
    });

    let mut config = ManagerConfig::new(root.clone());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![crate::lsp::ServerSpec {
            name: "rust-analyzer",
            command: "unused",
            args: &[],
            extensions: &[".rs"],
            filenames: &[],
            root_markers: &["Cargo.toml"],
            workspace_aware: true,
        }],
        factory,
    );

    manager.ensure_for_file(&file).await.unwrap();
    let captured = captured.lock().expect("capture lock");
    assert_eq!(captured.len(), 1, "um spawn esperado");
    let cfg = &captured[0];
    assert_eq!(
        cfg.initialization_options,
        json!({ "cargo": { "targetDir": true } }),
        "rust-analyzer deve isolar targetDir via initializationOptions"
    );
    assert_eq!(
        cfg.settings,
        json!({ "rust-analyzer": { "cargo": { "targetDir": true } } }),
        "mesmo payload no canal settings (workspace/configuration lê a seção)"
    );
    assert!(
        cfg.env.is_empty(),
        "nenhum env inventado para outros servidores"
    );
}

/// Servidores não-Rust seguem com config nula: contenção direcionada, sem
/// payload inventado onde não há evidência de ganho.
#[tokio::test]
async fn spawn_config_stays_null_for_other_servers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("main.go");
    std::fs::write(root.join("go.mod"), "module x\n").unwrap();
    std::fs::write(&file, "package main\n").unwrap();

    let captured: Arc<Mutex<Vec<LanguageServerConfig>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    let factory: ClientFactory = Arc::new(move |config: LanguageServerConfig| {
        captured_clone
            .lock()
            .expect("capture lock")
            .push(config.clone());
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(32 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            tokio::spawn(auto_respond(
                server,
                vec![
                    ("initialize".to_owned(), json!({ "capabilities": {} })),
                    ("shutdown".to_owned(), serde_json::Value::Null),
                ],
            ));
            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(Duration::from_secs(5)).await?;
            Ok(client)
        })
    });

    let mut config = ManagerConfig::new(root.clone());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![crate::lsp::ServerSpec {
            name: "gopls",
            command: "unused",
            args: &[],
            extensions: &[".go"],
            filenames: &[],
            root_markers: &["go.mod"],
            workspace_aware: false,
        }],
        factory,
    );

    manager.ensure_for_file(&file).await.unwrap();
    let captured = captured.lock().expect("capture lock");
    assert_eq!(captured.len(), 1);
    assert!(captured[0].initialization_options.is_null());
    assert!(captured[0].settings.is_null());
}

/// Cada `touch_file` fixa o texto integral no servidor; a LRU segura 64 docs.
///
/// Simula o agente lendo N arquivos (cada `read` do cosh faz `warm_lsp` →
/// `touch_file`): após 70 toques distintos, 64 seguem abertos no servidor.
/// 64 arquivos Rust completos + análise do workspace = working set de GBs no
/// rust-analyzer real. O opencode abre sob demanda e não mantém esse
/// acúmulo por leitura passiva.
#[tokio::test]
async fn open_docs_lru_pins_64_documents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    let factory: ClientFactory = Arc::new(move |config| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(64 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
            tokio::spawn(auto_respond(
                server,
                vec![
                    ("initialize".to_owned(), json!({ "capabilities": {} })),
                    ("shutdown".to_owned(), serde_json::Value::Null),
                ],
            ));
            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(Duration::from_secs(5)).await?;
            Ok(client)
        })
    });

    let mut config = ManagerConfig::new(root.clone());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![crate::lsp::ServerSpec {
            name: "fake",
            command: "unused",
            args: &[],
            extensions: &[".rs"],
            filenames: &[],
            root_markers: &[],
            workspace_aware: false,
        }],
        factory,
    );

    let anchor = root.join("a0.rs");
    std::fs::write(&anchor, "fn a() {}\n").unwrap();
    let handles = manager.ensure_for_file(&anchor).await.unwrap();
    assert_eq!(handles.len(), 1);
    let client = handles[0].clone();

    for i in 0..70 {
        let p = root.join(format!("a{i}.rs"));
        std::fs::write(&p, format!("fn f{i}() {{}}\n")).unwrap();
        client.touch_file(&p).await.unwrap();
    }

    let open = client.open_documents();
    assert_eq!(
        open.len(),
        64,
        "diagnóstico: LRU fixa 64 documentos abertos (textos integrais no \
         servidor) — working set que o rust-analyzer real multiplica em análise"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1, "um único servidor");
}
