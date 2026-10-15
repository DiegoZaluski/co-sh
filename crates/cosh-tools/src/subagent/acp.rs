//! ACP (Agent Client Protocol) integration for external sub-agents.
//!
//! External sub-agents are driven through the [Agent Client Protocol
//! (ACP)](https://agentclientprotocol.com/) — the JSON-RPC 2.0 protocol
//! created by the Zed team for client↔agent communication — using the
//! official [`agent_client_protocol`] crate.
//!
//! Instead of spawning a one-shot CLI process and scraping its stdout (a
//! per-agent contract of flags that breaks on every CLI update), cosh acts
//! as an ACP **client** and follows the protocol contract:
//!
//! 1. Spawns the agent harness as a subprocess speaking ACP over stdio.
//! 2. `initialize` — negotiates the protocol version and capabilities.
//! 3. `session/new` — opens a session rooted at the workspace directory.
//!    Authentication is LAZY: when the agent advertises auth methods, cosh
//!    does NOT authenticate proactively — several harnesses treat an
//!    `authenticate` call as a fresh interactive login even when they
//!    already hold valid credentials (measured on `devin acp`: 19.5 s,
//!    a browser flow and a credentials rewrite, EVERY session), while the
//!    same harness accepts the whole session silently from its own stored
//!    login. `authenticate` is sent only after an operation is rejected
//!    with `auth_required` (see [`authenticate_on_auth_required`]).
//! 4. `session/prompt` — sends the task as the user prompt.
//! 5. `session/update` notifications — mapped events stream live to the
//!    TUI's sub-agent box, and message chunks feed the turn's closure
//!    ([`TurnClosure`](crate::subagent::closure)): only the message written
//!    after the LAST tool call is the report returned to the caller.
//! 6. The turn ends when the `session/prompt` response arrives with a
//!    [`StopReason`](agent_client_protocol::schema::v1::StopReason).
//!
//! Because the interaction follows a protocol contract, ANY harness that
//! speaks ACP can act as a sub-agent — including third-party harnesses —
//! with zero per-agent maintenance. Client-side capabilities are honored:
//! permission requests are auto-approved (first option, YOLO style) so a
//! headless call never blocks, and the client advertises the `fs` capability
//! so agents may delegate `fs/read_text_file` / `fs/write_text_file` to the
//! real workspace files (served below).
//!
//! # Supported agents
//!
//! Only agents with ACP support are registered. Agents without ACP support
//! were removed (they cannot satisfy the protocol contract):
//!
//! | Name | ACP invocation |
//! |------|----------------|
//! | `gemini` | `gemini --experimental-acp` |
//! | `goose` | `goose acp` |
//! | `opencode` | `opencode acp` |
//! | `kilo` | `kilo acp` |
//! | `cline` | `cline --acp` |
//! | `devin` | `devin acp` |
//! | `claude` | `npx -y @agentclientprotocol/claude-agent-acp@latest` (official adapter) |
//! | `codex` | `npx -y @agentclientprotocol/codex-acp@latest` (official adapter) |
//!
//! # Adding new agents
//!
//! Add an entry to [`ACP_AGENTS`] — an [`Agent`] of
//! `(name, command, [args], [required binaries], install_hint)`. Anything
//! registered in the official ACP registry works with no code changes
//! beyond the table entry.

use std::iter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities,
    ContentBlock, FileSystemCapabilities, InitializeRequest, LoadSessionRequest,
    LoadSessionResponse, NewSessionRequest, PermissionOptionKind, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, ResumeSessionResponse,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectOptions,
    SessionConfigValueId, SessionId, SessionNotification, SetSessionConfigOptionRequest,
    StopReason, TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{
    AcpAgent, Agent as AcpRole, Client, ConnectionTo, Error as AcpError, ErrorCode, LineDirection,
    on_receive_notification, on_receive_request,
};

use super::events::SubagentEvent;

/// A single registered ACP agent harness.
///
/// Maps a public `name` (used by the LLM in the tool call) to the command
/// that launches the harness in ACP mode. Unlike the previous one-shot
/// integration there are no per-agent prompt flags: the prompt travels as
/// the ACP `session/prompt` payload, so adding an agent is purely a table
/// entry.
///
/// `requires` lists the binaries that must be present in PATH for the agent
/// to be considered installed — the launch command itself plus whatever
/// engine backs it. The official `claude`/`codex` entries spawn the ACP
/// adapter via `npx`, so they additionally require `npx`.
#[derive(Debug, Clone, Copy)]
pub struct Agent {
    /// Public name used by the LLM in the `subagent_call` tool.
    pub name: &'static str,
    /// Executable that speaks ACP over stdio.
    pub command: &'static str,
    /// Static arguments that put the executable into ACP server mode.
    pub args: &'static [&'static str],
    /// Binaries that must be in PATH for this agent to be offered.
    pub requires: &'static [&'static str],
    /// Installation guidance surfaced when the spawn fails.
    pub install_hint: &'static str,
    /// Preferred session model, selected through the standard ACP
    /// `session/set_config_option` request when the harness advertises a
    /// `model` config option in `session/new`.
    ///
    /// Harness defaults are frequently a *paid-tier* model, while the same
    /// CLI's own `run` command uses a working free/default model — the
    /// harness's ACP default and its CLI default need not agree. An empty
    /// value leaves the harness default untouched.
    pub model: &'static str,
}

impl Agent {
    /// Human-readable ACP launch command (e.g. `gemini --experimental-acp`)
    /// used in the tool description and docs. The prompt travels as the ACP
    /// `session/prompt` payload, so only the launch command is shown.
    #[must_use]
    pub fn invocation(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.command);
        parts.extend_from_slice(self.args);
        parts.join(" ")
    }
}

