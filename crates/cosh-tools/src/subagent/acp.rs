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
//! 3. `authenticate` — when the agent advertises auth methods.
//! 4. `session/new` — opens a session rooted at the workspace directory.
//! 5. `session/prompt` — sends the task as the user prompt.
//! 6. `session/update` notifications — agent message chunks are streamed
//!    live (to the TUI) and accumulated.
//! 7. The turn ends when the `session/prompt` response arrives with a
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
    AgentCapabilities, AuthenticateRequest, CancelNotification, ClientCapabilities, ContentBlock,
    FileSystemCapabilities, InitializeRequest, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, PermissionOptionKind, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, ResumeSessionResponse,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigKind, SessionConfigOption,
    SessionConfigSelectOptions, SessionConfigValueId, SessionId, SessionNotification,
    SetSessionConfigOptionRequest, StopReason, TextContent, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::{
    AcpAgent, Agent as AcpRole, Client, ConnectionTo, Error as AcpError, LineDirection,
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

/// Default max time to wait for the prompt turn to complete.
pub(crate) const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// Resolve the effective call timeout. Honors the `COSH_SUBAGENT_TIMEOUT_SECS`
/// environment variable (in seconds); falls back to [`DEFAULT_CALL_TIMEOUT`].
///
/// Values that are missing, non-numeric, or less than one second are ignored
/// (they fall back to [`DEFAULT_CALL_TIMEOUT`]) so a `0`/garbage value cannot
/// make calls time out instantly.
fn call_timeout() -> Duration {
    timeout_from_secs(std::env::var("COSH_SUBAGENT_TIMEOUT_SECS").ok().as_deref())
}

/// Pure resolver for the call timeout, parameterized over the raw env value
/// so the parsing contract is testable without mutating process-global env.
pub(crate) fn timeout_from_secs(raw: Option<&str>) -> Duration {
    match raw {
        Some(s) => match s.trim().parse::<u64>() {
            Ok(secs) if secs >= 1 => Duration::from_secs(secs),
            _ => DEFAULT_CALL_TIMEOUT,
        },
        None => DEFAULT_CALL_TIMEOUT,
    }
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

/// Sentinel error distinguishing a timeout teardown from other failures.
const TIMEOUT_ERROR: &str = "cosh:subagent:timeout";

/// Call a sub-agent harness over ACP and return its final output.
///
/// 1. Looks up `agent` in [`ACP_AGENTS`] to get the ACP launch command.
/// 2. Runs a full ACP client turn (see the [module docs](self)):
///    `initialize` → `authenticate` (when advertised) → `session/new` or,
///    when resuming, `session/resume`/`session/load` (falling back to
///    `session/new` on any resume failure) → `session/prompt`,
///    auto-approving permission requests and serving `fs/*` requests.
///    The shared stop flag is raced against the prompt await; a trigger
///    sends `session/cancel` and the agent ends the turn itself.
/// 3. Streams typed [`SubagentEvent`]s through `chunk_tx` (for the TUI)
///    while accumulating the message text into the full output.
/// 4. Returns `(accumulated_output, stop_reason, Option<session_id>)` when
///    the turn ends — the session id only when it ended protocol-clean
///    (`Some` is withheld on error and timeout arms) — or an error when
///    the harness fails before producing any output.
///
/// A timeout (`COSH_SUBAGENT_TIMEOUT_SECS`, default 2 minutes) tears down the
/// harness; partial output is returned with stop reason `"timeout"` when any
/// was produced.
///
/// The ACP session runs on a dedicated current-thread runtime inside a
/// blocking task, isolating the SDK's connection machinery from the ambient
/// runtime flavor and keeping the caller's future `Send`.
///
/// # Errors
///
/// Returns an error if the agent is unsupported, the harness cannot be
/// launched, the ACP turn fails before any output, or the timeout fires with
/// no output.
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

    let accumulated: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let timeout = call_timeout();
    let input = input.to_string();

    let turn = {
        let accumulated = accumulated.clone();
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to build ACP runtime: {e}"))?
                .block_on(tokio::time::timeout(
                    timeout,
                    run_session(
                        launcher,
                        input,
                        resume,
                        cwd,
                        entry.model,
                        accumulated,
                        stop_signal,
                        chunk_tx,
                    ),
                ))
                // Map the `Elapsed` error to the sentinel so the whole
                // closure shares one error type (String).
                .map_err(|_| TIMEOUT_ERROR.to_string())
        })
        .await
        .map_err(|e| format!("sub-agent task failed: {e}"))?
    };

    let final_output = accumulated.lock().unwrap().clone();

    match turn {
        // Turn completed: the session already wrote every chunk into the buffer.
        Ok(Ok((stop_reason, session_id))) => Ok((final_output, stop_reason, Some(session_id))),
        // Turn errored: partial output (if any) is still worth returning.
        // The turn's own session id is NOT propagated — an errored turn's
        // session state is unreliable, so its id is not (re)stored here.
        // An OLDER stored id for this agent stays in place and is resumed
        // again by the next default call; a stale one self-heals via the
        // `session/new` fallback in `open_or_resume_session`.
        Ok(Err(session_error)) => {
            if final_output.is_empty() {
                Err(session_error)
            } else {
                log::warn!("sub-agent '{agent}' ACP turn failed: {session_error}");
                Ok((final_output, "error".to_string(), None))
            }
        }
        // Timeout: the dropped session future tears down the harness process.
        Err(_elapsed) => {
            if final_output.is_empty() {
                Err(format!(
                    "sub-agent '{agent}' did not respond within {} seconds. \
                     The ACP harness started but produced no output; verify it \
                     is authenticated and speaks ACP. Install/setup: {}",
                    timeout.as_secs(),
                    entry.install_hint,
                ))
            } else {
                log::warn!(
                    "sub-agent '{agent}' timed out after {}s, returning partial output",
                    timeout.as_secs(),
                );
                // The session id is NOT propagated — a torn-down harness's
                // session state is unreliable, so the next call starts fresh.
                Ok((final_output, "timeout".to_string(), None))
            }
        }
    }
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
        .map_err(|e| acp_error(format!("ACP session/new failed: {e}")))?;
    Ok((session.session_id, session.config_options))
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
/// values, or when the `session/set_config_option` request fails.
async fn select_session_model(
    connection: &ConnectionTo<AcpRole>,
    session_id: &SessionId,
    config_options: Option<&[SessionConfigOption]>,
    preferred_model: &str,
) -> Result<(), String> {
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
        return Err(format!(
            "ACP session model '{preferred_model}' is not offered by this harness; \
             update the agent's `model` entry in the registry",
        ));
    }
    connection
        .send_request(SetSessionConfigOptionRequest::new(
            session_id.clone(),
            SessionConfigId::new("model"),
            SessionConfigValueId::new(preferred_model),
        ))
        .block_task()
        .await
        .map_err(|e| format!("ACP set model failed: {e}"))?;
    Ok(())
}

