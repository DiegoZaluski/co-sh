//! Structured user preferences stored in `~/.config/cosh/setup.json`.
//!
//! Each category groups related settings so new fields have a natural home
//! and the on-disk format stays readable. The [`Setup`] struct is the single
//! owner of the file: callers read fields through accessor methods and write
//! changes through [`Setup::save`]. A missing or corrupt file is treated as
//! defaults.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::mcp::McpConfig;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

// Top-level struct

/// Root object persisted as `~/.config/cosh/setup.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    pub appearance: Appearance,
    pub tools: Tools,
    pub routing: Routing,
    pub hooks: Hooks,
    pub providers: Providers,
    pub model: Model,
    /// The last agent mode the user selected, restored for NEW sessions
    /// (global mode persistence).
    pub mode: Mode,
    pub cache: Cache,
    /// Skill-discovery configuration (Settings screen).
    pub skills: SkillsConfig,
    /// Decision-model configuration (Settings screen; `setup.json` →
    /// `model_decision`). The section names what it configures — the
    /// resident decision model and its audited decisions — explicitly, for
    /// the user reading the file; the ONNX runtime is gated by the build
    /// feature (`onnx`), while this schema stays backend-agnostic and grows
    /// with new audited decisions as siblings of `termination`. Legacy
    /// aliases still deserialize: "checkup" (the pre-daemon section name).
    #[serde(rename = "model_decision", alias = "checkup")]
    pub decision: DecisionConfig,
    /// Master switch for the LSP engine (kept flat: it has no sub-options).
    pub lsp: bool,
    /// Registered MCP servers (empty when the user never added one, so
    /// legacy files without the section keep loading unchanged).
    pub mcp: McpConfig,
    /// Terminal editor used by the file explorer (Ctrl+F). Empty: fall back
    /// to the first available of `nvim`, `vim`, `nano`.
    pub editor: String,
    /// Telemetry consent (Settings screen toggle). DEFAULT ON (opt-out,
    /// like the industry standard): the user can disable it in Settings or
    /// via `COSH_TELEMETRY=off`; CI environments are hard-off regardless.
    /// This flag is only the USER's choice — the effective consent still
    /// goes through `Telemetry::resolve` (env override + CI hard-off).
    pub telemetry: bool,
    /// Whether discovered model windows are resized to the reduced
    /// sweet-spot budget (`effective_context_window`). DEFAULT OFF: the
    /// full discovered window is used as-is.
    #[serde(default)]
    pub effective_context: bool,
    /// Raw bytes of the file this struct was last loaded from (or last
    /// wrote), kept as JSON text. NOT serialized — this is the merge
    /// baseline for [`Setup::save`]: with several cosh processes sharing one
    /// config file, a save must re-read the disk, carry forward sections
    /// this process never touched, and only overwrite what actually
    /// changed here. Without it the second process's save clobbers the
    /// first's unrelated changes (the "settings keep vanishing" bug).
    #[serde(skip)]
    disk_snapshot: Option<String>,
    /// How long a save waits for a foreign process's config lock before
    /// skipping the write (the change stays in memory for the next save).
    /// Not persisted; only tests shrink it (a real holder is waited out).
    #[serde(skip)]
    pub(crate) lock_wait: Duration,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            tools: Tools::default(),
            routing: Routing::default(),
            hooks: Hooks::default(),
            providers: Providers::default(),
            model: Model::default(),
            mode: Mode::default(),
            cache: Cache::default(),
            skills: SkillsConfig::default(),
            decision: DecisionConfig::default(),
            lsp: true,
            mcp: McpConfig::default(),
            editor: String::new(),
            telemetry: true,
            effective_context: false,
            disk_snapshot: None,
            lock_wait: Duration::from_millis(250),
        }
    }
}

// Categories

/// Theme, bell, and visual preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Selected theme name (empty → registry default).
    pub theme: String,
    /// Ring the terminal bell when the agent loop finishes.
    pub bell_enabled: bool,
    /// `/background` toggle: paint the TUI's base background with the
    /// terminal's own default color (`Color::Reset`) instead of the theme
    /// color. Panels/elements keep their theme colors.
    pub transparent_background: bool,
    /// `/anim` toggle: show the animated chat-logo on the empty-session
    /// landing screen. When off, the static `LOGO_CHAT` is drawn instead.
    pub anim_enabled: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: String::new(),
            bell_enabled: true,
            transparent_background: false,
            anim_enabled: false,
        }
    }
}

/// Tool-call mode and disabled tool set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    /// `"native"` or `"inline"`.
    pub tool_call_mode: String,
    /// Tool names the user has disabled (hidden from the model).
    pub disabled: Vec<String>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool_call_mode: "native".to_string(),
            disabled: Vec::new(),
        }
    }
}

/// The application's default provider→model fallback chain, applied when
/// the user has not customized `setup.json` → `routing.fallbacks`. Lives
/// here, next to the [`Routing`] section it defaults, because the chain is
/// persisted app configuration — the TUI's Router Settings screen edits it,
/// and the harness connectors consume it.
pub const DEFAULT_FALLBACKS: &[(&str, &str)] = &[
    ("nvidia", "deepseek-ai/deepseek-v4-pro"),
    ("openrouter", "deepseek/deepseek-v4-pro"),
    ("groq", "openai/gpt-oss-120b"),
    ("charm", "deepseek-ai/deepseek-v4-pro"),
];

/// Model routing / fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    /// Ordered fallback entries (provider, model).
    pub fallbacks: Vec<FallbackEntry>,
    /// Ordered correction candidates. Entries may be a direct REST model or
    /// an external ACP agent; their stored order is the correction fallback
    /// order without any provider/agent priority.
    pub fallback_prompt_corrector: Vec<PromptCorrectorFallback>,
    /// Explicit summarization chain. Empty uses the active agent model only.
    pub summarization_models: Vec<FallbackEntry>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            summarization_models: Vec::new(),
            fallback_prompt_corrector: Vec::new(),
            fallbacks: DEFAULT_FALLBACKS
                .iter()
                .map(|&(p, m)| FallbackEntry {
                    provider: p.to_string(),
                    model: m.to_string(),
                })
                .collect(),
        }
    }
}

/// A single provider+model pair in the fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
}

/// A correction candidate in the exact order chosen in Router Settings.
///
/// The tagged representation keeps provider-backed requests and ACP CLI
/// turns unambiguous in `setup.json` while allowing both kinds to share one
/// ordered chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptCorrectorFallback {
    Model { provider: String, model: String },
    Acp { agent: String },
}

/// The last model the user selected, restored for NEW sessions
/// (global model persistence).
///
/// The whole struct represents a single selection slot: every time the user
/// picks a model, `model` is overwritten with the new choice, `provider` with
/// the provider that serves it, and `reasoning` with its reasoning effort.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Model {
    /// Provider name of the last selected model (empty when the selection is
    /// `auto` — the fallback chain routes there).
    pub provider: String,
    /// Model id of the last selected model. Empty → no global selection yet.
    pub model: String,
    /// Reasoning effort for the last selected model (`None` = model default).
    pub reasoning: Option<String>,
}

/// The last agent mode the user selected (Build/Ask/Yolo/Command),
/// restored for NEW sessions (global mode persistence) — the app starts in
/// the mode the user most likely wants to use again. Empty: fall back to
/// the `Build` default.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Mode {
    /// Lowercase name of the last selected agent mode ("build", "ask",
    /// "yolo", "command"). Empty → no global selection yet.
    pub mode: String,
}

// Cache

