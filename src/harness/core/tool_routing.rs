//! Native tool registration, routing and error enrichment: wiring the
//! connector's tool definitions and interpreting dispatch rejections.

use aiface::connector::ToolDefinition;

use super::{Harness, Mode, looks_like_argument_error};
use crate::harness::tools::{Tools, is_tool_disabled};

/// The Manager mode's tool deny-list: the tools that WRITE/EDIT files.
/// Everything else stays — `fs_rollback` only restores a previously recorded
/// version (it cannot create or rewrite content), and `bash_run` stays
/// exposed but is fenced per-command by the mode's bash blocklist
/// (see `is_manager_blocked_bash`).
pub(crate) fn is_manager_restricted(name: &str) -> bool {
    matches!(
        name,
        "fs_write" | "fs_edit" | "fs_edit_lines" | "fs_ast_edit"
    )
}

/// The Manager mode's bash blocklist: commands that WRITE to the
/// filesystem. `bash_run` stays exposed in Manager (reading is the
/// manager's verification instrument), but a command whose purpose is to
/// mutate the tree is denied at the guardrail. The patterns match the
/// common write shapes deliberately — this is a behavior fence for a
/// cooperating model, not a security sandbox against a hostile one (the
/// critical-pattern validation in `cosh_tools::bash` already covers the
/// hostile cases).
///
/// The check is string-level over the WHOLE command, so a write hidden in
/// a pipeline, subshell or quoted argument (`find . | xargs rm`,
/// `eval "echo x > f"`) is caught the same way a plain one is. It errs
/// toward denial: an over-blocked read command costs the model one
/// rephrase; an under-blocked write defeats the mode.
pub(crate) fn is_manager_blocked_bash(command: &str) -> bool {
    let command = command.trim();
    // 1. Output redirection: any `>` that is not an fd duplication
    // (`2>&1`, `>&2` — duplicating onto an already-open descriptor writes
    // nothing new). `/dev/null` targets are allowed: `2>/dev/null` is the
    // standard read-command idiom and writes nowhere.
    if has_output_redirect(command) {
        return true;
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    // 2. Hard-write commands anywhere in the line: `rm`, `cp`, `touch`, …
    // as ANY token, so `xargs rm` and `find -exec cp` are caught too.
    // Interpreters with inline code (`python -c`, `node -e`, heredoc-fed
    // `python`) are in the same bucket: their code can open/write anything,
    // and a manager verifies with test runners, not scripts.
    if tokens.iter().any(|t| {
        let stripped = strip_token(t);
        HARD_WRITE_COMMANDS.contains(&stripped) || INTERPRETERS.contains(&stripped)
    }) {
        return true;
    }
    if tokens.is_empty() {
        return false;
    }
    let first = strip_token(tokens[0]);
    // 3. `sed -i[.suffix]` / `sed --in-place`: in-place edit writes files
    // (including the common `-i.bak` idiom). A plain `sed` stream (print to
    // stdout) stays allowed.
    if first == "sed"
        && tokens[1..]
            .iter()
            .any(|t| *t == "-i" || strip_token(t).starts_with("--in-place") || sed_i_flag(t))
    {
        return true;
    }
    // 4. git: deny its state-changing subcommands wherever they appear
    // (`git -c k=v commit` has no clean "first subcommand"), allow the
    // read ones (status/diff/log/show/blame/grep). `stash` is the one
    // write-shaped subcommand with read forms: `stash list|show|log`
    // only display — those stay allowed.
    if first == "git" {
        let stripped: Vec<&str> = tokens[1..].iter().map(|t| strip_token(t)).collect();
        for (i, t) in stripped.iter().enumerate() {
            if !GIT_WRITE_SUBCOMMANDS.contains(t) {
                continue;
            }
            if *t == "stash"
                && matches!(
                    stripped.get(i + 1).copied().unwrap_or(""),
                    "list" | "show" | "log"
                )
            {
                continue;
            }
            return true;
        }
        return false;
    }
    // 5. gh: allow only its read subcommand paths — two levels deep
    // (`pr view`, `issue list`, `run view`), so a manager verifying a PR
    // is not denied. `api` is DENIED: `gh api --method POST …` mutates
    // through the same word a GET uses.
    if first == "gh" {
        let args: Vec<&str> = tokens[1..]
            .iter()
            .filter(|t| !t.starts_with('-'))
            .map(|t| strip_token(t))
            .collect();
        if args.is_empty() {
            return true;
        }
        return !GH_READ_SUBCOMMANDS.iter().any(|read| {
            read.split(' ')
                .eq(args.iter().copied().take(args.len().min(2)))
        });
    }
    // 6. Package managers / build tools: BUILD and TEST runs are the
    // manager's verification instrument (their output lands in target/,
    // node_modules — build artifacts, not the source tree); package
    // installation and everything else is denied. `fmt` is DENIED even
    // though it looks like a verification verb: `cargo fmt` rewrites every
    // source file it touches. `clippy` stays allowed for the report, but
    // an explicit `--fix` is a write and is denied below.
    if PACKAGE_MANAGERS.contains(&first) {
        let verb = tokens.get(1).map(|t| strip_token(t)).unwrap_or_default();
        return !PACKAGE_READ_VERBS.contains(&verb)
            || tokens[1..].iter().any(|t| strip_token(t) == "fix");
    }
    // 7. Build runners: allowed except an explicit `install` target.
    if BUILD_RUNNERS.contains(&first) {
        return tokens[1..].iter().any(|t| strip_token(t) == "install");
    }
    false
}

/// Any `>` in `command` that creates/overwrites a file: plain `>`, append
/// `>>`, `&>`, and the fd forms `n>`/`n>>` — EXCEPT the fd-duplication
/// forms `n>&m` / `>&m` (both digits) and targets of `/dev/null`.
fn has_output_redirect(command: &str) -> bool {
    let bytes = command.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'>' {
            continue;
        }
        // fd-dup `>&m`: the target is another descriptor, not a file.
        if bytes.get(i + 1) == Some(&b'&') && bytes.get(i + 2).is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        // `> /dev/null` (also `2>/dev/null`, `&>/dev/null`): the bit bucket.
        let target: String = bytes[i + 1..]
            .iter()
            .skip_while(|&&c| c.is_ascii_whitespace() || c == b'&')
            .take_while(|&&c| !c.is_ascii_whitespace())
            .map(|&c| c as char)
            .collect();
        if target.trim_matches(|c| c == '"' || c == '\'') == "/dev/null" {
            continue;
        }
        return true;
    }
    false
}

