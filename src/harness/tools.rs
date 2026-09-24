use std::collections::HashSet;
use std::fmt::Write;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::events::HarnessEvent;
use super::truncate::{
    WEB_FETCH_MAX_TOKENS, truncate_tool_output, truncate_tool_output_with_budget,
};
use cosh_sdk::extract_action::ToolSchema;
use cosh_sdk::find::{GlobMatch, GrepMatch};
use cosh_tools::{
    bash::{Bash, BashRunInput},
    computer::{
        Computer,
        types::{
            ComputerAct, ComputerApps, ComputerControl, ComputerScreenshot, ComputerSnapshot,
            ComputerWait,
        },
    },
    find::{Find, GlobCallOptions, GlobMatchCallback, GrepMatchCallback},
    fs::{Fs, FsRollbackInput, LspNotes, Target, TargetFile},
    lsp::Lsp,
    plan::{Plan, types::TodoWriteInput},
    question::{Question, types::QuestionInput},
    skills::{
        SkillSource, Skills,
        types::{SkillsMatchInput, SkillsReadAssetInput, SkillsReadInput},
    },
    subagent::{
        SubAgent,
        types::{SubAgentCallInput, SubAgentCallOutput},
    },
    web::{Web, WebFetch, WebSearchInput},
};

#[cfg(feature = "embed")]
use cosh_tools::recall::Recall;

use tokio_stream::StreamExt;

#[allow(async_fn_in_trait)]
pub trait Tools: Send + Sync {
    fn schemas(&self) -> Vec<ToolSchema>;
    fn tool_descriptions(&self) -> Vec<serde_json::Value>;
    fn write_tool_descriptions(&self, out: &mut String) {
        for desc in self.tool_descriptions() {
            let name = desc["name"].as_str().unwrap_or_default();
            let description = desc["description"].as_str().unwrap_or_default();
            let schema = serde_json::to_string_pretty(&desc["inputSchema"]).unwrap_or_default();
            let _ = write!(out, "- **{name}**: {description}\n  Schema: {schema}\n");
        }
    }
    async fn dispatch(&self, name: &str, args: serde_json::Value) -> Result<String, String>;
}

/// Simplified embedder config for the `recall_search` dispatch.
/// Does not depend on TUI types — model names are stored as strings.
#[cfg(feature = "embed")]
#[derive(Debug, Clone)]
pub enum RecallEmbedderConfig {
    /// Local fastembed model — model_name must match serde renames of
    /// `fastembed::EmbeddingModel` (e.g. "AllMiniLML6V2", "BGESmallENV15Q").
    Local { model_name: String },
    /// Cloud embedding provider.
    Cloud {
        provider: String,
        model: String,
        dim: usize,
    },
}

/// Minimal DB info needed by the recall_search dispatch.
#[cfg(feature = "embed")]
#[derive(Debug, Clone)]
pub struct RecallDb {
    pub name: String,
    pub uri: String,
    pub table_name: String,
    pub embedder: RecallEmbedderConfig,
}

/// Batches streamed search matches and flushes them to the TUI in chunks
/// (rate-limited), so a huge scan does not flood the event channel with one
/// event per match and the tool part's live output stays bounded.
///
/// The reference implementation throttles live updates to ~200ms; we flush
/// on a line count OR an interval, whichever comes first.
struct StreamBatcher {
    tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
    tool: &'static str,
    lines: Vec<String>,
    last_flush: Instant,
}

impl StreamBatcher {
    const MAX_LINES_PER_FLUSH: usize = 100;
    const FLUSH_INTERVAL: Duration = Duration::from_millis(150);

    fn new(tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>, tool: &'static str) -> Self {
        Self {
            tx,
            tool,
            lines: Vec::new(),
            last_flush: Instant::now(),
        }
    }

    /// Buffer one streamed match line; flush when the batch or the interval
    /// is exceeded.
    fn push(&mut self, line: String) {
        self.lines.push(line);
        if self.lines.len() >= Self::MAX_LINES_PER_FLUSH
            || self.last_flush.elapsed() >= Self::FLUSH_INTERVAL
        {
            self.flush();
        }
    }

    /// Send whatever is buffered (called by the dispatch after the search
    /// finishes so the tail of a slow scan is not lost).
    fn flush(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.lines);
        let _ = self.tx.send(HarnessEvent::ToolOutput {
            tool: self.tool.to_string(),
            output: format!("{}\n", batch.join("\n")),
            finished: false,
        });
        self.last_flush = Instant::now();
    }
}

pub struct CoshTools {
    bash: Bash,
    fs: Fs,
    find: Find,
    web: Web,
    plan: Mutex<Plan>,
    question: Question,
    lsp: Option<Arc<cosh_tools::lsp::Lsp>>,
    #[cfg(feature = "embed")]
    recall: Recall,
    #[cfg(feature = "embed")]
    recall_dbs: Vec<RecallDb>,
    skills: Skills,
    subagent: SubAgent,
    computer: Computer,
    /// PNG payload staged by the last `computer_screenshot` dispatch
    /// (`dispatch` takes `&self`, so staging is interior-mutable). The
    /// harness drains it right after the dispatch returns `Ok` and lowers
    /// it into the tool-result `ChatMessage` — see `push_tool_history`.
    pending_images: Mutex<Option<Vec<cosh_sdk::connector::ImageBlock>>>,
    /// Optional event sender for streaming tool output.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<HarnessEvent>>,
    /// Shared stop flag for interruptible tools (Phase 5): the agent loop's
    /// `stop_signal` reaches a running `subagent_call` through this, turning
    /// an ESC into a spec-conformant ACP `session/cancel` — the only way a
    /// turn ends early (there is no time limit). `None` → turns run to
    /// completion.
    stop_signal: Option<Arc<AtomicBool>>,
    /// Configured base URLs for local providers (used by the cloud embedder).
    #[cfg(feature = "embed")]
    local_base_urls: std::collections::HashMap<String, String>,
}

/// Default skill sources: the user's `~/.skills` directory.
///
/// A missing (or non-directory) default is NOT registered: discovery is a
/// hard `InvalidSource` error for unreadable sources, and a default that
/// does not exist must never break the skills tools — they simply report
/// an empty list until the directory (or a setup.json override) appears.
fn default_skill_sources() -> Vec<SkillSource> {
    default_skill_sources_for(cosh_tools::skills::home_dir().as_deref())
}

/// [`default_skill_sources`] with the home directory injected (testable).
fn default_skill_sources_for(home: Option<&str>) -> Vec<SkillSource> {
    let Some(home) = home else {
        return Vec::new();
    };
    // A HOME inherited from Git Bash / MSYS uses POSIX drive syntax
    // ("/c/Users/..."), which native filesystem calls cannot resolve on
    // Windows — normalize it before the `is_dir` probe, or the default
    // source would be silently dropped (empty skills list).
    let dir = format!("{home}/.skills");
    let dir = cosh_tools::skills::normalize_shell_path(&dir);
    if std::path::Path::new(&dir).is_dir() {
        vec![SkillSource::Directory { path: dir }]
    } else {
        Vec::new()
    }
}

