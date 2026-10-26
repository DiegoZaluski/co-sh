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
//! | `copilot` | `copilot --acp` |
//! | `qwen` | `qwen --acp` |
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
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1 as schema;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities,
    ContentBlock, FileSystemCapabilities, InitializeRequest, LoadSessionRequest,
    LoadSessionResponse, NewSessionRequest, PermissionOptionKind, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, ResumeSessionResponse,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectOptions,
    SessionConfigValueId, SessionId, SessionNotification, SetSessionConfigOptionRequest,
    SetSessionModeRequest, StopReason, TextContent, WriteTextFileRequest, WriteTextFileResponse,
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

    /// Build an agent from a user-registered launch line (`command args…`,
    /// the same shape the ACP registry documents for custom agents). The
    /// command becomes the spawn binary and every remaining word an
    /// argument — the exact convention the static [`ACP_AGENTS`] table
    /// uses, so a user line behaves identically to a built-in entry.
    ///
    /// The parsed words are leaked into `&'static str`s: [`Agent`] is a
    /// `Copy` type of static strings, and re-registration happens only on
    /// setup edits — a handful of short strings per edit, never in a loop.
    #[must_use]
    pub fn from_launch_line(name: &str, launch: &str) -> Option<Self> {
        let launch = launch.trim();
        if name.trim().is_empty() || launch.is_empty() {
            return None;
        }
        let leak =
            |s: &str| Box::leak(s.to_string().into_boxed_str()) as &'static str;
        // The whole word slice is leaked too, so `split_first` refs outlive
        // this call the same way the strings do.
        let words: &'static [&'static str] = Box::leak(
            launch
                .split_whitespace()
                .map(leak)
                .collect::<Vec<&'static str>>()
                .into_boxed_slice(),
        );
        let (command, args) = words.split_first()?;
        Some(Self {
            name: leak(name),
            command,
            args,
            requires: std::slice::from_ref(command),
            install_hint: "",
            model: "",
        })
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
    Agent {
        name: "copilot",
        // GitHub Copilot CLI is ACP-native: `copilot --acp` speaks the
        // protocol over stdio (public preview, per the GitHub changelog).
        command: "copilot",
        args: &["--acp"],
        requires: &["copilot"],
        install_hint: "Install: npm install -g @github/copilot. Then: copilot (sign in on first run). More: https://github.com/features/copilot/cli/",
        model: "",
    },
    Agent {
        name: "qwen",
        // Qwen Code (Alibaba's Gemini-CLI fork) is ACP-native via the
        // `--acp` flag; `--experimental-skills` is the registry's
        // documented launch line.
        command: "qwen",
        args: &["--acp"],
        requires: &["qwen"],
        install_hint: "Install: npm install -g @qwen-code/qwen-code. More: https://github.com/QwenLM/qwen-code",
        model: "",
    },
];
/// User-registered ACP harnesses (Settings → Integrations → ACP agents),
/// held as owned [`Agent`] values whose strings were leaked into
/// `&'static str`s by [`Agent::from_launch_line`]. Every lookup helper
/// (registry, validation, installation detection, launch) merges these with
/// the built-in [`ACP_AGENTS`] table; a user entry may NOT shadow a
/// built-in name (the registrar rejects duplicates, so the merge can never
/// hide a conflict).
///
/// The registry is process-global by design: the harness's tool description
/// is built once per agent loop, while setup edits land between agent
/// loops — each loop rebuilds its harness, so a freshly saved registration
/// is picked up by the next loop and the `subagent_call` description stays
/// consistent with what it can spawn.
static CUSTOM_AGENTS: RwLock<Vec<Agent>> = RwLock::new(Vec::new());
/// Replace the user-registered agent set (Settings save path). `entries`
/// maps agent name → launch command (`command args…`). Names must be single
/// words (they key the `subagent_call` tool's agent enum), must not collide
/// with a built-in name even in different case, and must not duplicate each
/// other; violations are rejected with an error, so a typo can never
/// silently shadow a built-in or hide a sibling.
///
/// # Errors
///
/// Returns an error when a name is empty or multi-word, duplicates a
/// built-in agent, or duplicates another entry in the same call.
pub fn register_custom_agents(entries: &[(String, String)]) -> Result<(), String> {
    let mut agents = Vec::with_capacity(entries.len());
    for (name, launch) in entries {
        let name = name.trim();
        if name.is_empty() {
            return Err("Agent name is required.".into());
        }
        if name.split_whitespace().count() != 1 {
            return Err(format!(
                "'{name}' must be a single word (it is the agent id the model calls)."
            ));
        }
        if ACP_AGENTS
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "'{name}' is a built-in agent and cannot be registered again."
            ));
        }
        if agents.iter().any(|a: &Agent| a.name == name) {
            return Err(format!("'{name}' is registered more than once."));
        }
        let agent = Agent::from_launch_line(name, launch).ok_or_else(|| {
            format!("'{name}' needs a launch command, e.g. my-agent --acp.")
        })?;
        agents.push(agent);
    }
    *CUSTOM_AGENTS
        .write()
        .unwrap_or_else(PoisonError::into_inner) = agents;
    // A fresh registration invalidates the probe cache: the entry the user
    // just saved (or removed) must not show a stale installed glyph.
    invalidate_custom_probe_cache();
    Ok(())
}