/// Registered ACP agent harnesses. This is the whole integration surface
/// for adding a new sub-agent: any entry of the official ACP registry works.
pub const ACP_AGENTS: &[Agent] = &[
    Agent {
        name: "gemini",
        command: "gemini",
        args: &["--experimental-acp"],
        requires: &["gemini"],
        install_hint: "Install: npm install -g @google/gemini-cli. Then: gemini auth login. More: https://github.com/google-gemini/gemini-cli",
        model: "",
    },
    Agent {
        name: "goose",
        command: "goose",
        args: &["acp"],
        requires: &["goose"],
        install_hint: "Install: curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | bash. More: https://block.github.io/goose",
        model: "",
    },
    Agent {
        name: "opencode",
        command: "opencode",
        args: &["acp"],
        requires: &["opencode"],
        install_hint: "Install: curl -fsSL https://opencode.ai/install | bash (or: npm install -g opencode-ai). More: https://opencode.ai/docs",
        model: "",
    },
    Agent {
        name: "kilo",
        command: "kilo",
        args: &["acp"],
        requires: &["kilo"],
        install_hint: "Install: curl -fsSL https://kilo.ai/install.sh | sh. More: https://kilo.ai/docs/code-with-ai/platforms/cli",
        // The harness's ACP default (`kilo/google/gemini-3-pro-image`) is a
        // Kilo paid-tier model; the CLI's own default is this free Nvidia
        // model, which works with the provider keys the user already has.
        model: "kilo/nvidia/nemotron-3-ultra-550b-a55b:free",
    },
    Agent {
        name: "cline",
        command: "cline",
        args: &["--acp"],
        requires: &["cline"],
        install_hint: "Install: npm install -g cline. Then: cline auth (or sign in from the first session). More: https://docs.cline.bot/usage/acp",
        model: "",
    },
    Agent {
        name: "devin",
        command: "devin",
        args: &["acp"],
        requires: &["devin"],
        install_hint: "Install: curl -fsSL https://cli.devin.ai/install.sh | bash. Then: devin auth login (or set WINDSURF_API_KEY). More: https://docs.devin.ai/cli",
        model: "",
    },
    Agent {
        name: "claude",
        // Claude Code is not ACP-native yet: the official route is Zed's
        // adapter, the same one `AcpAgent::claude_agent()` in the SDK uses.
        command: "npx",
        args: &["-y", "@agentclientprotocol/claude-agent-acp@latest"],
        requires: &["claude", "npx"],
        install_hint: "Install: npm install -g @anthropic-ai/claude-code (the ACP adapter runs via npx). More: https://code.claude.com/docs",
        model: "",
    },
    Agent {
        name: "codex",
        // Codex CLI is not ACP-native yet: the official route is Zed's
        // adapter, the same one `AcpAgent::codex()` in the SDK uses.
        command: "npx",
        args: &["-y", "@agentclientprotocol/codex-acp@latest"],
        requires: &["codex", "npx"],
        install_hint: "Install: npm install -g @openai/codex (the ACP adapter runs via npx). More: https://developers.openai.com/codex",
        model: "",
    },
];

/// Build the [`AcpAgent`] launcher for a registered agent.
///
/// On Windows, npm-style launch commands (`.cmd`/`.bat` shims, e.g. `npx`)
/// are routed through `cmd /d /s /c …` (see [`windows_script_launcher`]).
pub(crate) fn agent_launcher(entry: &Agent) -> Result<AcpAgent, String> {
    let args: Vec<String> = iter::once(entry.command)
        .chain(entry.args.iter().copied())
        .map(String::from)
        .collect();
    let args = windows_script_launcher(&args).unwrap_or(args);
    AcpAgent::from_args(args)
        .map_err(|e| format!("invalid ACP launch command for '{}': {e}", entry.name))
}

/// Build an ACP JSON-RPC error carrying a human-readable `message`.
pub(crate) fn acp_error(message: impl Into<String>) -> AcpError {
    let mut error = AcpError::internal_error();
    error.message = message.into();
    error
}

/// Wrap a wire error into a context-carrying `AcpError` WITHOUT discarding
/// its protocol code. The lazy-auth retry points match on
/// `ErrorCode::AuthRequired`; a plain `acp_error` rewrite (internal_error)
/// would erase that code and strand the turn unauthenticated.
pub(crate) fn wrap_acp_failure(prefix: &str, e: AcpError) -> AcpError {
    let mut error = acp_error(format!("{prefix}: {e}"));
    error.code = e.code;
    error
}

/// Check whether `binary` exists in PATH (with the `.exe` variant on Windows).
fn binary_in_path(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            dir.join(binary).is_file()
                // Windows PATHEXT resolution: only relevant on Windows. npm
                // installs CLIs as `.cmd`/`.bat` batch shims (plus an
                // extensionless sh script), all of which are launchable.
                || (cfg!(windows)
                    && ["exe", "cmd", "bat"]
                        .into_iter()
                        .any(|ext| dir.join(format!("{binary}.{ext}")).is_file()))
        })
    })
}

/// Whether `binary` resolves to a native `.exe` image in PATH (Windows only).
///
/// `CreateProcessW` (used by `AcpAgent::spawn_process`) can only launch
/// `.exe` images directly; batch-file shims need the `cmd` detour below.
#[cfg(windows)]
fn exe_in_path(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(format!("{binary}.exe")).is_file())
    })
}

/// Whether `binary` resolves to a `.cmd`/`.bat` batch shim in PATH (Windows
/// only) — the shape npm takes when installing global CLIs.
#[cfg(windows)]
fn script_shim_in_path(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            ["cmd", "bat"]
                .into_iter()
                .any(|ext| dir.join(format!("{binary}.{ext}")).is_file())
        })
    })
}

/// Windows workaround for npm-style launch commands (`npx …`, or any CLI
/// installed through `npm install -g`): those are `.cmd`/`.bat` batch shims,
/// and the direct `CreateProcessW` spawn inside `AcpAgent::spawn_process`
/// fails on them with `program not found`. When the launch command is a
/// script shim, the whole invocation is routed through
/// `cmd /d /s /c "<full command line>"` — `/s` makes `cmd` strip the outer
/// quotes that the process-spawning layer adds around the joined line.
///
/// The joined line is interpreted by `cmd.exe`, so any argument containing
/// cmd metacharacters (`& | ^ % < > "`) would be executed/expanded rather
/// than passed through. Those cannot appear in the static [`ACP_AGENTS`]
/// registry, and the guard below refuses to wrap if one ever does — the
/// direct spawn then fails loudly with `program not found` instead of
/// running something unintended.
///
/// Returns `Some(wrapped_args)` when a `cmd` detour is needed, `None`
/// otherwise (native `.exe`, non-Windows, or unsafe-to-wrap argument).
#[cfg(windows)]
fn windows_script_launcher(args: &[String]) -> Option<Vec<String>> {
    const CMD_METACHARACTERS: [char; 7] = ['&', '|', '^', '%', '<', '>', '"'];
    if args
        .iter()
        .any(|arg| arg.chars().any(|c| CMD_METACHARACTERS.contains(&c)))
    {
        return None;
    }
    let command = args.first()?;
    if exe_in_path(command) || !script_shim_in_path(command) {
        return None;
    }
    Some(vec![
        "cmd".to_string(),
        "/d".to_string(),
        "/s".to_string(),
        "/c".to_string(),
        args.join(" "),
    ])
}

