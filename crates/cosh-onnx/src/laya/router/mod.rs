//! Route a request to the Laya checkpoint best suited to it.
//!
//! Three checkpoints, measured on a shared benchmark (17,416 questions, one
//! T4, identical questions per model — see the repository's benchmark
//! notebook):
//!
//! ```text
//!   english          convaiinnovations/laya                421M  ModernBERT-large, 512 tokens
//!   multilingual     convaiinnovations/laya-multilingual   322M  mmBERT-base, 1024 tokens, 100+ langs
//!   typed-decisions  convaiinnovations/laya-typed-decisions 421M  ModernBERT-large, 1024 tokens,
//!                                                                 fine-tuned on the typed-decisions
//!                                                                 workflows
//! ```
//!
//! Why routing is worth it — accuracy by language family:
//!
//! ```text
//!                       english   multilingual
//!   MASSIVE intent  en    0.783       0.657        <- English checkpoint wins
//!   MASSIVE intent  non-en 0.306      0.451
//!   XNLI            en    0.860       0.843
//!   XNLI            non-en 0.521      0.731        <- +21 points for multilingual
//!   English suites        0.684       0.619
//! ```
//!
//! The English checkpoint does not gently degrade off English, it collapses:
//! on 20-option MASSIVE intent it scores 0.100 on Hindi and 0.103 on Korean,
//! against 0.050 for random guessing — and it reports high confidence while
//! doing so (ECE 0.855 on Hindi). Script detection is therefore the primary
//! routing signal.
//!
//! `typed-decisions` is never selected automatically unless you opt in with
//! `auto_task_detection=True` or pass `task="typed_decisions"`: it is
//! fine-tuned on four specific synthetic workflows and should not be a
//! silent default.
//!
//! Divergences from the Python original, all mechanical consequences of the
//! runtime stack:
//! - The Router drives the ONNX backend: `load` builds an
//!   [`crate::laya::agent::OnnxAgent`]
//!   where upstream builds the torch `Agent` (out of scope for this port).
//!   Residents hold the duck-typed [`AgentLike`] trait object instead of a
//!   concrete agent, because upstream accepts any agent-like object:
//!   `attach` takes one that is already built, the agent factory is
//!   swappable, and upstream's `TypeError` tolerance for an agent whose
//!   `system_one` predates the `lang` argument is a trait default here.
//!   That trait is also how the tests substitute stub agents for
//!   `unittest.mock.patch` / `pytest.monkeypatch`.
//! - Resident agents sit behind `Arc<Mutex<dyn AgentLike>>` and an agent
//!   behind that `Mutex` is owned by exactly one `predict` at a time, so the
//!   hook-cycle `&mut self` and session `&mut self` stay sound. The upstream
//!   RLock is a [`std::sync::Mutex`] (with the same poison-tolerance the
//!   hooks registry uses), and Python's `del agent` + `gc.collect()` + CUDA
//!   cache clear have no Rust analogue: dropping the agent frees it.
//! - Python's `task=` and `lang=` accept any value via `str(...)`; here the
//!   caller types are `&str`, so the coercion is a compile-time guarantee.
//! - A callable `lang_guess` hint is [`LangGuess::Callable`]
//!   ([`LangGuessFn`]); a code is [`LangGuess::Code`]. This mirrors "a
//!   code, or a callable taking the state and returning one (or None to
//!   abstain)".
//! - Python `TypeError`s surface as [`Error::Value`], keeping the
//!   message texts verbatim.
//! - `predict_batch` calls the agent-level batch ([`AgentLike::
//!   predict_batch`]), whose default runs one `system_one` per state — the
//!   ONNX backend's shape. The torch `Agent.predict_batch` micro-batching is
//!   out of scope, but the trait method is where it would slot in; the
//!   Router-side group semantics — schema splitting, `lang` keys, routing
//!   keys, hook cycle — are identical either way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value, json};

use crate::decision::question::json_type_name;
use crate::error::{Error, Result};
use crate::pycompat::{py_repr_str, py_repr_value};

pub use crate::decision::model::AgentLike;
use crate::decision::model::SharedAgent;
use crate::hooks::{self, HookEvent, PerCall, PredictContext, SharedHook, compose_hooks, dispatch};
use crate::lang::{Analysis, analyse};

/// The hub repo bundles all three checkpoints; only the requested subfolder
/// is downloaded.
pub const BUNDLE_REPO: &str = "convaiinnovations/laya";

/// `DEFAULT_MODELS` — the bundled specs, in upstream key order. Built per
/// call because a `String` cannot live in a `const` table.
pub fn default_models() -> Vec<(&'static str, ModelSpec)> {
    vec![
        ("english", ModelSpec::Repo(BUNDLE_REPO.to_string())),
        (
            "multilingual",
            ModelSpec::Bundled(BUNDLE_REPO.to_string(), "multilingual".to_string()),
        ),
        (
            "typed-decisions",
            ModelSpec::Bundled(BUNDLE_REPO.to_string(), "typed-decisions".to_string()),
        ),
    ]
}

/// The same checkpoints also live in their own repos, for anyone who
/// prefers them (`STANDALONE_MODELS`).
pub fn standalone_models() -> Vec<(&'static str, ModelSpec)> {
    vec![
        (
            "english",
            ModelSpec::Repo("convaiinnovations/laya".to_string()),
        ),
        (
            "multilingual",
            ModelSpec::Repo("convaiinnovations/laya-multilingual".to_string()),
        ),
        (
            "typed-decisions",
            ModelSpec::Repo("convaiinnovations/laya-typed-decisions".to_string()),
        ),
    ]
}

/// `ModelSpec`: a checkpoint's hub id or local path, optionally with a
/// subfolder inside a bundle (upstream: a plain string, or a
/// `(repo, subfolder)` tuple/list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSpec {
    /// `"some/repo"` — the repo root, or a local directory path.
    Repo(String),
    /// `(repo, subfolder)` — one checkpoint out of a bundling repo.
    Bundled(String, String),
}

impl ModelSpec {
    /// The hub id / path part (upstream `_split(spec)[0]`).
    pub fn repo(&self) -> &str {
        match self {
            Self::Repo(r) => r,
            Self::Bundled(r, _) => r,
        }
    }

    /// The subfolder part (upstream `_split(spec)[1]`).
    pub fn subfolder(&self) -> Option<&str> {
        match self {
            Self::Repo(_) => None,
            Self::Bundled(_, sub) => Some(sub.as_str()),
        }
    }
}

/// `_repo_str`: human-readable id for a model spec: `repo` or
/// `repo/subfolder`.
pub fn repo_str(spec: &ModelSpec) -> String {
    match spec {
        ModelSpec::Repo(repo) => repo.clone(),
        ModelSpec::Bundled(repo, sub) => format!("{}/{}", repo, sub),
    }
}

/// Aliases people are likely to type (`_ALIASES`). Keyed by the already
/// lowered name; both spellings of a dashed alias are present upstream.
fn alias_of(key: &str) -> Option<&'static str> {
    Some(match key {
        "en" | "laya" | "default" => "english",
        "multi" | "ml" | "laya-multilingual" => "multilingual",
        "typed" | "typed_decisions" | "laya-typed-decisions" | "decisions" => "typed-decisions",
        _ => return None,
    })
}

/// `sorted(DEFAULT_MODELS)` — for the unknown-model message.
const KNOWN_SORTED: [&str; 3] = ["english", "multilingual", "typed-decisions"];

/// `sorted(_ALIASES)` — for the unknown-model message.
const ALIASES_SORTED: [&str; 10] = [
    "decisions",
    "default",
    "en",
    "laya",
    "laya-multilingual",
    "laya-typed-decisions",
    "ml",
    "multi",
    "typed",
    "typed_decisions",
];