/// Load the persisted registrations at startup, tolerating individual bad
/// entries: each invalid entry is skipped and reported, the valid ones are
/// still published. The interactive save path keeps the strict all-or-
/// nothing contract ([`register_custom_agents`]) because there the user
/// gets immediate feedback; a hand-edited `setup.json` must not disable
/// every working harness because one entry rotted (e.g. a name that a
/// later cosh release turned into a built-in).
///
/// Returns one message per skipped entry, in entry order (empty when all
/// entries were accepted).
#[must_use]
pub fn register_custom_agents_lossy(entries: &[(String, String)]) -> Vec<String> {
    let mut kept: Vec<(String, String)> = Vec::with_capacity(entries.len());
    let mut rejected: Vec<String> = Vec::new();
    for (name, launch) in entries {
        let probe: &[(String, String)] = &kept;
        if let Err(error) = register_probe(probe, name, launch) {
            rejected.push(error);
        } else {
            // Store the TRIMMED name: `register_probe` validates the
            // trimmed form, so the re-check below must see the same
            // strings or a whitespace-padded duplicate would slip through
            // probing and then fail the strict pass (publishing nothing).
            kept.push((name.trim().to_string(), launch.clone()));
        }
    }
    if let Err(error) = register_custom_agents(&kept) {
        // Can't happen: every kept entry passed the same checks above
        // against the same predecessor set. Fail closed rather than
        // publishing a partially validated set.
        rejected.push(error);
        return rejected;
    }
    rejected
}

/// Validate one entry against the entries accepted before it (the shared
/// kernel of [`register_custom_agents`] and [`register_custom_agents_lossy`]).
fn register_probe(
    accepted: &[(String, String)],
    name: &str,
    launch: &str,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Agent name is required.".into());
    }
    if name.split_whitespace().count() != 1 {
        return Err(format!(
            "'{name}' must be a single word (it is the agent id the model calls)."
        ));
    }
    if ACP_AGENTS
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case(name))
    {
        return Err(format!(
            "'{name}' is a built-in agent and cannot be registered again."
        ));
    }
    if accepted.iter().any(|(kept, _)| kept == name) {
        return Err(format!("'{name}' is registered more than once."));
    }
    if Agent::from_launch_line(name, launch).is_none() {
        return Err(format!("'{name}' needs a launch command, e.g. my-agent --acp."));
    }
    Ok(())
}

/// Look up a registered agent (built-ins first, then user registrations).
#[must_use]
pub fn find_agent(agent: &str) -> Option<Agent> {
    ACP_AGENTS
        .iter()
        .find(|a| a.name == agent)
        .copied()
        .or_else(|| {
            CUSTOM_AGENTS
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|a| a.name == agent)
                .copied()
        })
}

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

/// Wall-clock bound for ONE ACP handshake step (initialize, session open,
/// config picks, mode bind). A harness that never answers its handshake
/// would otherwise block the requesting agent loop forever with no event and
/// no error — the docs of [`call`] keep the TURN unbounded by design (work
/// can legitimately be long), but the handshake is setup, not work: past
/// this bound it is a hung launch, and the tool must FAIL so the requesting
/// loop continues.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// Run one handshake step under [`HANDSHAKE_TIMEOUT`].
async fn handshake_step<T>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, AcpError>>,
) -> Result<T, AcpError> {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, fut).await {
        Ok(result) => result,
        Err(_) => Err(acp_error(format!(
            "ACP {what} timed out after {}s — the harness never answered the handshake; \
             the launch is hung",
            HANDSHAKE_TIMEOUT.as_secs()
        ))),
    }
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
/// User-registered agents carry no canned guidance (their entry IS the
/// configuration), so only built-ins resolve to text.
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

/// Whether the user-registered `agent` has every required binary in PATH.
/// Answers come from a short-TTL cache (see [`custom_installed_cached`])
/// invalidated on every re-registration, so repeated renders stay cheap
/// while a save/remove is still reflected immediately.
#[must_use]
pub fn custom_agent_installed(agent: &str) -> bool {
    custom_installed_cached().contains(&agent)
}

/// Every registered agent name, built-ins first, then user registrations in
/// registration order. The single source of the `subagent_call` tool's
/// agent enum and the pickers' listings, so a setup-registered harness is
/// offered exactly where a built-in one is.
#[must_use]
pub fn all_agent_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ACP_AGENTS.iter().map(|a| a.name).collect();
    names.extend(
        CUSTOM_AGENTS
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|a| a.name),
    );
    names
}