/// Non-Windows no-op: Unix spawn resolves scripts through the shebang line.
#[cfg(not(windows))]
fn windows_script_launcher(_args: &[String]) -> Option<Vec<String>> {
    None
}

/// Return installation / configuration guidance for a given agent.
#[must_use]
pub fn install_hint(agent: &str) -> &'static str {
    ACP_AGENTS
        .iter()
        .find(|entry| entry.name == agent)
        .map_or("", |entry| entry.install_hint)
}

/// Check which ACP agent harnesses the user has installed (all required
/// binaries found in PATH).
///
/// Results are cached in a `OnceLock` so detection runs exactly once
/// per process lifetime. Subsequent calls return the cached list.
pub fn detect_installed() -> &'static Vec<&'static str> {
    static INSTALLED: OnceLock<Vec<&'static str>> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        ACP_AGENTS
            .iter()
            .filter(|agent| agent.requires.iter().all(|bin| binary_in_path(bin)))
            .map(|agent| agent.name)
            .collect()
    })
}

/// Validate-then-serve path sandbox (non-Unix fallback, and the unit-test
/// subject for sandbox semantics).
///
/// On Unix the fs handlers use the kernel-pinned component walk in
/// [`super::sandbox`] instead — the read/write descriptor is obtained during
/// the walk, so there is no validate-then-use window — while this helper
/// remains the non-Unix fallback and the direct unit-test subject.
///
/// ACP agents send absolute paths; a request that resolves OUTSIDE the
/// workspace — a different absolute prefix, or the same prefix escaping it
/// through `..` segments or symlinks — is rejected instead of served.
///
/// Three gates run in sequence:
///
/// 1. **Canonical root** — the workspace root is canonicalized first, so
///    containment runs in fully-resolved space (on macOS `/tmp` is a symlink
///    to `/private/tmp`).
/// 2. **Lexical** — the request must be rooted at the workspace (either its
///    given or canonical spelling) and must not climb out through `..`
///    components.
/// 3. **Component walk** — each relative component is appended to the
///    resolved base and checked with `symlink_metadata` (which does NOT
///    follow the final entry): an encountered symlink is canonicalized
///    immediately and its target must stay inside the workspace; a DANGLING
///    symlink therefore fails closed (its target cannot be verified), while
///    a plain MISSING component is appended as-is — a write legitimately
///    creates new files and directories. Because the walk never descends
///    into a non-existent component, no later component can hide a symlink.
///
/// NOTE: on Unix this check alone is NOT sufficient for serving (a TOCTOU
/// window opens between this validation and the subsequent `std::fs`
/// operation); it is the non-Unix fallback where that window is accepted,
/// and a fast lexical pre-filter in tests.
///
/// The returned path is the resolved one the handlers operate on (symlinks
/// encountered on the way are already resolved).
///
/// # Errors
///
/// Returns an ACP error when the path escapes the workspace root, a symlink
/// inside it dangles or points outside, or the workspace itself is not
/// accessible.
#[cfg(any(not(unix), test))]
pub(crate) fn ensure_path_within(root: &Path, requested: &Path) -> Result<PathBuf, AcpError> {
    let canonical_root = root.canonicalize().map_err(|e| {
        acp_error(format!(
            "session workspace '{}' is not accessible: {e}",
            root.display()
        ))
    })?;

    // Lexical gate (see above).
    let inside = requested
        .strip_prefix(root)
        .ok()
        .or_else(|| requested.strip_prefix(&canonical_root).ok());
    let Some(relative) = inside else {
        return Err(outside_workspace_error(requested));
    };
    if relative
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(outside_workspace_error(requested));
    }

    // Component walk (see above).
    let mut resolved = canonical_root.clone();
    for component in relative.components() {
        let candidate = resolved.join(component);
        match candidate.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                // The entry itself exists and is a symlink: resolve it NOW.
                // A dangling symlink (unresolvable target) fails closed.
                let target = candidate
                    .canonicalize()
                    .map_err(|_| outside_workspace_error(requested))?;
                if !target.starts_with(&canonical_root) {
                    return Err(outside_workspace_error(requested));
                }
                resolved = target;
            }
            Ok(_) => resolved = candidate,
            // Missing entry: plain append. Everything below a missing
            // component is necessarily missing too (no hidden symlinks),
            // and the fs handlers create the intermediate directories.
            Err(_) => resolved = candidate,
        }
    }
    if !resolved.starts_with(&canonical_root) {
        return Err(outside_workspace_error(requested));
    }
    Ok(resolved)
}

/// The rejection error for a path that escapes the session workspace.
pub(crate) fn outside_workspace_error(requested: &Path) -> AcpError {
    acp_error(format!(
        "path '{}' resolves outside the session workspace; refusing to serve it",
        requested.display()
    ))
}

/// Apply the protocol's 1-based `line`/`limit` window to file content
/// (shared by the sandboxed and fallback read paths).
#[must_use]
pub(crate) fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    let start = line.map_or(0, |line| line.saturating_sub(1) as usize);
    match limit {
        None if start == 0 => content.to_string(),
        limit => content
            .lines()
            .skip(start)
            .take(limit.map_or(usize::MAX, |l| l as usize))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Serve `fs/read_text_file`, sandboxed to the workspace root.
///
/// Unix: kernel-pinned component walk (see [`sandbox`](super::sandbox)) —
/// the read descriptor is obtained DURING the walk, so a concurrent swap of
/// a validated directory for an outside symlink is caught at open time and
/// there is no validate-then-use window. Other platforms fall back to
/// validate-then-serve via [`ensure_path_within`].
#[cfg(unix)]
fn serve_read(
    root: &super::sandbox::PinnedRoot,
    requested: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, AcpError> {
    super::sandbox::read(root, requested, line, limit)
}

/// See [`serve_read`]; non-Unix platforms use validate-then-serve.
#[cfg(not(unix))]
fn serve_read(
    root: &Path,
    requested: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, AcpError> {
    let path = ensure_path_within(root, requested)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| acp_error(format!("failed to read {}: {e}", requested.display())))?;
    Ok(slice_lines(&content, line, limit))
}

/// Serve `fs/write_text_file`, sandboxed to the workspace root (see
/// [`serve_read`] for the platform split). Missing intermediate directories
/// are created inside the workspace.
#[cfg(unix)]
fn serve_write(
    root: &super::sandbox::PinnedRoot,
    requested: &Path,
    content: &str,
) -> Result<(), AcpError> {
    super::sandbox::write(root, requested, content)
}

/// See [`serve_write`]; non-Unix platforms use validate-then-serve.
#[cfg(not(unix))]
fn serve_write(root: &Path, requested: &Path, content: &str) -> Result<(), AcpError> {
    let path = ensure_path_within(root, requested)?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, content)
        .map_err(|e| acp_error(format!("failed to write {}: {e}", requested.display())))
}

/// Stable, serde-shaped string for an ACP [`StopReason`].
///
/// The persisted `SubAgentCallOutput.stop_reason` must not depend on `Debug`
/// formatting (which can change with crate versions); these snake_case names
/// mirror the protocol's wire spelling. Unknown variants (the enum is
/// `#[non_exhaustive]`) degrade to their debug form instead of panicking.
#[must_use]
pub(crate) fn stop_reason_str(reason: StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::MaxTurnRequests => "max_turn_requests".to_string(),
        StopReason::Refusal => "refusal".to_string(),
        StopReason::Cancelled => "cancelled".to_string(),
        other => format!("{other:?}"),
    }
}