impl CoshTools {
    #[must_use]
    pub fn new(cwd: &str) -> Self {
        let lsp = build_lsp(cwd);
        let mut fs = Fs::new().cwd(cwd);
        if let Some(handle) = &lsp {
            // The handle is the toggle: every fs operation now reports
            // passive LSP diagnostics on the files it touches.
            fs = fs.with_lsp(Arc::clone(handle));
        }
        Self {
            // PTY mode: the child sees a terminal, so line-buffered output
            // (cargo, python, make, …) streams in real time instead of
            // arriving in one block-buffered dump at exit. The dispatch
            // strips ANSI escapes / CRLF per chunk (see `strip_ansi`).
            // The timeout default is owned by the tool (`Bash::new`, 10
            // minutes): it is the hang guard for a PTY child whose stdin is
            // the slave side and nothing feeds it — a command that reads
            // stdin (`cat`, `ssh`, a prompt) would otherwise block forever.
            // The value is interpolated into the `bash_run` tool description,
            // and the model can raise it per call via the optional
            // `timeout_ms` argument (validated inside the tool).
            bash: Bash::new().cwd(cwd).pty(true),
            fs,
            find: Find::new().cwd(cwd),
            web: Web::new(),
            plan: Mutex::new(Plan::new()),
            question: Question::new(),
            lsp,
            #[cfg(feature = "embed")]
            recall: Recall::new(),
            #[cfg(feature = "embed")]
            recall_dbs: Vec::new(),
            skills: Skills::new().sources(default_skill_sources()),
            subagent: SubAgent::new(),
            computer: Computer::new(),
            pending_images: Mutex::new(None),
            event_tx: None,
            stop_signal: None,
            #[cfg(feature = "embed")]
            local_base_urls: std::collections::HashMap::new(),
        }
    }

    /// Configured base URLs for local providers, honored by the cloud
    /// embedder when a provider is local.
    #[cfg(feature = "embed")]
    #[must_use]
    pub fn with_local_base_urls(mut self, urls: std::collections::HashMap<String, String>) -> Self {
        self.local_base_urls = urls;
        self
    }

    /// Replace the skills wrapper (config-driven discovery sources from
    /// setup.json). Discovery happens at dispatch time on the wrapper,
    /// so this takes effect for every later `skills_*` call.
    pub fn set_skills(&mut self, skills: Skills) {
        self.skills = skills;
    }

    /// Clone of the skills wrapper — propagated to nested sub-agent
    /// harnesses so they serve the same skill set as the parent.
    #[must_use]
    pub fn skills(&self) -> Skills {
        self.skills.clone()
    }

    /// Get the project root directory (used for path validation).
    #[must_use]
    pub fn project_root(&self) -> &std::path::PathBuf {
        self.fs.root()
    }

    /// Snapshot of the current tool TODO list, mirrored by the harness into
    /// its dedicated protected TODO context block (see
    /// `context_manager::todo_ctxt`). Cloned so the caller owns the data.
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn todo_list(&self) -> cosh_tools::plan::types::TodoList {
        self.plan.lock().unwrap().list().clone()
    }

    /// Restore the tool projection before dispatching tools in a resumed turn.
    ///
    /// # Panics
    /// Panics if the internal plan mutex is poisoned.
    pub fn restore_todo_list(&self, list: cosh_tools::plan::types::TodoList) {
        self.plan.lock().unwrap().restore_list(list);
    }

    /// Add a path to the file-system write allowlist.
    pub fn add_fs_allowlist_path(&mut self, path: std::path::PathBuf) {
        self.fs.add_allowlist_path(path);
    }

    /// Add a path to the find tools allowlist.
    pub fn add_find_allowlist_path(&mut self, path: std::path::PathBuf) {
        self.find.add_allowlist_path(path);
    }

    /// Remove a path from the file-system write allowlist (for AllowOnce cleanup).
    pub fn remove_fs_allowlist_path(&mut self, path: &std::path::Path) {
        self.fs.remove_allowlist_path(path);
    }

    /// Remove a path from the find tools allowlist (for AllowOnce cleanup).
    pub fn remove_find_allowlist_path(&mut self, path: &std::path::Path) {
        self.find.remove_allowlist_path(path);
    }