/// Prompt-cache preferences.
///
/// The duration is the user's WISH in minutes; the connector layer maps it
/// onto the closest value the target API actually supports (Anthropic only
/// accepts 5m/1h TTLs; OpenAI only the 24h retention), so an arbitrary
/// choice can never produce a rejected request. `0` = provider default.
/// These are per-profile trade-offs (a longer-lived cache costs more per
/// write and only pays off for certain usage patterns), so they are user
/// settings chosen in the Settings screen rather than fixed behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Cache {
    /// Anthropic prompt-cache TTL wish, in minutes (`0` = the 5-minute
    /// default; any value above 5 maps onto the 1h TTL).
    pub anthropic_ttl_min: u32,
    /// OpenAI prompt-cache retention wish, in minutes (`0` = the
    /// model-dependent default; any value above 0 maps onto the 24h
    /// retention).
    pub openai_retention_min: u32,
}

/// Parse a user-entered cache duration into minutes: `45`, `45m`, `1h`,
/// `1h30m`, `90 m` (case-insensitive; a bare number means minutes).
/// `""`, `"0"` and `"default"` mean the provider default (`None`).
pub fn parse_cache_duration(input: &str) -> Result<Option<u32>, String> {
    const INVALID: &str =
        "Invalid duration. Use minutes or hours, e.g. 30m, 1h, 1h30m — or \"default\".";
    let s = input.trim().to_lowercase();
    if s.is_empty() || s == "default" {
        return Ok(None);
    }
    let mut total: u32 = 0;
    let mut num = String::new();
    for ch in s.chars() {
        match ch {
            '0'..='9' => num.push(ch),
            'm' => {
                total = total.saturating_add(parse_duration_segment(&mut num, 1, INVALID)?);
            }
            'h' => {
                total = total.saturating_add(parse_duration_segment(&mut num, 60, INVALID)?);
            }
            ' ' => {}
            _ => return Err(INVALID.to_string()),
        }
    }
    // A trailing bare number means minutes ("45" == "45m").
    if !num.is_empty() {
        total = total.saturating_add(parse_duration_segment(&mut num, 1, INVALID)?);
    }
    if total > 24 * 60 {
        return Err("Maximum cache duration is 24h.".to_string());
    }
    Ok(Some(total))
}

/// Consume the accumulated digits as `value × multiplier` minutes.
fn parse_duration_segment(num: &mut String, multiplier: u32, invalid: &str) -> Result<u32, String> {
    let value: u32 = num.parse().map_err(|_| invalid.to_string())?;
    num.clear();
    Ok(value.saturating_mul(multiplier))
}

/// Human rendering of a duration in minutes: `0` → "default", whole hours →
/// "1h", sub-hour → "45m", mixed → "1h30m".
pub fn format_cache_duration(min: u32) -> String {
    if min == 0 {
        "default".to_string()
    } else if min.is_multiple_of(60) {
        format!("{}h", min / 60)
    } else if min < 60 {
        format!("{min}m")
    } else {
        format!("{}h{:02}m", min / 60, min % 60)
    }
}

// Decision

/// Decision-model configuration (`setup.json` → `model_decision`; the
/// legacy section name "checkup" still deserializes via the alias on
/// `Setup`).
///
/// The section names the DOMAIN, not the technology: the ONNX runtime is
/// gated by the build feature (`onnx`), while this schema stays
/// backend-agnostic. What belongs to the **resident model** (checkpoint
/// identity) lives at the section root; what belongs to an **audited
/// decision** (enable, confidence floor) lives in a sub-key named after the
/// decision domain — every future audited decision becomes a sibling of
/// `termination`, never a rename of existing config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecisionConfig {
    /// The resident decision model: one checkpoint identity shared by the
    /// whole app. Its `kind` strings are exactly the names
    /// `ModelKind::name()` publishes (`english` / `multilingual` /
    /// `typed-decisions`) plus `custom` — no translation map.
    pub model: DecisionModel,
    /// The agent-loop termination audit (the current audited decision).
    pub termination: TerminationDecision,
    /// The hub mirror the weights installer downloads from. THE single
    /// application-level default for the mirror repo id: the `cosh-onnx`
    /// crate carries no repo default — every install resolves this value
    /// (env override `COSH_ONNX_HUB_REPO` wins over the config).
    pub hub: DecisionHub,
}

/// The weights mirror configuration (`setup.json` → `model_decision.hub`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionHub {
    /// The mirror repo id (`Zaluski/laya-onnx` by default). The ONLY place
    /// in the app this value lives as a default — never in the library.
    pub repo: String,
    /// The repo id the recorded pin belongs to. A pin is meaningful ONLY
    /// against the repo it resolved from — a mirror-A SHA applied to a
    /// mirror-B request 404s and silently disables the audit. Empty means
    /// no pin (or a pre-repo-scoping pin, which [`Self::pin_for`] then
    /// ignores, forcing one fresh verified install under the current
    /// mirror).
    pub pinned_repo: String,
    /// The commit SHA the mirror resolved to on its first install. Empty
    /// (the default) resolves the mirror's current revision on the first
    /// install and pins THAT; a non-empty value forces the pin — but only
    /// when [`Self::pinned_repo`] matches the effective repo.
    pub pinned_revision: String,
}

impl Default for DecisionHub {
    fn default() -> Self {
        Self {
            // The application-level default mirror. Overridable per config
            // (an org mirror) and per environment (`COSH_ONNX_HUB_REPO`).
            repo: "Zaluski/laya-onnx".to_string(),
            pinned_repo: String::new(),
            pinned_revision: String::new(),
        }
    }
}

impl DecisionHub {
    /// The effective repo id: the env override wins over the config value,
    /// mirroring how the other env-backed knobs behave.
    pub fn effective_repo(&self) -> String {
        match std::env::var("COSH_ONNX_HUB_REPO") {
            Ok(repo) if !repo.trim().is_empty() => repo.trim().to_string(),
            _ => self.repo.clone(),
        }
    }

    /// The revision pin valid for `repo`: the recorded SHA only when it was
    /// resolved FROM that same repo. A pin from another mirror never
    /// applies — and a legacy config (pin recorded before
    /// `pinned_repo` existed) is treated as unpinned, so the next install
    /// re-resolves and re-records a properly scoped pin.
    pub fn pin_for(&self, repo: &str) -> Option<String> {
        let pinned = self.pinned_revision.trim();
        if pinned.is_empty() || self.pinned_repo != repo {
            return None;
        }
        Some(pinned.to_string())
    }
}

/// The resident model's checkpoint identity — a kind-discriminated schema
/// mapping 1:1 to `cosh_onnx::ModelKind` (named variants + `Custom`), so a
/// new model family becomes a new `kind` without a config migration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionModel {
    /// The English root checkpoint (`ModelKind::English`).
    #[default]
    English,
    /// The multilingual checkpoint (`ModelKind::Multilingual`).
    Multilingual,
    /// The typed-decisions checkpoint (`ModelKind::TypedDecisions`).
    TypedDecisions,
    /// A raw hub repo id, a bundled `(repo, subfolder)` checkpoint, or a
    /// local checkpoint directory (`ModelKind::Custom`).
    Custom {
        /// The repo id or local path.
        repo: String,
        /// One checkpoint out of a bundling repo.
        subfolder: Option<String>,
    },
}

impl DecisionModel {
    /// The kind name `ModelKind::name()` publishes for the named variants;
    /// a `Custom` checkpoint renders as its repo id. The Settings rows
    /// (toggle value and model picker) display exactly this.
    pub fn kind_name(&self) -> &str {
        match self {
            Self::English => "english",
            Self::Multilingual => "multilingual",
            Self::TypedDecisions => "typed-decisions",
            Self::Custom { repo, .. } => repo,
        }
    }

    /// The model's display name for user-facing copy: the upstream model
    /// family and checkpoint (`Laya`, `Laya-Multilingual`,
    /// `Laya-Typed-Decisions`), or the repo id for a custom checkpoint.
    /// The weights install copy addresses the model by THIS name —
    /// "decision model" says the role, not the model.
    pub fn display_name(&self) -> &str {
        match self {
            Self::English => "Laya",
            Self::Multilingual => "Laya-Multilingual",
            Self::TypedDecisions => "Laya-Typed-Decisions",
            Self::Custom { repo, .. } => repo,
        }
    }
}