/// Validate that `agent` is registered in [`ACP_AGENTS`].
///
/// # Errors
///
/// Returns an error if the agent name is not found.
pub fn validate_agent(agent: &str) -> Result<(), String> {
    if ACP_AGENTS.iter().any(|entry| entry.name == agent) {
        Ok(())
    } else {
        let supported: Vec<&str> = ACP_AGENTS.iter().map(|a| a.name).collect();
        Err(format!(
            "Unsupported agent '{agent}'. Supported agents (ACP): {}. Use bash_run for shell commands.",
            supported.join(", "),
        ))
    }
}

/// Call a sub-agent harness over ACP and return its final output.
///
/// 1. Looks up `agent` in [`ACP_AGENTS`] to get the ACP launch command.
/// 2. Runs a full ACP client turn (see the [module docs](self)):
///    `initialize` → `session/new` (or, when resuming,
///    `session/resume`/`session/load`, falling back to `session/new` on any
///    resume failure) → `session/prompt`, auto-approving permission
///    requests and serving `fs/*` requests. Authentication is lazy: an
///    `auth_required` rejection triggers one `authenticate` attempt and a
///    retry of the rejected operation (see
///    [`authenticate_on_auth_required`]).
///    The shared stop flag is raced against the prompt await; a trigger
///    sends `session/cancel` and the agent ends the turn itself.
/// 3. Streams typed [`SubagentEvent`]s through `chunk_tx` (for the TUI)
///    while feeding the message text to the turn's closure
///    ([`TurnClosure`](crate::subagent::closure::TurnClosure)).
/// 4. Returns `(final_report, stop_reason, Option<session_id>)` when
///    the turn ends — the report is the message written after the LAST
///    tool call, with a fallback to the last completed message — and the
///    session id only when it ended protocol-clean
///    (`Some` is withheld on error arms) — or an error when
///    the harness fails before producing any output.
///
/// The ACP session runs on a dedicated current-thread runtime inside a
/// blocking task, isolating the SDK's connection machinery from the ambient
/// runtime flavor and keeping the caller's future `Send`.
///
/// # Errors
///
/// Returns an error if the agent is unsupported, the harness cannot be
/// launched, or the ACP turn fails before any output. There is NO time
/// limit: the turn runs until the agent ends it or the user stops it.
///
/// # Panics
///
/// Panics if the internal mutex protecting the output buffer is poisoned
/// (only possible if another thread panicked while holding the lock).
#[allow(clippy::unwrap_used)]
pub async fn call(
    agent: &str,
    input: &str,
    resume: Option<String>,
    cwd: PathBuf,
    stop_signal: Arc<AtomicBool>,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(String, String, Option<String>), String> {
    validate_agent(agent)?;
    let entry = ACP_AGENTS
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("unknown agent '{agent}'"))?;
    let launcher = agent_launcher(entry)?
        // Surface harness stderr in the logs to ease debugging of
        // unauthenticated/failed launches.
        .with_debug(|line, direction| {
            if matches!(direction, LineDirection::Stderr) {
                log::debug!("subagent ACP stderr: {line}");
            }
        });

    let accumulated: Arc<Mutex<super::closure::TurnClosure>> =
        Arc::new(Mutex::new(super::closure::TurnClosure::default()));
    let input = input.to_string();

    // The turn runs to completion with NO time limit: a sub-agent may work
    // for hours and the only legitimate way to end it is the user's stop
    // signal (raced against the prompt await inside `run_session`), which
    // makes the agent end the turn itself with `StopReason::Cancelled`.
    let turn = {
        let accumulated = accumulated.clone();
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to build ACP runtime: {e}"))?
                .block_on(run_session(
                    launcher,
                    input,
                    resume,
                    cwd,
                    entry.model,
                    accumulated,
                    stop_signal,
                    chunk_tx,
                ))
        })
        .await
        .map_err(|e| format!("sub-agent task failed: {e}"))?
    };

    let final_output = accumulated.lock().unwrap().output();

    match turn {
        // Turn completed: the closure already retained the final message
        // (post-tool-calls) — see the `TurnClosure` contract on `output`.
        Ok((stop_reason, session_id)) => Ok((final_output, stop_reason, Some(session_id))),
        // Turn errored: partial output (if any) is still worth returning.
        // The turn's own session id is NOT propagated — an errored turn's
        // session state is unreliable, so its id is not (re)stored here.
        // An OLDER stored id for this agent stays in place and is resumed
        // again by the next default call; a stale one self-heals via the
        // `session/new` fallback in `open_or_resume_session`.
        Err(session_error) => {
            if final_output.is_empty() {
                Err(session_error)
            } else {
                log::warn!("sub-agent '{agent}' ACP turn failed: {session_error}");
                Ok((final_output, "error".to_string(), None))
            }
        }
    }
}