/// `normalise_name`: strip, lower, resolve aliases, require a known name.
///
/// Message note: Python formats `name` with `%r` after `str(...)`; here the
/// value arrives as a `&str` already, so the repr is over the string.
pub fn normalise_name(name: &str) -> Result<&'static str> {
    let key = name.trim().to_lowercase();
    let key = alias_of(&key).unwrap_or(key.as_str());
    if KNOWN_SORTED.contains(&key) {
        // Re-key to the static spelling.
        return Ok(KNOWN_SORTED[KNOWN_SORTED.iter().position(|k| *k == key).unwrap()]);
    }
    Err(Error::Value(format!(
        "unknown model {}; choose one of {} (or an alias: {})",
        py_repr_str(name),
        py_repr_value(&json!(KNOWN_SORTED)),
        py_repr_value(&json!(ALIASES_SORTED)),
    )))
}

/// Question-id signatures of the four typed-decisions workflows, used only
/// when auto_task_detection is enabled (`_TYPED_DECISION_WORKFLOWS`, in
/// upstream iteration order — the exact id-set match makes the order moot,
/// but the order pins `match_typed_decisions_workflow`'s scan).
const TYPED_DECISION_WORKFLOWS: [(&str, &[&str]); 4] = [
    (
        "agent_trace_observability",
        &["action", "needs_review", "outcome", "risk", "urgency"],
    ),
    (
        "customer_service",
        &["action", "category", "churn_risk", "needs_human", "urgency"],
    ),
    (
        "invoice_processing",
        &[
            "discrepancy_severity",
            "disposition",
            "duplicate",
            "matches_order",
            "urgency",
        ],
    ),
    (
        "security_incidents",
        &[
            "credential_compromise",
            "disposition",
            "severity",
            "true_positive",
            "urgency",
        ],
    ),
];

/// `match_typed_decisions_workflow`: name of the typed-decisions workflow
/// whose question ids these are, else None.
///
/// Requires an exact id-set match, so an unrelated schema that happens to
/// contain `urgency` is never captured.
pub fn match_typed_decisions_workflow(questions: &Map<String, Value>) -> Option<&'static str> {
    for (wf, sig) in TYPED_DECISION_WORKFLOWS {
        if questions.len() == sig.len() && sig.iter().all(|id| questions.contains_key(*id)) {
            return Some(wf);
        }
    }
    None
}

/// `_question_schema`: order-sensitive signature of a question schema, for
/// sharing forward passes.
///
/// `sort_keys=False` keeps insertion order significant at every nesting
/// level, because option order is positional in `render_options`. The
/// `preserve_order` feature on serde_json gives this the same behaviour, and
/// compact separators match `json.dumps`' default `", "/": "` only for
/// non-ASCII-free content; the grouping key only has to be stable and
/// order-sensitive, which it is. `ensure_ascii=False` matches serde_json's
/// escaping of no BMP characters. `default=str` has no equivalent here: a
/// `Value` is already JSON-shaped, so there is nothing unserialisable left.
pub(crate) fn question_schema(questions: &Map<String, Value>) -> String {
    Value::Object(questions.clone()).to_string()
}

// Subtags that mean "the English checkpoint can read this". Routing needs
// one bit — is this English Latin text, or something the English checkpoint
// cannot read — not a language id, so every other code that names a language
// resolves to the multilingual checkpoint (`_ENGLISH_SUBTAGS`).
const ENGLISH_SUBTAGS: [&str; 3] = ["en", "eng", "english"];

// Codes that are valid `$LANG` values but name no language, so they answer
// nothing about the state. `C`, `POSIX` and `C.UTF-8` are what minimal
// images ship — `C.UTF-8` is the default `LANG` in the official Python
// image, which is where `laya-serve` runs — and the ISO 639-2 special codes
// say the same thing in the standard's own vocabulary: `und` undetermined,
// `zxx` no linguistic content, `mul` multiple languages. They abstain, which
// is what the blank case below already does, rather than forcing the
// multilingual checkpoint on English text (`_LANGUAGE_AGNOSTIC_CODES`).
const LANGUAGE_AGNOSTIC_CODES: [&str; 5] = ["c", "posix", "und", "zxx", "mul"];

/// `_english_from_code`: True/False for a language code, or None when the
/// code identifies nothing.
///
/// Accepts the forms a caller is likely to have to hand: `"en"`, `"EN"`,
/// `"en-US"`, the POSIX `"en_US"` (which `$LANG` holds), and
/// `"en_US.UTF-8"`. `None` here means "no usable hint", which is what lets a
/// language-identification model abstain — and it is also what a code that
/// names no language returns, so `LANG=C` falls through to detection instead
/// of pinning every request to one checkpoint.
pub fn english_from_code(value: Option<&str>) -> Option<bool> {
    let value = value?;
    let code = value.trim().to_lowercase();
    if code.is_empty() {
        return None;
    }
    let code = code.split('.').next().unwrap_or_default(); // en_US.UTF-8 -> en_US
    let primary = code.replace('_', "-");
    let primary = primary.split('-').next().unwrap_or_default(); // en_US -> en
    if primary.is_empty() || LANGUAGE_AGNOSTIC_CODES.contains(&primary) {
        return None;
    }
    Some(ENGLISH_SUBTAGS.contains(&primary))
}

/// A callable `lang_guess` hint: takes the state, returns a language code or
/// `None` to abstain.
pub type LangGuessFn = Arc<dyn Fn(&Value) -> Option<String> + Send + Sync>;

/// A caller-supplied language hint (`lang_guess`): a code, or a callable
/// taking the state and returning one (or None to abstain). Checked before
/// the built-in detection, never before an explicit `model`, `task` or
/// `lang`. The default path is unchanged, so the heuristic stays
/// dependency-free; this is the seam for a real LID model.
#[derive(Clone)]
pub enum LangGuess {
    /// A language code in any of the forms [`english_from_code`] accepts.
    Code(String),
    /// A callable taking the state; its return value goes through the same
    /// code resolution (a `None` return abstains).
    Callable(LangGuessFn),
}

impl std::fmt::Debug for LangGuess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(c) => f.debug_tuple("Code").field(c).finish(),
            Self::Callable(_) => f.write_str("Callable(..)"),
        }
    }
}

/// The routing outcome: which model, why, and what was detected
/// (`RouteDecision`).
///
/// Behaves as a dict upstream so it serialises straight into an API
/// response; here the five keys live in insertion order
/// (model/repo/reason/detection/workflow) and serde serialises it as that
/// same object, so `result["routing"]` round-trips through JSON exactly as
/// upstream's `dict(decision)` does. Detection is embedded in the same key
/// order `analyse` returns upstream: script, script_profile, language,
/// is_english, language_undecided, diacritic_rate, non_latin_fraction,
/// mixed_segment.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecision {
    pub model: String,
    pub repo: String,
    pub reason: String,
    pub detection: Option<Value>,
    pub workflow: Option<String>,
}

impl RouteDecision {
    fn new(
        model: &str,
        repo: String,
        reason: String,
        detection: Option<Value>,
        workflow: Option<String>,
    ) -> Self {
        Self {
            model: model.to_string(),
            repo,
            reason,
            detection,
            workflow,
        }
    }

    /// `dict(decision)`: the keys in insertion order, as a JSON object.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("model".to_string(), json!(self.model));
        map.insert("repo".to_string(), json!(self.repo));
        map.insert("reason".to_string(), json!(self.reason));
        map.insert(
            "detection".to_string(),
            self.detection.clone().unwrap_or(Value::Null),
        );
        map.insert(
            "workflow".to_string(),
            self.workflow
                .as_ref()
                .map_or(Value::Null, |w| Value::String(w.clone())),
        );
        Value::Object(map)
    }
}

