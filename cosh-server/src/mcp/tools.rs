use std::{path::PathBuf, sync::Arc};

use cosh_tools::{
    bash::Bash,
    fs::{Fs, FsMetadata, FsRead, FsWrite},
    lsp::{DiagnosticsInput, Lsp},
    plan::Plan,
    web::Web,
};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

/// Passive feedback renders at most this many error lines after a write.
const PASSIVE_MAX_ITEMS: u32 = 20;
/// Settle budget for the passive check after a write.
const PASSIVE_SETTLE_MS: u32 = 2_000;

#[allow(dead_code)]
pub struct Server {
    fs: Fs,
    web: Web,
    plan: Plan,
    bash: Bash,
    /// Language-server tooling. `None` when disabled via `COSH_LSP=off`
    /// or when no tokio runtime was available at construction.
    lsp: Option<Arc<Lsp>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ParametersFsRead {
    metadata: FsMetadata,
    targets: FsRead,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ParametersFsWrite {
    metadata: FsMetadata,
    targets: FsWrite,
}

/// Typed parameter shells for the lsp tools: the model-facing inputs live in
/// the tools crate and already carry JsonSchema.
macro_rules! parameters_lsp {
    ($name:ident, $input:ty) => {
        #[allow(dead_code)]
        #[derive(Debug, Deserialize, JsonSchema)]
        struct $name {
            params: $input,
        }
    };
}

parameters_lsp!(ParametersLspDiagnostics, DiagnosticsInput);
parameters_lsp!(
    ParametersDefinitions,
    cosh_tools::lsp::types::DefinitionsInput
);
parameters_lsp!(
    ParametersReferences,
    cosh_tools::lsp::types::ReferencesInput
);
parameters_lsp!(ParametersSymbols, cosh_tools::lsp::types::SymbolsInput);
parameters_lsp!(ParametersRestart, cosh_tools::lsp::types::RestartInput);

#[allow(dead_code)]
#[tool_router(server_handler)]
impl Server {
    pub fn new() -> Self {
        let lsp = Self::build_lsp();
        Self {
            fs: Fs::new(),
            web: Web::new(),
            plan: Plan::new(),
            bash: Bash::new(),
            lsp,
        }
    }

    /// Test entry point with an injected wrapper (fake manager/engine).
    #[cfg(test)]
    fn with_lsp(lsp: Arc<Lsp>) -> Self {
        Self {
            fs: Fs::new(),
            web: Web::new(),
            plan: Plan::new(),
            bash: Bash::new(),
            lsp: Some(lsp),
        }
    }

    /// Build the language-server tooling bound to the process working
    /// directory, unless `COSH_LSP=off|0|false` disables it.
    ///
    /// A relay task forwards every managed event into the diagnostics store —
    /// that store is what makes passive injection after writes possible.
    /// Children die on drop (`kill_on_drop`), so teardown needs no explicit
    /// hook here.
    fn build_lsp() -> Option<Arc<Lsp>> {
        if matches!(
            std::env::var("COSH_LSP").as_deref(),
            Ok("off" | "0" | "false")
        ) {
            log::info!("LSP tooling disabled via COSH_LSP");
            return None;
        }

        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut config = cosh_sdk::lsp::ManagerConfig::new(root);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        config.events = Some(event_tx);

        let manager = Arc::new(cosh_sdk::lsp::Manager::with_config(config));
        let diagnostics = Arc::new(cosh_sdk::lsp::DiagnosticsEngine::new());

        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let engine = Arc::clone(&diagnostics);
                runtime.spawn(async move {
                    while let Some(event) = event_rx.recv().await {
                        engine.ingest_event(&event);
                    }
                });
            }
            Err(_) => {
                log::warn!("no tokio runtime at LSP setup; diagnostics relay disabled");
                return None;
            }
        }

        Some(Arc::new(Lsp::with_manager(manager, diagnostics)))
    }

    // ............................... TOOLS SECTION ...

    // --- FILESYSTEM ---