    /// Set the event sender for streaming tool output.
    pub fn set_event_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>) {
        self.event_tx = Some(tx);
    }

    /// Share the agent loop's stop flag with interruptible tools (Phase 5).
    /// Called wherever `set_event_tx` is called, so a running
    /// `subagent_call` honours ESC via an ACP `session/cancel` notification.
    pub fn set_stop_signal(&mut self, stop_signal: Arc<AtomicBool>) {
        self.stop_signal = Some(stop_signal);
    }

    /// Forward passive LSP findings from an fs operation to the TUI as a
    /// dedicated event (errors render red, warnings yellow). The model sees
    /// the same findings inline in the serialized result; this event only
    /// carries the user-facing styling signal.
    fn emit_lsp_notes<'a>(&self, tool: &str, notes: impl Iterator<Item = &'a LspNotes>) {
        let mut merged = LspNotes::default();
        for notes in notes {
            merged.errors.extend(notes.errors.iter().cloned());
            merged.warnings.extend(notes.warnings.iter().cloned());
        }
        if merged.errors.is_empty() && merged.warnings.is_empty() {
            return;
        }
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(HarnessEvent::ToolDiagnostics {
                tool: tool.to_string(),
                notes: merged,
            });
        }
    }

    /// Clone of the event sender, if one was set (streaming tool output to
    /// the TUI). The internal sub-agent path uses it to bridge its stream
    /// without touching the harness's own context.
    #[must_use]
    pub fn event_tx(&self) -> Option<tokio::sync::mpsc::UnboundedSender<HarnessEvent>> {
        self.event_tx.clone()
    }

    /// Drain the image payload staged by the last `computer_screenshot`
    /// dispatch (empty when the last dispatch was another tool or carried
    /// no images). Called by the harness immediately after the dispatch
    /// `Ok` so the bytes reach the tool-result `ChatMessage`; a payload
    /// left unconsumed (dispatch error path) is dropped here too.
    pub fn take_pending_images(&self) -> Vec<cosh_sdk::connector::ImageBlock> {
        self.pending_images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .unwrap_or_default()
    }

    /// Interpolate an informational chunk into the `subagent_call` tool
    /// description (the harness tells the model that omitting `agent`
    /// routes the call to an internal agent).
    pub fn set_subagent_note(&mut self, note: impl Into<String>) {
        self.subagent.set_note(note.into());
    }

    /// Access the LSP wrapper for passive diagnostics injection.
    #[must_use]
    pub fn lsp(&self) -> Option<&Arc<Lsp>> {
        self.lsp.as_ref()
    }

    /// Resolve the effective input message for a sub-agent call (external
    /// CLI or internal agent): reuse the last message when `input` is
    /// omitted/empty, error when nothing is stored yet.
    ///
    /// # Errors
    ///
    /// Returns an error if `input` is omitted and no sub-agent message is
    /// stored in this session yet.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn resolve_subagent_input(&self, input: Option<String>) -> Result<String, String> {
        self.subagent.resolve_input(input)
    }

    /// Set the list of RAG databases for the `recall_search` dispatch.
    #[cfg(feature = "embed")]
    pub fn set_recall_dbs(&mut self, dbs: Vec<RecallDb>) {
        self.recall_dbs = dbs;
    }

    /// Inject DB context into the `recall_search` tool description.
    ///
    /// The `suffix` is appended to the default description so the agent
    /// sees which knowledge bases are currently available.
    #[cfg(feature = "embed")]
    pub fn set_recall_context(&mut self, suffix: impl Into<String>) {
        self.recall.rebuild_description(suffix);
    }

    /// All tool descriptions filtered to Ask-mode-appropriate tools (read-only +
    /// planning), skipping disabled ones.
    ///
    /// Matches the tool set exposed by [`write_tool_descriptions_filtered`]
    /// and [`schemas_filtered`].
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn tool_descriptions_filtered(
        &self,
        disabled_tools: &HashSet<String>,
    ) -> Vec<serde_json::Value> {
        let mut v = vec![
            self.fs.description_read.clone(),
            self.find.description_glob.clone(),
            self.find.description_grep.clone(),
            self.web.description_fetch.clone(),
            self.web.description_search.clone(),
        ];
        v.push(self.question.description_ask.clone());
        #[cfg(feature = "embed")]
        v.push(self.recall.description_search.clone());
        v.push(self.skills.description_list.clone());
        v.push(self.skills.description_read.clone());
        v.push(self.skills.description_read_asset.clone());
        v.push(self.skills.description_match_skills.clone());
        // Ask mode keeps computer_screenshot: it is read-only observation and
        // Ask mode has no approval dialog (nothing can move the pointer
        // between capture and use). apps/snapshot are structural (accessibility
        // tree), so they are position-independent by definition. computer_wait
        // is likewise read-only: it sends no input and blocks on a state
        // check, so it stays exposed here too.
        v.push(self.computer.description_apps.clone());
        v.push(self.computer.description_snapshot.clone());
        v.push(self.computer.description_wait.clone());
        v.push(self.computer.description_screenshot.clone());
        if let Some(lsp) = &self.lsp {
            v.push(lsp.description_definitions.clone());
            v.push(lsp.description_references.clone());
            v.push(lsp.description_symbols.clone());
            v.push(lsp.description_hover.clone());
            v.push(lsp.description_workspace_symbols.clone());
            v.push(lsp.description_rename.clone());
            v.push(lsp.description_call_hierarchy.clone());
            v.push(lsp.description_restart.clone());
        }
        v.into_iter()
            .filter(|desc| {
                let name = desc["name"].as_str().unwrap_or_default();
                !is_tool_disabled(name, disabled_tools)
            })
            .collect()
    }

    /// All tool descriptions, skipping disabled ones.
    ///
    /// `include_schema` controls whether the full `inputSchema` is written
    /// inline after each tool: local model servers (which may lack reliable
    /// native function calling and emit inline JSON the extractor parses)
    /// keep it, cloud providers omit it — they hold the schemas in their
    /// native tool definitions, so re-sending them in the system prompt
    /// would duplicate every schema on each request.
    pub fn write_tool_descriptions_enabled(
        &self,
        out: &mut String,
        disabled_tools: &HashSet<String>,
        include_schema: bool,
    ) {
        let all = self.tool_descriptions();
        for desc in all {
            let name = desc["name"].as_str().unwrap_or_default();
            if is_tool_disabled(name, disabled_tools) {
                continue;
            }
            write_single_tool(out, &desc, include_schema);
        }
    }

    /// Tool descriptions restricted to read-only and search tools (Ask mode).
    ///
    /// `include_schema` behaves as in [`Self::write_tool_descriptions_enabled`].
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn write_tool_descriptions_filtered(
        &self,
        out: &mut String,
        disabled_tools: &HashSet<String>,
        include_schema: bool,
    ) {
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.fs.description_read,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.find.description_glob,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.find.description_grep,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.web.description_fetch,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.web.description_search,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.question.description_ask,
            include_schema,
        );
        #[cfg(feature = "embed")]
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.recall.description_search,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.skills.description_list,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.skills.description_read,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.skills.description_read_asset,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.skills.description_match_skills,
            include_schema,
        );
        // Ask mode exposes the READ-ONLY observation tools of the computer
        // set (apps/snapshot/screenshot/wait) — no synthetic input, so they
        // are as safe as a web search. act/control stay out (they need
        // approval even in Build mode).
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.computer.description_apps,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.computer.description_snapshot,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.computer.description_wait,
            include_schema,
        );
        write_tool_if_enabled(
            out,
            disabled_tools,
            &self.computer.description_screenshot,
            include_schema,
        );
    }

    /// All schemas, skipping disabled ones.
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn schemas_enabled(&self, disabled_tools: &HashSet<String>) -> Vec<ToolSchema> {
        let all = self.tool_descriptions();
        all.iter()
            .filter(|desc| {
                let name = desc["name"].as_str().unwrap_or_default();
                !is_tool_disabled(name, disabled_tools)
            })
            .map(extract_schema)
            .collect()
    }

    /// Schemas restricted to read-only and search tools (Ask mode), skipping disabled ones.
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn schemas_filtered(&self, disabled_tools: &HashSet<String>) -> Vec<ToolSchema> {
        let mut v = vec![
            extract_schema(&self.fs.description_read),
            extract_schema(&self.find.description_glob),
            extract_schema(&self.find.description_grep),
            extract_schema(&self.web.description_fetch),
            extract_schema(&self.web.description_search),
        ];
        v.push(extract_schema(&self.question.description_ask));
        #[cfg(feature = "embed")]
        v.push(extract_schema(&self.recall.description_search));
        v.push(extract_schema(&self.skills.description_list));
        v.push(extract_schema(&self.skills.description_read));
        v.push(extract_schema(&self.skills.description_read_asset));
        v.push(extract_schema(&self.skills.description_match_skills));
        // Ask mode exposes the READ-ONLY observation tools of the computer
        // set (apps/snapshot/screenshot) — mirroring
        // `tool_descriptions_filtered`/`write_tool_descriptions_filtered`.
        v.push(extract_schema(&self.computer.description_apps));
        v.push(extract_schema(&self.computer.description_snapshot));
        v.push(extract_schema(&self.computer.description_wait));
        v.push(extract_schema(&self.computer.description_screenshot));
        if let Some(lsp) = &self.lsp {
            v.push(extract_schema(&lsp.description_definitions));
            v.push(extract_schema(&lsp.description_references));
            v.push(extract_schema(&lsp.description_symbols));
            v.push(extract_schema(&lsp.description_hover));
            v.push(extract_schema(&lsp.description_workspace_symbols));
            v.push(extract_schema(&lsp.description_rename));
            v.push(extract_schema(&lsp.description_call_hierarchy));
            v.push(extract_schema(&lsp.description_restart));
            v.push(extract_schema(&lsp.description_code_actions));
        }
        v.into_iter()
            .filter(|schema| !is_tool_disabled(&schema.name, disabled_tools))
            .collect()
    }
}

// ── recall_search dispatch (embedding + search) ───────────────────────
//
// The model only provides { db_name, query, limit }. The harness resolves
// the DB from the registry, creates an embedder, embeds the query, and
// searches the vector database. This requires at least one of the
// `fastembed` or `cloud` features to be enabled at build time.

