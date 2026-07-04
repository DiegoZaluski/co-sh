use std::fmt::Write;
use std::sync::Mutex;

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
    vision::{TerminalInput, Vision},
    web::{Web, WebFetch, WebSearchInput},
};
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

pub struct CoshTools {
    bash: Bash,
    fs: Fs,
    find: Find,
    web: Web,
    vision: Vision,
    plan: Mutex<Plan>,
    question: Question,
    skills: Skills,
}

impl CoshTools {
    #[must_use]
    pub fn new(cwd: &str) -> Self {
        Self {
            bash: Bash::new(),
            fs: Fs::new().cwd(cwd),
            find: Find::new(),
            web: Web::new(),
            vision: Vision::new(),
            plan: Mutex::new(Plan::new()),
            question: Question::new(),
            skills: Skills::new(),
        }
    }
}

fn extract_schema(desc: &serde_json::Value) -> ToolSchema {
    ToolSchema {
        name: desc["name"].as_str().unwrap_or_default().to_string(),
        input_schema: desc["inputSchema"].clone(),
    }
}

#[allow(clippy::too_many_lines)]
fn write_single_tool(out: &mut String, desc: &serde_json::Value) {
    let name = desc["name"].as_str().unwrap_or_default();
    let description = desc["description"].as_str().unwrap_or_default();
    let schema = serde_json::to_string_pretty(&desc["inputSchema"]).unwrap_or_default();
    let _ = write!(out, "- **{name}**: {description}\n  Schema: {schema}\n");
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
        write_single_tool(out, &self.skills.description_list);
        write_single_tool(out, &self.skills.description_read);
        write_single_tool(out, &self.skills.description_read_asset);
        write_single_tool(out, &self.skills.description_match_skills);
    }

    fn tool_descriptions(&self) -> Vec<serde_json::Value> {
        vec![
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
            self.plan.lock().unwrap().description_todo_write.clone(),
            self.plan.lock().unwrap().description_todo_edit.clone(),
            self.plan.lock().unwrap().description_todo_cross_off.clone(),
            self.plan.lock().unwrap().description_todo_read.clone(),
            self.plan.lock().unwrap().description_load_from_md.clone(),
            self.question.description_ask.clone(),
            self.skills.description_list.clone(),
            self.skills.description_read.clone(),
            self.skills.description_read_asset.clone(),
            self.skills.description_match_skills.clone(),
        ]
    }

    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
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
            extract_schema(&self.plan.lock().unwrap().description_todo_write),
            extract_schema(&self.plan.lock().unwrap().description_todo_edit),
            extract_schema(&self.plan.lock().unwrap().description_todo_cross_off),
            extract_schema(&self.plan.lock().unwrap().description_todo_read),
            extract_schema(&self.plan.lock().unwrap().description_load_from_md),
            extract_schema(&self.question.description_ask),
            extract_schema(&self.skills.description_list),
            extract_schema(&self.skills.description_read),
            extract_schema(&self.skills.description_read_asset),
            extract_schema(&self.skills.description_match_skills),
        ]
    }

    #[allow(clippy::too_many_lines)]
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
                let mut last = String::new();
                while let Some(chunk) = stream.next().await {
                    last = format!(
                        "stdout: {}\nstderr: {}\nexit_code: {:?}\nsignal: {:?}\ntruncated: {}",
                        String::from_utf8_lossy(&chunk.stdout),
                        String::from_utf8_lossy(&chunk.stderr),
                        chunk.exit_code,
                        chunk.signal,
                        chunk.truncated,
                    );
                }
                Ok(last)
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
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| "missing 'path'".to_string())?;
                let result = self.find.grep(pattern, path)?;
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

            "ask_questions" => {
                let input: QuestionInput =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let output = self.question.ask(&input).map_err(|e| e.to_string())?;
                serde_json::to_string(&output).map_err(|e| e.to_string())
            }

            _ => Err(format!("unknown cosh tool: {name}")),
        }
    }
}