/// The [`Analysis`] as the JSON object upstream's `analyse` returns (the
/// `detection` key of a decision): script, script_profile, language,
/// is_english, language_undecided, diacritic_rate, non_latin_fraction,
/// mixed_segment.
fn detection_value(det: &Analysis) -> Value {
    let mut map = Map::new();
    map.insert("script".to_string(), json!(det.script));
    let mut profile = Map::new();
    for (name, value) in &det.script_profile {
        profile.insert(
            name.clone(),
            serde_json::Number::from_f64(*value).map_or(Value::Null, Value::Number),
        );
    }
    map.insert("script_profile".to_string(), Value::Object(profile));
    map.insert(
        "language".to_string(),
        det.language
            .as_ref()
            .map_or(Value::Null, |l| Value::String(l.clone())),
    );
    map.insert("is_english".to_string(), json!(det.is_english));
    map.insert(
        "language_undecided".to_string(),
        json!(det.language_undecided),
    );
    map.insert(
        "diacritic_rate".to_string(),
        serde_json::Number::from_f64(det.diacritic_rate).map_or(Value::Null, Value::Number),
    );
    map.insert(
        "non_latin_fraction".to_string(),
        serde_json::Number::from_f64(det.non_latin_fraction).map_or(Value::Null, Value::Number),
    );
    map.insert(
        "mixed_segment".to_string(),
        det.mixed_segment
            .as_ref()
            .map_or(Value::Null, |s| Value::String(s.clone())),
    );
    Value::Object(map)
}

/// `Router(...)`: the constructor keyword set, mirrored as an options struct
/// (`RouterOptions(...)`); [`Router::new`] is the no-argument form.
#[derive(Default)]
pub struct RouterOptions {
    /// `models=`: per-name spec overrides, applied with `dict.update`
    /// semantics (a known key keeps its position, a new key appends).
    pub models: Vec<(String, ModelSpec)>,
    pub device: Option<String>,
    /// `token=`: falls back to the `HF_TOKEN` environment variable, as
    /// upstream does.
    pub token: Option<String>,
    /// `revision=`: one commit applied to every model; `revisions=`
    /// overrides it per normalised model name.
    pub revision: Option<String>,
    pub revisions: Vec<(String, Option<String>)>,
    /// `max_loaded=`: how many checkpoints stay resident
    /// (least-recently-used is evicted). Upstream default 2; `0` coerces to
    /// 1 (`max(1, int(max_loaded))`).
    pub max_loaded: Option<usize>,
    /// `default=`: the checkpoint undecidable states take.
    pub default: Option<String>,
    pub auto_task_detection: bool,
    /// `standalone_repos=True`: use the per-checkpoint repositories instead
    /// of the bundle.
    pub standalone_repos: bool,
    /// `preload=True`: build every checkpoint up front.
    pub preload: bool,
    pub lang_guess: Option<LangGuess>,
    pub hooks: Vec<SharedHook>,
    pub on_predict_start: Option<crate::hooks::PredictHook>,
    pub on_predict_end: Option<crate::hooks::PredictHook>,
    /// Defaults `true` upstream.
    pub hooks_raise: Option<bool>,
    /// Defaults `true` upstream.
    pub hooks_concurrent: Option<bool>,
    pub hooks_timeout: Option<f64>,
}

/// Builds the agent for one checkpoint: `(repo_or_path, subfolder,
/// resolved_revision)`. Swappable so tests can stand a stub in for
/// `unittest.mock.patch("laya.agent.Agent")`.
type AgentFactory =
    Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> Result<Box<dyn AgentLike>> + Send + Sync>;

