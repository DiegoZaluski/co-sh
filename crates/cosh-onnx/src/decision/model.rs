//! The model contract: what it means to be a decision model in this crate.
//!
//! A decision model answers a batch of questions against a state. The trait
//! is the seam every backend plugs into — the ONNX agent now, future
//! backends later — and the only type the generic consumers (shortlist,
//! schema decisions) depend on.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::error::Result;
use crate::hooks::{PredictHook, SharedHook};

/// A caller that can answer questions; the upstream `runner.predict`.
///
/// `&self`: a prediction only reads immutable model state (temperatures,
/// config); the tokenizer and session are interior-mutable seams
/// (`HfTokenizer`, `OrtSession`), so a shared runner predicts concurrently.
pub trait PredictRunner {
    /// Run one prediction; returns the full result object (`answers`,
    /// `usage`, ...).
    fn predict(&self, state: &Value, questions: &Map<String, Value>) -> Result<Value>;
}

/// A resident agent handle: the duck-typed [`AgentLike`] shared by every
/// prediction. An `Arc` clone shares one agent with no outer lock — a
/// prediction only reads immutable model state, and the mutable seams
/// (session, tokenizer, hook lock) are interior to the agent (upstream: one
/// `Agent` object shared by all callers, inference serialised on the
/// interpreter).
///
/// Lives here because [`DecisionModel`] hands out exactly this handle — the
/// Router and the facade share one shape of residency.
pub type SharedAgent = Arc<dyn AgentLike>;

/// The agent-like surface the Router drives (`agent.system_one`,
/// `agent.predict_batch`, `agent.revision`, `agent.lang_temperatures`).
///
/// Upstream accepts any object with those attributes — the torch `Agent`, an
/// `ONNXAgent`, or a test stub — so `Router.load`/`Router.attach` hold a
/// trait object here instead of a concrete type. The ONNX agent implements
/// this trait; tests implement it over their own stubs, which is the
/// equivalent of `unittest.mock.patch("laya.agent.Agent")`.
///
/// Upstream tolerates an agent-like object whose `system_one` predates the
/// `lang` argument by catching the `TypeError` and retrying without it; the
/// Rust signature carries `lang` as an `Option` parameter, so the tolerance
/// is structural (an implementor that ignores the language simply ignores
/// the argument) and no retry path exists to get wrong.
///
/// `Send + Sync` supertraits: a resident is shared by reference
/// ([`SharedAgent`]) and predicts concurrently. Upstream's `&mut self` comes
/// from Python's single-threaded interpreter, not from the model — a
/// prediction reads immutable state (temperatures, config) and drives
/// interior-mutable seams (session, tokenizer, hook lock).
pub trait AgentLike: Send + Sync {
    /// `agent.system_one(state, questions, lang=..., max_len=...,
    /// head_max_len=...)`. The Router passes `lang` on every call (None
    /// included), exactly as upstream passes `lang=effective_lang`.
    fn system_one(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value>;

    /// `agent.predict_batch(states, questions, batch_size=..., lang=...)`:
    /// one shared question schema and token budget over many states. The
    /// default runs one [`AgentLike::system_one`] per state — the ONNX
    /// backend's shape; a batching backend overrides this. `lang` is the
    /// group's forwarded language, set only when the agent carries
    /// per-language temperatures (upstream adds the kwarg only when not
    /// None, which an `Option` parameter expresses directly).
    fn predict_batch(
        &self,
        states: &[Value],
        questions: &Map<String, Value>,
        _batch_size: Option<usize>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Vec<Value>> {
        states
            .iter()
            .map(|state| self.system_one(state, questions, lang, max_len, head_max_len))
            .collect()
    }

    /// `agent.revision`: the commit SHA the agent was loaded from (None for
    /// a local path).
    fn revision(&self) -> Option<String> {
        None
    }

    /// `agent.lang_temperatures`: whether the agent carries per-language
    /// temperature overrides. Empty means `lang` is unused, so the Router
    /// must not name it in a batch group key (upstream `getattr(agent,
    /// "lang_temperatures", None)`).
    fn has_lang_temperatures(&self) -> bool {
        false
    }
}

/// Which checkpoint to load.
///
/// The named variants are the three published laya checkpoints, addressed by
/// the same names the Router routes by (`english`, `multilingual`,
/// `typed-decisions`). The repo ids and their subfolders are the laya
/// backend's data — this enum stays backend-agnostic so a future backend can
/// resolve the same names to its own artifacts. A caller holding a raw hub
/// repo id, a bundled `(repo, subfolder)` pair or a local checkpoint
/// directory passes [`ModelKind::Custom`].
///
/// The default is [`ModelKind::English`], matching upstream `load()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelKind {
    /// The English root checkpoint (`english`).
    #[default]
    English,
    /// The multilingual checkpoint (`multilingual`).
    Multilingual,
    /// The typed-decisions checkpoint (`typed-decisions`).
    TypedDecisions,
    /// A raw hub repo id, a bundled `(repo, subfolder)` checkpoint, or a
    /// local checkpoint directory (`rl_agent_config.json`, the tokenizer and
    /// the ONNX graph).
    Custom {
        /// The repo id or local path.
        repo: String,
        /// One checkpoint out of a bundling repo (`subfolder=` upstream).
        subfolder: Option<String>,
    },
}

impl ModelKind {
    /// The checkpoint name the Router routes by, for the named variants;
    /// `None` for [`ModelKind::Custom`].
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Self::English => Some("english"),
            Self::Multilingual => Some("multilingual"),
            Self::TypedDecisions => Some("typed-decisions"),
            Self::Custom { .. } => None,
        }
    }
}