/// The agent-loop termination audit config: whether the decision model
/// reviews ambiguous stops, and how confident a "not finished" verdict must
/// be before it vetoes the loop's end.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminationDecision {
    /// Default-ON (opt-out): the audit runs unless explicitly disabled —
    /// flipping `enabled` back to `false` (or deleting the section from
    /// setup.json) restores the unaudited loop.
    pub enabled: bool,
    /// Consumer floor for the calibrated `answer_confidence`: a
    /// `NotTerminated` verdict only continues the loop at or above it.
    /// The 0.75 default is the measured optimum edge of the
    /// confidence-floor sweep over the loop-termination corpus
    /// (`confidence_floor_sweep` in
    /// crates/cosh-onnx/tests/eval/loop_termination.rs): it keeps the
    /// sweep's best score (23/24 correct) on the zero-false-continue
    /// edge — no legitimate ending is vetoed, the single error is one
    /// missed stop. Lower floors trade that for false continues
    /// (0.70 → 1, 0.60 → 3); 0.80+ starts missing real stops in bulk
    /// (5 at 0.80, 7 at 0.85).
    pub min_confidence: f64,
}

impl Default for TerminationDecision {
    fn default() -> Self {
        Self {
            enabled: true,
            min_confidence: 0.75,
        }
    }
}

// Skills

/// Skill-discovery configuration (Settings screen → Skill directories).
///
/// `dirs` is the user-editable list of source directories; when it is empty
/// the harness falls back to the `~/.skills` default. Each listed directory
/// holds one subdirectory per skill (a skill is a directory containing a
/// `SKILL.md`). Missing directories are skipped silently, so a default path
/// that does not exist is never an error.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillsConfig {
    /// Source directories, in priority order (first source wins on name
    /// collision). Tilde (`~`) prefixes are expanded at resolve time
    /// (`resolved_dirs` / `to_skills`). Entries must not contain `:` —
    /// the Settings dialog joins/splits the list on it.
    pub dirs: Vec<String>,
    /// Scan each directory recursively (skills at any depth) instead of
    /// only the immediate subdirectories.
    pub recursive: bool,
    /// Glob-style name exclusions (e.g. `"deprecated-*"`). Empty: keep all.
    pub ignore: Vec<String>,
    /// Allowlist of exact skill names. Empty: everything passes.
    pub include: Vec<String>,
}

impl SkillsConfig {
    /// The default source directory (`~/.skills`).
    #[must_use]
    pub fn default_dir() -> String {
        "~/.skills".to_string()
    }

    /// The effective source directories: the user's list when non-empty,
    /// otherwise the `~/.skills` default. Tilde prefixes are expanded
    /// against `$HOME`; entries that still start with `~` (no resolvable
    /// home) are dropped.
    #[must_use]
    pub fn resolved_dirs(&self) -> Vec<String> {
        // cosh_tools::skills::home_dir normalizes MSYS-style HOME values
        // ("/c/Users/..." from Git Bash) that native fs calls cannot
        // resolve on Windows, falling back to USERPROFILE.
        self.resolved_dirs_with(cosh_tools::skills::home_dir().as_deref())
    }

    /// [`Self::resolved_dirs`] with the home directory injected (pure —
    /// the testable core, no environment access).
    #[must_use]
    pub fn resolved_dirs_with(&self, home: Option<&str>) -> Vec<String> {
        let dirs = if self.dirs.is_empty() {
            vec![Self::default_dir()]
        } else {
            self.dirs.clone()
        };
        dirs.into_iter()
            .filter_map(|d| expand_tilde_with(&d, home))
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect()
    }

    /// Build the skills wrapper for the harness: directory sources from
    /// [`Self::resolved_dirs`] (existing directories only — discovery is a
    /// hard error for unreadable sources, so a missing path is skipped),
    /// plus the recursion and name filters.
    #[must_use]
    pub fn to_skills(&self) -> cosh_tools::skills::Skills {
        use cosh_tools::skills::{SkillSource, normalize_shell_path};
        let sources: Vec<SkillSource> = self
            .resolved_dirs()
            .into_iter()
            // Normalize before the existence probe: an MSYS-spelled entry
            // ("/c/...") would fail `is_dir` natively on Windows and be
            // silently dropped.
            .map(|d| normalize_shell_path(&d))
            .filter(|native| std::path::Path::new(native).is_dir())
            .map(|native| SkillSource::Directory { path: native })
            .collect();
        cosh_tools::skills::Skills::new()
            .recursive(self.recursive)
            .ignore(self.ignore.clone())
            .include(self.include.clone())
            .sources(sources)
    }
}

/// Expand a leading `~` (alone or followed by `/`) to the given home
/// directory. Returns `None` when the path starts with `~` but `home` is
/// `None` — the caller drops such entries instead of passing a literal
/// `~/...` path to the filesystem.
fn expand_tilde_with(path: &str, home: Option<&str>) -> Option<String> {
    // An empty HOME is no home: expanding against it would produce
    // root-anchored garbage ("/x", "/.skills").
    let home = home.filter(|h| !h.is_empty());
    if path == "~" {
        return home.map(String::from);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|h| format!("{h}/{rest}"));
    }
    Some(path.to_string())
}

// Providers

/// Local model server endpoints configured through the ADD Provider screen.
/// Stored per provider name; used to override the registry's default
/// `localhost` port when building connectors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Providers {
    /// Map of local provider name → endpoint configuration.
    pub local: std::collections::HashMap<String, LocalEndpoint>,
}

/// A single local provider endpoint (`http://host:port`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEndpoint {
    pub base_url: String,
}

// Hooks

/// A single hook configuration entry (stored in setup.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Friendly display name (falls back to command when empty).
    #[serde(default)]
    pub name: String,
    /// Regex pattern tested against the tool name. Empty = match all.
    #[serde(default)]
    pub matcher: String,
    /// Shell command to execute.
    pub command: String,
    /// Timeout in seconds (default 30).
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Hook lifecycle configuration: one toggle + entry list per event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hooks {
    /// Switch for PreToolUse hooks (on unless the user turns it off).
    ///
    /// `alias = "enabled"` keeps pre-split setups loading: their single
    /// legacy master switch now drives PreToolUse only. Without the alias,
    /// the old `"enabled": <bool>` key would fall into the flattened events
    /// map and reject the whole document.
    #[serde(default = "default_true", alias = "enabled")]
    pub pre_tool_use_enabled: bool,
    /// Switch for PostToolUse hooks (on unless the user turns it off).
    #[serde(default = "default_true")]
    pub post_tool_use_enabled: bool,
    /// Hook configs keyed by event name (e.g. "PreToolUse").
    #[serde(flatten)]
    pub events: std::collections::HashMap<String, Vec<HookEntry>>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            pre_tool_use_enabled: true,
            post_tool_use_enabled: true,
            events: std::collections::HashMap::new(),
        }
    }
}

impl Hooks {
    /// Whether the given event's hooks are switched on. Unknown events
    /// default to on so new sections work without schema changes.
    pub fn is_event_enabled(&self, event: &str) -> bool {
        match event {
            "PreToolUse" => self.pre_tool_use_enabled,
            "PostToolUse" => self.post_tool_use_enabled,
            _ => true,
        }
    }
}

/// Serde default for boolean switches that start enabled.
fn default_true() -> bool {
    true
}

// Persistence

/// Resolve the config directory path (`~/.config/cosh/`).
///
/// `COSH_CONFIG_DIR` overrides the location when set — the test harness
/// uses this because `directories::ProjectDirs` resolves through the
/// platform's known-folders API (on Windows: `APPDATA`/`LOCALAPPDATA`),
/// which IGNORES the `$HOME` redirect that `isolate_home()` performs. A
/// plain `$HOME` override cannot isolate the config on Windows; an explicit
/// path override can.
fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("COSH_CONFIG_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let proj = ProjectDirs::from("", "", "cosh")
        .expect("could not determine project directories (is $HOME set?)");
    proj.config_dir().to_path_buf()
}