/// A token compared against the blocklists: stripped of the shell
/// decorations that can ride along (`-x` flags, `(...)` subshells, quotes).
fn strip_token(token: &str) -> &str {
    token
        .trim_start_matches('-')
        .trim_start_matches(['(', '"', '\''])
        .trim_end_matches([')', '"', '\'', ';', ','])
}

/// Commands whose PURPOSE is to write/move/delete files, denied wherever
/// they appear in the line.
const HARD_WRITE_COMMANDS: &[&str] = &[
    "touch", "mkdir", "rmdir", "cp", "mv", "rm", "ln", "install", "dd", "shred", "truncate",
    "patch", "rsync", "scp", "sftp", "wget", "curl", "chmod", "chown", "chgrp", "setfacl",
    "mktemp", "tee",
];

/// `git` subcommands that change repository or worktree state.
const GIT_WRITE_SUBCOMMANDS: &[&str] = &[
    "add",
    "apply",
    "am",
    "checkout",
    "cherry-pick",
    "clean",
    "commit",
    "config",
    "fetch",
    "gc",
    "init",
    "merge",
    "mv",
    "prune",
    "pull",
    "push",
    "rebase",
    "reset",
    "restore",
    "revert",
    "rm",
    "stash",
    "switch",
    "tag",
    "worktree",
];