/// Convert `RecallEmbedderConfig` to a `cosh_recall::embed::Embedder`.
#[cfg(feature = "embed")]
#[cfg(any(feature = "fastembed", feature = "cloud"))]
fn config_to_embedder(
    config: &RecallEmbedderConfig,
    _base_urls: &std::collections::HashMap<String, String>,
) -> Result<cosh_recall::embed::Embedder, String> {
    match config {
        #[cfg(feature = "fastembed")]
        RecallEmbedderConfig::Local { model_name } => {
            let fb_model = cosh_recall::embed::fastembed_model_from_str(&model_name)?;
            cosh_recall::embed::Embedder::try_new_local(fb_model)
                .map_err(|e| format!("failed to create local embedder: {e}"))
        }
        #[cfg(not(feature = "fastembed"))]
        RecallEmbedderConfig::Local { .. } => {
            Err("Local embedding requires building with --features fastembed".into())
        }
        #[cfg(feature = "cloud")]
        RecallEmbedderConfig::Cloud {
            provider,
            model,
            dim,
        } => {
            let mut connector = cosh_sdk::connector::Connector::new(provider)
                .map_err(|e| format!("connector error: {e}"))?
                .with_model(model);
            if let Some(url) = _base_urls.get(provider) {
                connector = connector.with_base_url(url.clone());
            }
            Ok(cosh_recall::embed::Embedder::new_cloud(connector, *dim))
        }
        #[cfg(not(feature = "cloud"))]
        RecallEmbedderConfig::Cloud { .. } => {
            Err("Cloud embedding requires building with --features cloud".into())
        }
    }
}

/// Execute `recall_search` by embedding `query` and searching the vector DB.
///
/// Uses `VecDb::connect_readonly` — fails with a clear error if the table
/// does not exist (no silent table creation).
///
/// This function is only available when at least one embedding feature
/// (`fastembed` or `cloud`) is enabled.
#[cfg(feature = "embed")]
#[cfg(any(feature = "fastembed", feature = "cloud"))]
async fn dispatch_recall_search(
    db: &RecallDb,
    query: &str,
    limit: usize,
    base_urls: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    // 1. Create embedder from the DB's config
    let embedder = config_to_embedder(&db.embedder, base_urls)?;

    // 2. Connect to the LanceDB table in READ-ONLY mode
    let vec_db = cosh_recall::embed::VecDb::connect_readonly(&db.uri, &db.table_name)
        .await
        .map_err(|e| format!("database connection error: {e}"))?;

    // 3. Embed the query text
    let embeddings = embedder
        .embed(&[query])
        .await
        .map_err(|e| format!("embedding error: {e}"))?;
    let query_vector = &embeddings[0];

    // 4. Vector search (read-only)
    let entries = vec_db
        .get(query_vector, limit)
        .await
        .map_err(|e| format!("search error: {e}"))?;

    // 5. Serialize results
    let results: Vec<cosh_tools::recall::types::RecallEntry> = entries
        .into_iter()
        .map(|e| cosh_tools::recall::types::RecallEntry {
            id: e.id,
            content: e.content,
        })
        .collect();

    let output = cosh_tools::recall::types::RecallOutput {
        query: query.to_string(),
        results,
    };

    serde_json::to_string(&output).map_err(|e| e.to_string())
}

#[cfg(feature = "embed")]
#[cfg(not(any(feature = "fastembed", feature = "cloud")))]
async fn dispatch_recall_search(
    _db: &RecallDb,
    _query: &str,
    _limit: usize,
    _base_urls: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    Err(
        "RAG search requires building with --features lancedb,fastembed \
         or --features lancedb,cloud for embedding support"
            .into(),
    )
}

fn extract_schema(desc: &serde_json::Value) -> ToolSchema {
    ToolSchema {
        name: desc["name"].as_str().unwrap_or_default().to_string(),
        input_schema: desc["inputSchema"].clone(),
        // Tools may embed a curated example (`exampleArgs`, e.g.
        // `ask_questions`) that the extractor shows in rejection hints.
        example_args: desc.get("exampleArgs").filter(|v| v.is_object()).cloned(),
    }
}

/// Write one tool as `- **name**: description`, optionally followed by the
/// full input schema.
///
/// Cloud providers hold the schemas in their native function-calling
/// mechanism, so the harness omits the inline `Schema:` block for them (see
/// [`CoshTools::write_tool_descriptions_enabled`]) — re-sending it would
/// duplicate every schema on each request. Local model servers without
/// reliable native calling keep the schemas because the model emits inline
/// JSON the extractor parses.
fn write_single_tool(out: &mut String, desc: &serde_json::Value, include_schema: bool) {
    let name = desc["name"].as_str().unwrap_or_default();
    let description = desc["description"].as_str().unwrap_or_default();
    if include_schema {
        let schema = serde_json::to_string_pretty(&desc["inputSchema"]).unwrap_or_default();
        let _ = write!(out, "- **{name}**: {description}\n  Schema: {schema}\n");
    } else {
        let _ = writeln!(out, "- **{name}**: {description}");
    }
}

/// Write a single tool description only if its name is not in the disabled set.
/// A tool is disabled when its exact name is in `disabled`, OR when its
/// namespace prefix (e.g. `lsp`) is disabled as a group toggle.
pub(crate) fn is_tool_disabled(name: &str, disabled: &HashSet<String>) -> bool {
    if disabled.contains(name) {
        return true;
    }
    if let Some((namespace, _)) = name.split_once('_') {
        return disabled.contains(namespace);
    }
    false
}

#[cfg(test)]
mod disabled_tests {
    use super::is_tool_disabled;
    use std::collections::HashSet;

    #[test]
    fn group_prefix_disables_every_tool_in_its_namespace() {
        let disabled: HashSet<String> = ["lsp".to_string(), "bash_run".to_string()]
            .into_iter()
            .collect();
        let empty: HashSet<String> = HashSet::new();
        assert!(is_tool_disabled("lsp_diagnostics", &disabled));
        assert!(is_tool_disabled("lsp_rename", &disabled));
        assert!(is_tool_disabled("bash_run", &disabled));
        assert!(!is_tool_disabled("fs_read", &disabled));
        assert!(!is_tool_disabled("lsp", &empty));
        assert!(!is_tool_disabled("subagent_call", &disabled));
    }
}

fn write_tool_if_enabled(
    out: &mut String,
    disabled: &HashSet<String>,
    desc: &serde_json::Value,
    include_schema: bool,
) {
    let name = desc["name"].as_str().unwrap_or_default();
    if !is_tool_disabled(name, disabled) {
        write_single_tool(out, desc, include_schema);
    }
}

/// Build the language-server wrapper bound to `cwd`.
///
/// Shares the process-wide singleton ([`crate::harness::lsp::global_lsp`]) with
/// the passive diagnostics path — one server process per language, not one per
/// wrapper. Returns `None` when `COSH_LSP=off|0|false`, the config-driven LSP
/// switch is off, no tokio runtime is active, or under `cfg(test)`.
fn build_lsp(cwd: &str) -> Option<Arc<Lsp>> {
    #[cfg(test)]
    {
        let _ = cwd;
        None
    }
    #[cfg(not(test))]
    {
        if !crate::harness::lsp::lsp_enabled()
            || matches!(
                std::env::var("COSH_LSP").as_deref(),
                Ok("off" | "0" | "false")
            )
        {
            return None;
        }
        crate::harness::lsp::global_lsp(cwd)
    }
}