/// Resolve the data directory path (`~/.local/share/cosh/` on Unix).
///
/// `COSH_DATA_DIR` overrides the location when set — same rationale as
/// [`config_dir`]'s `COSH_CONFIG_DIR` override. Public: the TUI's session
/// store and usage cache resolve their storage roots through it.
pub fn data_dir_override() -> PathBuf {
    if let Ok(dir) = std::env::var("COSH_DATA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let proj = ProjectDirs::from("", "", "cosh")
        .expect("could not determine project directories (is $HOME set?)");
    proj.data_dir().to_path_buf()
}

/// Full path to `setup.json`.
fn setup_path() -> PathBuf {
    config_dir().join("setup.json")
}

/// Try to acquire the exclusive cross-process lock guarding `setup.json`.
///
/// Kernel-managed (`flock` via the standard library — the same mechanism the
/// session store uses): released on drop AND automatically when the holder
/// dies, so a crashed process can never wedge the config. The lock file is
/// a sibling (`setup.json.lock`) and is left on disk after release: removing
/// it would race with a foreign holder.
///
/// On contention the call polls until the wait window expires, then gives
/// up with `None` (the caller skips the write rather than blocking the UI).
fn acquire_config_lock(path: &Path, wait: Duration) -> Option<std::fs::File> {
    let lock_path = path.with_extension("json.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    const POLL: Duration = Duration::from_millis(25);
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Err(std::fs::TryLockError::WouldBlock) => return None,
            Err(std::fs::TryLockError::Error(_)) => return None,
        }
    }
}

/// True when two serde section values differ. Serialization-based because
/// the section structs are generated data carriers with no `PartialEq`;
/// an empty diff means "untouched" for the save-time merge.
fn section_differs<T: Serialize>(current: &T, baseline: &T) -> bool {
    let (Ok(a), Ok(b)) = (
        serde_json::to_value(current),
        serde_json::to_value(baseline),
    ) else {
        // Unserializable section (cannot happen for the schema types): treat
        // as changed so local state wins instead of being dropped.
        return true;
    };
    a != b
}

impl Setup {
    /// Load from disk. Returns defaults when the file is missing.
    ///
    /// Resilient to corruption: an unparsable file is NOT reset silently —
    /// it is quarantined to `setup.json.corrupt-<epoch-ms>` beside the
    /// original (nothing is ever lost, and the cause stays visible) and
    /// defaults are returned with a logged warning. The old behavior
    /// (`unwrap_or_default`) made a truncated JSON look like "the user
    /// never configured anything", hiding the real cause.
    ///
    /// The raw file text is also kept as this instance's merge baseline for
    /// [`Setup::save`]: with several cosh processes sharing one config
    /// file, a save must be able to tell what changed on disk since this
    /// process last read or wrote it.
    pub fn load() -> Self {
        let path = setup_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            // Missing file: defaults and no baseline — a later save
            // overwrites wholesale, which is correct (nothing to merge with).
            Err(_) => return Self::default(),
        };
        match serde_json::from_str::<Self>(&content) {
            Ok(mut setup) => {
                setup.disk_snapshot = Some(content);
                setup
            }
            Err(error) => {
                let epoch_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                // The pid suffix keeps two processes quarantining in the
                // same millisecond from clobbering each other's backup.
                let quarantine = path.with_extension(format!(
                    "json.corrupt-{epoch_ms}-{}",
                    std::process::id()
                ));
                match std::fs::rename(&path, &quarantine) {
                    Ok(()) => log::warn!(
                        "setup.json is corrupt ({error}); quarantined to {} and defaults apply",
                        quarantine.display()
                    ),
                    Err(rename_error) => log::warn!(
                        "setup.json is corrupt ({error}) and could not be quarantined \
                         ({rename_error}); defaults apply"
                    ),
                }
                Self::default()
            }
        }
    }

    /// Persist to disk, creating the config directory if needed.
    ///
    /// Cross-process safe (the old save was neither, which is how two cosh
    /// processes erased each other's settings):
    ///
    /// - the whole read-decide-write window holds an exclusive advisory
    ///   lock on `setup.json.lock` (same flock mechanism as the session
    ///   store), so two saves never interleave;
    /// - sections this process did NOT touch are carried over from the disk
    ///   content — a concurrent process's change survives this save instead
    ///   of being clobbered by a stale in-memory copy;
    /// - the write lands in a sibling temp file and is atomically renamed
    ///   over the target, so a reader (or a crash mid-write) never sees a
    ///   half-written JSON — the corruption that used to reset every
    ///   setting on the next launch.
    ///
    /// When the lock cannot be taken within the wait window the write is
    /// skipped (`false` return): the change stays in memory and the next
    /// save persists it. Skipping beats blocking the UI thread behind a
    /// stuck holder — the same trade-off the session store makes for its
    /// write jobs. The wait itself is kept short (250ms): saves are
    /// millisecond-scale, so any longer contention is a stuck holder, and
    /// this method runs on the UI thread.
    ///
    /// Returns `true` when the merged state was persisted; `false` when the
    /// write was skipped (lock unavailable) or the file could not be
    /// written — the change stays in memory for the next save either way,
    /// and the reason is always logged.
    pub fn save(&mut self) -> bool {
        let path = setup_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let Some(_lock) = acquire_config_lock(&path, self.lock_wait) else {
            log::warn!(
                "setup.json is locked by another process; the change stays in memory \
                 and will be saved on the next change"
            );
            return false;
        };
        // Baseline: the content this process last loaded or wrote. Sections
        // equal to it are "untouched by us" and follow the disk; sections
        // that differ carry this process's change.
        let baseline: Self = self
            .disk_snapshot
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        // Disk now: whatever other processes wrote since our baseline.
        let merged = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
        {
            Some(disk) => self.merged_over(&baseline, disk),
            // Absent or unparsable disk file: the in-memory state is the
            // only trustworthy source. (A corrupt file was already
            // quarantined at load; one corrupted out-of-band gets the same
            // treatment on the next load.)
            None => {
                log::warn!(
                    "setup.json on disk is missing or unreadable; rewriting it from this process's settings"
                );
                self.clone()
            }
        };
        let Ok(json) = serde_json::to_string_pretty(&merged) else {
            log::warn!("setup.json could not be serialized; the change stays in memory");
            return false;
        };
        // Atomic replace: write the sibling temp file fully (fsync'd), then
        // rename it over the target. `rename` replaces the destination
        // atomically on Unix AND Windows (MOVEFILE_REPLACE_EXISTING).
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).ok().and_then(|mut file| {
            file.write_all(json.as_bytes()).ok()?;
            file.sync_all().ok()?;
            Some(())
        });
        match written {
            Some(()) if std::fs::rename(&tmp, &path).is_ok() => {
                // The baseline moves to exactly what this write produced, so
                // the NEXT save compares against it — a value reverted back
                // to a pre-session state still registers as a change.
                self.disk_snapshot = Some(json);
                true
            }
            _ => {
                log::warn!(
                    "failed to persist setup.json; the change stays in memory \
                     and will be saved on the next change"
                );
                let _ = std::fs::remove_file(&tmp);
                false
            }
        }
    }

    /// Merge policy for [`Setup::save`]: a section this process CHANGED
    /// since its baseline keeps this process's value (the user's latest
    /// action wins); an untouched section adopts the disk's value, so a
    /// concurrent process's change survives. When both sides changed the
    /// same section, ours wins — a true conflict is unavoidable, and the
    /// local user's intent is the one they can see and react to.
    fn merged_over(&self, baseline: &Self, disk: Self) -> Self {
        let mut merged = disk;
        if section_differs(&self.appearance, &baseline.appearance) {
            merged.appearance = self.appearance.clone();
        }
        if section_differs(&self.tools, &baseline.tools) {
            merged.tools = self.tools.clone();
        }
        if section_differs(&self.routing, &baseline.routing) {
            merged.routing = self.routing.clone();
        }
        if section_differs(&self.hooks, &baseline.hooks) {
            merged.hooks = self.hooks.clone();
        }
        if section_differs(&self.providers, &baseline.providers) {
            merged.providers = self.providers.clone();
        }
        if section_differs(&self.model, &baseline.model) {
            merged.model = self.model.clone();
        }
        if section_differs(&self.mode, &baseline.mode) {
            merged.mode = self.mode.clone();
        }
        if section_differs(&self.cache, &baseline.cache) {
            merged.cache = self.cache.clone();
        }
        if section_differs(&self.skills, &baseline.skills) {
            merged.skills = self.skills.clone();
        }
        if section_differs(&self.decision, &baseline.decision) {
            merged.decision = self.decision.clone();
        }
        if section_differs(&self.mcp, &baseline.mcp) {
            merged.mcp = self.mcp.clone();
        }
        // Flat fields compare directly (no section wrapper).
        if self.lsp != baseline.lsp {
            merged.lsp = self.lsp;
        }
        if self.editor != baseline.editor {
            merged.editor = self.editor.clone();
        }
        if self.telemetry != baseline.telemetry {
            merged.telemetry = self.telemetry;
        }
        if self.effective_context != baseline.effective_context {
            merged.effective_context = self.effective_context;
        }
        merged
    }

    /// The configured base URL for a local provider, if the user saved one
    /// through the ADD Provider screen.
    #[must_use]
    pub fn local_base_url(&self, provider: &str) -> Option<&str> {
        self.providers
            .local
            .get(provider)
            .map(|endpoint| endpoint.base_url.as_str())
    }

    /// Save (or replace) the base URL for a local provider and persist.
    pub fn set_local_base_url(&mut self, provider: &str, base_url: &str) {
        self.providers.local.insert(
            provider.to_string(),
            LocalEndpoint {
                base_url: base_url.to_string(),
            },
        );
        self.save();
    }

    /// Forget a local provider's configured base URL and persist.
    pub fn remove_local_base_url(&mut self, provider: &str) {
        if self.providers.local.remove(provider).is_some() {
            self.save();
        }
    }

    /// The globally persisted model id, if the user ever selected one.
    #[must_use]
    pub fn persisted_model(&self) -> Option<&str> {
        if self.model.model.is_empty() {
            None
        } else {
            Some(&self.model.model)
        }
    }

    /// Record the last model selected by the user and persist it globally.
    ///
    /// The selection is a single overwritten slot (provider + model +
    /// reasoning) — the same shape the per-session header stores.
    pub fn set_model_selection(&mut self, provider: &str, model: &str, reasoning: Option<&str>) {
        self.model.provider = provider.to_string();
        self.model.model = model.to_string();
        self.model.reasoning = reasoning.map(String::from);
        self.save();
    }

    /// The globally persisted agent mode (`crate::harness::Mode`), if the
    /// user ever cycled away from the default. Unrecognized stored names
    /// (e.g. from a newer version) fall back to `None`.
    #[must_use]
    pub fn persisted_mode(&self) -> Option<crate::harness::Mode> {
        match self.mode.mode.as_str() {
            "build" => Some(crate::harness::Mode::Build),
            "ask" => Some(crate::harness::Mode::Ask),
            "yolo" => Some(crate::harness::Mode::Yolo),
            "command" => Some(crate::harness::Mode::Command),
            _ => None,
        }
    }

    /// Record the last agent mode the user selected and persist it globally.
    ///
    /// An unchanged selection skips the disk write (cycling through the
    /// modes with Tab must not rewrite the file four times per decision).
    pub fn set_mode_selection(&mut self, mode: crate::harness::Mode) {
        let name = match mode {
            crate::harness::Mode::Build => "build",
            crate::harness::Mode::Ask => "ask",
            crate::harness::Mode::Yolo => "yolo",
            crate::harness::Mode::Command => "command",
        };
        if self.mode.mode == name {
            return;
        }
        self.mode.mode = name.to_string();
        self.save();
    }

    /// Whether the user chose an extended Anthropic prompt-cache TTL (any
    /// wish above the 5-minute default maps onto the 1h TTL — the only
    /// extended value the API supports).
    #[must_use]
    pub fn anthropic_cache_ttl_1h(&self) -> bool {
        self.cache.anthropic_ttl_min > 5
    }

    /// The extended OpenAI prompt-cache retention the user chose
    /// (`None` = the model-dependent default; any wish maps onto the 24h
    /// retention — the only extended value the API supports).
    #[must_use]
    pub fn openai_cache_retention(&self) -> Option<&'static str> {
        (self.cache.openai_retention_min > 0).then_some("24h")
    }
}