/// `load(...)` keyword arguments, as an options struct.
///
/// `hooks` / `on_predict_start` / `on_predict_end` observe or shape every
/// prediction, `hooks_raise=false` warns and continues when a hook fails,
/// `hooks_concurrent=false` serialises hooks that are not safe to run in
/// parallel, and `hooks_timeout` bounds each hook call in seconds (`None`
/// means no limit). `hooks_raise` / `hooks_concurrent` default to `true`.
#[derive(Default)]
pub struct LoadOptions {
    /// The ONNX graph file inside the checkpoint. Defaults to `"laya.onnx"`,
    /// the name the upstream export script writes.
    pub onnx_path: Option<String>,
    /// Pin the download to an explicit commit SHA/branch/tag (`revision=`
    /// upstream). Upstream pins are opt-in: without one, the Hub default
    /// revision is used so existing offline caches keep working; the
    /// reviewed SHAs are in [`crate::laya::checkpoints`] for callers that
    /// opt in.
    pub revision: Option<String>,
    /// `expected_sha256`: artifact path -> digest, verified before anything
    /// in the checkpoint is parsed or executed.
    pub expected_sha256: Option<HashMap<String, String>>,
    /// Per-language temperature overrides (`lang_temperatures=` upstream).
    pub lang_temperatures: Option<Map<String, Value>>,
    /// Hooks observing or shaping every prediction (`hooks=` upstream).
    pub hooks: Vec<SharedHook>,
    /// A hook firing before each forward pass (`on_predict_start=`).
    pub on_predict_start: Option<PredictHook>,
    /// A hook firing after each forward pass (`on_predict_end=`).
    pub on_predict_end: Option<PredictHook>,
    /// Defaults `true` upstream.
    pub hooks_raise: Option<bool>,
    /// Defaults `true` upstream.
    pub hooks_concurrent: Option<bool>,
    /// Per-hook-call bound in seconds; `None` means no limit.
    pub hooks_timeout: Option<f64>,
    /// Rust-only addition (no upstream counterpart): cap the ONNX Runtime
    /// intra-op thread pool of the session. `None` keeps ORT's default (one
    /// pool sized to the machine per session — a Router holding several
    /// residents then stacks that many pools). A small value deduplicates
    /// the CPU budget across residents. Opt-in because the parallel
    /// reduction order inside ORT can differ from the default pool's, and
    /// this port's contract is bit-stable confidence numbers.
    pub intra_op_threads: Option<usize>,
}

/// A loaded decision model: one checkpoint built for inference, shared by
/// every holder of the handle.
///
/// The trait is the stable facade surface: `load` hands out an
/// `Arc<dyn DecisionModel>` (cloning the handle shares the model, and
/// concurrent calls serialise on the single forward pass exactly as they do
/// through the Router). The laya ONNX backend is the first implementation;
/// a future backend implements the same trait.
pub trait DecisionModel: Send + Sync {
    /// The id the model was loaded from: a repo id or local path, with the
    /// subfolder appended for a bundled checkpoint (`repo/subfolder`).
    fn model_id(&self) -> &str;