/// Run one full ACP prompt turn against the given agent-side transport.
///
/// `transport` is anything implementing [`ConnectTo`] for the agent role: in
/// production it is the [`AcpAgent`] subprocess launcher; tests use an
/// in-memory [`Channel`](agent_client_protocol::Channel) instead. Returns
/// the prompt's stop reason on success. `select_session_model` runs before
/// the prompt, and the shared stop flag is raced against the prompt await
/// (Phase 5 cancellation: a trigger sends `session/cancel` and the turn
/// still ends with the agent's own answer). Message text is appended to
/// `accumulated`; every mapped session update is streamed through
/// `chunk_tx` as a typed [`SubagentEvent`].
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
    accumulated: Arc<Mutex<String>>,
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
    // pass through untouched.
    Client
        .builder()
        .name("cosh")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                // Map every session update with a TUI representation into a
                // typed event; message chunks additionally keep flowing into
                // the accumulated text buffer (the persisted final output).
                match SubagentEvent::from_session_update(notification.update) {
                    Some(event) => {
                        if let SubagentEvent::Message { text } = &event {
                            accumulated.lock().unwrap().push_str(text);
                        }
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

            // Authenticate when the agent advertises methods (e.g. `goose
            // acp`). The user's existing login is reused: the first method
            // that succeeds wins, and a failing method is skipped rather
            // than aborting — an adapter can advertise several (codex:
            // `api-key` requires env keys the user may not have, while
            // `chat-gpt` works with the stored `codex login` state). A
            // harness that manages auth itself advertises no methods and is
            // skipped.
            let mut auth_error = None;
            for method in &init.auth_methods {
                match connection
                    .send_request(AuthenticateRequest::new(method.id().clone()))
                    .block_task()
                    .await
                {
                    Ok(_) => {
                        auth_error = None;
                        break;
                    }
                    Err(e) => {
                        log::debug!(
                            "sub-agent ACP authenticate with method '{}' failed: {e}",
                            method.id().0
                        );
                        auth_error = Some(format!("ACP authenticate failed: {e}"));
                    }
                }
            }
            if let Some(e) = auth_error {
                return Err(acp_error(e));
            }

            let (session_id, config_options) =
                open_or_resume_session(
                    &connection,
                    resume,
                    &cwd,
                    session_resume_support(&init.agent_capabilities),
                )
                .await?;

            // Select the preferred session model when the agent registered
            // one and the harness advertises a `model` config option. A
            // mismatch here is fatal on some harnesses (e.g. kilo's ACP
            // default is a paid-tier model that fails the prompt with "You
            // need to sign in"), so a failed selection aborts the turn.
            // Resumed sessions carry their config options in the resume
            // response, so the selection applies to them too.
            select_session_model(
                &connection,
                &session_id,
                config_options.as_deref(),
                preferred_model,
            )
            .await
            .map_err(acp_error)?;

            let prompt_request = connection
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(input))],
                ))
                .block_task();

            // Cancellation (Phase 5): the shared stop flag is raced against
            // the prompt await. On trigger, `session/cancel` is sent as a
            // fire-and-forget notification and the prompt is STILL awaited —
            // the ACP spec requires the agent to end the turn itself,
            // answering with `StopReason::Cancelled`. The outer timeout in
            // `call` is the backstop for a harness that never answers.
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
                .map_err(|e| acp_error(format!("ACP session/prompt failed: {e}")))?;

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