/// Rehydrate one stored ACP session: reconnect, `session/load` the stored
/// id and stream the agent's retained conversation history back as typed
/// [`SubagentEvent`]s — WITHOUT prompting.
///
/// This is the cold-restore path for a pane whose process died with the
/// app (`/agents` split reopened after a reboot): the session id is the
/// only thing persisted, so the transcript must come back FROM the agent.
/// The v1 `session/load` request replays the entire conversation through
/// `session/update` notifications (the same variants live turns stream —
/// user/agent/thought chunks, tool calls, plans) before responding, so the
/// mapped event stream reconstructs the visible timeline with no storage
/// on cosh's side beyond the session id itself.
///
/// Unlike a live pane (which shows only agent narration — the user's
/// prompt echoes locally through the pane input), replay's
/// `user_message_chunk` updates ARE mapped, so the rehydrated timeline
/// shows both sides of the conversation.
///
/// # Errors
///
/// Returns an error when the agent is unsupported, the harness cannot be
/// launched, the handshake fails, or `session/load` fails (a stale id
/// after the harness lost its sessions). The caller degrades gracefully:
/// the pane rehydrates to its (empty) live state and the next prompt opens
/// a fresh session.
#[allow(clippy::unwrap_used)]
pub async fn replay(
    agent: &str,
    session_id: String,
    cwd: PathBuf,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(), String> {
    validate_agent(agent)?;
    let entry = ACP_AGENTS
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("unknown agent '{agent}'"))?;
    let launcher = agent_launcher(entry)?
        .with_debug(|line, direction| {
            if matches!(direction, LineDirection::Stderr) {
                log::debug!("subagent ACP stderr: {line}");
            }
        });

    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to build ACP runtime: {e}"))?
            .block_on(run_replay_session(
                launcher, session_id, cwd, chunk_tx,
            ))
    })
    .await
    .map_err(|e| format!("sub-agent task failed: {e}"))?
}

/// The rehydrate connection itself: initialize → authenticate (when
/// advertised) → `session/load` → replay notifications, then disconnect.
/// No `session/prompt` is ever sent — a rehydrate pass MUST NOT append to
/// the agent's history. Unlike [`open_or_resume_session`] there is NO
/// fallback: the stored id is the whole point of the call, and falling
/// back to `session/new` would silently replace "the transcript is gone"
/// with a fresh empty session (and a new id binding that loses the old
/// context on the next prompt too).
pub(crate) async fn run_replay_session<T>(
    transport: T,
    session_id: String,
    cwd: PathBuf,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(), String>
where
    T: agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
{
    // The badge emission below runs OUTSIDE the notification closure that
    // moves `chunk_tx`: it gets its own sender clone.
    let model_tx = chunk_tx.clone();
    Client
        .builder()
        .name("cosh")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                // Replay arrives as ordinary `session/update` notifications
                // (the same variants a live turn streams), so the same
                // mapping reconstructs the timeline. Message chunks feed
                // NO closure here — a rehydrate pass returns no report.
                if let Some(event) =
                    SubagentEvent::from_session_update(notification.update)
                {
                    let _ = chunk_tx.send(event);
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<AcpRole>| {
            // No `fs` capabilities: nothing here serves file requests —
            // replay only ever sends notifications.
            let capabilities = ClientCapabilities::new();
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(capabilities),
                )
                .block_task()
                .await
                .map_err(|e| acp_error(format!("ACP initialize failed: {e}")))?;

            // LAZY authentication, same contract as a live turn: the
            // stored id is loaded first, and `authenticate` runs only when
            // the load is rejected with `auth_required` (a proactive call
            // forces harnesses like `devin acp` into a fresh interactive
            // browser login on EVERY connection).
            let loaded = connection
                .send_request(LoadSessionRequest::new(
                    SessionId::new(session_id.clone()),
                    cwd.clone(),
                ))
                .block_task()
                .await
                .map_err(|e| wrap_acp_failure("ACP session/load failed", e));
            let loaded = match loaded {
                Ok(loaded) => loaded,
                Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
                    authenticate_on_auth_required(
                        &connection,
                        &init.auth_methods,
                        &auth_error,
                    )
                    .await?;
                    connection
                        .send_request(LoadSessionRequest::new(
                            SessionId::new(session_id),
                            cwd,
                        ))
                        .block_task()
                        .await
                        .map_err(|e| wrap_acp_failure("ACP session/load failed", e))?
                }
                Err(auth_error) => return Err(auth_error),
            };
            // The replayed pane's badge: same triple as a live turn (the
            // load response carries the session's config options), so a
            // rebooted pane shows its model without needing a new turn.
            let loaded: LoadSessionResponse = loaded;
            let _ = model_tx.send(session_model_info(loaded.config_options.as_deref(), ""));
            Ok(())
        })
        .await
        .map_err(|e| format!("ACP connection failed: {e}"))
}

/// Whether the agent harness supports resuming sessions, read from the
/// capabilities advertised at `initialize`. Both ACP mechanisms count:
/// `sessionCapabilities.resume` (`session/resume`) and the older top-level
/// `session/load` capability (`session/load`).
fn session_resume_support(capabilities: &AgentCapabilities) -> SessionResumeSupport {
    if capabilities.session_capabilities.resume.is_some() {
        SessionResumeSupport::Resume
    } else if capabilities.load_session {
        SessionResumeSupport::Load
    } else {
        SessionResumeSupport::None
    }
}

/// The ACP resume mechanism a harness advertises (Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionResumeSupport {
    /// `session/resume` (`sessionCapabilities.resume`).
    Resume,
    /// The older top-level `session/load` capability.
    Load,
    /// No resume support — every call opens a fresh session.
    None,
}

/// Open (or resume) the session for one ACP prompt turn.
///
/// With `resume: Some(id)` and advertised support, the stored session id is
/// resumed (`session/resume`, falling back to the older `session/load`).
/// ANY failure on the resume path — no advertised capability, a stale id
/// after a harness restart, a transport error — silently falls back to a
/// fresh `session/new`: resume is a default, never a hard dependency.
///
/// Returns the session id to prompt against plus the config options the
/// session came back with (used by [`select_session_model`]; `None` for a
/// plain `session/new` response is carried through as-is).
///
/// # Errors
///
/// Only a failed `session/new` is fatal — the turn cannot proceed without
/// a session.
async fn open_or_resume_session(
    connection: &ConnectionTo<AcpRole>,
    resume: Option<String>,
    cwd: &Path,
    support: SessionResumeSupport,
) -> Result<(SessionId, Option<Vec<SessionConfigOption>>), AcpError> {
    // Exhaustive over (resume, support): no `unreachable!()` arm — the
    // `None`-support and no-id cases fall through to `session/new` below.
    match (resume, support) {
        (Some(id), SessionResumeSupport::Resume) => {
            let response = connection
                .send_request(ResumeSessionRequest::new(
                    SessionId::new(id.as_str()),
                    cwd.to_path_buf(),
                ))
                .block_task()
                .await;
            match response.map(|r: ResumeSessionResponse| (r.config_options,)) {
                Ok((config_options,)) => {
                    log::debug!("sub-agent ACP session resumed: {id}");
                    return Ok((SessionId::new(id), config_options));
                }
                Err(e) => {
                    // Stale id (harness restart) or a resume quirk: fall
                    // through to a fresh session instead of failing the
                    // turn — the sub-agent just loses its previous context.
                    log::warn!(
                        "sub-agent ACP resume of session '{id}' failed ({e}); \
                         falling back to session/new"
                    );
                }
            }
        }
        (Some(id), SessionResumeSupport::Load) => {
            let response = connection
                .send_request(LoadSessionRequest::new(
                    SessionId::new(id.as_str()),
                    cwd.to_path_buf(),
                ))
                .block_task()
                .await;
            match response.map(|r: LoadSessionResponse| (r.config_options,)) {
                Ok((config_options,)) => {
                    log::debug!("sub-agent ACP session loaded: {id}");
                    return Ok((SessionId::new(id), config_options));
                }
                Err(e) => {
                    log::warn!(
                        "sub-agent ACP load of session '{id}' failed ({e}); \
                         falling back to session/new"
                    );
                }
            }
        }
        (Some(_), SessionResumeSupport::None) => {
            log::debug!(
                "sub-agent harness advertises no session resume support; \
                 starting a fresh session"
            );
        }
        (None, _) => {}
    }

    let session = connection
        .send_request(NewSessionRequest::new(cwd.to_path_buf()))
        .block_task()
        .await
        .map_err(|e| wrap_acp_failure("ACP session/new failed", e))?;
    Ok((session.session_id, session.config_options))
}