/// Names of every registered agent whose required binaries are all in PATH,
/// built-ins answered from the cached [`detect_installed`] snapshot (one
/// PATH scan per process) and user registrations probed on demand.
#[must_use]
pub fn installed_agents_snapshot() -> Vec<&'static str> {
    let cached = detect_installed();
    all_agent_names()
        .into_iter()
        .filter(|name| cached.contains(name) || custom_agent_installed(name))
        .collect()
}

/// How long a PATH probe for user-registered harnesses stays valid. Short
/// on purpose: the Settings screen re-renders on every keystroke and each
/// row shows an installed/missing glyph, so without a TTL every redraw
/// would stat every registered binary.
const CUSTOM_PROBE_TTL: Duration = Duration::from_secs(2);

/// Cached PATH-probe answer for user-registered agents: the probe time and
/// the names whose binaries were all found. Invalidated on every
/// re-registration so a save/remove is not shadowed by a stale glyph.
static CUSTOM_PROBE_CACHE: Mutex<Option<(Instant, Vec<&'static str>)>> = Mutex::new(None);

/// Cached PATH probe results for user-registered agents, refreshed at most
/// once per [`CUSTOM_PROBE_TTL`].
fn custom_installed_cached() -> Vec<&'static str> {
    {
        let cache = CUSTOM_PROBE_CACHE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((probed_at, names)) = cache.as_ref()
            && probed_at.elapsed() < CUSTOM_PROBE_TTL
        {
            return names.clone();
        }
    }
    let names: Vec<&'static str> = CUSTOM_AGENTS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|agent| agent.requires.iter().all(|bin| binary_in_path(bin)))
        .map(|agent| agent.name)
        .collect();
    *CUSTOM_PROBE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), names.clone()));
    names
}