impl Tools for CoshTools {
    fn write_tool_descriptions(&self, out: &mut String) {
        // This trait method always renders the full schema (the harness uses
        // the `include_schema`-aware writers on `CoshTools` directly).
        write_single_tool(out, &self.bash.description_run, true);
        write_single_tool(out, &self.fs.description_read, true);
        write_single_tool(out, &self.fs.description_write, true);
        write_single_tool(out, &self.fs.description_edit, true);
        write_single_tool(out, &self.fs.description_edit_lines, true);
        write_single_tool(out, &self.fs.description_ast_edit, true);
        write_single_tool(out, &self.fs.description_rollback, true);
        write_single_tool(out, &self.find.description_glob, true);
        write_single_tool(out, &self.find.description_grep, true);
        write_single_tool(out, &self.web.description_fetch, true);
        write_single_tool(out, &self.web.description_search, true);
        {
            let plan = self.plan.lock().unwrap();
            write_single_tool(out, &plan.description_todo_write, true);
        }
        write_single_tool(out, &self.question.description_ask, true);
        #[cfg(feature = "embed")]
        write_single_tool(out, &self.recall.description_search, true);
        write_single_tool(out, &self.subagent.description_call, true);
        write_single_tool(out, &self.skills.description_list, true);
        write_single_tool(out, &self.skills.description_read, true);
        write_single_tool(out, &self.skills.description_read_asset, true);
        write_single_tool(out, &self.skills.description_match_skills, true);
        write_single_tool(out, &self.computer.description_apps, true);
        write_single_tool(out, &self.computer.description_snapshot, true);
        write_single_tool(out, &self.computer.description_wait, true);
        write_single_tool(out, &self.computer.description_screenshot, true);
        write_single_tool(out, &self.computer.description_act, true);
        write_single_tool(out, &self.computer.description_control, true);
        if let Some(lsp) = &self.lsp {
            write_single_tool(out, &lsp.description_definitions, true);
            write_single_tool(out, &lsp.description_references, true);
            write_single_tool(out, &lsp.description_symbols, true);
            write_single_tool(out, &lsp.description_hover, true);
            write_single_tool(out, &lsp.description_workspace_symbols, true);
            write_single_tool(out, &lsp.description_rename, true);
            write_single_tool(out, &lsp.description_call_hierarchy, true);
            write_single_tool(out, &lsp.description_restart, true);
            write_single_tool(out, &lsp.description_code_actions, true);
        }
    }

    fn tool_descriptions(&self) -> Vec<serde_json::Value> {
        let mut v = vec![
            self.bash.description_run.clone(),
            self.fs.description_read.clone(),
            self.fs.description_write.clone(),
            self.fs.description_edit.clone(),
            self.fs.description_edit_lines.clone(),
            self.fs.description_ast_edit.clone(),
            self.fs.description_rollback.clone(),
            self.find.description_glob.clone(),
            self.find.description_grep.clone(),
            self.web.description_fetch.clone(),
            self.web.description_search.clone(),
        ];
        {
            let plan = self.plan.lock().unwrap();
            v.push(plan.description_todo_write.clone());
        }
        v.push(self.question.description_ask.clone());
        #[cfg(feature = "embed")]
        v.push(self.recall.description_search.clone());
        v.push(self.subagent.description_call.clone());
        v.push(self.skills.description_list.clone());
        v.push(self.skills.description_read.clone());
        v.push(self.skills.description_read_asset.clone());
        v.push(self.skills.description_match_skills.clone());
        v.push(self.computer.description_apps.clone());
        v.push(self.computer.description_snapshot.clone());
        v.push(self.computer.description_wait.clone());
        v.push(self.computer.description_screenshot.clone());
        v.push(self.computer.description_act.clone());
        v.push(self.computer.description_control.clone());
        if let Some(lsp) = &self.lsp {
            v.push(lsp.description_definitions.clone());
            v.push(lsp.description_references.clone());
            v.push(lsp.description_symbols.clone());
            v.push(lsp.description_hover.clone());
            v.push(lsp.description_workspace_symbols.clone());
            v.push(lsp.description_rename.clone());
            v.push(lsp.description_call_hierarchy.clone());
            v.push(lsp.description_restart.clone());
            v.push(lsp.description_code_actions.clone());
        }
        v
    }

    fn schemas(&self) -> Vec<ToolSchema> {
        let mut v = vec![
            extract_schema(&self.bash.description_run),
            extract_schema(&self.fs.description_read),
            extract_schema(&self.fs.description_write),
            extract_schema(&self.fs.description_edit),
            extract_schema(&self.fs.description_rollback),
            extract_schema(&self.find.description_glob),
            extract_schema(&self.find.description_grep),
            extract_schema(&self.web.description_fetch),
            extract_schema(&self.web.description_search),
        ];
        {
            let plan = self.plan.lock().unwrap();
            v.push(extract_schema(&plan.description_todo_write));
        }
        v.push(extract_schema(&self.question.description_ask));
        #[cfg(feature = "embed")]
        v.push(extract_schema(&self.recall.description_search));
        v.push(extract_schema(&self.subagent.description_call));
        v.push(extract_schema(&self.skills.description_list));
        v.push(extract_schema(&self.skills.description_read));
        v.push(extract_schema(&self.skills.description_read_asset));
        v.push(extract_schema(&self.skills.description_match_skills));
        v.push(extract_schema(&self.computer.description_apps));
        v.push(extract_schema(&self.computer.description_snapshot));
        v.push(extract_schema(&self.computer.description_wait));
        v.push(extract_schema(&self.computer.description_screenshot));
        v.push(extract_schema(&self.computer.description_act));
        v.push(extract_schema(&self.computer.description_control));
        if let Some(lsp) = &self.lsp {
            v.push(extract_schema(&lsp.description_definitions));
            v.push(extract_schema(&lsp.description_references));
            v.push(extract_schema(&lsp.description_symbols));
            v.push(extract_schema(&lsp.description_hover));
            v.push(extract_schema(&lsp.description_workspace_symbols));
            v.push(extract_schema(&lsp.description_rename));
            v.push(extract_schema(&lsp.description_call_hierarchy));
            v.push(extract_schema(&lsp.description_restart));
            v.push(extract_schema(&lsp.description_code_actions));
        }
        v
    }