/// Lazy ACP authentication: after an `auth_required` rejection, send
/// `authenticate` for the first method the harness handles ITSELF; the
/// caller then retries the rejected operation.
///
/// Proactive authentication turned out to be wrong for several harnesses:
/// `devin acp` (measured) treats every `authenticate` as a fresh
/// interactive login even while holding valid stored credentials — about
/// twenty seconds, a browser flow and a credentials-file rewrite on EVERY
/// session — while the same harness accepts sessions and prompts silently
/// from its own stored login. The protocol's answer is `auth_required`
/// (`ErrorCode::AuthRequired`): work unauthenticated, and authenticate
/// only when the harness rejects an operation.
///
/// `terminal` methods are skipped: the protocol REQUIRES clients to run
/// those as separate interactive processes instead of passing them to
/// `authenticate`, and a headless sub-agent turn cannot open a TUI. The
/// enum is `#[non_exhaustive]` upstream: any future variant is treated as
/// agent-managed (attempted, not skipped) — failing open toward auth, not
/// away from it. When no self-managed method exists (or every one fails),
/// the ORIGINAL error is returned — its message carries the harness's own
/// auth guidance (e.g. "Please log in to use Devin"), which the caller
/// surfaces verbatim.
///
/// # Errors
///
/// Returns the passed-in `trigger` unchanged when there is nothing to
/// authenticate with, or the last `authenticate` failure otherwise.
async fn authenticate_on_auth_required(
    connection: &ConnectionTo<AcpRole>,
    auth_methods: &[AuthMethod],
    trigger: &AcpError,
) -> Result<(), AcpError> {
    log::debug!("sub-agent ACP operation rejected with auth_required ({trigger})");
    let mut last_failure: Option<AcpError> = None;
    for method in auth_methods {
        if matches!(method, AuthMethod::Terminal(_)) {
            continue;
        }
        match connection
            .send_request(AuthenticateRequest::new(method.id().clone()))
            .block_task()
            .await
        {
            Ok(_) => {
                log::debug!("sub-agent ACP authenticated with method '{}'", method.id().0);
                return Ok(());
            }
            Err(e) => {
                log::debug!(
                    "sub-agent ACP authenticate with method '{}' failed: {e}",
                    method.id().0
                );
                // Deliberately NOT `wrap_acp_failure`: the failure of an
                // authenticate attempt itself is terminal (retrying the
                // operation without a successful authentication is
                // pointless), so preserving its wire code would only
                // invite an outer retry loop.
                last_failure = Some(acp_error(format!(
                    "ACP authenticate with method '{}' failed: {e}",
                    method.id().0
                )));
            }
        }
    }
    match last_failure {
        Some(failure) => Err(failure),
        // Nothing self-managed to try: surface the harness's own guidance.
        None => Err(trigger.clone()),
    }
}

/// Select the agent's preferred session model, when one is registered and
/// the harness advertises a `model` config option in `session/new`.
///
/// A mismatch here is fatal on some harnesses (e.g. kilo's ACP default is a
/// paid-tier model that fails the prompt with "You need to sign in", while
/// its own CLI default is a working free model), so a selection that the
/// harness rejects aborts the turn. An unregistered model (empty string) or
/// a harness without a `model` option is a no-op.
///
/// # Errors
///
/// Returns an error when the model is not among the harness's offered
/// values, or when the `session/set_config_option` request fails. The wire
/// error's protocol code is preserved so a rejection can be recognized as
/// `auth_required` by the lazy-auth retry.
async fn select_session_model(
    connection: &ConnectionTo<AcpRole>,
    session_id: &SessionId,
    config_options: Option<&[SessionConfigOption]>,
    preferred_model: &str,
) -> Result<(), AcpError> {
    if preferred_model.is_empty() {
        return Ok(());
    }
    let Some(option) = config_options
        .unwrap_or_default()
        .iter()
        .find(|option| option.id.0.as_ref() == "model")
    else {
        return Ok(());
    };
    let SessionConfigKind::Select(select) = &option.kind else {
        return Ok(());
    };
    if select.current_value.0.as_ref() == preferred_model {
        return Ok(());
    }
    let offered: Vec<&str> = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => {
            options.iter().map(|o| o.value.0.as_ref()).collect()
        }
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|g| g.options.iter().map(|o| o.value.0.as_ref()))
            .collect(),
        _ => Vec::new(),
    };
    if !offered.contains(&preferred_model) {
        return Err(acp_error(format!(
            "ACP session model '{preferred_model}' is not offered by this harness; \
             update the agent's `model` entry in the registry",
        )));
    }
    connection
        .send_request(SetSessionConfigOptionRequest::new(
            session_id.clone(),
            SessionConfigId::new("model"),
            SessionConfigValueId::new(preferred_model),
        ))
        .block_task()
        .await
        .map_err(|e| wrap_acp_failure("ACP session/set model failed", e))?;
    Ok(())
}