/// The default factory: the torch `Agent` upstream builds is out of scope for
/// this port, so the Router drives the ONNX backend. `onnx_path` stays at its
/// upstream default (`"laya.onnx"`, resolved against the working directory),
/// and `device`/`token` are consumed the way [`crate::laya::agent::load`]
/// consumes them (CPU-only build; the token comes from the HF cache /
/// `HF_TOKEN`).
fn default_agent_factory(
    repo: &str,
    subfolder: Option<&str>,
    revision: Option<&str>,
) -> Result<Box<dyn AgentLike>> {
    // Router-side opt-in for the session thread cap, mirroring
    // `LoadOptions::intra_op_threads` for the facade path. The Router
    // factory signature is upstream-shaped (no options struct), so the
    // environment is the only channel; `COSH_ONNX_INTRA_THREADS` is a
    // positive integer. Absent/invalid keeps ORT's default.
    let intra_op_threads = std::env::var("COSH_ONNX_INTRA_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0);
    Ok(Box::new(crate::laya::agent::load_agent(
        repo,
        "laya.onnx",
        subfolder,
        revision,
        None,
        None,
        Vec::new(),
        None,
        None,
        true,
        true,
        None,
        intra_op_threads,
    )?))
}

/// Resident agents plus the LRU order, behind one mutex (`self._lock`,
/// upstream an RLock — here a plain [`Mutex`] is enough because no method
/// re-enters it while held: lifecycle hooks fire after the guard drops).
#[derive(Default)]
struct Lifecycle {
    /// Insertion-ordered residents. Upstream `self._agents` is a Python dict
    /// whose iteration order is insertion order — `list(_agents) ==
    /// loaded` is asserted upstream, so the two views must agree. At most a
    /// handful of entries live here (`max_loaded`), so the linear scans are
    /// constant-sized.
    agents: Vec<(String, SharedAgent)>,
    /// Least-recently-used first (`self._order`).
    order: Vec<String>,
}

/// Remove and drop one resident by name; true when it was there
/// (`self._agents.pop(key, None)`).
fn remove_agent(agents: &mut Vec<(String, SharedAgent)>, key: &str) -> bool {
    match agents.iter().position(|(k, _)| k == key) {
        Some(pos) => {
            agents.remove(pos);
            true
        }
        None => false,
    }
}

/// `Router`: lazily loads Laya checkpoints and sends each request to the
/// right one.
///
/// ```ignore
/// let r = Router::new()?;
/// r.predict(&json!({"message": "Mein Konto wurde zweimal belastet"}), &questions, &PredictOptions::default())?;
/// ```
///
/// Models are downloaded and built on first use. `max_loaded` caps how many
/// stay resident (least-recently-used is evicted), because all three
/// together are ~1.16B parameters. The default is 2, because automatic
/// routing only ever chooses between `english` and `multilingual`: a cap of
/// one rebuilds the checkpoint it just evicted on every script switch, which
/// is seconds per request on exactly the traffic the Router exists for.
pub struct Router {
    /// Opt-in hooks; the defaults keep a hand-built instance working
    /// (upstream class/`__init__` defaults).
    pub hooks: Vec<SharedHook>,
    pub hooks_raise: bool,
    pub hooks_concurrent: bool,
    pub hooks_timeout: Option<f64>,
    /// `_hooks_lock`: serialises dispatch when `hooks_concurrent` is false.
    pub hooks_lock: Option<Mutex<()>>,
    /// `self.models`: insertion-ordered name -> spec map.
    models: Vec<(String, ModelSpec)>,
    /// Kept for parity with upstream construction; the ONNX factory is
    /// CPU-only, so `device` is not consulted on this backend.
    #[allow(dead_code)]
    device: Option<String>,
    /// As upstream, the constructor folds in `HF_TOKEN`; the ONNX loader
    /// reads the same variable itself.
    #[allow(dead_code)]
    token: Option<String>,
    revision: Option<String>,
    revisions: HashMap<String, Option<String>>,
    max_loaded: std::sync::atomic::AtomicUsize,
    default: &'static str,
    auto_task_detection: bool,
    lang_guess: Option<LangGuess>,
    lifecycle: Mutex<Lifecycle>,
    factory: AgentFactory,
}

impl Router {
    /// `Router()` with every default.
    pub fn new() -> Result<Self> {
        Self::configure(RouterOptions::default())
    }

    /// `Router(**kwargs)`.
    pub fn configure(opts: RouterOptions) -> Result<Self> {
        let hooks = hooks::normalise_hooks(opts.hooks, opts.on_predict_start, opts.on_predict_end);
        let hooks_raise = opts.hooks_raise.unwrap_or(true);
        let hooks_concurrent = opts.hooks_concurrent.unwrap_or(true);
        let hooks_timeout = hooks::validate_timeout(opts.hooks_timeout)?;
        let mut models: Vec<(String, ModelSpec)> = if opts.standalone_repos {
            standalone_models()
        } else {
            default_models()
        }
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        for (k, v) in opts.models {
            let key = normalise_name(&k)?;
            match models.iter_mut().find(|(name, _)| name == key) {
                Some((_, spec)) => *spec = v,
                None => models.push((key.to_string(), v)),
            }
        }
        let token = opts.token.or_else(|| std::env::var("HF_TOKEN").ok());
        let revisions: HashMap<String, Option<String>> = opts
            .revisions
            .into_iter()
            .map(|(k, v)| normalise_name(&k).map(|key| (key.to_string(), v)))
            .collect::<Result<_>>()?;
        let router = Self {
            hooks,
            hooks_raise,
            hooks_concurrent,
            hooks_timeout,
            hooks_lock: (!hooks_concurrent).then(|| Mutex::new(())),
            models,
            device: opts.device,
            token,
            revision: opts.revision,
            revisions,
            max_loaded: std::sync::atomic::AtomicUsize::new(opts.max_loaded.unwrap_or(2).max(1)),
            default: normalise_name(opts.default.as_deref().unwrap_or("english"))?,
            auto_task_detection: opts.auto_task_detection,
            lang_guess: opts.lang_guess,
            lifecycle: Mutex::new(Lifecycle::default()),
            factory: Arc::new(default_agent_factory),
        };
        if opts.preload {
            router.preload(None)?;
        }
        Ok(router)
    }

    /// Install a different agent factory (the test seam for upstream's
    /// `monkeypatch.setattr(laya.agent, "Agent", ...)`).
    pub fn with_agent_factory(
        mut self,
        factory: impl Fn(&str, Option<&str>, Option<&str>) -> Result<Box<dyn AgentLike>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.factory = Arc::new(factory);
        self
    }

    /// `self.max_loaded` (readable and raised by `attach`/`preload`).
    pub fn max_loaded(&self) -> usize {
        self.max_loaded.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `self.default`.
    pub fn default_model(&self) -> &'static str {
        self.default
    }

    /// `self.lang_guess` (the installed hint; `None` when absent).
    pub fn lang_guess(&self) -> Option<&LangGuess> {
        self.lang_guess.as_ref()
    }

    /// `self.models[name]` (a normalised name).
    fn model_spec(&self, key: &str) -> &ModelSpec {
        &self
            .models
            .iter()
            .find(|(name, _)| name == key)
            .expect("normalise_name guarantees a known model key")
            .1
    }

    /// Poison tolerance, matching the hooks registry: a panicking hook or
    /// caller must not permanently break the router.
    fn lock_lifecycle(&self) -> MutexGuard<'_, Lifecycle> {
        self.lifecycle.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------------ loading

    /// `load`: return the agent for `name`, downloading and building it on
    /// first use.
    ///
    /// Concurrent callers share a single agent instead of building
    /// duplicates.
    pub fn load(&self, name: &str) -> Result<SharedAgent> {
        let key = normalise_name(name)?;
        let evicted;
        let agent;
        {
            let mut lc = self.lock_lifecycle();
            let existing = lc
                .agents
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, agent)| agent.clone());
            if let Some(existing) = existing {
                Self::touch_locked(&mut lc, key);
                return Ok(existing);
            }
            let spec = self.model_spec(key);
            let revision = self
                .revisions
                .get(key)
                .cloned()
                .unwrap_or_else(|| self.revision.clone());
            let built = (self.factory)(spec.repo(), spec.subfolder(), revision.as_deref())?;
            let shared: SharedAgent = Arc::from(built);
            lc.agents.push((key.to_string(), shared.clone()));
            lc.order.push(key.to_string());
            evicted = Self::evict_locked(&mut lc, self.max_loaded());
            agent = shared;
        }
        // Lifecycle hooks fire after the lock is released, so a hook can
        // safely call the Router.
        self.dispatch_lifecycle(HookEvent::Evict, &evicted)?;
        let mut ctx =
            PredictContext::new(Vec::new(), Map::new(), Some(key.to_string()), None, None);
        dispatch(
            &compose_hooks(&self.hooks, &PerCall::default()),
            HookEvent::Load,
            &mut ctx,
            self.hooks_raise,
            self.hooks_lock.as_ref(),
            self.hooks_timeout,
        )?;
        Ok(agent)
    }

    /// `_touch`: move `key` to the most-recently-used end.
    fn touch_locked(lc: &mut Lifecycle, key: &str) {
        if let Some(pos) = lc.order.iter().position(|k| k == key) {
            lc.order.remove(pos);
        }
        lc.order.push(key.to_string());
    }

    /// `_evict_locked`: drop least-recently-used agents until `max_loaded`
    /// holds. Returns evicted names.
    fn evict_locked(lc: &mut Lifecycle, max_loaded: usize) -> Vec<String> {
        let mut evicted = Vec::new();
        while lc.order.len() > max_loaded {
            let victim = lc.order.remove(0);
            if remove_agent(&mut lc.agents, &victim) {
                evicted.push(victim);
            }
        }
        if lc.order.len() < lc.agents.len() {
            // keep the two views consistent
            let orphans: Vec<String> = lc
                .agents
                .iter()
                .map(|(k, _)| k.clone())
                .filter(|k| !lc.order.contains(k))
                .collect();
            for k in orphans {
                if remove_agent(&mut lc.agents, &k) {
                    evicted.push(k);
                }
            }
        }
        // Python follows with gc.collect() and a CUDA cache clear; dropping
        // the Arc above is the whole story here.
        evicted
    }

    /// `_dispatch_lifecycle`: fire one lifecycle hook per name.
    /// `_dispatch_lifecycle`: fire one lifecycle hook per name. A raising
    /// hook propagates (upstream lets it rise out of `load`/`unload`); with
    /// `hooks_raise=false` the dispatcher warns and continues.
    fn dispatch_lifecycle(&self, event: HookEvent, names: &[String]) -> Result<()> {
        for name in names {
            let mut ctx =
                PredictContext::new(Vec::new(), Map::new(), Some(name.clone()), None, None);
            dispatch(
                &compose_hooks(&self.hooks, &PerCall::default()),
                event,
                &mut ctx,
                self.hooks_raise,
                self.hooks_lock.as_ref(),
                self.hooks_timeout,
            )?;
        }
        Ok(())
    }

    /// `attach`: register an already-built agent under `name` instead of
    /// loading a second copy.
    ///
    /// Useful when the process has a checkpoint loaded for other reasons: a
    /// demo that already built `convaiinnovations/laya` can hand it to the
    /// router rather than pay for — and hold in memory — a duplicate 421M
    /// parameters.
    pub fn attach(&self, name: &str, agent: Box<dyn AgentLike>) -> Result<SharedAgent> {
        let key = normalise_name(name)?;
        let shared: SharedAgent = Arc::from(agent);
        {
            let mut lc = self.lock_lifecycle();
            // Replacing an existing agent keeps its insertion position (an
            // in-place dict update); a new key appends.
            match lc.agents.iter_mut().find(|(k, _)| k == key) {
                Some((_, resident)) => *resident = shared.clone(),
                None => lc.agents.push((key.to_string(), shared.clone())),
            }
            Self::touch_locked(&mut lc, key);
            let residents = lc.agents.len();
            self.max_loaded
                .fetch_max(residents, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(shared)
    }

    /// `preload`: download and build checkpoints up front so no request ever
    /// pays a model load.
    ///
    /// A cold load costs seconds; language detection costs microseconds.
    /// `max_loaded` is raised to fit both the requested checkpoints and all
    /// already-resident agents, so incremental preloading does not evict
    /// either.
    pub fn preload(&self, names: Option<&[&str]>) -> Result<&Self> {
        let names: Vec<&'static str> = match names {
            None => self
                .models
                .iter()
                .map(|(k, _)| normalise_name(k))
                .collect::<Result<_>>()?,
            Some(names) => names
                .iter()
                .map(|n| normalise_name(n))
                .collect::<Result<_>>()?,
        };
        {
            let lc = self.lock_lifecycle();
            let unique: std::collections::HashSet<&str> = names
                .iter()
                .copied()
                .chain(lc.agents.iter().map(|(k, _)| k.as_str()))
                .collect();
            self.max_loaded
                .fetch_max(unique.len(), std::sync::atomic::Ordering::SeqCst);
        }
        for n in names {
            let already = self.lock_lifecycle().agents.iter().any(|(k, _)| k == n);
            if !already {
                // load() dispatches on_load outside the lock; do not hold it
                // across the call.
                self.load(n)?;
            }
        }
        Ok(self)
    }

    /// `unload`: free one model, or all of them.
    pub fn unload(&self, name: Option<&str>) -> Result<()> {
        let freed = {
            let mut lc = self.lock_lifecycle();
            match name {
                None => {
                    let freed = lc.order.clone();
                    lc.agents.clear();
                    lc.order.clear();
                    freed
                }
                Some(name) => {
                    let key = normalise_name(name)?;
                    let existed = remove_agent(&mut lc.agents, key);
                    if let Some(pos) = lc.order.iter().position(|k| k == key) {
                        lc.order.remove(pos);
                    }
                    if existed {
                        vec![key.to_string()]
                    } else {
                        Vec::new()
                    }
                }
            }
        };
        self.dispatch_lifecycle(HookEvent::Evict, &freed)?;
        Ok(())
    }

    /// `loaded`: the resident checkpoint names, least-recently-used first.
    pub fn loaded(&self) -> Vec<String> {
        self.lock_lifecycle().order.clone()
    }

    /// `loaded_revisions`: the commit SHA each resident agent was loaded
    /// from (None for local paths).
    pub fn loaded_revisions(&self) -> Vec<(String, Option<String>)> {
        self.lock_lifecycle()
            .agents
            .iter()
            .map(|(name, agent)| {
                let revision = agent.revision();
                (name.clone(), revision)
            })
            .collect()
    }

    /// `_resolve_hint`: True/False for a hint about whether the English
    /// checkpoint can read `state`.
    ///
    /// `hint` is either a language code or a callable taking the state.
    /// Anything the hint cannot answer returns None, which makes `route`
    /// fall through to detection rather than picking a checkpoint on no
    /// evidence.
    fn resolve_hint(&self, hint: Option<&LangGuess>, state: &Value) -> Option<bool> {
        let hint = hint?;
        match hint {
            LangGuess::Code(code) => english_from_code(Some(code)),
            LangGuess::Callable(f) => english_from_code(f(state).as_deref()),
        }
    }

    // ------------------------------------------------------------------ routing

    /// `route`: decide which checkpoint to use, then let `on_route` hooks
    /// observe or replace it.
    ///
    /// `ctx.decision` is the decision; a hook may replace it (for example to
    /// pin a checkpoint) and the replacement is what gets returned and used.
    /// `hooks` are per-call hooks, appended after any installed on the
    /// Router.
    pub fn route(
        &self,
        state: &Value,
        questions: Option<&Map<String, Value>>,
        opts: &RouteOptions<'_>,
    ) -> Result<RouteDecision> {
        let mut decision = self.route_inner(state, questions, opts)?;
        let raise_errors = opts.hooks_raise.unwrap_or(self.hooks_raise);
        let active = compose_hooks(&self.hooks, &opts.per_call);
        let timeout = match opts.per_call.hooks_timeout {
            Some(value) => hooks::validate_timeout(Some(value))?,
            None => self.hooks_timeout,
        };
        let mut ctx = PredictContext::new(
            vec![state.clone()],
            questions.cloned().unwrap_or_default(),
            None,
            None,
            None,
        );
        ctx.decision = Some(decision.to_value());
        dispatch(
            &active,
            HookEvent::Route,
            &mut ctx,
            raise_errors,
            self.hooks_lock.as_ref(),
            timeout,
        )?;
        // A hook may replace the decision outright (upstream returns
        // `ctx.decision`, and `predict` accepts a plain dict there too) or
        // mutate fields in place. Re-read all five keys, falling back to the
        // computed values, so an in-place mutation keeps the rest of the
        // decision and a replacement carries whatever the hook wrote.
        if let Some(Value::Object(map)) = ctx.decision {
            let string = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_string);
            if let Some(m) = string("model") {
                decision.model = m;
            }
            if let Some(r) = string("repo") {
                decision.repo = r;
            }
            if let Some(r) = string("reason") {
                decision.reason = r;
            }
            decision.detection = match map.get("detection") {
                None => decision.detection,
                // `detection: null` upstream IS None (the hint-decided and
                // explicit branches ship it as None); a hook that nulls it
                // means the same.
                Some(Value::Null) => None,
                Some(v) => Some(v.clone()),
            };
            if let Some(w) = string("workflow") {
                decision.workflow = Some(w);
            }
        }
        Ok(decision)
    }

    /// `_route`: decide which checkpoint to use, without loading or running
    /// anything.
    ///
    /// Precedence: explicit `model` > explicit `task` > detected workflow
    /// (opt-in) > explicit `lang` > `lang_guess` > detected script/language
    /// > default.
    fn route_inner(
        &self,
        state: &Value,
        questions: Option<&Map<String, Value>>,
        opts: &RouteOptions<'_>,
    ) -> Result<RouteDecision> {
        if let Some(model) = opts.model {
            let key = normalise_name(model)?;
            return Ok(RouteDecision::new(
                key,
                repo_str(self.model_spec(key)),
                format!("explicit model={}", py_repr_str(model)),
                None,
                None,
            ));
        }

        if let Some(task) = opts.task {
            let resolved = if task.to_lowercase().replace('-', "_") == "typed_decisions" {
                "typed-decisions"
            } else {
                task
            };
            let key = normalise_name(resolved)?;
            return Ok(RouteDecision::new(
                key,
                repo_str(self.model_spec(key)),
                format!("explicit task={}", py_repr_str(task)),
                None,
                None,
            ));
        }

        let workflow = match_typed_decisions_workflow(&questions.cloned().unwrap_or_default());
        if let Some(wf) = workflow
            && self.auto_task_detection
        {
            return Ok(RouteDecision::new(
                "typed-decisions",
                repo_str(self.model_spec("typed-decisions")),
                format!(
                    "question ids match the {} typed-decisions workflow",
                    py_repr_str(wf)
                ),
                None,
                Some(wf.to_string()),
            ));
        }

        if let Some(lang) = opts.lang {
            // An explicit `lang` is decisive only when the code names a
            // language. Blank or whitespace resolves to no usable hint, so
            // it falls through to lang_guess/detection exactly as an
            // abstaining hint does; real English/non-English codes still
            // route now.
            if let Some(resolved) = english_from_code(Some(lang)) {
                let key = if resolved { "english" } else { "multilingual" };
                return Ok(RouteDecision::new(
                    key,
                    repo_str(self.model_spec(key)),
                    format!("explicit lang={}", py_repr_str(lang)),
                    None,
                    workflow.map(str::to_string),
                ));
            }
        }

        // Caller-supplied hint, per-call first then the one installed on the
        // Router. Only a hint that actually answers the question routes here;
        // anything else falls through.
        for (source, hint) in [
            ("lang_guess", opts.lang_guess),
            ("Router(lang_guess=...)", self.lang_guess.as_ref()),
        ] {
            if let Some(resolved) = self.resolve_hint(hint, state) {
                let key = if resolved { "english" } else { "multilingual" };
                return Ok(RouteDecision::new(
                    key,
                    repo_str(self.model_spec(key)),
                    format!(
                        "{}: the caller identified this as {} text",
                        source,
                        if resolved { "English" } else { "non-English" }
                    ),
                    None,
                    workflow.map(str::to_string),
                ));
            }
        }

        let det = analyse(state);
        let (key, reason) = if det.script == "unknown" {
            (
                self.default,
                format!(
                    "no letters detected in state; using default ({})",
                    self.default
                ),
            )
        } else if det.script != "latin" {
            (
                "multilingual",
                format!(
                    "non-Latin script ({}, {:.0}% of letters); the English checkpoint cannot read it",
                    det.script,
                    100.0 * det.non_latin_fraction
                ),
            )
        } else if !det.is_english {
            if let Some(segment) = &det.mixed_segment {
                (
                    "multilingual",
                    format!(
                        "Latin script, mostly English, but a line or field reads as {} ({}); the English checkpoint cannot read it",
                        py_repr_value(&det.language.as_ref().map_or(Value::Null, |l| json!(l))),
                        py_repr_str(&segment.chars().take(60).collect::<String>()),
                    ),
                )
            } else if let Some(language) = &det.language {
                (
                    "multilingual",
                    format!(
                        "Latin script but language looks like {}, not English",
                        py_repr_str(language)
                    ),
                )
            } else {
                // Unidentified Latin-script language: routed on the
                // non-English letters alone, because no stopword list here
                // covers it.
                (
                    "multilingual",
                    format!(
                        "Latin script, language not identified but {:.0}% non-English letters; not safe for the English checkpoint",
                        100.0 * det.diacritic_rate
                    ),
                )
            }
        } else if det.language_undecided {
            // Nothing identifies the language: too short, or only content
            // words ("Quero cancelar", "Esqueci minha senha"). That is no
            // evidence of English either, so it takes the same `default` as
            // a state with no letters. A deployment that serves mostly
            // non-English traffic sets `Router(default="multilingual")`; the
            // stock default keeps it English.
            (
                self.default,
                format!(
                    "Latin script, language not identified and no non-English letters; using default ({})",
                    self.default
                ),
            )
        } else {
            ("english", "English Latin text".to_string())
        };
        Ok(RouteDecision::new(
            key,
            repo_str(self.model_spec(key)),
            reason,
            Some(detection_value(&det)),
            workflow.map(str::to_string),
        ))
    }

    // ------------------------------------------------------------------ running

    /// `predict`: route, then answer every question in one forward pass on
    /// the chosen checkpoint.
    ///
    /// The result is the usual `system_one` payload plus a `routing` key
    /// recording the decision. Router-level `on_predict_start` /
    /// `on_predict_end` hooks wrap the whole route+infer call and see
    /// `ctx.decision`; see [`crate::hooks`]. `max_len` / `head_max_len`
    /// override the agent token budget for this call (a start hook may set
    /// `ctx.max_len` / `ctx.head_max_len`).
    pub fn predict(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        opts: &PredictOptions<'_>,
    ) -> Result<Value> {
        let active = compose_hooks(&self.hooks, &opts.per_call);
        let raise_errors = opts.hooks_raise.unwrap_or(self.hooks_raise);
        let timeout = match opts.per_call.hooks_timeout {
            Some(value) => hooks::validate_timeout(Some(value))?,
            None => self.hooks_timeout,
        };

        // Per-call hooks apply to the whole call, including on_route inside
        // route().
        let decision = self.route(state, Some(questions), &opts.route_options())?;
        let agent = self.load(&decision.model)?;
        let mut effective_lang = opts.lang.map(str::to_string);
        if effective_lang.is_none()
            && let Some(language) = decision
                .detection
                .as_ref()
                .and_then(|d| d.get("language"))
                .and_then(Value::as_str)
        {
            effective_lang = Some(language.to_string());
        }

        let mut ctx = PredictContext::new(
            vec![state.clone()],
            questions.clone(),
            Some(decision.model.clone()),
            opts.max_len,
            opts.head_max_len,
        );
        ctx.decision = Some(decision.to_value());

        let mut error: Option<Error> = None;
        if let Err(exc) = dispatch(
            &active,
            HookEvent::PredictStart,
            &mut ctx,
            raise_errors,
            self.hooks_lock.as_ref(),
            timeout,
        ) {
            error = Some(exc);
        }
        if error.is_none() {
            match ctx.results {
                Some(ref mut results) => {
                    // A cache hit short-circuits inference, but Router.predict
                    // still promises a `routing` key. Add it without
                    // overwriting a routing the cached payload has.
                    for result in results.iter_mut() {
                        if let Value::Object(map) = result {
                            map.entry("routing".to_string())
                                .or_insert_with(|| decision.to_value());
                        }
                    }
                }
                None => {
                    // Pass token-budget overrides only when set: with `None`
                    // the agent config stands (upstream omits the kwargs
                    // entirely so an Agent-like object that predates them
                    // still works; the Rust signature takes `Option`, which
                    // is the same tolerance by construction). The agent runs
                    // its own hook cycle: the Router suppresses the
                    // process-wide defaults (`_SKIP_DEFAULTS` guard) but the
                    // agent-installed hooks still see the call, exactly as
                    // upstream's `agent.system_one(state, questions,
                    // lang=...)` leaves the agent's own hooks untouched.
                    let _guard = hooks::skip_default_hooks_guard();
                    match agent.system_one(
                        &ctx.states[0],
                        &ctx.questions,
                        effective_lang.as_deref(),
                        ctx.max_len,
                        ctx.head_max_len,
                    ) {
                        Ok(mut result) => {
                            if let Value::Object(map) = &mut result {
                                map.insert("routing".to_string(), decision.to_value());
                            }
                            ctx.results = Some(vec![result]);
                        }
                        Err(exc) => error = Some(exc),
                    }
                }
            }
        }

        if let Some(exc) = &error {
            ctx.error = Some(exc.clone());
            if let Err(hook_exc) = dispatch(
                &active,
                HookEvent::Error,
                &mut ctx,
                raise_errors,
                self.hooks_lock.as_ref(),
                timeout,
            ) {
                // A failing on_error hook must not hide the failure that
                // triggered it (Python chains it as __context__).
                log::warn!(
                    "cosh-onnx: hook on_error failed while handling {}: {}",
                    exc,
                    hook_exc
                );
            }
        }

        // The `finally` block: elapsed_ms, usage aggregation, end hooks —
        // they run on the failure path too.
        ctx.elapsed_ms = Some(ctx.started_at.elapsed().as_secs_f64() * 1000.0);
        if let Some(results) = &ctx.results {
            ctx.usage = Some(hooks::aggregate_usage(results));
        }
        if let Err(hook_exc) = dispatch(
            &active,
            HookEvent::PredictEnd,
            &mut ctx,
            raise_errors,
            self.hooks_lock.as_ref(),
            timeout,
        ) {
            if error.is_none() {
                return Err(hook_exc);
            }
            log::warn!("cosh-onnx: hook on_predict_end failed: {}", hook_exc);
        }
        if let Some(exc) = error {
            return Err(exc);
        }
        Ok(ctx
            .results
            .and_then(|mut results| (!results.is_empty()).then(|| results.remove(0)))
            .unwrap_or(Value::Null))
    }

    /// `system_one = predict` (the upstream alias).
    pub fn system_one(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        opts: &PredictOptions<'_>,
    ) -> Result<Value> {
        self.predict(state, questions, opts)
    }

    /// `route_batch`: route a heterogeneous request batch without loading
    /// any checkpoints.
    ///
    /// Each request is a mapping with `state` and `questions` plus the same
    /// optional routing overrides accepted by [`Router::route`]: `model`,
    /// `task`, `lang` and `lang_guess`. The returned decisions preserve
    /// input order.
    ///
    /// This is intentionally separate from inference so callers can inspect
    /// or aggregate routing decisions before paying model-load cost.
    pub fn route_batch(&self, requests: &[Value]) -> Result<Vec<RouteDecision>> {
        let mut decisions = Vec::with_capacity(requests.len());
        for (i, request) in requests.iter().enumerate() {
            let Some(request) = request.as_object() else {
                return Err(Error::Value(format!(
                    "request {} must be a dict, got {}",
                    i,
                    json_type_name(request)
                )));
            };
            if !request.contains_key("state") {
                return Err(Error::Value(format!(
                    "request {} is missing required key 'state'",
                    i
                )));
            }
            if !request.contains_key("questions") {
                return Err(Error::Value(format!(
                    "request {} is missing required key 'questions'",
                    i
                )));
            }
            let Some(questions) = request["questions"].as_object() else {
                return Err(Error::Value(format!(
                    "request {} 'questions' must be a dict, got {}",
                    i,
                    json_type_name(&request["questions"])
                )));
            };
            // Upstream hands `request.get("model")` (and task/lang/lang_guess)
            // to the routing path raw: `normalise_name` and
            // `_english_from_code` both `str(...)` their argument, so a
            // non-string value is coerced rather than rejected — an integer
            // model `123` reports "unknown model 123", and a list
            // `lang_guess` str()s to something that names no language and
            // abstains into detection. Python `str(v)` equals `repr(v)` for
            // every JSON type except strings, which str() leaves bare.
            let py_str = |key: &str| -> Option<String> {
                match request.get(key) {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(other) => Some(py_repr_value(other)),
                }
            };
            let model = py_str("model");
            let task = py_str("task");
            let lang = py_str("lang");
            let lang_guess = py_str("lang_guess").map(LangGuess::Code);
            decisions.push(self.route(
                &request["state"],
                Some(questions),
                &RouteOptions {
                    model: model.as_deref(),
                    task: task.as_deref(),
                    lang: lang.as_deref(),
                    lang_guess: lang_guess.as_ref(),
                    per_call: PerCall::default(),
                    hooks_raise: None,
                },
            )?);
        }
        Ok(decisions)
    }

    /// `predict_batch`: route and execute a heterogeneous request batch with
    /// minimal model churn.
    ///
    /// Requests are routed first and grouped by checkpoint. Within each
    /// checkpoint, requests that share the same question schema are grouped
    /// again so their states can share a forward pass; on the ONNX backend
    /// the agent answers one state per `system_one`, so a group runs that
    /// per state. Results are restored to the original request order.
    ///
    /// Requests may independently specify `model`, `task`, `lang` or
    /// `lang_guess` and may use different question schemas.
    pub fn predict_batch(
        &self,
        requests: &[Value],
        batch_size: Option<usize>,
        hooks_timeout: Option<f64>,
    ) -> Result<Vec<Value>> {
        let decisions = self.route_batch(requests)?;
        if decisions.is_empty() {
            return Ok(Vec::new());
        }

        // Insertion order preserves the order in which model groups first
        // appear. This keeps cache effects deterministic while collapsing an
        // arbitrarily interleaved workload to at most one load per routed
        // checkpoint for this call.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, decision) in decisions.iter().enumerate() {
            match groups.iter_mut().find(|(name, _)| name == &decision.model) {
                Some((_, indices)) => indices.push(i),
                None => groups.push((decision.model.clone(), vec![i])),
            }
        }

        let mut results: Vec<Option<Value>> = (0..requests.len()).map(|_| None).collect();
        let active = compose_hooks(&self.hooks, &PerCall::default());
        let raise_errors = self.hooks_raise;
        let timeout = match hooks_timeout {
            Some(value) => hooks::validate_timeout(Some(value))?,
            None => self.hooks_timeout,
        };

        for (model_name, indices) in groups {
            let agent = self.load(&model_name)?;
            let mut started: Vec<(usize, PredictContext)> = Vec::new();
            let mut group_error: Option<Error> = None;
            // One context per request, built and started the way `predict`
            // does it, so a start hook sees — and can redact, rewrite or
            // skip — each request before it joins a shared forward pass.
            for i in &indices {
                let request = &requests[*i];
                let questions = request["questions"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                let mut ctx = PredictContext::new(
                    vec![request["state"].clone()],
                    questions,
                    Some(model_name.clone()),
                    None,
                    None,
                );
                ctx.decision = Some(decisions[*i].to_value());
                if let Err(exc) = dispatch(
                    &active,
                    HookEvent::PredictStart,
                    &mut ctx,
                    raise_errors,
                    self.hooks_lock.as_ref(),
                    timeout,
                ) {
                    group_error = Some(exc);
                    started.push((*i, ctx));
                    break;
                }
                started.push((*i, ctx));
            }

            if group_error.is_none() {
                // Split each checkpoint group again on what the start hooks
                // left: schema, token budgets and the forwarded language all
                // keep a group to one shared shape.
                let has_lang_temperatures = agent.has_lang_temperatures();
                let mut question_groups: Vec<QuestionGroup> = Vec::new();
                for (pos, (i, ctx)) in started.iter_mut().enumerate() {
                    if let Some(results) = &mut ctx.results {
                        // A cache hit short-circuits inference; the `routing`
                        // key predict adds is set here, before any
                        // inference, without overwriting a cached routing —
                        // so a later failure in the same group still leaves
                        // the cached payload consistent.
                        for result in results.iter_mut() {
                            if let Value::Object(map) = result {
                                map.entry("routing".to_string())
                                    .or_insert_with(|| decisions[*i].to_value());
                            }
                        }
                        continue;
                    }
                    let lang_key = if has_lang_temperatures {
                        let explicit = requests[*i]
                            .get("lang")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        explicit.or_else(|| {
                            decisions[*i]
                                .detection
                                .as_ref()
                                .and_then(|d| d.get("language"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                    } else {
                        None
                    };
                    let schema = question_schema(&ctx.questions);
                    let overrides = (ctx.max_len, ctx.head_max_len);
                    let existing = question_groups.iter_mut().find(|g| {
                        g.schema == schema && g.overrides == overrides && g.lang == lang_key
                    });
                    match existing {
                        Some(g) => g.items.push(pos),
                        None => question_groups.push(QuestionGroup {
                            questions: ctx.questions.clone(),
                            schema,
                            overrides,
                            lang: lang_key,
                            items: vec![pos],
                        }),
                    }
                }

                // One agent-level batch per group: `Agent.predict_batch`
                // upstream evaluates one shared question schema and token
                // budget over many states in a single call and returns one
                // result per state, in order. The ONNX backend's trait
                // default runs `system_one` per state; the batch call is the
                // seam where a batching backend slots in. The Router's
                // `_SKIP_DEFAULTS` guard suppresses the process-wide default
                // hooks; the agent's own hooks still run inside the call.
                for group in question_groups {
                    let states: Vec<Value> = group
                        .items
                        .iter()
                        .map(|&pos| {
                            let (_, ctx) = &started[pos];
                            ctx.states[0].clone()
                        })
                        .collect();
                    let _guard = hooks::skip_default_hooks_guard();
                    match agent.predict_batch(
                        &states,
                        &group.questions,
                        batch_size,
                        group.lang.as_deref(),
                        group.overrides.0,
                        group.overrides.1,
                    ) {
                        Ok(batch_results) => {
                            if batch_results.len() != group.items.len() {
                                group_error = Some(Error::Runtime(format!(
                                    "internal error: Agent.predict_batch returned {} results for {} states",
                                    batch_results.len(),
                                    group.items.len()
                                )));
                                break;
                            }
                            for (&pos, result) in group.items.iter().zip(batch_results) {
                                let (i, ctx) = &mut started[pos];
                                let mut result = result;
                                if let Value::Object(map) = &mut result {
                                    map.insert("routing".to_string(), decisions[*i].to_value());
                                }
                                ctx.results = Some(vec![result]);
                            }
                        }
                        Err(exc) => {
                            group_error = Some(exc);
                            break;
                        }
                    }
                }
            }

            if let Some(exc) = &group_error {
                // Every started request is ended, so a hook that opens
                // something in start (a span, an in-flight count) always
                // sees the matching end. A request that already has its
                // result keeps it; the rest failed with the batch.
                for (_, ctx) in &mut started {
                    if ctx.results.is_none() {
                        ctx.error = Some(exc.clone());
                        if let Err(hook_exc) = dispatch(
                            &active,
                            HookEvent::Error,
                            ctx,
                            raise_errors,
                            self.hooks_lock.as_ref(),
                            timeout,
                        ) {
                            log::warn!(
                                "cosh-onnx: hook on_error failed while handling {}: {}",
                                exc,
                                hook_exc
                            );
                        }
                    }
                }
            }
            // Upstream runs `_end_contexts` both on the success path and inside
            // the `except` block. On the failure path a raising end hook is
            // chained as `__context__` on the original error and the ORIGINAL
            // error is what rises out of `predict_batch`; the end-hook failure
            // never masks it.
            if group_error.is_none() {
                self.end_contexts(
                    &active,
                    &mut started.iter_mut().map(|(_, c)| c).collect::<Vec<_>>(),
                    raise_errors,
                    timeout,
                )?;
            } else if let (Some(exc), Err(hook_exc)) = (
                group_error.as_ref(),
                self.end_contexts(
                    &active,
                    &mut started.iter_mut().map(|(_, c)| c).collect::<Vec<_>>(),
                    raise_errors,
                    timeout,
                ),
            ) {
                log::warn!(
                    "cosh-onnx: on_predict_end failed while handling the batch error {exc}: {hook_exc}"
                );
            }
            for (i, ctx) in started {
                if let Some(mut result) = ctx
                    .results
                    .and_then(|mut r| (!r.is_empty()).then(|| r.remove(0)))
                {
                    results[i] = Some(std::mem::take(&mut result));
                }
            }
            if let Some(exc) = group_error {
                return Err(exc);
            }
        }

        // Every input index is assigned exactly once by construction. Keep
        // this check local so a future refactor cannot silently return a
        // partially-filled batch.
        if results.iter().any(Option::is_none) {
            return Err(Error::Runtime(
                "internal error: batch execution did not produce every result".to_string(),
            ));
        }
        Ok(results.into_iter().map(Option::unwrap).collect())
    }

    /// `predict_many = predict_batch` (the upstream alias).
    pub fn predict_many(
        &self,
        requests: &[Value],
        batch_size: Option<usize>,
        hooks_timeout: Option<f64>,
    ) -> Result<Vec<Value>> {
        self.predict_batch(requests, batch_size, hooks_timeout)
    }

    /// `_end_contexts`: finish each request of a batch the way `predict`'s
    /// `finally` finishes one.
    ///
    /// Every context gets its `on_predict_end` even if an earlier one's end
    /// hook raises; the first such failure is raised afterwards. Timing and
    /// usage are set on all of them first, so one request's `elapsed_ms`
    /// never includes another's end hooks.
    fn end_contexts(
        &self,
        active: &[SharedHook],
        contexts: &mut Vec<&mut PredictContext>,
        raise_errors: bool,
        timeout: Option<f64>,
    ) -> Result<()> {
        let now = std::time::Instant::now();
        for ctx in contexts.iter_mut() {
            ctx.elapsed_ms = Some(now.duration_since(ctx.started_at).as_secs_f64() * 1000.0);
            if let Some(results) = &ctx.results {
                ctx.usage = Some(hooks::aggregate_usage(results));
            }
        }
        let mut first_error: Option<Error> = None;
        for ctx in contexts.iter_mut() {
            if let Err(hook_exc) = dispatch(
                active,
                HookEvent::PredictEnd,
                ctx,
                raise_errors,
                self.hooks_lock.as_ref(),
                timeout,
            ) {
                if ctx.error.is_some() {
                    log::warn!("cosh-onnx: hook on_predict_end failed: {}", hook_exc);
                } else if first_error.is_none() {
                    first_error = Some(hook_exc);
                }
            }
        }
        match first_error {
            Some(exc) => Err(exc),
            None => Ok(()),
        }
    }
}

/// One forward-pass group inside [`Router::predict_batch`]: the schema the
/// grouping split on, kept for the call, plus the token budgets and the
/// forwarded language. `items` holds positions into the current group's
/// `started` vector (added in `started` order), not request indices — the
/// request index is `started[pos].0`.
struct QuestionGroup {
    questions: Map<String, Value>,
    schema: String,
    overrides: (Option<usize>, Option<usize>),
    lang: Option<String>,
    items: Vec<usize>,
}

/// The per-request routing overrides of [`Router::route`]
/// (`model=`, `task=`, `lang=`, `lang_guess=` plus the per-call hook set).
#[derive(Default)]
pub struct RouteOptions<'a> {
    pub model: Option<&'a str>,
    pub task: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub lang_guess: Option<&'a LangGuess>,
    pub per_call: PerCall,
    pub hooks_raise: Option<bool>,
}

/// The per-request options of [`Router::predict`]: the routing overrides
/// plus the token budgets.
#[derive(Default)]
pub struct PredictOptions<'a> {
    pub model: Option<&'a str>,
    pub task: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub lang_guess: Option<&'a LangGuess>,
    pub max_len: Option<usize>,
    pub head_max_len: Option<usize>,
    pub per_call: PerCall,
    pub hooks_raise: Option<bool>,
}

impl PredictOptions<'_> {
    /// The routing subset, for the internal `route` call.
    ///
    /// Upstream `predict` forwards only `hooks=`, `hooks_raise=` and
    /// `hooks_timeout=` to `route` — the call's `on_predict_start` /
    /// `on_predict_end` wrap the whole route+infer and do not observe the
    /// `on_route` event inside it.
    fn route_options(&self) -> RouteOptions<'_> {
        RouteOptions {
            model: self.model,
            task: self.task,
            lang: self.lang,
            lang_guess: self.lang_guess,
            per_call: PerCall {
                hooks: self.per_call.hooks.clone(),
                on_predict_start: None,
                on_predict_end: None,
                hooks_raise: self.per_call.hooks_raise,
                hooks_timeout: self.per_call.hooks_timeout,
            },
            hooks_raise: self.hooks_raise,
        }
    }
}

impl crate::decision::model::PredictRunner for Router {
    fn predict(&self, state: &Value, questions: &Map<String, Value>) -> Result<Value> {
        Router::predict(self, state, questions, &PredictOptions::default())
    }
}

impl std::fmt::Debug for Router {
    /// `__repr__`: `Router(loaded=%s, max_loaded=%d, default=%r)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Router(loaded={}, max_loaded={}, default={})",
            py_repr_value(&json!(self.loaded())),
            self.max_loaded(),
            py_repr_str(self.default)
        )
    }
}

#[cfg(test)]
mod tests;