/// Interpreters that can execute inline code writing anything: a
/// one-liner is never a manager's verification instrument — reading is
/// done with `cat`/`grep`/diff, verification with test runners.
const INTERPRETERS: &[&str] = &[
    "python", "python3", "node", "perl", "ruby", "php", "lua", "awk", "gawk",
];

/// True when a `sed` token is the in-place edit flag in its suffixed
/// form (`-i`, `-i.bak`, `-i .bak` handled by the caller): `strip_token`
/// removes the leading dash, so `-i.bak` arrives as `i.bak`.
fn sed_i_flag(token: &str) -> bool {
    let stripped = strip_token(token);
    stripped == "i" || stripped.starts_with("i.")
}

/// `gh` subcommand paths that only read (two levels: `<area> <verb>` or a
/// bare top-level verb). `api` is deliberately absent: it mutates through
/// `--method POST` the same word a GET uses.
const GH_READ_SUBCOMMANDS: &[&str] = &[
    "pr view",
    "pr list",
    "pr checks",
    "pr diff",
    "pr status",
    "pr edits",
    "issue view",
    "issue list",
    "issue status",
    "run view",
    "run list",
    "run watch",
    "release view",
    "release list",
    "repo view",
    "status",
    "search",
    "browse",
];

/// Package managers: denied except their read/build/test verbs.
const PACKAGE_MANAGERS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "poetry", "gem", "go", "apt",
    "apt-get", "dnf", "yum", "pacman", "brew", "nix", "nix-env", "snap", "flatpak", "uvx", "npx",
];

/// Verbs that only read or produce build artifacts — allowed for every
/// package manager in [`PACKAGE_MANAGERS`].
const PACKAGE_READ_VERBS: &[&str] = &[
    "test", "check", "build", "clippy", "run", "doc", "bench", "list", "ls", "info", "view", "why",
    "show", "search", "version", "tree", "audit", "outdated", "freeze", "exec", "x", "vet",
];

/// Build runners: allowed (their artifacts are build output), except an
/// explicit `install` target.
const BUILD_RUNNERS: &[&str] = &[
    "make", "cmake", "ninja", "just", "task", "rake", "gradle", "mvn",
];

impl Harness {
    // Populate the connector's native tool definitions once.
    pub(super) fn init_native_tools(&mut self) {
        use aiface::connector::ToolFunction as TFunc;

        let mut defs: Vec<ToolDefinition> = Vec::new();

        // Harness tools (e.g. stop_agent_loop)
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            defs.push(ToolDefinition::new(
                TFunc::new(&tool.name)
                    .with_description(&tool.description)
                    .with_parameters(tool.input_schema.clone()),
            ));
        }

        // Cosh tools — filtered by mode so the native API only exposes
        // tools the model is allowed to call in the current mode.
        if let Some(ref cosh) = self.cosh_tools {
            let descriptions: Vec<serde_json::Value> = match self.mode {
                Mode::Build => {
                    // All tools minus disabled: every mode sees the full
                    // toolset now. The coordinate forms of computer_pointer /
                    // computer_screenshot are gated per-call by the guardrail
                    // (denied in Build with guidance toward the tree-grounded
                    // forms), not by hiding the tool.
                    cosh.tool_descriptions()
                        .into_iter()
                        .filter(|desc| {
                            let name = desc["name"].as_str().unwrap_or_default();
                            !is_tool_disabled(name, &self.disabled_tools)
                        })
                        .collect()
                }
                Mode::Yolo | Mode::Command | Mode::Manager => {
                    // All tools minus disabled — and for Manager, minus the
                    // file-writing tools: the mode's deny-list is enforced at
                    // registration so the model never even sees a schema for
                    // a tool it cannot call (defense in depth with the
                    // guardrail denial).
                    cosh.tool_descriptions()
                        .into_iter()
                        .filter(|desc| {
                            let name = desc["name"].as_str().unwrap_or_default();
                            !is_tool_disabled(name, &self.disabled_tools)
                                && !(self.mode == Mode::Manager && is_manager_restricted(name))
                        })
                        .collect()
                }
                Mode::Ask => {
                    // Only read-only / planning tools
                    cosh.tool_descriptions_filtered(&self.disabled_tools)
                }
            };

            for desc in descriptions {
                let name = desc["name"].as_str().unwrap_or_default();
                let description = desc["description"].as_str().unwrap_or_default();
                if let Some(input_schema) = desc.get("inputSchema").cloned() {
                    defs.push(ToolDefinition::new(
                        TFunc::new(name)
                            .with_description(description)
                            .with_parameters(input_schema),
                    ));
                }
            }
        }