    #[tool]
    pub async fn fs_read(
        &self,
        Parameters(params): Parameters<ParametersFsRead>,
    ) -> Result<CallToolResult, ErrorData> {
        let results = self.fs.read(params.targets.targets).await;
        Ok(CallToolResult::success(
            results
                .into_iter()
                .map(|r| Content::text(serde_json::to_string(&r).unwrap_or_default()))
                .collect(),
        ))
    }

    #[tool]
    pub async fn fs_write(
        &self,
        Parameters(params): Parameters<ParametersFsWrite>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.fs.write(params.targets.targets.clone()).await {
            Ok(results) => {
                let mut contents: Vec<Content> = results
                    .into_iter()
                    .map(|r| Content::text(serde_json::to_string(&r).unwrap_or_default()))
                    .collect();

                // Passive feedback loop: fresh LSP errors land right where the
                // model just wrote, before it can consider the task done.
                if let Some(lsp) = &self.lsp {
                    for target in &params.targets.targets {
                        if let Some(note) = passive_error_note(lsp, &target.path).await {
                            contents.push(Content::text(note));
                            break; // one reminder per write is enough
                        }
                    }
                }

                Ok(CallToolResult::success(contents))
            }
            Err(err) => Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, err, None)),
        }
    }

    #[tool]
    pub fn fs_edit() {
        todo!()
    }

    // --- LANGUAGE SERVER ---

    #[tool]
    pub async fn lsp_diagnostics(
        &self,
        Parameters(params): Parameters<ParametersLspDiagnostics>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(lsp) = &self.lsp else {
            return Ok(CallToolResult::success(vec![Content::text(
                DISABLED_NOTICE,
            )]));
        };
        match lsp.diagnostics(&params.params).await {
            Ok(out) => Ok(CallToolResult::success(vec![Content::text(
                render_lsp_output(out.count, &out.formatted, out.settled),
            )])),
            Err(err) => Ok(CallToolResult::success(vec![Content::text(err)])),
        }
    }

    #[tool]
    pub async fn lsp_definitions(
        &self,
        Parameters(params): Parameters<ParametersDefinitions>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(lsp) = &self.lsp else {
            return Ok(CallToolResult::success(vec![Content::text(
                DISABLED_NOTICE,
            )]));
        };
        match lsp.definitions(&params.params).await {
            Ok(out) => Ok(CallToolResult::success(vec![Content::text(
                render_lsp_output(out.definitions.len(), &out.formatted, true),
            )])),
            Err(err) => Ok(CallToolResult::success(vec![Content::text(err)])),
        }
    }

    #[tool]
    pub async fn lsp_references(
        &self,
        Parameters(params): Parameters<ParametersReferences>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(lsp) = &self.lsp else {
            return Ok(CallToolResult::success(vec![Content::text(
                DISABLED_NOTICE,
            )]));
        };
        match lsp.references(&params.params).await {
            Ok(out) => Ok(CallToolResult::success(vec![Content::text(
                render_lsp_output(out.total, &out.formatted, true),
            )])),
            Err(err) => Ok(CallToolResult::success(vec![Content::text(err)])),
        }
    }

    #[tool]
    pub async fn lsp_symbols(
        &self,
        Parameters(params): Parameters<ParametersSymbols>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(lsp) = &self.lsp else {
            return Ok(CallToolResult::success(vec![Content::text(
                DISABLED_NOTICE,
            )]));
        };
        match lsp.symbols(&params.params).await {
            Ok(out) => Ok(CallToolResult::success(vec![Content::text(
                render_lsp_output(out.symbols.len(), &out.formatted, true),
            )])),
            Err(err) => Ok(CallToolResult::success(vec![Content::text(err)])),
        }
    }

    #[tool]
    pub async fn lsp_restart(
        &self,
        Parameters(params): Parameters<ParametersRestart>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(lsp) = &self.lsp else {
            return Ok(CallToolResult::success(vec![Content::text(
                DISABLED_NOTICE,
            )]));
        };
        match lsp.restart(&params.params).await {
            Ok(out) => {
                let summary = if out.restarted.is_empty() {
                    "no running servers to restart".to_owned()
                } else {
                    format!("restarted: {}", out.restarted.join(", "))
                };
                Ok(CallToolResult::success(vec![Content::text(summary)]))
            }
            Err(err) => Ok(CallToolResult::success(vec![Content::text(err)])),
        }
    }

    // --- WEB ---

    #[tool]
    pub fn web_search() {
        todo!()
    }

    #[tool]
    pub fn web_fetch() {
        todo!()
    }

    // --- BASH ---

    #[tool]
    pub fn bash_exec() {
        todo!()
    }

    // --- PLAN ---

    #[tool]
    pub fn todo_read() {
        todo!()
    }

    #[tool]
    pub fn todo_write(&self) {
        todo!()
    }

    #[tool]
    pub fn todo_edit(&self) {
        todo!()
    }

    #[tool]
    pub fn todo_cross_off(&self) {
        todo!()
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

const DISABLED_NOTICE: &str =
    "LSP tooling is disabled in this session (COSH_LSP=off or no runtime).";

fn render_lsp_output(count: usize, formatted: &str, settled: bool) -> String {
    if count == 0 {
        let suffix = if settled {
            ""
        } else {
            " (results may be stale — still settling)"
        };
        return format!("No results.{suffix}");
    }
    let staleness = if settled {
        String::new()
    } else {
        String::from("\n(note: still settling)")
    };
    format!("{formatted}{staleness}")
}

/// Errors-only rendering for one just-written file, wrapped in a
/// system-reminder block. `None` when nothing to report.
async fn passive_error_note(lsp: &Arc<Lsp>, path: &str) -> Option<String> {
    let input = DiagnosticsInput {
        file_path: Some(path.to_owned()),
        severity: Some("errors".into()),
        max_items: Some(PASSIVE_MAX_ITEMS),
        settle_ms: Some(PASSIVE_SETTLE_MS),
    };
    let out = lsp.diagnostics(&input).await.ok()?;
    if out.formatted.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n<system-reminder>\nLSP errors detected after writing {path}:\n{}\nFix these using the ¶header from your last edit result — no need to re-read the file.\n</system-reminder>",
        out.formatted
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_sdk::lsp::test_support::{auto_respond, spawn_fake_server};
    use cosh_sdk::lsp::{ClientFactory, LanguageServer, Manager, ManagerConfig, ServerSpec};
    use std::time::Duration;

    /// Minimal diagnostic builder shared by the server tests.
    fn lsp_types_shim_diagnostic(message: &str) -> cosh_sdk::lsp::lsp_types::Diagnostic {
        cosh_sdk::lsp::lsp_types::Diagnostic {
            range: cosh_sdk::lsp::lsp_types::Range {
                start: cosh_sdk::lsp::lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: cosh_sdk::lsp::lsp_types::Position {
                    line: 0,
                    character: 5,
                },
            },
            severity: Some(cosh_sdk::lsp::lsp_types::DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("fake".into()),
            message: message.to_owned(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    /// Extract the first textual payload from a tool result (rmcp models wrap
    /// text in `Annotated<RawContent>`, not an enum).
    #[cfg(test)]
    fn content_text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.clone())
            .unwrap_or_default()
    }

    /// A manager over a fake server factory bound to a fresh temp workspace.
    /// Returns the wrapper plus the workspace root (kept alive by leaking the
    /// tempdir handle — tests are short and the process reclaims it).
    fn fake_lsp() -> (Arc<Lsp>, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        // Leak-free for test lifetimes: the tempdir handle is intentionally
        // kept alive via the closure's owned copy of the path only — files are
        // recreated per test through fs_write itself.
        let mut config = ManagerConfig::new(dir.path().to_path_buf());
        config.spawn_backoff = Duration::ZERO;
        // Fake factories bring their own transports; skip the PATH lookup.
        config.resolves_binaries = false;
        let counter = Arc::clone(&counter);
        let factory: ClientFactory = Arc::new(move |cfg| {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = &counter;
            Box::pin(async move {
                let (server, client_stream) = spawn_fake_server(32 * 1024);
                let (dead_stderr, _dead_peer) = tokio::io::duplex(1);
                tokio::spawn(auto_respond(
                    server,
                    vec![(
                        "initialize".to_owned(),
                        serde_json::json!({ "capabilities": {} }),
                    )],
                ));
                let (read_half, write_half) = tokio::io::split(client_stream);
                let client =
                    LanguageServer::from_streams(cfg, read_half, write_half, Some(dead_stderr));
                client.initialize(Duration::from_secs(5)).await?;
                Ok(client)
            })
        });
        let catalog = vec![ServerSpec {
            name: "fake",
            command: "unused",
            args: &[],
            extensions: &[".fake"],
            root_markers: &[],
        }];
        let manager = Manager::build(config, catalog, factory);
        let root = dir.path().to_path_buf();

        // Keep the tempdir alive for as long as the wrapper lives by leaking
        // the handle — tests are short and process exit reclaims everything.
        std::mem::forget(dir);

        (
            Arc::new(Lsp::with_manager(
                Arc::new(manager),
                Arc::new(cosh_sdk::lsp::DiagnosticsEngine::new()),
            )),
            root,
        )
    }

    #[tokio::test]
    async fn disabled_lsp_answers_with_notice() {
        let server = Server {
            fs: Fs::new(),
            web: Web::new(),
            plan: Plan::new(),
            bash: Bash::new(),
            lsp: None,
        };
        let result = server
            .lsp_diagnostics(Parameters(ParametersLspDiagnostics {
                params: DiagnosticsInput::default(),
            }))
            .await
            .unwrap();

        let text = content_text(&result);
        assert!(text.contains("disabled"), "{text}");
    }

    #[tokio::test]
    async fn lsp_diagnostics_renders_pushed_errors() {
        let (lsp, root) = fake_lsp();
        let file = root.join("pushed.fake");
        std::fs::write(
            &file,
            "broken !!(
",
        )
        .unwrap();

        // Simulate a push arriving through the relay.
        let diag = lsp_types_shim_diagnostic("borrow of moved value");
        lsp.diagnostics_engine().ingest(
            "fake",
            &cosh_sdk::lsp::lsp_types::PublishDiagnosticsParams::new(
                cosh_sdk::lsp::uri_from_path(&file).unwrap(),
                vec![diag],
                None,
            ),
        );

        let server = Server::with_lsp(Arc::clone(&lsp));
        let result = server
            .lsp_diagnostics(Parameters(ParametersLspDiagnostics {
                params: DiagnosticsInput {
                    file_path: Some(file.display().to_string()),
                    severity: Some("errors".into()),
                    max_items: None,
                    settle_ms: Some(100),
                },
            }))
            .await
            .unwrap();

        let text = content_text(&result);
        assert!(text.contains("[error 1:1]"), "{text}");
        assert!(text.contains("borrow of moved value"), "{text}");
    }

    #[tokio::test]
    async fn passive_note_wraps_engine_errors() {
        let (lsp, root) = fake_lsp();
        let file = root.join("after-write.fake");
        std::fs::write(&file, "let x: String = 1;\n").unwrap();
        lsp.diagnostics_engine().ingest(
            "fake",
            &cosh_sdk::lsp::lsp_types::PublishDiagnosticsParams::new(
                cosh_sdk::lsp::uri_from_path(&file).unwrap(),
                vec![lsp_types_shim_diagnostic("type mismatch")],
                None,
            ),
        );

        let note = passive_error_note(&lsp, &file.display().to_string())
            .await
            .expect("note expected");
        assert!(note.contains("<system-reminder>"));
        assert!(
            note.contains(&format!("after writing {}", file.display())),
            "{note}"
        );
        assert!(note.contains("type mismatch"));

        // Quiet store → no note.
        let untouched = root.join("untouched.fake");
        assert!(
            passive_error_note(&lsp, &untouched.display().to_string())
                .await
                .is_none()
        );
    }
}
