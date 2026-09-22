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
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, ClientCapabilities, ContentBlock, FileSystemCapabilities,
    InitializeRequest, NewSessionRequest, NewSessionResponse, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigId, SessionConfigKind,
    SessionConfigSelectOptions, SessionConfigValueId, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{
    AcpAgent, Agent as AcpRole, Client, ConnectionTo, Error as AcpError, LineDirection,
    on_receive_notification, on_receive_request,
};

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
///    `initialize` → `authenticate` (when advertised) → `session/new` on
///    `cwd` → `session/prompt`, auto-approving permission requests and
///    serving `fs/*` requests.
/// 3. Streams agent message chunks through `chunk_tx` (for the TUI) while
///    accumulating the full output.
/// 4. Returns `(accumulated_output, stop_reason)` when the turn ends, or an
///    error when the harness fails before producing any output.
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
    cwd: PathBuf,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<(String, String), String> {
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
                    run_session(launcher, input, cwd, entry.model, accumulated, chunk_tx),
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
        Ok(Ok(stop_reason)) => Ok((final_output, stop_reason)),
        // Turn errored: partial output (if any) is still worth returning.
        Ok(Err(session_error)) => {
            if final_output.is_empty() {
                Err(session_error)
            } else {
                log::warn!("sub-agent '{agent}' ACP turn failed: {session_error}");
                Ok((final_output, "error".to_string()))
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
                Ok((final_output, "timeout".to_string()))
            }
        }
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
/// values, or when the `session/set_config_option` request fails.
async fn select_session_model(
    connection: &ConnectionTo<AcpRole>,
    session: &NewSessionResponse,
    preferred_model: &str,
) -> Result<(), String> {
    if preferred_model.is_empty() {
        return Ok(());
    }
    let Some(option) = session
        .config_options
        .as_deref()
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
            session.session_id.clone(),
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
/// the prompt's stop reason on success. Agent message chunks are appended
/// to `accumulated` and streamed through `chunk_tx` as they arrive.
///
/// # Errors
///
/// Returns an error if any step of the ACP handshake or the prompt turn
/// fails.
#[allow(clippy::unwrap_used)]
pub(crate) async fn run_session<T>(
    transport: T,
    input: String,
    cwd: PathBuf,
    preferred_model: &'static str,
    accumulated: Arc<Mutex<String>>,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<String, String>
where
    T: agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
{
    // Route the transport through the generic connection machinery. The
    // subprocess launcher gets stderr line logging; in-memory test channels
    // pass through untouched.
    Client
        .builder()
        .name("cosh")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let SessionUpdate::AgentMessageChunk(chunk) = notification.update else {
                    return Ok(());
                };
                if let ContentBlock::Text(text) = chunk.content {
                    accumulated.lock().unwrap().push_str(&text.text);
                    let _ = chunk_tx.send(text.text);
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        // Headless automation: auto-approve by selecting the first option
        // (YOLO style), so the sub-agent never blocks on a dialog nobody
        // can answer. With no options, cancel the request per the spec.
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| match request
                .options
                .first()
            {
                Some(option) => responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        option.option_id.clone(),
                    )),
                )),
                None => responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                )),
            },
            on_receive_request!(),
        )
        // Serve the agent's filesystem capability against the real workspace
        // (absolute paths, 1-based lines per the protocol contract).
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _connection| {
                let content = std::fs::read_to_string(&request.path).map_err(|e| {
                    acp_error(format!("failed to read {}: {e}", request.path.display()))
                })?;
                let start = request
                    .line
                    .map_or(0, |line| line.saturating_sub(1) as usize);
                let selected = match request.limit {
                    None if start == 0 => content,
                    limit => content
                        .lines()
                        .skip(start)
                        .take(limit.map_or(usize::MAX, |l| l as usize))
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                responder.respond(ReadTextFileResponse::new(selected))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _connection| {
                if let Some(parent) = request.path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&request.path, &request.content).map_err(|e| {
                    acp_error(format!("failed to write {}: {e}", request.path.display()))
                })?;
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

            let session = connection
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await
                .map_err(|e| acp_error(format!("ACP session/new failed: {e}")))?;

            // Select the preferred session model when the agent registered
            // one and the harness advertises a `model` config option. A
            // mismatch here is fatal on some harnesses (e.g. kilo's ACP
            // default is a paid-tier model that fails the prompt with "You
            // need to sign in"), so a failed selection aborts the turn.
            select_session_model(&connection, &session, preferred_model)
                .await
                .map_err(acp_error)?;

            let prompt = connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new(input))],
                ))
                .block_task()
                .await
                .map_err(|e| acp_error(format!("ACP session/prompt failed: {e}")))?;

            Ok(format!("{:?}", prompt.stop_reason))
        })
        .await
        .map_err(|e| format!("ACP connection failed: {e}"))
}
