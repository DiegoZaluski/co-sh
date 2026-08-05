use std::collections::HashSet;
use std::fmt::Write;
use std::sync::Mutex;

use super::events::HarnessEvent;
use cosh_sdk::extract_action::ToolSchema;
use cosh_tools::{
    bash::{Bash, BashRunInput},
    find::Find,
    fs::{EditTarget, Fs, FsRollbackInput, Target, TargetFile},
    plan::{
        Plan,
        types::{
            TodoCrossOffInput, TodoEditInput, TodoLoadFromMdInput, TodoReadInput, TodoWriteInput,
        },
    },
    question::{Question, types::QuestionInput},
    skills::{
        Skills,
        types::{SkillsMatchInput, SkillsReadAssetInput, SkillsReadInput},
    },
    subagent::{
        SubAgent,
        types::{SubAgentCallInput, SubAgentCallOutput},
    },
    vision::{TerminalInput, Vision},
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

pub struct CoshTools {
    bash: Bash,
    fs: Fs,
    find: Find,
    web: Web,
    vision: Vision,
    plan: Mutex<Plan>,
    question: Question,
    #[cfg(feature = "embed")]
    recall: Recall,
    #[cfg(feature = "embed")]
    recall_dbs: Vec<RecallDb>,
    skills: Skills,
    subagent: SubAgent,
    /// Optional event sender for streaming tool output.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<HarnessEvent>>,
}

impl CoshTools {
    #[must_use]
    pub fn new(cwd: &str) -> Self {
        Self {
            bash: Bash::new().cwd(cwd),
            fs: Fs::new().cwd(cwd),
            find: Find::new().cwd(cwd),
            web: Web::new(),
            vision: Vision::new(),
            plan: Mutex::new(Plan::new()),
            question: Question::new(),
            #[cfg(feature = "embed")]
            recall: Recall::new(),
            #[cfg(feature = "embed")]
            recall_dbs: Vec::new(),
            skills: Skills::new(),
            subagent: SubAgent::new(),
            event_tx: None,
        }
    }

    /// Get the project root directory (used for path validation).
    #[must_use]
    pub fn project_root(&self) -> &std::path::PathBuf {
        self.fs.root()
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
        {
            let plan = self.plan.lock().unwrap();
            v.push(plan.description_todo_read.clone());
            v.push(plan.description_load_from_md.clone());
        }
        v.push(self.question.description_ask.clone());
        #[cfg(feature = "embed")]
        v.push(self.recall.description_search.clone());
        v.push(self.skills.description_list.clone());
        v.push(self.skills.description_read.clone());
        v.push(self.skills.description_read_asset.clone());
        v.push(self.skills.description_match_skills.clone());
        v.into_iter()
            .filter(|desc| {
                let name = desc["name"].as_str().unwrap_or_default();
                !disabled_tools.contains(name)
            })
            .collect()
    }

    /// All tool descriptions, skipping disabled ones.
    pub fn write_tool_descriptions_enabled(
        &self,
        out: &mut String,
        disabled_tools: &HashSet<String>,
    ) {
        let all = self.tool_descriptions();
        for desc in all {
            let name = desc["name"].as_str().unwrap_or_default();
            if disabled_tools.contains(name) {
                continue;
            }
            let description = desc["description"].as_str().unwrap_or_default();
            let schema = serde_json::to_string_pretty(&desc["inputSchema"]).unwrap_or_default();
            let _ = write!(out, "- **{name}**: {description}\n  Schema: {schema}\n");
        }
    }

    /// Tool descriptions restricted to read-only and search tools (Ask mode).
    ///
    /// # Panics
    ///
    /// Panics if the internal `plan` mutex is poisoned.
    pub fn write_tool_descriptions_filtered(
        &self,
        out: &mut String,
        disabled_tools: &HashSet<String>,
    ) {
        write_tool_if_enabled(out, disabled_tools, &self.fs.description_read);
        write_tool_if_enabled(out, disabled_tools, &self.find.description_glob);
        write_tool_if_enabled(out, disabled_tools, &self.find.description_grep);
        write_tool_if_enabled(out, disabled_tools, &self.web.description_fetch);
        write_tool_if_enabled(out, disabled_tools, &self.web.description_search);
        {
            let plan = self.plan.lock().unwrap();
            write_tool_if_enabled(out, disabled_tools, &plan.description_todo_read);
            write_tool_if_enabled(out, disabled_tools, &plan.description_load_from_md);
        }
        write_tool_if_enabled(out, disabled_tools, &self.question.description_ask);
        #[cfg(feature = "embed")]
        write_tool_if_enabled(out, disabled_tools, &self.recall.description_search);
        write_tool_if_enabled(out, disabled_tools, &self.skills.description_list);
        write_tool_if_enabled(out, disabled_tools, &self.skills.description_read);
        write_tool_if_enabled(out, disabled_tools, &self.skills.description_read_asset);
        write_tool_if_enabled(out, disabled_tools, &self.skills.description_match_skills);
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
                !disabled_tools.contains(name)
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
        {
            let plan = self.plan.lock().unwrap();
            v.push(extract_schema(&plan.description_todo_read));
            v.push(extract_schema(&plan.description_load_from_md));
        }
        v.push(extract_schema(&self.question.description_ask));
        #[cfg(feature = "embed")]
        v.push(extract_schema(&self.recall.description_search));
        v.push(extract_schema(&self.skills.description_list));
        v.push(extract_schema(&self.skills.description_read));
        v.push(extract_schema(&self.skills.description_read_asset));
        v.push(extract_schema(&self.skills.description_match_skills));
        v.into_iter()
            .filter(|schema| !disabled_tools.contains(&schema.name))
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
            let connector = cosh_sdk::connector::Connector::new(provider)
                .map_err(|e| format!("connector error: {e}"))?
                .with_model(model);
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
) -> Result<String, String> {
    // 1. Create embedder from the DB's config
    let embedder = config_to_embedder(&db.embedder)?;

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
    }
}

fn write_single_tool(out: &mut String, desc: &serde_json::Value) {
    let name = desc["name"].as_str().unwrap_or_default();
    let description = desc["description"].as_str().unwrap_or_default();
    let schema = serde_json::to_string_pretty(&desc["inputSchema"]).unwrap_or_default();
    let _ = write!(out, "- **{name}**: {description}\n  Schema: {schema}\n");
}

/// Write a single tool description only if its name is not in the disabled set.
fn write_tool_if_enabled(out: &mut String, disabled: &HashSet<String>, desc: &serde_json::Value) {
    let name = desc["name"].as_str().unwrap_or_default();
    if !disabled.contains(name) {
        write_single_tool(out, desc);
    }
}

impl Tools for CoshTools {
    fn write_tool_descriptions(&self, out: &mut String) {
        write_single_tool(out, &self.bash.description_run);
        write_single_tool(out, &self.fs.description_read);
        write_single_tool(out, &self.fs.description_write);
        write_single_tool(out, &self.fs.description_edit);
        write_single_tool(out, &self.fs.description_rollback);
        write_single_tool(out, &self.find.description_glob);
        write_single_tool(out, &self.find.description_grep);
        write_single_tool(out, &self.web.description_fetch);
        write_single_tool(out, &self.web.description_search);
        write_single_tool(out, &self.vision.description_terminal);
        {
            let plan = self.plan.lock().unwrap();
            write_single_tool(out, &plan.description_todo_write);
            write_single_tool(out, &plan.description_todo_edit);
            write_single_tool(out, &plan.description_todo_cross_off);
            write_single_tool(out, &plan.description_todo_read);
            write_single_tool(out, &plan.description_load_from_md);
        }
        write_single_tool(out, &self.question.description_ask);
        #[cfg(feature = "embed")]
        write_single_tool(out, &self.recall.description_search);
        write_single_tool(out, &self.subagent.description_call);
        write_single_tool(out, &self.skills.description_list);
        write_single_tool(out, &self.skills.description_read);
        write_single_tool(out, &self.skills.description_read_asset);
        write_single_tool(out, &self.skills.description_match_skills);
    }

    fn tool_descriptions(&self) -> Vec<serde_json::Value> {
        let mut v = vec![
            self.bash.description_run.clone(),
            self.fs.description_read.clone(),
            self.fs.description_write.clone(),
            self.fs.description_edit.clone(),
            self.fs.description_rollback.clone(),
            self.find.description_glob.clone(),
            self.find.description_grep.clone(),
            self.web.description_fetch.clone(),
            self.web.description_search.clone(),
            self.vision.description_terminal.clone(),
        ];
        {
            let plan = self.plan.lock().unwrap();
            v.push(plan.description_todo_write.clone());
            v.push(plan.description_todo_edit.clone());
            v.push(plan.description_todo_cross_off.clone());
            v.push(plan.description_todo_read.clone());
            v.push(plan.description_load_from_md.clone());
        }
        v.push(self.question.description_ask.clone());
        #[cfg(feature = "embed")]
        v.push(self.recall.description_search.clone());
        v.push(self.subagent.description_call.clone());
        v.push(self.skills.description_list.clone());
        v.push(self.skills.description_read.clone());
        v.push(self.skills.description_read_asset.clone());
        v.push(self.skills.description_match_skills.clone());
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
            extract_schema(&self.vision.description_terminal),
        ];
        {
            let plan = self.plan.lock().unwrap();
            v.push(extract_schema(&plan.description_todo_write));
            v.push(extract_schema(&plan.description_todo_edit));
            v.push(extract_schema(&plan.description_todo_cross_off));
            v.push(extract_schema(&plan.description_todo_read));
            v.push(extract_schema(&plan.description_load_from_md));
        }
        v.push(extract_schema(&self.question.description_ask));
        #[cfg(feature = "embed")]
        v.push(extract_schema(&self.recall.description_search));
        v.push(extract_schema(&self.subagent.description_call));
        v.push(extract_schema(&self.skills.description_list));
        v.push(extract_schema(&self.skills.description_read));
        v.push(extract_schema(&self.skills.description_read_asset));
        v.push(extract_schema(&self.skills.description_match_skills));
        v
    }

    async fn dispatch(&self, name: &str, args: serde_json::Value) -> Result<String, String> {
        match name {
            "bash_run" => {
                let input: BashRunInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let stream = self.bash.run(&input.command).map_err(|e| {
                    e.text_err.unwrap_or_else(|| {
                        format!(
                            "exec error (signal={:?})",
                            e.exec_err.as_ref().map(|ee| ee.signal)
                        )
                    })
                })?;
                tokio::pin!(stream);

                let mut output = String::new();

                while let Some(chunk) = stream.next().await {
                    // Stream clean stdout for PTY display
                    if !chunk.stdout.is_empty() {
                        let text = String::from_utf8_lossy(&chunk.stdout).to_string();
                        if let Some(ref tx) = self.event_tx {
                            let _ = tx.send(HarnessEvent::ToolOutput {
                                tool: "bash_run".to_string(),
                                output: text.clone(),
                                finished: false,
                            });
                        }
                        output.push_str(&text);
                    }

                    // Stream clean stderr for PTY display and accumulate
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

                Ok(output)
            }

            "fs_read" => {
                let targets: Vec<Target> =
                    serde_json::from_value(args["targets"].clone()).map_err(|e| e.to_string())?;
                let results = self.fs.read(targets).await;
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_write" => {
                let targets: Vec<TargetFile> =
                    serde_json::from_value(args["targets"].clone()).map_err(|e| e.to_string())?;
                let results = self.fs.write(targets).await?;
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_edit" => {
                let targets: Vec<EditTarget> =
                    serde_json::from_value(args["targets"].clone()).map_err(|e| e.to_string())?;
                let results = self.fs.edit(targets).await?;
                serde_json::to_string(&results).map_err(|e| e.to_string())
            }

            "fs_rollback" => {
                let input: FsRollbackInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let result = self.fs.rollback(&input.path, &input.hash).await?;
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "find_glob" => {
                let pattern = args["pattern"]
                    .as_str()
                    .ok_or_else(|| "missing 'pattern'".to_string())?;
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| "missing 'path'".to_string())?;
                let result = self.find.glob(pattern, path)?;
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "find_grep" => {
                let pattern = args["pattern"]
                    .as_str()
                    .ok_or_else(|| "missing 'pattern'".to_string())?;
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let paths = args
                    .get("paths")
                    .and_then(|v| v.as_array())
                    .map(|list| {
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
                let result =
                    self.find
                        .grep_with(pattern, path, paths, skip, line_range)?;
                serde_json::to_string(&result).map_err(|e| e.to_string())
            }

            "web_fetch" => {
                let input: WebFetch = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.web.fetch(input).await
            }

            "web_search" => {
                let input: WebSearchInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.web.search(&input.query).await
            }

            "vision_terminal" => {
                let input: TerminalInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.vision.terminal(&input)
            }

            "plan_todo_write" => {
                let input: TodoWriteInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let mut plan = self.plan.lock().unwrap();
                let output = plan.todo_write(&input.action).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "plan_todo_edit" => {
                let input: TodoEditInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let mut plan = self.plan.lock().unwrap();
                let output = plan.todo_edit(&input.edit).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "plan_todo_cross_off" => {
                let input: TodoCrossOffInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let mut plan = self.plan.lock().unwrap();
                let output = plan
                    .todo_cross_off(&input.action)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "plan_todo_read" => {
                let input: TodoReadInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let plan = self.plan.lock().unwrap();
                let output = plan.todo_read(&input.action).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            "plan_load_from_md" => {
                let input: TodoLoadFromMdInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let mut plan = self.plan.lock().unwrap();
                plan.load_from_md(&input.path).map_err(|e| e.to_string())?;
                Ok("ok".into())
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

            "subagent_call" => {
                let input: SubAgentCallInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let agent = input.agent.clone();
                let call_input = input.input.clone();
                let event_tx_during = self.event_tx.clone();

                let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

                let mut call_handle = tokio::task::spawn_blocking(move || {
                    cosh_tools::subagent::call::call(&agent, &call_input, chunk_tx)
                });

                // Stream chunks while waiting for the blocking call to complete.
                let call_result = loop {
                    tokio::select! {
                        result = &mut call_handle => {
                            break result;
                        }
                        chunk = chunk_rx.recv() => {
                            if let Some(c) = chunk
                                && let Some(ref tx) = event_tx_during {
                                    let _ = tx.send(HarnessEvent::ToolOutput {
                                        tool: "subagent_call".to_string(),
                                        output: c,
                                        finished: false,
                                    });
                                }
                        }
                    }
                };

                let (accumulated, exit_code) = call_result.map_err(|e| e.to_string())??;

                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(HarnessEvent::ToolOutput {
                        tool: "subagent_call".to_string(),
                        output: accumulated.clone(),
                        finished: true,
                    });
                }

                let result = SubAgentCallOutput {
                    output: accumulated,
                    exit_code,
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
                let output = dispatch_recall_search(db, query, limit).await?;
                Ok(output)
            }

            _ => Err(format!("unknown cosh tool: {name}")),
        }
    }
}