    /// The commit SHA the model was loaded from; `None` for a local path.
    fn revision(&self) -> Option<String>;

    /// Run one decision: a state and a question schema in, the full result
    /// object (`answers`, `usage`) out — upstream `agent.predict` /
    /// `agent.system_one`.
    ///
    /// `lang` selects a per-language temperature override; `max_len` /
    /// `head_max_len` override the checkpoint's token budgets.
    fn decide(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value>;

    /// Many states against one shared question schema (`predict_batch`
    /// upstream): `states[i]` answers into `results[i]`, input order kept.
    /// `batch_size` is advisory — the default runs one
    /// [`DecisionModel::decide`] per state; a backend with a real batched
    /// forward pass honours it.
    fn predict_batch(
        &self,
        states: &[Value],
        questions: &Map<String, Value>,
        _batch_size: Option<usize>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Vec<Value>> {
        states
            .iter()
            .map(|state| self.decide(state, questions, lang, max_len, head_max_len))
            .collect()
    }
}

/// A typed view over one prediction result — the JSON object
/// [`DecisionModel::decide`] returns.
///
/// Read-only accessors over the answer contract. On every question type,
/// `confidence` is 1 minus the normalised entropy of the distribution (how
/// concentrated it is) while `answer_confidence` is the probability of the
/// reported answer — the one number to gate on across types.
#[derive(Debug, Clone)]
pub struct Prediction(Value);

impl From<Value> for Prediction {
    fn from(raw: Value) -> Self {
        Self(raw)
    }
}

impl Prediction {
    /// The full result object, unchanged.
    pub fn raw(&self) -> &Value {
        &self.0
    }

    /// The `answers` map; empty when the result carries none.
    pub fn answers(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        self.0
            .get("answers")
            .and_then(Value::as_object)
            .unwrap_or(&*EMPTY)
    }

    /// One answer object (`answers[qid]`); `None` when the question is absent.
    fn answer(&self, qid: &str) -> Option<&Value> {
        self.0.get("answers").and_then(Value::as_object)?.get(qid)
    }

    /// A `choice` answer's winning label (`answers[qid]["choice"]`).
    pub fn choice(&self, qid: &str) -> Option<&str> {
        self.answer(qid)?.get("choice").and_then(Value::as_str)
    }

    /// A `score` answer's expected score (`answers[qid]["score"]`).
    pub fn score(&self, qid: &str) -> Option<f64> {
        self.answer(qid)?.get("score").and_then(Value::as_f64)
    }

    /// A `noul` answer's P(true) (`answers[qid]["noul"]`).
    pub fn noul(&self, qid: &str) -> Option<f64> {
        self.answer(qid)?.get("noul").and_then(Value::as_f64)
    }

    /// The distribution-concentration confidence of one answer
    /// (`answers[qid]["confidence"]`).
    pub fn confidence(&self, qid: &str) -> Option<f64> {
        self.answer(qid)?.get("confidence").and_then(Value::as_f64)
    }

    /// The reported answer's probability (`answers[qid]["answer_confidence"]`).
    pub fn answer_confidence(&self, qid: &str) -> Option<f64> {
        self.answer(qid)?
            .get("answer_confidence")
            .and_then(Value::as_f64)
    }

    /// The `usage` block (`input_tokens` / `output_tokens`).
    pub fn usage(&self) -> Option<&Map<String, Value>> {
        self.0.get("usage").and_then(Value::as_object)
    }

    /// The `usage.input_tokens` count.
    pub fn input_tokens(&self) -> Option<u64> {
        self.usage()
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64)
    }

    /// The `routing` block (a Router result's checkpoint decision).
    pub fn routing(&self) -> Option<&Map<String, Value>> {
        self.0.get("routing").and_then(Value::as_object)
    }

    /// The checkpoint a Router result used (`routing["model"]`).
    pub fn routing_model(&self) -> Option<&str> {
        self.routing()
            .and_then(|r| r.get("model"))
            .and_then(Value::as_str)
    }
}