/// Drop the user-registration PATH-probe cache. Called after every
/// successful re-registration so a save/remove is reflected on the next
/// render instead of serving the stale [`CUSTOM_PROBE_TTL`]-old answer.
fn invalidate_custom_probe_cache() {
    *CUSTOM_PROBE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
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

/// Validate that `agent` is registered (built-in table or user registry).
///
/// # Errors
///
/// Returns an error if the agent name is not found.
pub fn validate_agent(agent: &str) -> Result<(), String> {
    if find_agent(agent).is_some() {
        Ok(())
    } else {
        Err(format!(
            "Unsupported agent '{agent}'. Supported agents (ACP): {}. Use bash_run for shell commands.",
            all_agent_names().join(", "),
        ))
    }
}

/// Call a sub-agent harness over ACP and return its final output.
///
/// 1. Looks up `agent` (built-in table or user registry) via [`find_agent`]
///    to get the ACP launch command.
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
/// 4. Permission handling is MODE-AWARE: `permission_rx: Some(rx)` makes
///    every `session/request_permission` surface as a
///    [`SubagentEvent::PermissionRequest`] and AWAIT the user's option pick
///    on that channel (the TUI's permission box). `None` keeps the
///    headless auto-approve (the Yolo contract — the split ran every pane
///    YOLO by design long before modes mapped).
/// 5. Returns `(final_report, stop_reason, Option<session_id>)` when
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
#[allow(clippy::too_many_arguments)]
pub async fn call(
    agent: &str,
    input: &str,
    resume: Option<String>,
    picks: &[(String, String)],
    mode: Option<&str>,
    permission_rx: Option<tokio::sync::mpsc::UnboundedReceiver<
        super::events::PanePermissionAnswer,
    >>,
    cwd: PathBuf,
    stop_signal: Arc<AtomicBool>,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(String, String, Option<String>), String> {
    validate_agent(agent)?;
    let entry = find_agent(agent).ok_or_else(|| format!("unknown agent '{agent}'"))?;
    // The caller's picks win — the /agents config box writes the user's
    // choices into the pane's config cache and every turn re-applies them
    // (`apply_session_picks`), so the choices survive. A model-less pick
    // set falls back to the registry default (kilo's ACP default is a
    // paid-tier model that fails the prompt). OWNED: the picks move into
    // the blocking turn task, outliving this call's borrows.
    let mut owned: Vec<(String, String)> = picks.to_vec();
    if !entry.model.is_empty() && !owned.iter().any(|(id, _)| id == "model") {
        owned.push(("model".to_string(), entry.model.to_string()));
    }
    let picks = owned;
    let launcher = agent_launcher(&entry)?
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
    let mode = mode.map(str::to_string);

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
                    &picks,
                    mode,
                    permission_rx,
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
    let entry = find_agent(agent).ok_or_else(|| format!("unknown agent '{agent}'"))?;
    let launcher = agent_launcher(&entry)?
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

/// Shared collector for the on-demand snapshot probes: one probe stores the
/// commands and config options the harness publishes during the handshake /
/// resume window, so the awaiting side can poll both slots.
#[derive(Default)]
struct SnapshotCollector {
    commands: Option<Vec<super::events::AgentCommand>>,
    config_options: Option<Vec<super::events::AgentConfigOption>>,
    /// `(current mode id, the full mode list)`. Does NOT gate the probe's
    /// completion — modes are optional protocol state (gemini advertises
    /// none), so an absent slot must not hold the probe open.
    modes: Option<(String, Vec<super::events::AgentSessionMode>)>,
}

/// Whether the collector already holds BOTH probe results.
fn snapshot_complete(collector: &SnapshotCollector) -> bool {
    collector.commands.is_some() && collector.config_options.is_some()
}

/// Poll the collector every 50 ms until BOTH probe slots are filled or the
/// deadline passes. Harnesses publish `available_commands_update` at
/// different moments (right after the handshake, during `session/load`
/// replay, or lazily); the deadline bounds the wait without starving the
/// ones that need the full window.
async fn wait_for_snapshot(
    collector: &Mutex<SnapshotCollector>,
    timeout: Duration,
) -> (
    Vec<super::events::AgentCommand>,
    Vec<super::events::AgentConfigOption>,
    Vec<super::events::AgentSessionMode>,
    String,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        {
            let collected = collector.lock().unwrap_or_else(|e| e.into_inner());
            if snapshot_complete(&collected) {
                return snapshot_parts(&collected);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let collected = collector.lock().unwrap_or_else(|e| e.into_inner());
    snapshot_parts(&collected)
}

/// The collector's snapshot as plain parts (modes default empty).
fn snapshot_parts(
    collected: &SnapshotCollector,
) -> (
    Vec<super::events::AgentCommand>,
    Vec<super::events::AgentConfigOption>,
    Vec<super::events::AgentSessionMode>,
    String,
) {
    let (current, modes) = collected
        .modes
        .clone()
        .unwrap_or_else(|| (String::new(), Vec::new()));
    (
        collected.commands.clone().unwrap_or_default(),
        collected.config_options.clone().unwrap_or_default(),
        modes,
        current,
    )
}

/// The result of an on-demand ACP probe: what the harness advertises for
/// its session without any prompt being sent.
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    /// The harness's slash commands (its OWN spellings).
    pub commands: Vec<super::events::AgentCommand>,
    /// The session's config options (model, provider, thought level, …).
    pub config_options: Vec<super::events::AgentConfigOption>,
    /// Every mode the agent can operate in (empty when it advertises none).
    pub modes: Vec<super::events::AgentSessionMode>,
    /// The id of the mode the session starts in (empty without modes).
    pub current_mode: String,
}

/// Probe an agent's session ON DEMAND: connect, `session/load`/`resume`
/// (falling back to `session/new` like every resume path), and collect the
/// `available_commands_update` / `config_option_update` notifications the
/// harness publishes around the handshake. NO prompt is ever sent — the
/// probe only reads what the harness advertises. Returns whatever arrived
/// within the window (either slot may stay empty when the harness publishes
/// lazily); the caller decides how to degrade.
///
/// Used by the `/agents` slash-menu plumbing when a pane is focused with no
/// cached command list (see the app's snapshot dispatch).
///
/// # Errors
///
/// Returns an error when the agent is unsupported or the ACP handshake
/// fails outright (the process cannot even be launched). A resume failure
/// is NOT an error — the fallback session/new still probes.
#[allow(clippy::unwrap_used)]
pub async fn session_snapshot(
    agent: &str,
    session_id: Option<String>,
    cwd: PathBuf,
) -> Result<SessionSnapshot, String> {
    validate_agent(agent)?;
    let entry = find_agent(agent).ok_or_else(|| format!("unknown agent '{agent}'"))?;
    // The harnesses validate `cwd` as an ABSOLUTE path (claude-code ACP
    // rejects `session/new` with a relative one — "must be an absolute
    // path"). The TUI stores the working directory as the user typed it,
    // which may be relative (`.` in a `cosh` session started in-place), so
    // canonicalize here; an unresolvable path keeps the original and fails
    // at the harness with the same clarity as before.
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let launcher = agent_launcher(&entry)?
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
            .block_on(run_snapshot_session(
                launcher, session_id, cwd,
            ))
    })
    .await
    .map_err(|e| format!("sub-agent task failed: {e}"))?
}

/// The snapshot connection itself: initialize → authenticate (when
/// advertised) → open-or-resume → collect the probe notifications for a
/// short window, then disconnect. The notification handler fills the shared
/// collector; the response payloads (`session/new`, `session/resume`)
/// fill the config-option slot synchronously.
async fn run_snapshot_session<T>(
    transport: T,
    session_id: Option<String>,
    cwd: PathBuf,
) -> Result<SessionSnapshot, String>
where
    T: agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
{
    let collector = Arc::new(Mutex::new(SnapshotCollector::default()));
    let notify_collector = collector.clone();
    Client
        .builder()
        .name("cosh")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                // Both probe-relevant updates land in the collector; nothing
                // here touches a timeline (no prompt was ever sent).
                match notification.update {
                    schema::SessionUpdate::AvailableCommandsUpdate(update) => {
                        let commands = update
                            .available_commands
                            .into_iter()
                            .map(|cmd| super::events::AgentCommand {
                                name: cmd.name,
                                description: cmd.description,
                                input_hint: match cmd.input {
                                    Some(schema::AvailableCommandInput::Unstructured(input)) => {
                                        Some(input.hint)
                                    }
                                    _ => None,
                                },
                            })
                            .collect();
                        notify_collector
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .commands = Some(commands);
                    }
                    schema::SessionUpdate::ConfigOptionUpdate(update) => {
                        let options = update
                            .config_options
                            .iter()
                            .map(super::events::mirror_config_option)
                            .collect();
                        notify_collector
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .config_options = Some(options);
                    }
                    _ => {}
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<AcpRole>| {
            // No `fs` capabilities: the probe serves nothing.
            let capabilities = ClientCapabilities::new();
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(capabilities),
                )
                .block_task()
                .await
                .map_err(|e| acp_error(format!("ACP initialize failed: {e}")))?;

            // Same auth contract as a turn: reuse the user's existing login.
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
                        auth_error = Some(format!("ACP authenticate failed: {e}"));
                    }
                }
            }
            if let Some(e) = auth_error {
                return Err(acp_error(e));
            }

            // Resume-or-new (the shared fallback ladder). The returned
            // config options (when the response carries them) fill the
            // config slot without waiting for a notification.
            let support = session_resume_support(&init.agent_capabilities);
            let open = open_or_resume_session(&connection, session_id, &cwd, support).await?;
            if let Some(options) = open.config_options {
                collector
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .config_options = Some(
                    options.iter().map(super::events::mirror_config_option).collect(),
                );
            }
            if let Some((current, modes)) = &open.modes {
                collector.lock().unwrap_or_else(|e| e.into_inner()).modes =
                    Some((current.clone(), modes.clone()));
            }

            // Collect window: give the harness time to publish its command
            // list around the handshake/resume.
            let (commands, config, modes, current_mode) =
                wait_for_snapshot(&collector, Duration::from_secs(3)).await;
            Ok(SessionSnapshot {
                commands,
                config_options: config,
                modes,
                current_mode,
            })
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
/// session came back with (consumed by [`apply_session_picks`]; `None` for a
/// plain `session/new` response is carried through as-is).
///
/// # Errors
///
/// Only a failed `session/new` is fatal — the turn cannot proceed without
/// a session.
/// What the session came back with: the id, its config options and its
/// MODE state. `modes` mirrors the ACP `SessionModeState` the harness
/// carries in `session/new` / `session/resume` / `session/load` — the
/// current mode id plus every mode the agent can operate in. `None` when
/// the harness advertises no modes at all (gemini), so the client knows
/// mode binding is unavailable rather than merely empty.
pub(crate) struct SessionOpen {
    pub session_id: SessionId,
    pub config_options: Option<Vec<SessionConfigOption>>,
    /// `(current mode id, the full mode list)`; `None` when the harness
    /// advertises no modes at all.
    pub modes: Option<(String, Vec<super::events::AgentSessionMode>)>,
}

/// Mirror one ACP [`SessionModeState`] into the protocol-free tuple:
/// `(current mode id, the full mode list)`.
pub(crate) fn mirror_modes(
    state: &schema::SessionModeState,
) -> (String, Vec<super::events::AgentSessionMode>) {
    (
        state.current_mode_id.0.to_string(),
        state
            .available_modes
            .iter()
            .map(|m| super::events::AgentSessionMode {
                id: m.id.0.to_string(),
                name: m.name.clone(),
                description: m.description.clone(),
            })
            .collect(),
    )
}

async fn open_or_resume_session(
    connection: &ConnectionTo<AcpRole>,
    resume: Option<String>,
    cwd: &Path,
    support: SessionResumeSupport,
) -> Result<SessionOpen, AcpError> {
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
            match response.map(|r: ResumeSessionResponse| (r.config_options, r.modes)) {
                Ok((config_options, modes)) => {
                    log::debug!("sub-agent ACP session resumed: {id}");
                    return Ok(SessionOpen {
                        session_id: SessionId::new(id),
                        config_options,
                        modes: modes.as_ref().map(mirror_modes),
                    });
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
            match response.map(|r: LoadSessionResponse| (r.config_options, r.modes)) {
                Ok((config_options, modes)) => {
                    log::debug!("sub-agent ACP session loaded: {id}");
                    return Ok(SessionOpen {
                        session_id: SessionId::new(id),
                        config_options,
                        modes: modes.as_ref().map(mirror_modes),
                    });
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
    Ok(SessionOpen {
        session_id: session.session_id,
        config_options: session.config_options,
        modes: session.modes.as_ref().map(mirror_modes),
    })
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

///
/// Apply the pane's config picks inside the TURN's own connection. ACP
/// config options are per-connection state (a set on a disposable
/// connection dies with it — verified against opencode and kilo), so this
/// — re-run before every prompt — is the only path a pick survives.
///
/// Each pick is `(config id, value id)`. A pick whose option the harness
/// no longer advertises, or whose value is no longer offered (a stale
/// pane cache), is SKIPPED with a log — the turn proceeds on the
/// harness's current value instead of failing every prompt. A set
/// request that the harness or transport rejects is fatal: something is
/// genuinely wrong with the connection.
///
/// # Errors
///
/// Returns an error when the config id is unknown to the harness's own
/// pick list, or when the `session/set_config_option` request fails. The
/// wire error's protocol code is preserved so a rejection can be
/// recognized as `auth_required` by the lazy-auth retry.
async fn apply_session_picks(
    connection: &ConnectionTo<AcpRole>,
    session_id: &SessionId,
    config_options: Option<&[SessionConfigOption]>,
    picks: &[(String, String)],
) -> Result<(), AcpError> {
    let options = config_options.unwrap_or_default();
    for (config_id, value) in picks {
        let Some(option) = options.iter().find(|option| option.id.0.as_ref() == config_id)
        else {
            log::debug!("sub-agent ACP pick '{config_id}' skipped: option not advertised");
            continue;
        };
        let SessionConfigKind::Select(select) = &option.kind else {
            continue;
        };
        if select.current_value.0.as_ref() == value {
            continue;
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
        if !offered.contains(&value.as_str()) {
            log::debug!(
                "sub-agent ACP pick '{config_id}={value}' skipped: not offered by this harness"
            );
            continue;
        }
        connection
            .send_request(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SessionConfigId::new(config_id.as_str()),
                SessionConfigValueId::new(value.as_str()),
            ))
            .block_task()
            .await
            .map_err(|e| wrap_acp_failure("ACP session/set_config_option failed", e))?;
    }
    Ok(())
}

/// Bind the session to ONE of the agent's advertised modes — the client's
/// harness mode mapped onto a `SessionModeId` (the TUI does the mapping; see
/// the app's mode-map). `Some(mode_id)` only when the harness actually
/// advertises that id in its `SessionModeState`; `None` skips the request
/// entirely (an agent without modes — gemini — binds nothing, like every
/// unmapped mode).
///
/// Runs BEFORE the prompt, right after `apply_session_picks` (ACP session
/// state is per-connection: a set on a disposable connection dies with it,
/// so this re-runs before every prompt exactly like the config picks). An
/// `auth_required` rejection triggers one lazy-authentication attempt and a
/// retry (the same ladder `apply_session_picks` climbs).
///
/// # Errors
///
/// Returns the `session/set_mode` failure (after the auth ladder) — the
/// caller decides whether that aborts the turn.
async fn apply_session_mode(
    connection: &ConnectionTo<AcpRole>,
    session_id: &SessionId,
    mode_id: Option<&str>,
) -> Result<(), AcpError> {
    let Some(mode_id) = mode_id else {
        return Ok(());
    };
    let request = connection
        .send_request(SetSessionModeRequest::new(
            session_id.clone(),
            agent_client_protocol::schema::v1::SessionModeId::new(mode_id),
        ))
        .block_task()
        .await;
    match request {
        Ok(_) => Ok(()),
        Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
            // NOTE: the auth methods live at the call site (they belong to
            // the `initialize` response, not this function's inputs); the
            // ladder therefore lives in the caller — see `run_session`.
            Err(auth_error)
        }
        Err(e) => Err(wrap_acp_failure("ACP session/set_mode failed", e)),
    }
}

/// Read the session's badge triple from its ACP config options: the
/// effective model id (+ human label, when the harness names the value),
/// the serving provider (+ label, when a `provider` option is advertised
/// at all — cline advertises one, kilo/opencode do not), and the
/// reasoning/thought level display.
///
/// The model lookup prefers the spec's `Model` category and falls back to
/// the `model` option id (the same id [`apply_session_picks`] matches —
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
/// the prompt's stop reason on success. `apply_session_picks` re-applies
/// the caller's config picks inside the turn's own connection before the
/// prompt (ACP config options are per-connection state), and the shared
/// stop flag is raced against the prompt await
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
    picks: &[(String, String)],
    mode: Option<String>,
    permission_rx: Option<tokio::sync::mpsc::UnboundedReceiver<
        super::events::PanePermissionAnswer,
    >>,
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

    // Permission handling is MODE-AWARE (the mode mapping made the split's
    // modes real): with a permission channel the `session/request_permission`
    // request surfaces to the TUI (its box) and AWAITS the user's pick;
    // without one it stays the headless auto-approve (the Yolo contract —
    // the split ran every pane YOLO by design long before modes mapped).
    //
    // The receiver rides an `Arc<Mutex<Option<_>>>` because the SDK invokes
    // the permission handler per request over the same captured state: each
    // request takes the channel out, awaits one answer, and puts it back.
    let permission_rx = std::sync::Arc::new(std::sync::Mutex::new(permission_rx));
    // The permission handler's own captures (each closure below moves its
    // clones — the originals stay for the prompt race further down).
    let permission_tx = chunk_tx.clone();
    let permission_stop = stop_signal.clone();

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
        // Permission handling is MODE-AWARE (the mode mapping made the
        // split's modes real): with a permission channel the request
        // surfaces to the TUI (its box) and AWAITS the user's pick; without
        // one it stays the headless auto-approve (the Yolo contract — the
        // split ran every pane YOLO by design long before modes mapped).
        //
        // Auto-approve preference order: AllowAlways first, then AllowOnce —
        // both approve, but Always avoids the same prompt coming back for
        // every later call. Without any allow-kind option, the first listed
        // option is picked as a last resort; its kind is logged so a
        // reject-like auto-approval is visible in debug output. With no
        // options at all, cancel per the spec.
        //
        // The receiver rides a Mutex<Option<_>> because the SDK invokes this
        // handler per request over the same captured state: each request
        // takes the channel out, awaits one answer, and puts it back.
            .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let interactive = permission_rx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some();
                if !interactive {
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
                    return match selected {
                        Some(option) => {
                            log::debug!(
                                "sub-agent permission auto-approved: option '{}' (kind {:?}) for tool call {}",
                                option.option_id.0,
                                option.kind,
                                request.tool_call.tool_call_id.0
                            );
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(option.option_id.clone()),
                                ),
                            ))
                        }
                        None => responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Cancelled,
                        )),
                    };
                }
                // INTERACTIVE: mirror the request into a typed event (the TUI
                // opens its permission box for this pane) and await the
                // user's option pick. The await is raced against the shared
                // stop signal — a triggered stop must respond `Cancelled`
                // per the spec (the pending permission dies with the turn).
                let fields = &request.tool_call.fields;
                let permission_event = SubagentEvent::PermissionRequest {
                    call_id: request.tool_call.tool_call_id.0.to_string(),
                    title: fields.title.clone().unwrap_or_else(|| {
                        "tool call awaiting permission".to_string()
                    }),
                    kind: fields
                        .kind
                        .map(super::events::ToolKind::from_acp)
                        .unwrap_or(super::events::ToolKind::Unknown),
                    raw_input: fields.raw_input.clone(),
                    options: request
                        .options
                        .iter()
                        .map(|option| super::events::AgentPermissionOption {
                            id: option.option_id.0.to_string(),
                            name: option.name.clone(),
                            kind: match option.kind {
                                PermissionOptionKind::AllowOnce => "allow_once".to_string(),
                                PermissionOptionKind::AllowAlways => "allow_always".to_string(),
                                PermissionOptionKind::RejectOnce => "reject_once".to_string(),
                                PermissionOptionKind::RejectAlways => "reject_always".to_string(),
                                // `#[non_exhaustive]`: a future kind carries
                                // no name of its own here — the option still
                                // shows, just unclassified.
                                _ => "other".to_string(),
                            },
                        })
                        .collect(),
                };
                let _ = permission_tx.send(permission_event);

                // Take the channel out for THIS request (one answer), put it
                // back afterwards so later requests of the same turn reuse it.
                let mut rx = permission_rx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                let answer = async {
                    match rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => None,
                    }
                };
                let stop_wait = async {
                    while !permission_stop.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                };
                let picked = tokio::select! {
                    answer = answer => answer,
                    () = stop_wait => None,
                };
                // The channel returns to the handler state regardless of the
                // answer's shape: a later request of the same turn needs it.
                *permission_rx.lock().unwrap_or_else(|e| e.into_inner()) = rx;
                match picked {
                    // The user picked one of the agent's own options; the id
                    // goes back verbatim.
                    Some(super::events::PanePermissionAnswer::Selected(option_id)) => {
                        log::debug!(
                            "sub-agent permission granted by user: option '{option_id}' for tool call {}",
                            request.tool_call.tool_call_id.0
                        );
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                agent_client_protocol::schema::v1::PermissionOptionId::new(
                                    option_id,
                                ),
                            )),
                        ))
                    }
                    // The user dismissed the box, the channel closed (the
                    // pane's turn was force-closed), or the stop signal fired:
                    // the spec REQUIRES `Cancelled` for a pending permission on
                    // a cancelled turn.
                    answer => {
                        log::debug!(
                            "sub-agent permission cancelled ({answer:?}): tool call {}",
                            request.tool_call.tool_call_id.0
                        );
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Cancelled,
                        ))
                    }
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
            let opened = match handshake_step(
                "session open",
                open_or_resume_session(
                    &connection,
                    resume.clone(),
                    &cwd,
                    session_resume_support(&init.agent_capabilities),
                ),
            )
            .await
            {
                Ok(open) => open,
                Err(auth_error)
                    if auth_error.code == ErrorCode::AuthRequired =>
                {
                    authenticate_on_auth_required(
                        &connection,
                        &init.auth_methods,
                        &auth_error,
                    )
                    .await?;
                    handshake_step(
                        "session open",
                        open_or_resume_session(
                            &connection,
                            resume,
                            &cwd,
                            session_resume_support(&init.agent_capabilities),
                        ),
                    )
                    .await?
                }
                Err(auth_error) => return Err(auth_error),
            };
            let session_id = opened.session_id;