    async fn dispatch(&self, name: &str, args: serde_json::Value) -> Result<String, String> {
        match name {
            "bash_run" => {
                let input: BashRunInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                // `timeout_ms` is optional and validated inside the tool: it
                // may only raise the configured timeout for this call.
                let stream = self
                    .bash
                    .run_with_timeout(&input.command, input.timeout_ms)
                    .map_err(|e| {
                        e.text_err.unwrap_or_else(|| {
                            format!(
                                "exec error (signal={:?})",
                                e.exec_err.as_ref().map(|ee| ee.signal)
                            )
                        })
                    })?;
                tokio::pin!(stream);

                let mut output = String::new();
                // Raw-byte carry between PTY chunks. A PTY read is a raw
                // 4096-byte slice with no message framing: escape sequences,
                // multi-byte UTF-8 chars and CRLF pairs can split across two
                // chunks, and stripping per-chunk with a fresh parser would
                // corrupt them (half-swallowed escapes, U+FFFD garbage,
                // stray \r). We only clean up to the last COMPLETE line and
                // carry the unfinished tail for the next chunk.
                let mut carry: Vec<u8> = Vec::new();

                while let Some(chunk) = stream.next().await {
                    // PTY mode multiplexes stdout+stderr into `stdout` with
                    // ANSI escapes and CRLF line endings; clean complete
                    // lines before streaming them to the TUI and accumulating
                    // them for the model.
                    if !chunk.stdout.is_empty() {
                        carry.extend_from_slice(&chunk.stdout);
                        // Split after the last `\n` (or at 0 when no
                        // complete line arrived yet).
                        let split = carry.iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
                        let complete: Vec<u8> = carry[..split].to_vec();
                        carry.drain(..split);

                        let text = cosh_tools::bash::strip_ansi(&complete);
                        if !text.is_empty() {
                            if let Some(ref tx) = self.event_tx {
                                let _ = tx.send(HarnessEvent::ToolOutput {
                                    tool: "bash_run".to_string(),
                                    output: text.clone(),
                                    finished: false,
                                });
                            }
                            output.push_str(&text);
                        }
                    }

                    // The piped path (Bash built without `.pty(true)`) still
                    // delivers stderr separately, raw and escape-free. The
                    // PTY path only lands here for the synthetic
                    // `bash stream error:` items (see `bsh::run`).
                    if !chunk.stderr.is_empty() {
                        let text = String::from_utf8_lossy(&chunk.stderr).to_string();
                        if let Some(ref tx) = self.event_tx {
                            let _ = tx.send(HarnessEvent::ToolOutput {
                                tool: "bash_run".to_string(),
                                output: text.clone(),
                                finished: false,
                            });
                        }
                        output.push_str(&text);
                    }

                    // Flush the unfinished tail BEFORE the exit-code/signal
                    // annotations: a command whose last line lacks `\n` and
                    // exits non-zero must read `...tail\nexit code: N`, not
                    // `exit code: N...tail`.
                    if !carry.is_empty() {
                        let text = cosh_tools::bash::strip_ansi(&carry);
                        carry.clear();
                        if !text.is_empty() {
                            if let Some(ref tx) = self.event_tx {
                                let _ = tx.send(HarnessEvent::ToolOutput {
                                    tool: "bash_run".to_string(),
                                    output: text.clone(),
                                    finished: false,
                                });
                            }
                            output.push_str(&text);
                        }
                    }

                    // Append non-zero exit code
                    if let Some(code) = chunk.exit_code
                        && code != 0
                    {
                        if !output.is_empty() && !output.ends_with('\n') {
                            output.push('\n');
                        }
                        let _ = write!(output, "exit code: {code}");
                    }

                    // Append signal info
                    if let Some(sig) = chunk.signal {
                        if !output.is_empty() && !output.ends_with('\n') {
                            output.push('\n');
                        }
                        let _ = write!(output, "signal: {sig}");
                    }
                }

                // Keep the model context lean: outputs above the token budget
                // are head/tail-truncated with the middle saved to a scratch
                // log (see `truncate`). The TUI already received the full
                // output as a live stream above, so the user is unaffected.
                let truncated = truncate_tool_output(&output);
                Ok(truncated.text)
            }

            "fs_read" => {
                // Advertised single-target shape: a flat {path, offset?,
                // limit?, symbol?}. The legacy `targets` batch form is
                // kept for a single element (like `fs_write`) — batched reads
                // raised schema-error rates in agent sessions.
                let targets: Vec<Target> = if args.get("path").is_some() {
                    if args.get("targets").is_some() {
                        // Mixed shapes are ambiguous, mirroring fs_write's
                        // guard: a silent "flat wins" choice would read one
                        // file when the model asked for a batch.
                        return Err("fs_read accepts ONE file per call: provide either a flat \
                             {path, offset?, limit?, symbol?} or the legacy single-element \
                             {targets: [...]}, never both."
                            .to_string());
                    }
                    vec![serde_json::from_value(args.clone()).map_err(|e| e.to_string())?]
                } else {
                    let parsed: Vec<Target> = serde_json::from_value(
                        args.get("targets")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    )
                    .map_err(|e| e.to_string())?;
                    if parsed.len() > 1 {
                        return Err(
                            "fs_read accepts ONE file per call: {path, offset?, limit?, \
                             symbol?}. Issue one call per file."
                                .to_string(),
                        );
                    }
                    parsed
                };
                let results = self.fs.read(targets).await;
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_write" => {
                // Advertised single-file shape: a flat {path, content,
                // file_hash?}. The multi-file `targets` batch form is kept
                // (like `fs_edit`) but only for a single legacy target —
                // batched writes raised schema-error rates in agent sessions.
                let targets: Vec<TargetFile> = if args.get("path").is_some() {
                    if args.get("targets").is_some() {
                        // Mixed shapes are ambiguous: the edit tools reject
                        // them the same way (one shape per call). A silent
                        // "flat wins" choice would
                        // write one file when the model asked for a batch.
                        return Err("fs_write accepts ONE file per call: provide either a flat \
                             {path, content, file_hash?} or the legacy single-element \
                             {targets: [...]}, never both."
                            .to_string());
                    }
                    vec![serde_json::from_value(args.clone()).map_err(|e| e.to_string())?]
                } else {
                    let parsed: Vec<TargetFile> = serde_json::from_value(
                        args.get("targets")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    )
                    .map_err(|e| e.to_string())?;
                    if parsed.len() > 1 {
                        return Err(
                            "fs_write accepts ONE file per call: {path, content, file_hash?}. \
                             Issue one call per file."
                                .to_string(),
                        );
                    }
                    parsed
                };
                let results = self.fs.write(targets).await?;
                self.emit_lsp_notes(
                    "fs_write",
                    results.iter().filter_map(|r| r.lsp_notes.as_ref()),
                );
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_edit" | "fs_edit_lines" | "fs_ast_edit" => {
                let results = match name {
                    "fs_edit" => self.fs.edit(args).await?,
                    "fs_edit_lines" => self.fs.edit_lines(args).await?,
                    _ => self.fs.edit_ast(args).await?,
                };
                self.emit_lsp_notes(name, results.iter().filter_map(|r| r.lsp_notes.as_ref()));
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_rollback" => {
                let input: FsRollbackInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let result = self.fs.rollback(&input.path, &input.hash).await?;
                self.emit_lsp_notes("fs_rollback", result.lsp_notes.as_ref().into_iter());
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "find_glob" => {
                let pattern = args["pattern"]
                    .as_str()
                    .ok_or_else(|| "missing 'pattern'".to_string())?;
                let mut path = args.get("path").and_then(|v| v.as_str()).map(String::from);
                let paths = args.get("paths").and_then(|v| v.as_array()).map(|list| {
                    list.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect::<Vec<String>>()
                });
                if path.is_none() && paths.as_ref().is_none_or(Vec::is_empty) {
                    // Default to the workspace root (CWD) so a single pattern
                    // never needs an explicit `path`.
                    path = Some(".".to_string());
                }
                let max_results =
                    match args.get("max_results") {
                        Some(v) => Some(
                            u32::try_from(v.as_u64().ok_or_else(|| {
                                "max_results must be a positive integer".to_string()
                            })?)
                            .map_err(|_| "max_results must be a positive integer".to_string())?,
                        ),
                        None => None,
                    };
                let format = args
                    .get("format")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let file_type = args
                    .get("file_type")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let hidden = args.get("hidden").and_then(|v| v.as_bool());
                let gitignore = args.get("gitignore").and_then(|v| v.as_bool());
                let sort_by_mtime = args.get("sort_by_mtime").and_then(|v| v.as_bool());
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u32::try_from(v).ok());
                // Stream each match live to the TUI while the scan runs,
                // batched so a huge tree does not flood the event channel.
                let batcher = self
                    .event_tx
                    .clone()
                    .map(|tx| Arc::new(Mutex::new(StreamBatcher::new(tx, "find_glob"))));
                let on_match: Option<Arc<GlobMatchCallback>> = batcher.as_ref().map(|batcher| {
                    let batcher = batcher.clone();
                    let cb: Arc<GlobMatchCallback> = Arc::new(move |m: &GlobMatch| {
                        batcher
                            .lock()
                            .expect("stream batcher lock poisoned")
                            .push(m.path.clone());
                    });
                    cb
                });
                let result = self.find.glob_full(
                    pattern,
                    path,
                    paths,
                    GlobCallOptions {
                        file_type,
                        hidden,
                        gitignore,
                        max_results,
                        format,
                        sort_by_mtime,
                        timeout_ms,
                    },
                    on_match,
                );
                // Flush the tail even on error so the last streamed matches are
                // not lost when the scan fails at the very end.
                if let Some(batcher) = batcher {
                    batcher
                        .lock()
                        .expect("stream batcher lock poisoned")
                        .flush();
                }
                let result = result?;
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "find_grep" => {
                let pattern = args["pattern"]
                    .as_str()
                    .ok_or_else(|| "missing 'pattern'".to_string())?;
                let path = args.get("path").and_then(|v| v.as_str()).map(String::from);
                let paths = args.get("paths").and_then(|v| v.as_array()).map(|list| {
                    list.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect::<Vec<String>>()
                });
                if path.is_none() && paths.as_ref().is_none_or(Vec::is_empty) {
                    return Err("missing 'path' or 'paths'".to_string());
                }
                let skip = args
                    .get("skip")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u32::try_from(v).ok());
                let line_range = args
                    .get("line_range")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                // Stream each match live to the TUI while the search runs,
                // batched so a huge tree does not flood the event channel.
                let batcher = self
                    .event_tx
                    .clone()
                    .map(|tx| Arc::new(Mutex::new(StreamBatcher::new(tx, "find_grep"))));
                let on_match: Option<Arc<GrepMatchCallback>> = batcher.as_ref().map(|batcher| {
                    let batcher = batcher.clone();
                    let cb: Arc<GrepMatchCallback> = Arc::new(move |m: &GrepMatch| {
                        batcher
                            .lock()
                            .expect("stream batcher lock poisoned")
                            .push(m.path.clone());
                    });
                    cb
                });
                let result = self
                    .find
                    .grep_with_streaming(pattern, path, paths, skip, line_range, on_match);
                // Flush the tail even on error so the last streamed matches are
                // not lost when the search fails at the very end.
                if let Some(batcher) = batcher {
                    batcher
                        .lock()
                        .expect("stream batcher lock poisoned")
                        .flush();
                }
                let result = result?;
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "web_fetch" => {
                let input: WebFetch = serde_json::from_value(args).map_err(|e| e.to_string())?;
                let text = self.web.fetch(input).await?;
                // Keep the model context lean: only ridiculously dense pages
                // are head/tail-truncated (generous budget, middle → scratch
                // log — see `truncate`); normal articles pass through whole.
                let truncated = truncate_tool_output_with_budget(&text, WEB_FETCH_MAX_TOKENS);
                Ok(truncated.text)
            }

            "web_search" => {
                let input: WebSearchInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.web.search(&input.query).await
            }

            "plan_todo_write" => {
                let input: TodoWriteInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let mut plan = self.plan.lock().unwrap();
                let output = plan.todo_write(&input.todos).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "skills_list" => {
                let output = self.skills.list().map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "skills_read" => {
                let input: SkillsReadInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.skills.read(input.name).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "skills_read_asset" => {
                let input: SkillsReadAssetInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self
                    .skills
                    .read_asset(input.name, input.asset_path)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "skills_match_skills" => {
                let input: SkillsMatchInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self
                    .skills
                    .match_skills(input.match_paths)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "lsp_definitions" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::DefinitionsInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.definitions(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_references" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::ReferencesInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.references(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_symbols" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::SymbolsInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.symbols(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_hover" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::HoverInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.hover(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_workspace_symbols" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::WorkspaceSymbolsInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp
                    .workspace_symbols(&input)
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_rename" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::RenameInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.rename(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_call_hierarchy" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::CallHierarchyInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp
                    .call_hierarchy(&input)
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_code_actions" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::CodeActionsInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.code_actions(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }
            "lsp_restart" => {
                let Some(lsp) = &self.lsp else {
                    return Err("LSP tooling is disabled".into());
                };
                let input: cosh_tools::lsp::types::RestartInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = lsp.restart(&input).await.map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "subagent_call" => {
                let input: SubAgentCallInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                // The harness routes `subagent_call` calls with a missing or
                // empty `agent` to the INTERNAL sub-agent before reaching this
                // arm, so reaching here means an external ACP harness was
                // requested.
                let agent = input
                    .agent
                    .clone()
                    .ok_or_else(|| "missing 'agent'".to_string())?;
                // Resolve the effective input before driving the ACP turn:
                // when `input` is omitted, reuse the last message sent to a
                // sub-agent in this session (stored on `self.subagent`).
                let call_input = self.subagent.resolve_input(input.input)?;
                // Session resume (Phase 4): consecutive calls to the same
                // agent share the ACP session by default (the sub-agent
                // keeps its context); `continue_session: false` opts out.
                // `resume_id` maps the flag to the stored id (or `None`),
                // and the acp layer still falls back to a fresh session
                // when the harness lost it. Safe by the sequential-dispatch
                // invariant documented on `SubAgent::last_session`.
                let resume = self.subagent.resume_id(&agent, input.continue_session);
                // Review tasks carry the severity contract: the sub-agent's
                // FINAL REPORT (its last message, post-tool-calls — see
                // `subagent::closure`) must start with
                // `<!-- severity: green|yellow|red -->`. The contract is
                // appended AFTER the stored-input resolution so a retry
                // (reused raw input) never double-appends it.
                let call_input = cosh_tools::subagent::severity::with_severity_contract(
                    &call_input,
                    input.code_review,
                );
                let event_tx_during = self.event_tx.clone();
                // The ACP session is rooted at the workspace directory.
                let cwd = self.project_root().clone();
                // Cancellation (Phase 5): share the agent loop's stop flag so
                // ESC interrupts the remote turn via `session/cancel`. Tools
                // dispatched outside an agent loop (or before the flag is
                // wired) run to completion — a fresh flag can never fire.
                let stop_signal = self
                    .stop_signal
                    .clone()
                    .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

                let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<
                    cosh_tools::subagent::events::SubagentEvent,
                >();

                // `agent` is moved into the spawned turn below; the clone
                // records the successful session id under the same name
                // afterwards.
                let stored_agent = agent.clone();

                let mut call_handle = tokio::task::spawn(async move {
                    cosh_tools::subagent::acp::call(
                        &agent,
                        &call_input,
                        resume,
                        cwd,
                        stop_signal,
                        chunk_tx,
                    )
                    .await
                });

                // Stream typed sub-agent events while waiting for the ACP
                // turn to complete. ALL events go to the typed variant for
                // the TUI's sub-agent box — message text is part of the
                // box's chronological mini-chat timeline (Phase 3b.4), so
                // there is no per-chunk `ToolOutput` mirror (it would
                // render the text twice). One final `ToolOutput { finished:
                // true }` below still seeds the persisted report.
                let call_result = loop {
                    tokio::select! {
                        result = &mut call_handle => {
                            break result;
                        }
                        event = chunk_rx.recv() => {
                            if let Some(event) = event
                                && let Some(ref tx) = event_tx_during {
                                    // Message text renders INSIDE the sub-agent
                                    // box (the chronological mini-chat timeline,
                                    // Phase 3b.4) — the legacy `ToolOutput`
                                    // mirror that used to stream it into the
                                    // markdown body is gone, or the text would
                                    // appear twice.
                                    let _ = tx.send(HarnessEvent::SubagentEvent {
                                        tool: "subagent_call".to_string(),
                                        event,
                                    });
                                }
                        }
                    }
                };

                let (accumulated, stop_reason, session_id) =
                    call_result.map_err(|e| e.to_string())??;
                // A successful turn's session is the one the next
                // `continue_session` call resumes (Phase 4). A failed turn
                // returns `None` here, so nothing stale is stored.
                if let Some(session_id) = session_id {
                    self.subagent.store_session(&stored_agent, session_id);
                }

                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(HarnessEvent::ToolOutput {
                        tool: "subagent_call".to_string(),
                        output: accumulated.clone(),
                        finished: true,
                    });
                }

                let result = SubAgentCallOutput {
                    output: accumulated,
                    stop_reason,
                };
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "ask_questions" => {
                let input: QuestionInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.question.ask(&input)?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            #[cfg(feature = "embed")]
            "recall_search" => {
                // The model only provides db_name + query + limit.
                // The harness resolves the DB, embeds the query, and searches.
                let db_name = args["db_name"]
                    .as_str()
                    .ok_or_else(|| "missing 'db_name'".to_string())?;
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| "missing 'query'".to_string())?;
                let limit = args["limit"].as_u64().unwrap_or(5) as usize;

                // Find the database in the registry
                let db = self
                    .recall_dbs
                    .iter()
                    .find(|d| d.name == db_name)
                    .ok_or_else(|| {
                        format!(
                            "Database '{db_name}' not found. Active databases: {}",
                            self.recall_dbs
                                .iter()
                                .map(|d| d.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;

                // Embed the query and search — requires fastembed or cloud feature
                let output =
                    dispatch_recall_search(db, query, limit, &self.local_base_urls).await?;
                Ok(output)
            }

            "computer_apps" => {
                let input: ComputerApps =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.apps(&input).await?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "computer_snapshot" => {
                let input: ComputerSnapshot =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.snapshot(&input).await?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "computer_wait" => {
                let input: ComputerWait =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.wait(&input).await?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "computer_screenshot" => {
                let input: ComputerScreenshot =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.screenshot(&input).await?;
                // The PNG travels as a staged image payload (base64
                // ImageBlocks): `dispatch` takes `&self`, so the bytes are
                // parked here and drained by the harness right after this
                // `Ok` — see `take_pending_images` in `core.rs` — before
                // being lowered into the tool-result `ChatMessage`. The
                // JSON the model parses for coordinates never carries them.
                *self
                    .pending_images
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    (!output.images.is_empty()).then(|| output.images.clone());
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "computer_act" => {
                let input: ComputerAct = serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.act(&input).await?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "computer_control" => {
                let input: ComputerControl =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.computer.control(&input).await?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            _ => Err(format!("unknown cosh tool: {name}")),
        }
    }
}

#[cfg(test)]
mod skills_sources_tests {
    use super::default_skill_sources_for;

    /// The `~/.skills` default is only registered when the directory
    /// exists — a missing default must not become a discovery error.
    #[test]
    fn missing_default_skills_dir_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        assert!(default_skill_sources_for(Some(dir.path().to_str().unwrap())).is_empty());
        assert!(default_skill_sources_for(None).is_empty());
    }

    /// An existing `~/.skills` directory registers exactly one source.
    #[test]
    fn existing_default_skills_dir_registers_one_source() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".skills")).unwrap();
        let sources = default_skill_sources_for(Some(home.to_str().unwrap()));
        assert_eq!(sources.len(), 1);
    }

    /// Regression (Windows): a HOME inherited from Git Bash uses MSYS
    /// drive syntax ("/c/Users/...") which native fs calls cannot resolve.
    /// The resolver must normalize it, or the default source is silently
    /// dropped and the skills tools report an empty list.
    #[cfg(windows)]
    #[test]
    fn msys_style_home_is_normalized_before_the_existence_probe() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join(".skills")).unwrap();

        // Re-spell the native temp path ("C:/...") as MSYS ("/c/...").
        let mut chars = home.chars();
        let drive = chars.next().unwrap().to_ascii_lowercase();
        assert!(
            drive.is_ascii_alphabetic(),
            "tempdir must yield a plain drive path"
        );
        let rest = chars
            .collect::<String>()
            .trim_start_matches([':', '/', '\\'])
            .to_string();
        let msys_home = format!("/{drive}/{rest}");

        let sources = default_skill_sources_for(Some(&msys_home));
        assert_eq!(sources.len(), 1, "MSYS-spelled home must resolve natively");
        let cosh_tools::skills::SkillSource::Directory { path } = &sources[0] else {
            panic!("expected a Directory source");
        };
        assert!(
            std::path::Path::new(path).is_dir(),
            "source must be the native spelling"
        );
    }
}

#[cfg(test)]
mod stream_batcher_tests {
    use tokio::sync::mpsc;

    use super::HarnessEvent;

    #[test]
    fn flush_drains_tail_into_single_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut batcher = super::StreamBatcher::new(tx.clone(), "find_glob");
        batcher.push("src/a.rs".into());
        batcher.push("src/b.rs".into());
        batcher.flush();

        let event = rx.try_recv().expect("flush must emit the buffered lines");
        assert!(
            rx.try_recv().is_err(),
            "one flush must emit exactly one event"
        );
        let HarnessEvent::ToolOutput {
            tool,
            output,
            finished,
        } = event
        else {
            panic!("expected ToolOutput event");
        };
        assert_eq!(tool, "find_glob");
        assert!(!finished, "streamed chunks are never terminal events");
        assert_eq!(output, "src/a.rs\nsrc/b.rs\n");
        assert!(batcher.lines.is_empty(), "flush must empty the buffer");
    }

    #[test]
    fn flush_with_empty_buffer_sends_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut batcher = super::StreamBatcher::new(tx, "find_grep");
        batcher.flush();
        assert!(
            rx.try_recv().is_err(),
            "an empty flush must not emit an event"
        );
    }

    #[test]
    fn push_auto_flushes_when_batch_cap_is_reached() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut batcher = super::StreamBatcher::new(tx.clone(), "find_glob");
        assert!(rx.try_recv().is_err());

        for i in 0..super::StreamBatcher::MAX_LINES_PER_FLUSH {
            batcher.push(format!("f{i}.rs"));
        }
        let event = rx.try_recv().expect("the cap must auto-flush a batch");
        let HarnessEvent::ToolOutput { output, .. } = event else {
            panic!("expected ToolOutput event");
        };
        assert_eq!(
            output.lines().count(),
            super::StreamBatcher::MAX_LINES_PER_FLUSH,
            "a full batch must be sent as one event"
        );
        assert!(
            rx.try_recv().is_err(),
            "no extra event before the next flush"
        );
        assert!(batcher.lines.is_empty());
    }
}