#[cfg(test)]
pub(crate) static CONFIG_DIR_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(test)]
mod tests {
    /// Serializes tests that redirect the config directory: `config_dir()`
    /// reads the process-wide environment (`COSH_CONFIG_DIR`), and cargo
    /// runs test threads in parallel. Shared with every OTHER test in this
    /// crate that touches the env var (`uninstall`), so the two can never
    /// interleave each other's override.
    use super::CONFIG_DIR_LOCK as ENV_LOCK;

    /// Redirect `setup.json` to a scratch config dir for the test's
    /// lifetime. Every `save()` reached through a test MUST run under this:
    /// without it the test persists DEFAULTS over the developer's real
    /// `~/.config/cosh/setup.json` — a settings-vanishing bug caused by the
    /// test suite itself (found while investigating the multi-process
    /// lost-update regression; same symptom, third distinct cause).
    struct IsolatedConfig {
        /// Keeps the scratch dir alive for the guard's lifetime; the value
        /// itself is never read (the path is already in the env override).
        _dir: tempfile::TempDir,
    }

    impl IsolatedConfig {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("scratch config dir");
            // SAFETY: ENV_LOCK serializes env mutation across tests; the
            // override is removed when the guard drops.
            unsafe { std::env::set_var("COSH_CONFIG_DIR", dir.path()) };
            Self { _dir: dir }
        }
    }

    impl Drop for IsolatedConfig {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("COSH_CONFIG_DIR") };
        }
    }

    #[test]
    fn routing_round_trips_summarization_models_in_order() {
        let mut setup = super::Setup::default();
        setup.routing.fallbacks.clear();
        setup.routing.summarization_models = vec![
            super::FallbackEntry {
                provider: "ollama".into(),
                model: "first".into(),
            },
            super::FallbackEntry {
                provider: "openrouter".into(),
                model: "second".into(),
            },
        ];
        let restored: super::Setup =
            serde_json::from_str(&serde_json::to_string(&setup).unwrap()).unwrap();
        assert_eq!(restored.routing.summarization_models[0].model, "first");
        assert_eq!(restored.routing.summarization_models[1].model, "second");
        assert!(restored.routing.fallbacks.is_empty());
    }

    #[test]
    fn prompt_corrector_routes_round_trip_as_one_ordered_mixed_chain() {
        let mut setup = super::Setup::default();
        setup.routing.fallback_prompt_corrector = vec![
            super::PromptCorrectorFallback::Model {
                provider: "openrouter".into(),
                model: "openai/gpt-5".into(),
            },
            super::PromptCorrectorFallback::Acp {
                agent: "codex".into(),
            },
            super::PromptCorrectorFallback::Model {
                provider: "groq".into(),
                model: "openai/gpt-oss-120b".into(),
            },
        ];

        let restored: super::Setup =
            serde_json::from_str(&serde_json::to_string(&setup).unwrap()).unwrap();

        assert_eq!(
            restored.routing.fallback_prompt_corrector,
            setup.routing.fallback_prompt_corrector
        );
    }
    use super::*;

    #[test]
    fn default_setup_is_valid_json() {
        let setup = Setup::default();
        let json = serde_json::to_string_pretty(&setup).unwrap();
        let parsed: Setup = serde_json::from_str(&json).unwrap();
        assert!(parsed.appearance.bell_enabled);
        assert!(!parsed.appearance.transparent_background);
        assert_eq!(parsed.tools.tool_call_mode, "native");
        assert!(parsed.providers.local.is_empty());
        assert_eq!(parsed.cache.anthropic_ttl_min, 0);
        assert_eq!(parsed.cache.openai_retention_min, 0);
        // Telemetry is DEFAULT-ON (opt-out, industry standard): absent
        // field, empty file and corrupt file all resolve to ON. Users who
        // want out flip the Settings toggle or set COSH_TELEMETRY=off.
        assert!(parsed.telemetry);
        // Effective context is DEFAULT-OFF: the full window is used.
        assert!(!parsed.effective_context);
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert!(legacy.telemetry);
        assert!(!legacy.effective_context);
    }

    /// The telemetry consent flag round-trips and stays default-on: a legacy
    /// config without the key loads ON (opt-out), a saved `false` (explicit
    /// opt-out) survives the reload.
    #[test]
    fn telemetry_consent_is_opt_out_and_roundtrips() {
        let mut setup = Setup::default();
        assert!(setup.telemetry, "default is on");
        setup.telemetry = false;
        let json = serde_json::to_string(&setup).unwrap();
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert!(!loaded.telemetry);
        // A legacy file that never mentioned telemetry loads ON (opt-out).
        let legacy: Setup = serde_json::from_str(r#"{"appearance": {}}"#).unwrap();
        assert!(legacy.telemetry);
    }

    /// The effective-context toggle round-trips and stays default-off: a
    /// legacy config without the key loads OFF (full window).
    #[test]
    fn effective_context_is_opt_in_and_roundtrips() {
        let mut setup = Setup::default();
        assert!(!setup.effective_context, "default is off");
        setup.effective_context = true;
        let json = serde_json::to_string(&setup).unwrap();
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert!(loaded.effective_context);
        let legacy: Setup = serde_json::from_str(r#"{"appearance": {}}"#).unwrap();
        assert!(!legacy.effective_context);
    }

    /// Free-form duration parsing: bare minutes, unit suffixes, mixed
    /// forms, defaults — and the invalid inputs that must be rejected.
    #[test]
    fn cache_duration_parse_and_format_roundtrip() {
        assert_eq!(parse_cache_duration(""), Ok(None));
        assert_eq!(parse_cache_duration("default"), Ok(None));
        assert_eq!(parse_cache_duration("0m"), Ok(Some(0)));
        assert_eq!(parse_cache_duration("45"), Ok(Some(45)));
        assert_eq!(parse_cache_duration("45m"), Ok(Some(45)));
        assert_eq!(parse_cache_duration("90 M"), Ok(Some(90)));
        assert_eq!(parse_cache_duration("1h"), Ok(Some(60)));
        assert_eq!(parse_cache_duration("1H30m"), Ok(Some(90)));
        assert_eq!(parse_cache_duration("2 h"), Ok(Some(120)));
        assert!(parse_cache_duration("abc").is_err());
        assert!(parse_cache_duration("1.5h").is_err());
        assert!(parse_cache_duration("25h").is_err());
        assert!(parse_cache_duration("100000000000m").is_err());

        assert_eq!(format_cache_duration(0), "default");
        assert_eq!(format_cache_duration(45), "45m");
        assert_eq!(format_cache_duration(60), "1h");
        assert_eq!(format_cache_duration(90), "1h30m");
        assert_eq!(format_cache_duration(1440), "24h");

        // The mapping accessors: Anthropic maps anything above 5 minutes
        // onto the 1h TTL; OpenAI maps anything above 0 onto 24h.
        let mut setup = Setup::default();
        assert!(!setup.anthropic_cache_ttl_1h());
        assert_eq!(setup.openai_cache_retention(), None);
        setup.cache.anthropic_ttl_min = 30;
        setup.cache.openai_retention_min = 1440;
        assert!(setup.anthropic_cache_ttl_1h());
        assert_eq!(setup.openai_cache_retention(), Some("24h"));
    }

    /// The last agent mode is a single overwritten slot that round-trips
    /// through setup.json, and an absent/unknown `mode` category in an old
    /// config loads as "no selection yet" (the Build default applies).
    #[test]
    fn mode_selection_is_overwritten_and_roundtrips() {
        // set_mode_selection persists: the config dir must be a scratch
        // dir, never the developer's real setup.json. The env lock comes
        // FIRST: mutating the var under the lock is the only ordering that
        // keeps two env-touching tests from interleaving.
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();
        let mut setup = Setup::default();
        assert_eq!(setup.persisted_mode(), None);

        setup.set_mode_selection(crate::harness::Mode::Ask);
        assert_eq!(setup.persisted_mode(), Some(crate::harness::Mode::Ask));

        // A later selection OVERWRITES the slot — no history is kept.
        setup.set_mode_selection(crate::harness::Mode::Command);
        assert_eq!(setup.persisted_mode(), Some(crate::harness::Mode::Command));

        let restored: Setup =
            serde_json::from_str(&serde_json::to_string(&setup).unwrap()).unwrap();
        assert_eq!(
            restored.persisted_mode(),
            Some(crate::harness::Mode::Command)
        );

        // An old config without the `mode` category loads with no selection.
        let legacy: Setup = serde_json::from_str(r#"{"appearance": {}}"#).unwrap();
        assert_eq!(legacy.persisted_mode(), None);

        // An unrecognized stored name (future version) is ignored, not a
        // crash or a wrong mode.
        let unknown: Setup = serde_json::from_str(r#"{"mode": {"mode": "warp"}}"#).unwrap();
        assert_eq!(unknown.persisted_mode(), None);
    }

    #[test]
    fn roundtrip_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        setup.appearance.theme = "dracula".to_string();
        setup.appearance.transparent_background = true;
        setup.tools.disabled = vec!["bash".to_string()];
        setup.providers.local.insert(
            "llamacpp".to_string(),
            LocalEndpoint {
                base_url: "http://127.0.0.1:9999".to_string(),
            },
        );

        let json = serde_json::to_string_pretty(&setup).unwrap();
        std::fs::write(&path, &json).unwrap();

        let loaded: Setup = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.appearance.theme, "dracula");
        assert!(loaded.appearance.transparent_background);
        assert_eq!(loaded.tools.disabled, vec!["bash".to_string()]);
        assert_eq!(
            loaded.local_base_url("llamacpp"),
            Some("http://127.0.0.1:9999")
        );
    }

    #[test]
    fn mcp_section_defaults_empty_and_roundtrips() {
        // Legacy files without the section load as "no servers".
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert!(legacy.mcp.servers.is_empty());

        let mut setup = Setup::default();
        setup.mcp.servers.push(crate::mcp::McpServerEntry {
            name: "docs".to_string(),
            transport: crate::mcp::McpTransport::Http(crate::mcp::HttpTransport {
                url: "https://example.com/mcp".to_string(),
                headers: Default::default(),
                api_key_env: None,
                timeout_ms: 1000,
            }),
            enabled: false,
        });
        let json = serde_json::to_string_pretty(&setup).unwrap();
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mcp.servers.len(), 1);
        assert_eq!(loaded.mcp.servers[0].name, "docs");
        assert!(!loaded.mcp.servers[0].enabled);
    }

    #[test]
    fn hooks_switches_default_on_and_roundtrip_independently() {
        // Missing fields → both events enabled (preserves old configs that
        // only carried the single legacy `enabled` switch).
        let parsed: Setup = serde_json::from_str(r#"{"hooks": {}}"#).unwrap();
        assert!(parsed.hooks.pre_tool_use_enabled);
        assert!(parsed.hooks.post_tool_use_enabled);

        let mut setup = Setup::default();
        setup.hooks.post_tool_use_enabled = false;
        setup.hooks.events.insert(
            "PreToolUse".to_string(),
            vec![HookEntry {
                name: "block rm".to_string(),
                matcher: "bash_run".to_string(),
                command: "exit 2".to_string(),
                timeout: Some(5),
            }],
        );
        let json = serde_json::to_string(&setup);
        let loaded: Setup = serde_json::from_str(&json.unwrap()).unwrap();
        assert!(loaded.hooks.pre_tool_use_enabled);
        assert!(!loaded.hooks.post_tool_use_enabled);
        assert_eq!(loaded.hooks.events["PreToolUse"].len(), 1);
        assert_eq!(loaded.hooks.events["PreToolUse"][0].command, "exit 2");
    }

    /// Unknown keys from older configs (e.g. the removed free-gateway
    /// opt-in) are ignored on load.
    #[test]
    fn unknown_legacy_keys_are_ignored() {
        let legacy: Setup =
            serde_json::from_str(r#"{"providers":{"zen_public_opt_in":true}}"#).unwrap();
        assert!(legacy.providers.local.is_empty());
    }

    /// The skills section: legacy files without it fall back to the
    /// `~/.skills` default, a configured list replaces it, and tilde
    /// prefixes expand against `$HOME` (unexpandable entries are dropped).
    #[test]
    fn skills_config_defaults_and_tilde_expansion() {
        // Legacy file without the section → the ~/.skills default.
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert!(legacy.skills.dirs.is_empty());
        assert_eq!(legacy.skills.resolved_dirs().len(), 1);
        assert!(legacy.skills.resolved_dirs()[0].ends_with("/.skills"));

        // Round-trip of a custom list + filters.
        let mut setup = Setup::default();
        setup.skills.dirs = vec!["~/myskills".into(), "/opt/skills".into()];
        setup.skills.recursive = true;
        setup.skills.ignore = vec!["deprecated-*".into()];
        let json = serde_json::to_string(&setup).unwrap();
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.skills.dirs, setup.skills.dirs);
        assert!(loaded.skills.recursive);
        assert_eq!(loaded.skills.ignore, vec!["deprecated-*".to_string()]);

        // Tilde expansion with a resolvable home (pure core — no env access).
        let resolved = loaded.skills.resolved_dirs_with(Some("/home/tester"));
        assert_eq!(
            resolved,
            vec![
                "/home/tester/myskills".to_string(),
                "/opt/skills".to_string()
            ]
        );

        // Entries that keep a bare `~` prefix without a home are dropped.
        assert_eq!(
            loaded.skills.resolved_dirs_with(None),
            vec!["/opt/skills".to_string()]
        );
    }

    /// The global model selection is a single overwritten slot that
    /// round-trips through setup.json, and an absent `model` category in an
    /// old config loads as "no selection yet".
    #[test]
    fn model_selection_is_overwritten_and_roundtrips() {
        // set_model_selection persists: the config dir must be a scratch
        // dir, never the developer's real setup.json. The env lock comes
        // FIRST (same ordering contract as the mode-selection test).
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        assert_eq!(setup.persisted_model(), None);

        // Selecting a model persists provider + model + reasoning.
        setup.set_model_selection("nvidia", "deepseek-ai/deepseek-v4-pro", Some("high"));
        assert_eq!(setup.persisted_model(), Some("deepseek-ai/deepseek-v4-pro"));
        assert_eq!(setup.model.provider, "nvidia");
        assert_eq!(setup.model.reasoning.as_deref(), Some("high"));

        // A later selection OVERWRITES the slot — no history is kept.
        setup.set_model_selection("groq", "openai/gpt-oss-120b", None);
        assert_eq!(setup.persisted_model(), Some("openai/gpt-oss-120b"));
        assert_eq!(setup.model.provider, "groq");
        assert_eq!(setup.model.reasoning, None);

        let json = serde_json::to_string_pretty(&setup).unwrap();
        std::fs::write(&path, &json).unwrap();
        let loaded: Setup = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.persisted_model(), Some("openai/gpt-oss-120b"));
        assert_eq!(loaded.model.provider, "groq");
        assert_eq!(loaded.model.reasoning, None);

        // An old config without the `model` category loads with no selection.
        let legacy: Setup = serde_json::from_str(r#"{"appearance": {}}"#).unwrap();
        assert_eq!(legacy.persisted_model(), None);
    }

    /// The decision section: files without it load as "on with the
    /// measured default floor" (opt-out), the kind-discriminated model
    /// round-trips 1:1 against `ModelKind` shapes (named variants +
    /// `Custom` with repo/subfolder), the termination audit keeps its
    /// own enable flag and confidence floor, and both section names
    /// deserialize — `model_decision` (current) and `checkup` (legacy).
    #[test]
    fn decision_section_defaults_and_kind_roundtrip() {
        // Legacy file without the section → audit on (default-ON), default floor.
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert!(legacy.decision.termination.enabled);
        assert!((legacy.decision.termination.min_confidence - 0.75).abs() < f64::EPSILON);
        assert!(matches!(
            legacy.decision.model,
            super::DecisionModel::English
        ));

        // Round-trip of a named kind + enabled audit: serializes as
        // `model_decision`, the section name a reader of setup.json sees.
        let mut setup = Setup::default();
        setup.decision.model = super::DecisionModel::Multilingual;
        setup.decision.termination.enabled = true;
        setup.decision.termination.min_confidence = 0.8;
        let json = serde_json::to_string_pretty(&setup).unwrap();
        assert!(
            json.contains(r#""model_decision""#),
            "the section persists under its explicit name, got: {json}"
        );
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            loaded.decision.model,
            super::DecisionModel::Multilingual
        ));
        assert!(loaded.decision.termination.enabled);
        assert!((loaded.decision.termination.min_confidence - 0.8).abs() < f64::EPSILON);

        // The `custom` kind round-trips its repo/subfolder payload under
        // the CURRENT section name...
        let custom_json = serde_json::json!({
            "model_decision": {
                "model": {
                    "kind": "custom",
                    "repo": "/tmp/laya-eval/english",
                    "subfolder": null
                },
                "termination": {"enabled": true, "min_confidence": 0.7}
            }
        });
        let parsed: Setup = serde_json::from_value(custom_json).unwrap();
        match parsed.decision.model {
            super::DecisionModel::Custom { repo, subfolder } => {
                assert_eq!(repo, "/tmp/laya-eval/english");
                assert_eq!(subfolder, None);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
        assert!(parsed.decision.termination.enabled);
        assert!((parsed.decision.termination.min_confidence - 0.7).abs() < f64::EPSILON);

        // ...and under the LEGACY alias, so files written before the
        // rename keep loading unchanged.
        let legacy_section: Setup =
            serde_json::from_str(r#"{"checkup": {"termination": {"enabled": false}}}"#).unwrap();
        assert!(!legacy_section.decision.termination.enabled);
    }

    // ── Cross-process persistence (the settings-vanishing regression) ──────

    /// REGRESSION (settings vanishing across processes): two cosh processes
    /// share one setup.json. B changes the theme and saves; A — still
    /// holding its own older in-memory copy — then changes the model and
    /// saves. The old save wrote A's WHOLE in-memory struct, erasing B's
    /// theme. The save must merge: sections A did not touch follow the disk.
    #[test]
    fn save_from_a_stale_process_keeps_the_other_process_change() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        // Seed the disk so both processes have a real baseline.
        let seed = Setup::default();
        std::fs::write(setup_path(), serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        // Process A loads.
        let mut process_a = Setup::load();
        // Process B loads and changes ONLY the theme.
        let mut process_b = Setup::load();
        process_b.appearance.theme = "dracula".to_string();
        process_b.save();

        // Process A changes ONLY the model — through the same accessor the
        // model dialog uses (it persists inside the setter).
        process_a.set_model_selection("groq", "openai/gpt-oss-120b", None);

        // The NEXT load sees BOTH changes.
        let reloaded = Setup::load();
        assert_eq!(
            reloaded.appearance.theme, "dracula",
            "a concurrent process's theme change must survive process A's save"
        );
        assert_eq!(
            reloaded.persisted_model(),
            Some("openai/gpt-oss-120b"),
            "process A's own change must persist too"
        );
    }

    /// True conflict (both processes changed the SAME section): the local
    /// user's latest action wins — they can see and react to it, unlike a
    /// foreign change that happened behind their back.
    #[test]
    fn same_section_changed_by_both_processes_lets_the_local_change_win() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        let seed = Setup::default();
        std::fs::write(setup_path(), serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let mut process_a = Setup::load();
        let mut process_b = Setup::load();
        process_b.appearance.theme = "dracula".to_string();
        process_b.save();

        process_a.appearance.theme = "nord".to_string();
        process_a.save();

        assert_eq!(
            Setup::load().appearance.theme,
            "nord",
            "the last local save wins a true same-section conflict"
        );
    }

    /// A save must never leave a half-written file behind: the write lands
    /// in a temp file and is renamed over the target. A crash mid-write
    /// used to truncate setup.json, and the next launch reset EVERYTHING
    /// (the corruption half of the settings-vanishing report).
    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        let mut setup = Setup::default();
        setup.appearance.theme = "dracula".to_string();
        setup.save();

        let path = setup_path();
        assert!(
            !path.with_extension("json.tmp").exists(),
            "the temp file must be consumed by the rename"
        );
        // The on-disk file is complete, parseable JSON — never a fragment.
        let parsed: Setup =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.appearance.theme, "dracula");
    }

    /// A corrupt setup.json is quarantined beside the original instead of
    /// being silently reset: nothing is lost, and the cause stays visible.
    /// The old `unwrap_or_default` made truncation look like "the user
    /// never configured anything".
    #[test]
    fn corrupt_setup_json_is_quarantined_not_silently_reset() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        let path = setup_path();
        std::fs::write(&path, "{\"appearance\": { \"theme\": \"drac").unwrap();

        let loaded = Setup::load();
        assert_eq!(
            loaded.appearance.theme, "",
            "an unparsable file loads defaults"
        );
        assert!(!path.exists(), "the corrupt file must move out of the way");
        let quarantine = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry.file_name().to_string_lossy().starts_with("setup.json.corrupt-")
            })
            .expect("the corrupt file must be quarantined beside the original");
        assert!(
            std::fs::read_to_string(quarantine.path())
                .unwrap()
                .starts_with("{\"appearance\""),
            "the quarantine must preserve the original bytes"
        );
    }

    /// When another process holds the config lock past the wait window, the
    /// save is SKIPPED (the change stays in memory for the next save) — it
    /// must never block forever, and must never clobber the locked file.
    #[test]
    fn save_under_a_foreign_lock_is_skipped_not_clobbered() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        let path = setup_path();
        let mut setup = Setup::default();
        setup.save(); // seed the disk
        let disk_before = std::fs::read_to_string(&path).unwrap();

        // A foreign process holds the lock for longer than we are willing
        // to wait (tests shrink the window; production waits only 250ms —
        // saves are millisecond-scale, so longer contention means a stuck
        // holder, and save runs on the UI thread).
        let holder = acquire_config_lock(&path, std::time::Duration::ZERO).unwrap();
        setup.lock_wait = std::time::Duration::from_millis(80);
        setup.appearance.theme = "dracula".to_string();
        setup.save();
        drop(holder);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            disk_before,
            "a skipped save must not touch the locked file"
        );
        // The in-memory change survives for the NEXT save (once the lock is
        // free), which persists it.
        setup.lock_wait = std::time::Duration::from_secs(1);
        setup.save();
        assert_eq!(Setup::load().appearance.theme, "dracula");
    }

    /// A fresh in-memory `Setup` (never loaded — no disk baseline) still
    /// writes wholesale: there is nothing to merge with, so the explicit
    /// state is authoritative. This pins the "first save creates the file"
    /// path the isolation-dependent tests above never exercise.
    #[test]
    fn first_save_without_a_baseline_writes_wholesale() {
        let _env_guard = ENV_LOCK.lock();
        let _isolated = IsolatedConfig::new();

        let mut setup = Setup {
            lsp: false,
            ..Setup::default()
        };
        setup.save();
        let reloaded = Setup::load();
        assert!(!reloaded.lsp);
        // The next save's baseline is this file, so an unrelated concurrent
        // change is honored from now on (covered by the stale-process test).
    }

    // ── Merge-schema tripwire ───────────────────────────────────────────────

    /// Nudge the FIRST scalar found inside a JSON value (recursing through
    /// arrays and objects) so it differs from the original. Tag keys of
    /// serde enums (`kind`, `type`) are never touched: renaming one would
    /// break deserialization instead of changing a value. Returns `false`
    /// when the value contains no nudgeable scalar (empty structures).
    fn bump_first_scalar(value: &mut serde_json::Value) -> bool {
        match value {
            serde_json::Value::Bool(b) => {
                *b = !*b;
                true
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    *value = serde_json::json!(i + 1);
                } else if let Some(f) = n.as_f64() {
                    *value = serde_json::json!(f + 1.0);
                } else {
                    return false;
                }
                true
            }
            serde_json::Value::String(s) => {
                s.push_str("-changed");
                true
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().any(bump_first_scalar)
            }
            serde_json::Value::Object(map) => map
                .iter_mut()
                .filter(|(key, _)| key.as_str() != "kind" && key.as_str() != "type")
                .any(|(_, item)| bump_first_scalar(item)),
            serde_json::Value::Null => false,
        }
    }

    /// SCHEMA TRIPWIRE for the save-time merge: every serialized top-level
    /// field of `Setup` must be honored by `merged_over` — a field changed
    /// locally (against a default baseline, disk unchanged) must WIN the
    /// merge. Without this test, adding a new section to `Setup` and
    /// forgetting to add it to `merged_over` would compile fine and then
    /// silently DROP that field's local changes on every concurrent save
    /// (the exact class of bug these changes fix, reintroduced by growth).
    #[test]
    fn merged_over_honors_every_serialized_field() {
        let baseline = Setup::default();
        let base_json = serde_json::to_value(&baseline).unwrap();
        let serde_json::Value::Object(defaults) = base_json else {
            panic!("Setup must serialize as a JSON object");
        };

        let mut checked = 0usize;
        for field in defaults.keys() {
            // Build the "local" state by nudging one scalar inside this
            // field, through the SAME serde round-trip the real merge path
            // uses (this is also what keeps the test honest about renames
            // and tagged enums).
            let mut local_json = serde_json::to_value(&baseline).unwrap();
            let slot = local_json
                .get_mut(field.as_str())
                .expect("field just listed");
            if !bump_first_scalar(slot) {
                // A field with no nudgeable scalar (e.g. an empty map in the
                // default) cannot be tested through this construction — the
                // per-section merge tests cover it concretely.
                continue;
            }
            let local: Setup = serde_json::from_value(local_json.clone()).unwrap();
            assert_ne!(
                serde_json::to_value(&local).unwrap(),
                serde_json::to_value(&baseline).unwrap(),
                "the nudge must actually change field {field}"
            );

            // Disk = baseline (another process wrote nothing we care about):
            // the merge must adopt the LOCAL change for this field and leave
            // everything else at the disk value.
            let merged = local.merged_over(&baseline, baseline.clone());
            let merged_json = serde_json::to_value(&merged).unwrap();
            assert_eq!(
                merged_json.get(field),
                local_json.get(field),
                "merged_over dropped the local change to `{field}` — \
                 add the field to the merge (a Setup field not handled by \
                 merged_over loses its local changes whenever a concurrent \
                 process saves)"
            );
            checked += 1;
        }
        assert!(
            checked >= 10,
            "the merge tripwire must cover the real schema (checked {checked})"
        );
    }
}