// Re-apply the pane's config picks inside THIS turn's own
            // connection — per-connection state dies with the connection
            // (verified against opencode and kilo), so this re-run before
            // every prompt is the only path a pick survives. Stale picks
            // (option/value no longer offered) are skipped inside; a
            // transport/harness rejection aborts the turn. Resumed
            // sessions carry their config options in the resume response,
            // so the picks apply to them too. An `auth_required`
            // rejection triggers one lazy-authentication attempt (see
            // [`authenticate_on_auth_required`]) before the retry.
            let picks_applied = handshake_step(
                "config picks",
                apply_session_picks(
                    &connection,
                    &session_id,
                    opened.config_options.as_deref(),
                    picks,
                ),
            )
            .await;
            match picks_applied {
                Ok(()) => {}
                Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
                    authenticate_on_auth_required(
                        &connection,
                        &init.auth_methods,
                        &auth_error,
                    )
                    .await?;
                    handshake_step(
                        "config picks",
                        apply_session_picks(
                            &connection,
                            &session_id,
                            opened.config_options.as_deref(),
                            picks,
                        ),
                    )
                    .await?;
                }
                Err(auth_error) => return Err(auth_error),
            }

            // The prompt badge's data: the session's serving triple, read
            // from the same config options the picks above ran against.
            // Emitted BEFORE the prompt so the pane shows what serves the
            // turn from its first chunk; anything the harness does not
            // advertise stays empty.
            let preferred_model = picks
                .iter()
                .find(|(id, _)| id == "model")
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            let _ =
                model_tx.send(session_model_info(opened.config_options.as_deref(), preferred_model));

            // Bind the client's harness mode onto one of the agent's
            // advertised modes BEFORE the prompt (per-connection session
            // state, exactly like the picks). The caller resolved its own
            // mode id against the modes a PREVIOUS turn's `Modes` event
            // advertised; this side only verifies the agent still offers it
            // — a stale mapping (the agent dropped the mode) is skipped, a
            // transport rejection aborts the turn. `None` (no mapped mode)
            // binds nothing: gemini advertises no modes, and an unmapped
            // cosh mode keeps the agent's own current.
            if let Some(mode_id) = mode.as_deref()
                && opened
                    .modes
                    .as_ref()
                    .is_some_and(|(_, modes)| modes.iter().any(|m| m.id == mode_id))
            {
                let mode_applied = handshake_step(
                    "mode bind",
                    apply_session_mode(&connection, &session_id, Some(mode_id)),
                )
                .await;
                match mode_applied {
                    Ok(()) => {}
                    Err(auth_error) if auth_error.code == ErrorCode::AuthRequired => {
                        authenticate_on_auth_required(
                            &connection,
                            &init.auth_methods,
                            &auth_error,
                        )
                        .await?;
                        handshake_step(
                            "mode bind",
                            apply_session_mode(&connection, &session_id, Some(mode_id)),
                        )
                        .await?;
                    }
                    Err(auth_error) => return Err(auth_error),
                }
            }

            // The pane's MODE state: what the harness advertises (and starts
            // in) rides the turn's event stream as pane chrome, so the TUI
            // can map its own mode onto one of these ids and bind it through
            // `session/set_mode` on a later turn. Emitted even when the
            // harness advertises none (an empty event) so a pane that once
            // held modes drops them when the agent stops offering them.
            let _ = model_tx.send(SubagentEvent::Modes {
                modes: opened
                    .modes
                    .as_ref()
                    .map(|(_, modes)| modes.clone())
                    .unwrap_or_default(),
                current: opened
                    .modes
                    .as_ref()
                    .map(|(current, _)| current.clone())
                    .unwrap_or_default(),
            });

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