        // MCP server tools (hidden in Ask mode, like the header).
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if self.disabled_tools.contains(tool.name.as_ref()) {
                    continue;
                }
                defs.push(crate::mcp::tool_to_definition(tool));
            }
        }

        self.connector.set_tools(defs);
    }

    /// Mirror the tools' current TODO list into the dedicated protected TODO
    /// context block (see `context_manager::todo_ctxt`). Called at loop start
    /// and after every `plan_*` dispatch so the block always reflects the
    /// authoritative `Plan` state.
    pub(super) fn sync_todo_context(&mut self) {
        if let Some(cosh) = &self.cosh_tools {
            let list = cosh.todo_list();
            self.context_manager.set_todo_list(list);
        }
    }

    /// Enrich an argument-shaped rejection with the tool's expected argument
    /// shape (a minimal example rendered from its input schema). Runtime
    /// failures (file not found, permission denied, timeouts) pass through
    /// untouched — echoing the schema there would be noise.
    pub(super) fn enrich_with_schema_hint(&self, tool_name: &str, err: String) -> String {
        if !looks_like_argument_error(&err) {
            return err;
        }
        match self.schema_input(tool_name) {
            Some(schema) => format!(
                "{err}\nExpected `{tool_name}` arguments — minimal shape: {}",
                cosh_sdk::extract_action::schema_skeleton(&schema)
            ),
            None => err,
        }
    }

    /// Rejection for a tool nothing serves. The model may be following stale
    /// handoff context (naming a tool that no longer exists), so the reply
    /// echoes the CURRENT available tool set — the same mode-filtered,
    /// deduplicated set the model was registered with — instead of leaving
    /// it to re-derive from memory (which may name removed tools).
    pub(super) fn unknown_tool_message(&self, tool_name: &str) -> String {
        const MAX_LISTED: usize = 15;
        let mut names = self.effective_tool_names();
        if names.is_empty() {
            return format!("no server found for tool '{tool_name}'");
        }
        let more = if names.len() > MAX_LISTED {
            format!(" (+{} more)", names.len() - MAX_LISTED)
        } else {
            String::new()
        };
        names.truncate(MAX_LISTED);
        format!(
            "no server found for tool '{tool_name}'. Available tools: {}{more}",
            names.join(", ")
        )
    }

    /// The tool's input schema, from the same mode-filtered, disabled-aware
    /// set the model was registered with (never a hidden tool's schema).
    pub(super) fn schema_input(&self, tool_name: &str) -> Option<serde_json::Value> {
        if let Some(cosh) = &self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build | Mode::Yolo | Mode::Command => {
                    cosh.schemas_enabled(&self.disabled_tools)
                }
                Mode::Manager => cosh
                    .schemas_enabled(&self.disabled_tools)
                    .into_iter()
                    .filter(|schema| !is_manager_restricted(&schema.name))
                    .collect(),
                Mode::Ask => cosh.schemas_filtered(&self.disabled_tools),
            };
            for schema in schemas {
                if schema.name == tool_name {
                    return Some(schema.input_schema);
                }
            }
        }
        if matches!(self.mode, Mode::Ask) {
            return None;
        }
        self.mcp
            .all_tools()
            .into_iter()
            .find(|tool| tool.name.as_ref() == tool_name)
            .filter(|tool| !self.disabled_tools.contains(tool.name.as_ref()))
            .map(|tool| serde_json::Value::Object((*tool.input_schema).clone()))
    }
}