/// Read the session's badge triple from its ACP config options: the
/// effective model id (+ human label, when the harness names the value),
/// the serving provider (+ label, when a `provider` option is advertised
/// at all — cline advertises one, kilo/opencode do not), and the
/// reasoning/thought level display.
///
/// The model lookup prefers the spec's `Model` category and falls back to
/// the `model` option id (the same id [`select_session_model`] matches —
/// harnesses predate categories). The effective id is the preferred model
/// when one was selected, else the advertised current value. The provider
/// lookup is id-only (`provider`): no spec category exists for it.
/// Thought level is category-only (`ThoughtLevel`), and only when the
/// option offers MORE than one value — a single offered value (kilo's
/// fixed `thinking`) is a marker, not a level, and stays empty. Boolean
/// options are skipped in every slot — a model, provider or level
/// selector is a value, not a toggle.
pub(crate) fn session_model_info(
    config_options: Option<&[SessionConfigOption]>,
    preferred_model: &str,
) -> super::events::SubagentEvent {
    let options = config_options.unwrap_or_default();
    let model_option = options
        .iter()
        .find(|o| matches!(o.category, Some(SessionConfigOptionCategory::Model)))
        .or_else(|| options.iter().find(|o| o.id.0.as_ref() == "model"));
    let (model, model_label) = match model_option {
        Some(option) => match &option.kind {
            SessionConfigKind::Select(select) => {
                let current = if preferred_model.is_empty() {
                    select.current_value.0.to_string()
                } else {
                    preferred_model.to_string()
                };
                (Some(current.clone()), select_value_name(select, &current))
            }
            _ => (None, None),
        },
        None => (None, None),
    };
    let (provider, provider_label) = match options.iter().find(|o| o.id.0.as_ref() == "provider") {
        Some(option) => match &option.kind {
            SessionConfigKind::Select(select) => {
                let current = select.current_value.0.to_string();
                (Some(current.clone()), select_value_name(select, &current))
            }
            _ => (None, None),
        },
        None => (None, None),
    };
    let reasoning = options
        .iter()
        .find(|o| matches!(o.category, Some(SessionConfigOptionCategory::ThoughtLevel)))
        .and_then(|option| match &option.kind {
            SessionConfigKind::Select(select) if select_value_count(select) > 1 => {
                let current = select.current_value.0.to_string();
                Some(select_value_name(select, &current).unwrap_or(current))
            }
            _ => None,
        });
    super::events::SubagentEvent::SessionModel {
        model,
        model_label,
        provider,
        provider_label,
        reasoning,
    }
}

/// How many values a Select option offers (ungrouped + grouped): a
/// thought level with a single offered value is a fixed marker, not a
/// level the session could hold instead.
fn select_value_count(select: &SessionConfigSelect) -> usize {
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options.len(),
        SessionConfigSelectOptions::Grouped(groups) => groups.iter().map(|g| g.options.len()).sum(),
        _ => 0,
    }
}

/// Human label of `value` among a Select option's offered values
/// (ungrouped or grouped), `None` when the harness names nothing.
fn select_value_name(select: &SessionConfigSelect, value: &str) -> Option<String> {
    let found = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .find(|o| o.value.0.as_ref() == value)
            .map(|o| o.name.clone()),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|g| g.options.iter())
            .find(|o| o.value.0.as_ref() == value)
            .map(|o| o.name.clone()),
        _ => None,
    };
    found.filter(|name| !name.is_empty())
}

/// Run one full ACP prompt turn against the given agent-side transport.
///
/// `transport` is anything implementing [`ConnectTo`] for the agent role: in
/// production it is the [`AcpAgent`] subprocess launcher; tests use an
/// in-memory [`Channel`](agent_client_protocol::Channel) instead. Returns
/// the prompt's stop reason on success. `select_session_model` runs before
/// the prompt, and the shared stop flag is raced against the prompt await
/// (Phase 5 cancellation: a trigger sends `session/cancel` and the turn
/// still ends with the agent's own answer). Message text feeds the
/// [`TurnClosure`](super::closure::TurnClosure) — only the message written
/// after the LAST tool call is the returned report (the market-standard
/// "final message" contract), with a defensive fallback to the last
/// completed message when the turn ends right after a tool call; every
/// mapped session update is streamed through `chunk_tx` as a typed
/// [`SubagentEvent`].
///
/// # Errors
///
/// Returns an error if any step of the ACP handshake or the prompt turn
/// fails.
#[allow(clippy::unwrap_used)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_session<T>(
    transport: T,
    input: String,
    resume: Option<String>,
    cwd: PathBuf,
    preferred_model: &'static str,
    accumulated: Arc<Mutex<super::closure::TurnClosure>>,
    stop_signal: Arc<AtomicBool>,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(String, String), String>
where
    T: agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
{
    // `fs/*` requests are served against the real filesystem, sandboxed to
    // the session workspace root. The root is pinned to a VERIFIED directory
    // descriptor once per turn (see `sandbox::PinnedRoot`): the handlers
    // below clone that descriptor's handle and every `fs/*` resolution
    // walks `openat`-relative from it — no path is ever re-resolved after
    // verification, closing the validate-then-use window (including at the
    // root itself).
    // Non-Unix has no descriptor pinning; the handlers fall back to the
    // lexical validate-then-serve path (`ensure_path_within`), so hand them
    // the plain root path there.
    #[cfg(unix)]
    let read_root = super::sandbox::PinnedRoot::acquire(&cwd).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let write_root = super::sandbox::PinnedRoot::acquire(&cwd).map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let read_root = cwd.clone();
    #[cfg(not(unix))]
    let write_root = cwd.clone();

    // Route the transport through the generic connection machinery. The
    // subprocess launcher gets stderr line logging; in-memory test channels
    // pass through untouched. The badge emission below runs OUTSIDE the
    // notification closure that moves `chunk_tx`: its own sender clone.
    let model_tx = chunk_tx.clone();
    Client
        .builder()
        .name("cosh")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                // Map every session update with a TUI representation into a
                // typed event; message chunks additionally feed the turn's
                // closure (the last-message accumulator that produces the
                // returned report).
                match SubagentEvent::from_session_update(notification.update) {
                    Some(event) => {
                        accumulated.lock().unwrap().observe(&event);
                        let _ = chunk_tx.send(event);
                    }
                    // Updates with no display representation (user chunks,
                    // available-commands refreshes, unstable variants):
                    // log instead of silently dropping.
                    None => {
                        log::debug!("sub-agent session update without a display mapping");
                    }
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        // Headless automation: auto-approve (YOLO style), so the sub-agent
        // never blocks on a dialog nobody can answer. Option ORDER is
        // harness-defined — some harnesses list reject-like choices first —
        // so an explicit allow-kind option is preferred over "first option"
        // whenever one exists. With no options, cancel the request per the
        // spec.
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                // Explicit preference order: AllowAlways first, then
                // AllowOnce — both auto-approve, but Always avoids the same
                // prompt coming back for every later call. Without any
                // allow-kind option, the first listed option is picked as a
                // last resort; its kind is logged so a reject-like auto-
                // approval is visible in debug output.
                let selected = request
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowAlways)
                    .or_else(|| {
                        request
                            .options
                            .iter()
                            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    })
                    .or_else(|| request.options.first());
                match selected {
                    Some(option) => {
                        log::debug!(
                            "sub-agent permission auto-approved: option '{}' (kind {:?}) for tool call {}",
                            option.option_id.0,
                            option.kind,
                            request.tool_call.tool_call_id.0
                        );
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                            )),
                        ))
                    }
                    None => responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    )),
                }
            },
            on_receive_request!(),
        )
        // Serve the agent's filesystem capability against the real workspace
        // (absolute paths, 1-based lines per the protocol contract), SANDBOXED
        // to the session workspace root: a path that escapes `cwd` (through
        // `..` segments or symlinks) is rejected with an ACP error instead of
        // being served.
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _connection| {
                let content = serve_read(
                    &read_root,
                    &request.path,
                    request.line,
                    request.limit,
                )?;
                responder.respond(ReadTextFileResponse::new(content))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _connection| {
                serve_write(&write_root, &request.path, &request.content)?;
                responder.respond(WriteTextFileResponse::new())
            },
            on_receive_request!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<AcpRole>| {
            // Advertise exactly what we serve below: fs read/write. Without
            // this, spec-conformant agents never send `fs/*` requests.
            let capabilities = ClientCapabilities::new().fs(FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true));
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(capabilities),
                )
                .block_task()
                .await
                .map_err(|e| acp_error(format!("ACP initialize failed: {e}")))?;

            // LAZY authentication: no proactive `authenticate` call even
            // when the harness advertises auth methods — see
            // [`authenticate_on_auth_required`] for why (measured on
            // `devin acp`: a proactive call forces a fresh interactive
            // browser login on EVERY session, while the harness's own
            // stored login is honored silently when nobody asks it to
            // re-authenticate). The advertised methods are kept and used
            // ONLY when an operation below is rejected with
            // `auth_required`, at which point one self-managed
            // authentication attempt precedes the retry. A harness that
            // manages auth itself advertises no methods, and the retry
            // path then simply re-surfaces its own error message.
            let (session_id, config_options) =
                match open_or_resume_session(
                    &connection,
                    resume.clone(),
                    &cwd,
                    session_resume_support(&init.agent_capabilities),
                )
                .await
                {
                    Ok(session) => session,
                    Err(auth_error)
                        if auth_error.code == ErrorCode::AuthRequired =>
                    {
                        authenticate_on_auth_required(
                            &connection,
                            &init.auth_methods,
                            &auth_error,
                        )
                        .await?;
                        open_or_resume_session(
                            &connection,
                            resume,
                            &cwd,
                            session_resume_support(&init.agent_capabilities),
                        )
                        .await?
                    }
                    Err(auth_error) => return Err(auth_error),
                };

            // Select the preferred session model when the agent registered
            // one and the harness advertises a `model` config option. A
            // mismatch here is fatal on some harnesses (e.g. kilo's ACP
            // default is a paid-tier model that fails the prompt with "You
            // need to sign in"), so a failed selection aborts the turn.
            // Resumed sessions carry their config options in the resume
            // response, so the selection applies to them too.
            let model_selection = select_session_model(
                &connection,
                &session_id,
                config_options.as_deref(),
                preferred_model,
            )
            .await;
            match model_selection {
                Ok(()) => {}
                Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
                    authenticate_on_auth_required(
                        &connection,
                        &init.auth_methods,
                        &auth_error,
                    )
                    .await?;
                    select_session_model(
                        &connection,
                        &session_id,
                        config_options.as_deref(),
                        preferred_model,
                    )
                    .await?;
                }
                Err(auth_error) => return Err(auth_error),
            }

            // The prompt badge's data: the session's serving triple, read
            // from the same config options the selection above ran
            // against. Emitted BEFORE the prompt so the pane shows what
            // serves the turn from its first chunk; anything the harness
            // does not advertise stays empty.
            let _ = model_tx.send(session_model_info(config_options.as_deref(), preferred_model));

            let prompt_request = connection
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(&input))],
                ))
                .block_task();

            // Cancellation (Phase 5): the shared stop flag is raced against
            // the prompt await. On trigger, `session/cancel` is sent as a
            // fire-and-forget notification and the prompt is STILL awaited —
            // the ACP spec requires the agent to end the turn itself,
            // answering with `StopReason::Cancelled`. This is the ONLY way
            // the turn ends early: there is no time limit.
            tokio::pin!(prompt_request);
            let stop_wait = async {
                while !stop_signal.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            let prompt = tokio::select! {
                prompt = &mut prompt_request => prompt,
                () = stop_wait => {
                    log::debug!("sub-agent ACP turn cancelled by stop signal");
                    if let Err(e) =
                        connection.send_notification(CancelNotification::new(session_id.clone()))
                    {
                        log::warn!("sub-agent ACP cancel notification failed: {e}");
                    }
                    (&mut prompt_request).await
                }
            };
            let prompt = prompt
                .map_err(|e| wrap_acp_failure("ACP session/prompt failed", e));
            // The lazy-auth retry for the PROMPT is not raced against the
            // stop signal: a `auth_required` rejection arrives quickly (it
            // precedes any model work), and authenticating first is exactly
            // what a user-triggered login would have done before the turn.
            let prompt = match prompt {
                Ok(prompt) => Ok(prompt),
                Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
                    authenticate_on_auth_required(
                        &connection,
                        &init.auth_methods,
                        &auth_error,
                    )
                    .await?;
                    // Same session: the rejection happened after the
                    // session existed, so retrying on a NEW session would
                    // strand this turn's history in a fresh one.
                    connection
                        .send_request(PromptRequest::new(
                            session_id.clone(),
                            vec![ContentBlock::Text(TextContent::new(input))],
                        ))
                        .block_task()
                        .await
                        .map_err(|e| wrap_acp_failure("ACP session/prompt failed", e))
                }
                Err(auth_error) => Err(auth_error),
            }?;

            // The session id flows back to the caller: it is stored per
            // agent name and reused by the next `continue_session` call.
            Ok((
                stop_reason_str(prompt.stop_reason),
                session_id.0.to_string(),
            ))
        })
        .await
        .map_err(|e| format!("ACP connection failed: {e}"))
}
