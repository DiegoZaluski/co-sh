//! System 1 decision model runtime via ONNX: fast CPU-optimized decisions.
//!
//! Divergences from the Python original, all mechanical consequences of the
//! runtime stack:
//! - Warnings surface through the `log` facade instead of `warnings.warn`;
//!   the message texts are verbatim.
//! - CUDA detection: ort gates the CUDA execution provider behind the `cuda`
//!   cargo feature, which this crate does not enable, so the session is
//!   built CPU-only by construction. The upstream "provider listed but failed
//!   to initialize" fallback is therefore unreachable here and is documented
//!   rather than dead-coded.
//! - Config temperature lists: upstream keeps the raw list length, so a
//!   2-entry `temperature` crashes with `IndexError` at decode time and a
//!   4-entry list still clamps and warns entries >= 3. Here the first three
//!   entries fill the `[f64; 3]` (a malformed short list loads where
//!   upstream would crash) while longer lists are still clamped and warned
//!   per entry.
//! - Tokenizer fallback: `AutoTokenizer.from_pretrained` also accepts a Hub
//!   id for the config's `encoder` and reads special-token names from
//!   `tokenizer.json`'s added tokens. `HfTokenizer::from_dir` resolves local
//!   directories only, and falls back to the standard BERT special-token
//!   names when `tokenizer_config.json` does not name them — a checkpoint
//!   with a hub-id encoder and no `tokenizer/` directory does not load here.
//! - The Hub path is cache-first: a fully cached repo is served from the
//!   cache without network (upstream honours `HF_HUB_OFFLINE=1`; hf-hub 0.5
//!   has no offline switch, so the cache is probed directly). `HF_TOKEN` is
//!   honoured for gated repos, layered over hf-hub's cache-token file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::decision::confidence::{
    TEMP_MAX, TEMP_MIN, answer_confidence, clamp_temperature_default, confidence_from_probs,
    temp_bucket,
};
use crate::decision::question::{
    check_question, qtype_code, render_options, serialize_state, to_internal,
};
use crate::decision::sequence::{BuildOptions, build_sequence_with_options};
use crate::error::{Error, Result};
use crate::hub::verify_digests;
use crate::pycompat::{py_float, py_g, py_repr_str, py_repr_value, round4};
#[cfg(test)]
use crate::runtime::batch::CollatedBatch;
use crate::runtime::batch::{CollateItem, collate_items};
use crate::runtime::session::{OrtSession, SessionOutput, SessionRunner, ort_error};
use crate::runtime::tokenizer::{HfTokenizer, Tokenizer, fix_tokenizer_config};

/// Per-language temperature overrides, built exactly as the PyTorch Agent does
/// so a caller can hand the same `lang_temperatures` to either backend and
/// read the same confidence.
#[derive(Debug, Clone)]
pub struct LangTemperatures {
    pub temperature: Vec<f64>,
    pub temperature_by_options: HashMap<String, f64>,
}

/// A Laya agent backed by ONNX Runtime.
pub struct OnnxAgent {
    pub model_id: String,
    pub cfg: Map<String, Value>,
    pub tok: Box<dyn Tokenizer>,
    pub session: Box<dyn SessionRunner>,
    pub temperature_raw: Vec<Value>,
    pub temperature_by_options_raw: Map<String, Value>,
    pub temperature: [f64; 3],
    pub temperature_by_options: HashMap<String, f64>,
    pub lang_temperatures: HashMap<String, LangTemperatures>,
    pub revision: Option<String>,
    // Opt-in hooks; the defaults keep a hand-built instance working and make
    // an unset hook a no-op (upstream class/`__init__` defaults).
    pub hooks: Vec<crate::hooks::SharedHook>,
    pub hooks_raise: bool,
    pub hooks_concurrent: bool,
    pub hooks_timeout: Option<f64>,
    /// `_hooks_lock`: serialises hook dispatch when `hooks_concurrent` is
    /// false; `None` means hooks may run concurrently. Upstream uses a
    /// reentrant `RLock` — here the mutex is not reentrant, so a hook must
    /// not call back into dispatch on the same agent (the `PredictContext`
    /// carries no agent reference, so hooks capture what they need instead;
    /// a re-entering hook would deadlock rather than corrupt state).
    pub hooks_lock: Option<std::sync::Mutex<()>>,
}

/// Convert a digest map into CHECKPOINT-RELATIVE keys for the loader's
/// post-install re-verify.
///
/// The installer's snapshot-root verification hands back repo-relative
/// keys (its `normalize_caller_map` accepts both spellings); this verify
/// runs after the subfolder join, so keys carrying the subfolder prefix
/// are stripped back to checkpoint-relative. Insertion runs in TWO passes
/// with the installer's precedence so collisions are deterministic — an
/// explicit repo-relative key wins over a bare checkpoint-relative key
/// that strips/prefixes to the same path; HashMap iteration order must
/// never decide which digest applies. The `onnx`/`onnx_path` aliases pass
/// through untouched: `verify_digests` resolves them to the same graph
/// path it is handed.
fn checkpoint_relative_keys(
    map: &HashMap<String, String>,
    prefix: &str,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    // Pass 1: bare checkpoint-relative keys.
    for (k, v) in map {
        if !prefix.is_empty() && k.starts_with(prefix) {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    // Pass 2: prefixed keys, stripped — explicit repo-relative wins.
    if !prefix.is_empty() {
        for (k, v) in map {
            if let Some(stripped) = k.strip_prefix(prefix)
                && !stripped.is_empty()
            {
                out.insert(stripped.to_string(), v.clone());
            }
        }
    }
    out
}

/// The loader core: resolve `model_id_or_path` to a checkpoint directory
/// (local path or Hub snapshot with `allow_patterns` restricted to the
/// config, `tokenizer/` and `encoder/`), verify opt-in digests, patch the
/// tokenizer config, load the tokenizer and the ONNX session, and parse the
/// temperature calibration. Crate-internal — callers go through
/// [`crate::laya::facade::load`] (the facade) or the Router factory; the
/// positional `load` below is the deprecated upstream-shaped shim.
///
/// The trailing hook parameters mirror the upstream `__init__` hook arguments:
/// `hooks` / `on_predict_start` / `on_predict_end` observe or shape every
/// prediction, `hooks_raise=false` warns and continues when a hook fails,
/// `hooks_concurrent=false` serialises hooks that are not safe to run in
/// parallel, and `hooks_timeout` bounds each hook call in seconds (`None`
/// means no limit).
///
/// `intra_op_threads` is a Rust-only addition (no upstream counterpart): a
/// cap on the session's ONNX Runtime intra-op thread pool. `None` keeps
/// ORT's default. See [`LoadOptions::intra_op_threads`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_agent(
    model_id_or_path: &str,
    onnx_path: &str,
    subfolder: Option<&str>,
    revision: Option<&str>,
    expected_sha256: Option<&HashMap<String, String>>,
    lang_temperatures: Option<&Map<String, Value>>,
    hooks: Vec<crate::hooks::SharedHook>,
    on_predict_start: Option<crate::hooks::PredictHook>,
    on_predict_end: Option<crate::hooks::PredictHook>,
    hooks_raise: bool,
    hooks_concurrent: bool,
    hooks_timeout: Option<f64>,
    intra_op_threads: Option<usize>,
) -> Result<OnnxAgent> {
    let mut resolved_revision: Option<String> = None;
    // The checkpoint's subfolder as a repo-relative prefix, hoisted so the
    // digest-map normalization below can use it (the installer applies the
    // identical rule via `normalize_caller_map`).
    let prefix = subfolder.map(|s| format!("{}/", s)).unwrap_or_default();
    // The graph file the Hub snapshot carries (resolved by the installer
    // when the checkpoint is not local): `None` keeps the caller's
    // `onnx_path` (the local-checkpoint contract).
    let mut hub_graph: Option<String> = None;
    let model_dir: PathBuf = if Path::new(model_id_or_path).exists() {
        model_id_or_path.into()
    } else {
        let is_local = model_id_or_path.starts_with('/')
            || model_id_or_path.starts_with("./")
            || model_id_or_path.starts_with("../")
            || Path::new(model_id_or_path).is_absolute();
        if is_local {
            return Err(Error::Runtime(format!(
                "Local model path not found: {}.",
                py_repr_str(model_id_or_path)
            )));
        }
        // snapshot_download restricted to the files the runtime reads, via
        // the shared installer (`hub::install`): cache-first (the same
        // no-network behaviour as before — hf-hub 0.5 has no offline
        // switch, so a fully cached repo is served straight from the cache
        // directory), downloads with the built-in progress bar DISABLED
        // (silent callbacks here: a loader must not own a progress UI) and
        // the repo's published `sha256sums.txt` verified before the
        // install succeeds. Graph candidates prefer the quantized twin
        // when the repo ships one (`laya.int8.onnx`), falling back to the
        // fp32 graph — the explicit `onnx_path` wins over both.
        let graph_candidates: Vec<&str> = if onnx_path == "laya.onnx" {
            vec!["laya.int8.onnx", "laya.onnx"]
        } else {
            vec![onnx_path]
        };
        let (dir, graph_rel) = match crate::hub::cache_status(
            model_id_or_path,
            subfolder,
            &graph_candidates,
            revision,
        ) {
            Some((dir, rev, graph)) => {
                resolved_revision = Some(rev);
                (dir, graph)
            }
            None => {
                let installed = crate::hub::install(crate::hub::InstallRequest {
                    repo: model_id_or_path,
                    subfolder,
                    revision,
                    graphs: &graph_candidates,
                    // The caller's explicit digest map REPLACES the mirror's
                    // sums manifest for this install (the installer normalizes
                    // the loader's `onnx`/`onnx_path` keys to the resolved
                    // graph path and requires the graph to be covered). Keys
                    // stay checkpoint-dir-relative — the loader re-verifies
                    // them after the subfolder join below, the same rule as
                    // the local-checkpoint path.
                    expected_sha256: expected_sha256,
                    progress: crate::hub::ProgressCallbacks::silent(),
                })?;
                resolved_revision = Some(installed.revision.clone());
                (installed.snapshot, installed.graph)
            }
        };
        // The graph name relative to the checkpoint dir: the loader joins
        // the subfolder itself below, so the subfolder prefix comes off.
        let graph_name = if prefix.is_empty() {
            graph_rel.clone()
        } else {
            graph_rel
                .strip_prefix(&prefix)
                .unwrap_or(&graph_rel)
                .to_string()
        };
        hub_graph = Some(graph_name);
        dir
    };

    let mut model_dir = model_dir;
    if let Some(sub) = subfolder {
        let joined = model_dir.join(sub);
        if !joined.is_dir() {
            return Err(Error::Runtime(format!(
                "Subfolder {} not found in {}.",
                py_repr_str(sub),
                py_repr_str(model_id_or_path)
            )));
        }
        model_dir = joined;
    }

    // The graph path is resolved relative to the process CWD, which only
    // works for local checkpoints. A hub-downloaded snapshot keeps the
    // graph inside the snapshot dir, so fall back to the model dir when
    // the CWD does not have the file (an explicit user path that exists
    // still wins). The hub path names the graph the repo actually carries
    // (`hub_graph` — possibly the int8 twin), the local path keeps the
    // caller's `onnx_path`.
    let requested_graph: &str = hub_graph.as_deref().unwrap_or(onnx_path);
    let onnx_path: String = if Path::new(requested_graph).exists() {
        requested_graph.to_string()
    } else {
        let in_model_dir = model_dir.join(requested_graph);
        if in_model_dir.exists() {
            in_model_dir.to_string_lossy().into_owned()
        } else {
            // Keep the original: the exists() check below reports it.
            requested_graph.to_string()
        }
    };
    let onnx_path: &str = &onnx_path;

    // Verify integrity before any file in the checkpoint is parsed or executed.
    //
    // The caller's map keys are CHECKPOINT-relative (the loader's own
    // semantics — this verify runs after the subfolder join). The INSTALLER
    // may have handed back a map whose keys it already prefixed into
    // repo-relative paths (its snapshot-root verification accepts both
    // spellings), so strip the subfolder prefix back off here; keys without
    // the prefix keep their checkpoint-relative spelling. Without this, a
    // prefixed key would resolve to `snapshot/<sub>/<sub>/...` and fail
    // closed despite a valid digest map.
    let verify_map = expected_sha256.map(|map| checkpoint_relative_keys(map, &prefix));
    verify_digests(&model_dir, verify_map.as_ref(), Some(Path::new(onnx_path)))?;

    let cfg_path = model_dir.join("rl_agent_config.json");
    if !cfg_path.exists() {
        return Err(Error::Runtime(format!(
            "Incompatible model: {} does not contain 'rl_agent_config.json'.",
            py_repr_str(model_id_or_path)
        )));
    }
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).map_err(|e| {
        Error::Runtime(format!(
            "cosh-onnx: could not read {}: {}",
            cfg_path.display(),
            e
        ))
    })?)
    .map_err(|e| {
        Error::Runtime(format!(
            "cosh-onnx: {} is not valid JSON: {}",
            cfg_path.display(),
            e
        ))
    })?;
    let cfg = cfg.as_object().cloned().ok_or_else(|| {
        Error::Runtime("cosh-onnx: rl_agent_config.json must be a JSON object".to_string())
    })?;

    if !Path::new(onnx_path).exists() {
        return Err(Error::Runtime(format!(
            "ONNX model not found at {}. Please run export_onnx.py first.",
            py_repr_str(onnx_path)
        )));
    }

    // Keep this compatibility fix in sync with Agent: checkpoints produced by
    // newer Transformers versions can contain TokenizersBackend or a
    // list-valued extra_special_tokens field that older loaders cannot parse.
    fix_tokenizer_config(&model_dir);
    let tok_dir = model_dir.join("tokenizer");
    let tok = if tok_dir.exists() {
        HfTokenizer::from_dir(&tok_dir)?
    } else {
        let encoder = cfg
            .get("encoder")
            .and_then(Value::as_str)
            .unwrap_or_default();
        HfTokenizer::from_dir(Path::new(&encoder))?
    };

    // Initialize ONNX Runtime session. Graph optimization ALL is functionally
    // neutral and free at inference time; without it ORT runs the unoptimized
    // graph. CPU-only: see the module docs for the CUDA note.
    let mut builder = ort::session::Session::builder()
        .map_err(ort_error)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(ort_error)?;
    if let Some(threads) = intra_op_threads {
        builder = builder.with_intra_threads(threads).map_err(ort_error)?;
    }
    let session = builder
        .with_execution_providers([ort::ep::CPU::default().build()])
        .map_err(ort_error)?
        .commit_from_file(onnx_path)
        .map_err(ort_error)?;

    let mut agent = OnnxAgent {
        model_id: model_id_or_path.to_string(),
        cfg,
        tok: Box::new(tok),
        session: Box::new(OrtSession::new(session)),
        temperature_raw: Vec::new(),
        temperature_by_options_raw: Map::new(),
        temperature: [1.0, 1.0, 1.0],
        temperature_by_options: HashMap::new(),
        lang_temperatures: HashMap::new(),
        revision: resolved_revision,
        hooks: crate::hooks::normalise_hooks(hooks, on_predict_start, on_predict_end),
        hooks_raise,
        hooks_concurrent,
        hooks_timeout: crate::hooks::validate_timeout(hooks_timeout)?,
        hooks_lock: (!hooks_concurrent).then(|| std::sync::Mutex::new(())),
    };
    agent.apply_config_temperatures()?;
    if let Some(lang_temps) = lang_temperatures {
        agent.apply_lang_temperatures(lang_temps)?;
    }
    if let Some(message) = agent.temperature_warning() {
        log::warn!("{}", message);
    }
    Ok(agent)
}

impl OnnxAgent {
    /// `self.temperature*` from the config: raw values kept for the warning
    /// pass, clamped values applied.
    fn apply_config_temperatures(&mut self) -> Result<()> {
        self.temperature_raw = self
            .cfg
            .get("temperature")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![json!(1.0), json!(1.0), json!(1.0)]);
        self.temperature_by_options_raw = self
            .cfg
            .get("temperature_by_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut t = [1.0f64; 3];
        for (i, raw) in self.temperature_raw.iter().take(3).enumerate() {
            t[i] = clamp_temperature_default(raw);
        }
        self.temperature = t;
        let mut by_options: HashMap<String, f64> = HashMap::new();
        for (k, v) in &self.temperature_by_options_raw {
            by_options.insert(k.clone(), clamp_temperature_default(v));
        }
        self.temperature_by_options = by_options;
        Ok(())
    }

    /// Parse `lang_temperatures`, rejecting a malformed override up front with
    /// the upstream text.
    fn apply_lang_temperatures(&mut self, lang_temps: &Map<String, Value>) -> Result<()> {
        let mut parsed: HashMap<String, LangTemperatures> = HashMap::new();
        for (l, lcfg) in lang_temps {
            let norm_l = l.split('-').next().unwrap_or("").to_lowercase();
            let t_raw = lcfg
                .get("temperature")
                .cloned()
                .unwrap_or_else(|| Value::Array(self.temperature_raw.clone()));
            let Some(t_list) = t_raw.as_array() else {
                return Err(Error::Value(format!(
                    "Language override {} temperature must be a list of 3 floats",
                    py_repr_str(l)
                )));
            };
            if t_list.len() != 3 {
                return Err(Error::Value(format!(
                    "Language override {} temperature must be a list of 3 floats",
                    py_repr_str(l)
                )));
            }
            let tbo_raw = lcfg
                .get("temperature_by_options")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut tbo: HashMap<String, f64> = HashMap::new();
            for (k, v) in &tbo_raw {
                tbo.insert(k.clone(), clamp_temperature_default(v));
            }
            parsed.insert(
                norm_l,
                LangTemperatures {
                    temperature: t_list.iter().map(clamp_temperature_default).collect(),
                    temperature_by_options: tbo,
                },
            );
        }
        self.lang_temperatures = parsed;
        Ok(())
    }

    /// The load-time temperature diagnostic: entries whose applied clamp
    /// differs from the raw value (or that have no numeric raw value at all)
    /// are collected into one warning message, returned so tests can assert
    /// on it; `load` logs it through the `log` facade.
    fn temperature_warning(&self) -> Option<String> {
        let mut entries: Vec<(String, Value, f64)> = self
            .temperature_by_options_raw
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.clone(),
                    self.temperature_by_options.get(k).copied().unwrap_or(1.0),
                )
            })
            .collect();
        for (i, raw) in self.temperature_raw.iter().enumerate() {
            entries.push((
                format!("temperature[{}]", i),
                raw.clone(),
                self.temperature.get(i).copied().unwrap_or(1.0),
            ));
        }
        let mut rejected: Vec<String> = Vec::new();
        for (name, raw, applied) in entries {
            match py_float(&raw) {
                Some(f) if f == applied => continue,
                // Invalid entries already have a neutral fallback; diagnostics
                // must not repeat the failed conversion or prevent the
                // checkpoint from loading.
                _ => {}
            }
            rejected.push(format!(
                "{}={} -> {}",
                name,
                py_repr_value(&raw),
                py_g(applied)
            ));
        }
        if rejected.is_empty() {
            return None;
        }
        Some(format!(
            "cosh-onnx: this checkpoint ships temperatures outside [{}, {}] which would distort \
             confidence; clamping {}. Treat confidence from the affected buckets as uncalibrated.",
            py_g(TEMP_MIN),
            py_g(TEMP_MAX),
            rejected.join(", ")
        ))
    }

    /// `ONNXAgent.system_one`: evaluate typed questions, running any opt-in
    /// hooks around the inference.
    ///
    /// `lang` selects a per-language temperature override, matching the
    /// PyTorch `Agent.system_one` signature so either backend is a drop-in
    /// for the other. The per-call hook arguments mirror the upstream keyword
    /// parameters: a start hook may rewrite `states`/`questions`, set
    /// `max_len`/`head_max_len` on the context, or call `skip()` to
    /// short-circuit inference with a cached result; an end hook may rewrite
    /// `results`. A failing hook propagates when `hooks_raise` is on, or warns
    /// and continues when it is off; `on_error` runs on the failure path
    /// before the end hooks, and a failing `on_error`/end hook never masks the
    /// original failure (Python chains it as `__context__`; here it is
    /// logged).
    pub fn system_one(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
        per_call: &crate::hooks::PerCall,
    ) -> Result<Value> {
        // Fast path: no hook from any source would observe this call, so the
        // hook cycle (and the context clones it exists to feed) is skipped
        // entirely. Behaviour-identical: with no active hooks, `dispatch`
        // never fires, no context mutation can happen, and the result of the
        // full cycle below is exactly `infer(state, questions, ...)`.
        // The per-call timeout is validated first — the full path rejects it
        // before any hook runs, and a caller must get the same rejection
        // when no hook is registered. `hooks_raise` is only read by
        // `dispatch`, so it is not consulted here.
        if let Some(value) = per_call.hooks_timeout {
            crate::hooks::validate_timeout(Some(value))?;
        }
        if crate::hooks::no_hooks_active(&self.hooks, per_call) {
            return self.infer(state, questions, lang, max_len, head_max_len);
        }

        let active = crate::hooks::compose_hooks(&self.hooks, per_call);
        let raise_errors = per_call.hooks_raise.unwrap_or(self.hooks_raise);
        let timeout = match per_call.hooks_timeout {
            Some(value) => crate::hooks::validate_timeout(Some(value))?,
            None => self.hooks_timeout,
        };
        let mut ctx = crate::hooks::PredictContext::new(
            vec![state.clone()],
            questions.clone(),
            Some(self.model_id.clone()),
            max_len,
            head_max_len,
        );

        let mut error: Option<Error> = None;

        if let Err(exc) = crate::hooks::dispatch(
            &active,
            crate::hooks::HookEvent::PredictStart,
            &mut ctx,
            raise_errors,
            self.hooks_lock.as_ref(),
            timeout,
        ) {
            error = Some(exc);
        }

        if error.is_none() && ctx.results.is_none() {
            // A start hook may have rewritten the state/questions; only the
            // values that survive the hook are inferred. The per-call
            // overrides it may have set on the context win over the call
            // arguments.
            let hook_state = ctx.states.first().cloned().unwrap_or_else(|| state.clone());
            match self.infer(
                &hook_state,
                &ctx.questions,
                lang,
                ctx.max_len,
                ctx.head_max_len,
            ) {
                Ok(result) => ctx.results = Some(vec![result]),
                Err(exc) => error = Some(exc),
            }
        }

        if let Some(exc) = &error {
            ctx.error = Some(exc.clone());
            if let Err(hook_exc) = crate::hooks::dispatch(
                &active,
                crate::hooks::HookEvent::Error,
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

        // The `finally` block: elapsed_ms, usage aggregation, end hooks — they
        // run on the failure path too.
        ctx.elapsed_ms = Some(ctx.started_at.elapsed().as_secs_f64() * 1000.0);
        if let Some(results) = &ctx.results {
            ctx.usage = Some(crate::hooks::aggregate_usage(results));
        }
        if let Err(hook_exc) = crate::hooks::dispatch(
            &active,
            crate::hooks::HookEvent::PredictEnd,
            &mut ctx,
            raise_errors,
            self.hooks_lock.as_ref(),
            timeout,
        ) {
            if error.is_none() {
                return Err(hook_exc);
            }
            // The end hook runs on the failure path too; do not let one mask
            // the real error.
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

    /// `predict = system_one` (the upstream alias).
    pub fn predict(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
        per_call: &crate::hooks::PerCall,
    ) -> Result<Value> {
        self.system_one(state, questions, lang, max_len, head_max_len, per_call)
    }

    /// The token budget for a call: the caller override, else the config
    /// value, else the upstream default (512 / 192).
    fn token_budgets(&self, max_len: Option<usize>, head_max_len: Option<usize>) -> (usize, usize) {
        let max_len = max_len
            .or_else(|| {
                self.cfg
                    .get("max_len")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize)
            })
            .unwrap_or(512);
        let head_max_len = head_max_len
            .or_else(|| {
                self.cfg
                    .get("head_max_len")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize)
            })
            .unwrap_or(192);
        (max_len, head_max_len)
    }

    /// The response for zero questions (the `_infer` early return).
    fn empty_payload(&self) -> Value {
        json!({
            "model": "laya-rl-agent-onnx",
            "answers": {},
            "usage": {"input_tokens": 0, "output_tokens": 0}
        })
    }

    /// The row builder of `_infer`, shared by the single and batch shapes:
    /// validate every question, tokenize the state once, and build one
    /// internal question shape plus one collate row per question, in
    /// `questions` key order.
    fn build_rows(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        max_len: usize,
        head_max_len: usize,
    ) -> Result<(Vec<CollateItem>, Vec<Value>)> {
        let ids: Vec<String> = questions.keys().cloned().collect();
        for qid in &ids {
            check_question(qid, &questions[qid])?;
        }
        // Tokenize the shared state once and reuse it across questions, instead
        // of re-serializing and re-tokenizing the same document inside
        // build_sequence per question.
        let truncate_left = state.is_array();
        let state_text = serialize_state(state).replace(self.tok.mask_token(), " ");
        let state_ids = self.tok.encode(&state_text, false, None);
        let mut items: Vec<CollateItem> = Vec::with_capacity(ids.len());
        let mut internals: Vec<Value> = Vec::with_capacity(ids.len());
        for qid in &ids {
            // One owned internal shape per question: `build_sequence` and
            // `render_options` borrow the same value, and the value is then
            // moved into `internals` for the decode loop. The options render
            // once and feed both the sequence build and the marker-count
            // check below (`build_sequence` and `render_options` agree on
            // the option count by construction).
            let q = to_internal(&questions[qid])?;
            let q_value = Value::Object(q);
            let rendered = render_options(&q_value)?;
            let opts = BuildOptions {
                max_len,
                head_max_len,
                option_order: None,
                truncate_left,
                state_ids: Some(&state_ids),
            };
            let (seq, markers) =
                build_sequence_with_options(&*self.tok, state, &q_value, &opts, &rendered)?;
            if markers.len() != rendered.len() {
                return Err(Error::Value(format!(
                    "question {} options exceed head_max_len={}",
                    py_repr_str(qid),
                    head_max_len
                )));
            }
            let qt = qtype_code(q_value["t"].as_str().unwrap_or_default()).unwrap_or_else(|| {
                // Unreachable after `check_question`; a wrong-but-plausible
                // default would misroute temperatures, so fail loudly in
                // debug builds instead.
                debug_assert!(false, "validated question has no known type");
                2
            });
            items.push(CollateItem {
                ids: seq,
                markers,
                qtype: qt,
            });
            internals.push(q_value);
        }
        Ok((items, internals))
    }

    /// `ONNXAgent._infer`: validate, build one row per question, run a single
    /// forward pass, and decode the typed answers.
    pub fn infer(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value> {
        if questions.is_empty() {
            return Ok(self.empty_payload());
        }
        let (max_len, head_max_len) = self.token_budgets(max_len, head_max_len);
        let (items, internals) = self.build_rows(state, questions, max_len, head_max_len)?;
        let pad_id = self.tok.pad_token_id();
        let batch = collate_items(&[items], pad_id).ok_or_else(|| {
            Error::Value("collate_items returned nothing for a non-empty batch".to_string())
        })?;
        let outputs = self.session.run(&batch)?;
        let ids: Vec<String> = questions.keys().cloned().collect();
        let mut answers = Map::new();
        for (r, qid) in ids.iter().enumerate() {
            let k = batch.row_markers(r);
            answers.insert(
                qid.clone(),
                self.decode_row(&internals[r], &outputs[r], k, lang),
            );
        }
        let n_tokens: u32 = (0..batch.n_rows).map(|r| batch.row_tokens(r)).sum();
        Ok(json!({
            "model": "laya-rl-agent-onnx",
            "answers": answers,
            "usage": {"input_tokens": n_tokens, "output_tokens": 0}
        }))
    }

    /// The batch shape of `infer`: one shared question schema and token
    /// budget over many states, chunked into forward passes of `batch_size`
    /// states (`None` = all states in one pass; the trait default's
    /// one-pass-per-state is `Some(1)`).
    ///
    /// Every state's rows share one collated batch and one `session.run` per
    /// chunk; each per-state payload keeps its own `usage` (`input_tokens`
    /// counts only that state's rows), so a caller reading `usage` sees the
    /// same numbers the single-state path reports. Hooks do not run here —
    /// `predict_batch` falls back to the per-state `system_one` cycle when
    /// any hook is installed.
    pub fn infer_batch(
        &self,
        states: &[Value],
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
        batch_size: Option<usize>,
    ) -> Result<Vec<Value>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        if questions.is_empty() {
            return Ok(states.iter().map(|_| self.empty_payload()).collect());
        }
        let (max_len, head_max_len) = self.token_budgets(max_len, head_max_len);
        let mut rows: Vec<Vec<CollateItem>> = Vec::with_capacity(states.len());
        let mut internals: Vec<Vec<Value>> = Vec::with_capacity(states.len());
        for state in states {
            let (items, internal) = self.build_rows(state, questions, max_len, head_max_len)?;
            rows.push(items);
            internals.push(internal);
        }
        let ids: Vec<String> = questions.keys().cloned().collect();
        let pad_id = self.tok.pad_token_id();
        let chunk = batch_size.unwrap_or(states.len()).max(1);
        let mut results = Vec::with_capacity(states.len());
        for (base, chunk_rows) in rows.chunks(chunk).enumerate() {
            let flat: Vec<CollateItem> = chunk_rows.iter().flatten().cloned().collect();
            let batch = collate_items(&[flat], pad_id).ok_or_else(|| {
                Error::Value("collate_items returned nothing for a non-empty batch".to_string())
            })?;
            let outputs = self.session.run(&batch)?;
            let mut offset = 0usize;
            for (s, items) in chunk_rows.iter().enumerate() {
                let mut answers = Map::new();
                for (r, qid) in ids.iter().enumerate() {
                    let row = offset + r;
                    let k = batch.row_markers(row);
                    let internal = &internals[base * chunk + s][r];
                    answers.insert(
                        qid.clone(),
                        self.decode_row(internal, &outputs[row], k, lang),
                    );
                }
                let n_tokens: u32 = (offset..offset + items.len())
                    .map(|row| batch.row_tokens(row))
                    .sum();
                offset += items.len();
                results.push(json!({
                    "model": "laya-rl-agent-onnx",
                    "answers": answers,
                    "usage": {"input_tokens": n_tokens, "output_tokens": 0}
                }));
            }
        }
        Ok(results)
    }

    /// Decode one collated row: the typed answer for the internal question
    /// `q` from the row's logits, at the temperature scale for `lang` over
    /// the row's option-marker count `k`.
    fn decode_row(&self, q: &Value, output: &SessionOutput, k: usize, lang: Option<&str>) -> Value {
        let qt = qtype_code(q["t"].as_str().unwrap_or_default()).unwrap_or(2);
        let mut t_scale = self
            .temperature_by_options
            .get(&temp_bucket(qt, k))
            .copied()
            .unwrap_or(self.temperature[qt as usize]);
        // Python truthiness: an empty `lang` string is falsy and falls
        // through to the base temperatures.
        if let Some(lang) = lang.filter(|l| !l.is_empty()) {
            let norm = lang.split('-').next().unwrap_or("").to_lowercase();
            if let Some(l_cfg) = self.lang_temperatures.get(&norm) {
                t_scale = l_cfg
                    .temperature_by_options
                    .get(&temp_bucket(qt, k))
                    .copied()
                    .unwrap_or(l_cfg.temperature[qt as usize]);
            }
        }
        let z: Vec<f64> = output
            .logits
            .iter()
            .take(k)
            .map(|v| *v as f64 / t_scale)
            .collect();
        let p = softmax(&z);

        let conf_score = round4(confidence_from_probs(&p, k));
        // `answer_confidence` is the calibrated max(p) confidence, reported
        // on every question type so a caller can gate across types on one
        // number.
        let ans_conf = round4(answer_confidence(&p, k));
        let act = softmax(
            &output
                .act_logits
                .iter()
                .map(|v| *v as f64)
                .collect::<Vec<_>>(),
        );
        let mut ext = Map::new();
        ext.insert(
            "act_probability".into(),
            json!(round4(act.first().copied().unwrap_or(0.0))),
        );

        match q["t"].as_str().unwrap_or_default() {
            "choice" => {
                let keys: Vec<String> = q["crit"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                let mut probabilities = Map::new();
                for (kk, v) in keys.iter().zip(&p) {
                    probabilities.insert(kk.clone(), json!(round4(*v)));
                }
                let choice = keys.get(argmax(&p)).cloned().unwrap_or_default();
                json!({
                    "type": "choice",
                    "choice": choice,
                    "probabilities": probabilities,
                    "confidence": conf_score,
                    "answer_confidence": ans_conf,
                    "action": ext,
                })
            }
            "score" => {
                let crit = q["crit"].as_array().cloned().unwrap_or_default();
                let exp_score: f64 = p.iter().enumerate().map(|(i, v)| i as f64 * v).sum();
                let mut legend = Map::new();
                let mut probabilities = Map::new();
                for (i, c) in crit.iter().enumerate() {
                    legend.insert(i.to_string(), c.clone());
                    probabilities.insert(
                        i.to_string(),
                        json!(round4(p.get(i).copied().unwrap_or(0.0))),
                    );
                }
                json!({
                    "type": "score",
                    "score": round4(exp_score),
                    "legend": legend,
                    "probabilities": probabilities,
                    "confidence": conf_score,
                    "answer_confidence": ans_conf,
                    "action": ext,
                })
            }
            _ => {
                let p1 = p.get(1).copied().unwrap_or(0.0);
                json!({
                    "type": "noul",
                    "noul": round4(p1),
                    "confidence": round4(p1.max(1.0 - p1)),
                    "answer_confidence": ans_conf,
                    "action": ext,
                })
            }
        }
    }

    // HookRegistry surface: hooks can be added, removed or scoped after
    // construction (upstream `add_hook` / `remove_hook` /
    // `hooks_installed`).

    /// Install one hook. Returns `self` for chaining.
    pub fn add_hook(&mut self, hook: crate::hooks::SharedHook) -> &mut Self {
        self.hooks.push(hook);
        self
    }

    /// Remove a hook by identity (`Arc` pointer equality, upstream removes by
    /// object identity). Returns true if it was installed.
    pub fn remove_hook(&mut self, hook: &crate::hooks::SharedHook) -> bool {
        let before = self.hooks.len();
        self.hooks
            .retain(|installed| !std::sync::Arc::ptr_eq(installed, hook));
        self.hooks.len() != before
    }

    /// `hooks_installed`: install hooks for the duration of `f`, then remove
    /// them. Rust has no `with` block; the closure takes its place. The
    /// removal runs in a `Drop` guard, so the scoped hooks come off even when
    /// `f` panics (upstream wraps the body in try/finally).
    pub fn hooks_installed<R>(
        &mut self,
        hooks: Vec<crate::hooks::SharedHook>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.hooks.extend(hooks.iter().cloned());
        struct RemoveOnDrop<'a> {
            agent: &'a mut OnnxAgent,
            hooks: Vec<crate::hooks::SharedHook>,
        }
        impl Drop for RemoveOnDrop<'_> {
            fn drop(&mut self) {
                self.agent.hooks.retain(|installed| {
                    !self
                        .hooks
                        .iter()
                        .any(|one| std::sync::Arc::ptr_eq(installed, one))
                });
            }
        }
        let guard = RemoveOnDrop { agent: self, hooks };
        f(guard.agent)
    }
}

/// numpy-style softmax over a 1-D slice (max-subtracted for stability, as the
/// upstream act softmax and the per-question decode both are).
fn softmax(z: &[f64]) -> Vec<f64> {
    if z.is_empty() {
        return Vec::new();
    }
    let max = z.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = z.iter().map(|v| (v - max).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum == 0.0 {
        return vec![1.0 / z.len() as f64; z.len()];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

fn argmax(p: &[f64]) -> usize {
    let mut best = 0usize;
    for (i, v) in p.iter().enumerate() {
        if *v > p[best] {
            best = i;
        }
    }
    best
}

/// A caller that can answer questions — `decide` consumes this.
impl crate::decision::model::PredictRunner for OnnxAgent {
    fn predict(&self, state: &Value, questions: &Map<String, Value>) -> Result<Value> {
        // `runner.predict = system_one` upstream, so the hooks around the
        // decision fire through `structured.decide` as well.
        self.system_one(
            state,
            questions,
            None,
            None,
            None,
            &crate::hooks::PerCall::default(),
        )
    }
}

/// The Router's duck-typed agent surface: the ONNX agent plugs into a
/// [`crate::laya::router::Router`] exactly as the torch `Agent` does upstream.
/// The Router passes the hooks of the *call* itself around the whole
/// route+infer (with the process defaults suppressed by its
/// `_SKIP_DEFAULTS` guard), so the agent here sees its own installed hooks
/// only — the same division of labour as `Router.predict` calling
/// `agent.system_one(...)` with no hook arguments.
impl crate::decision::model::AgentLike for OnnxAgent {
    fn system_one(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value> {
        // The inherent six-argument method takes precedence over the trait
        // method in resolution, so this delegates, not recurses. The agent
        // runs its own hook cycle with no per-call hooks, matching upstream
        // `agent.system_one(ctx.states[0], ctx.questions, lang=..., ...)`.
        self.system_one(
            state,
            questions,
            lang,
            max_len,
            head_max_len,
            &crate::hooks::PerCall::default(),
        )
    }

    fn revision(&self) -> Option<String> {
        self.revision.clone()
    }

    /// The ONNX batch shape: without installed hooks, all states share one
    /// collated batch per `batch_size` chunk and a single forward pass per
    /// chunk. With hooks installed, the per-state hook cycle is preserved
    /// (upstream `Agent.predict_batch` runs hooks around every `_infer`,
    /// so the default one-`system_one`-per-state path is the parity shape).
    fn predict_batch(
        &self,
        states: &[Value],
        questions: &Map<String, Value>,
        batch_size: Option<usize>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Vec<Value>> {
        if crate::hooks::no_hooks_active(&self.hooks, &crate::hooks::PerCall::default()) {
            return self.infer_batch(states, questions, lang, max_len, head_max_len, batch_size);
        }
        states
            .iter()
            .map(|state| {
                self.system_one(
                    state,
                    questions,
                    lang,
                    max_len,
                    head_max_len,
                    &crate::hooks::PerCall::default(),
                )
            })
            .collect()
    }

    fn has_lang_temperatures(&self) -> bool {
        !self.lang_temperatures.is_empty()
    }
}

/// A fixed-logits session for tests, mirroring the upstream `_StubSession`:
/// one row per question, K columns covering the widest question.
#[cfg(test)]
#[doc(hidden)]
pub(crate) struct StubSession {
    pub logits: Vec<Vec<f32>>,
    pub act_logits: Vec<Vec<f32>>,
}

#[cfg(test)]
impl SessionRunner for StubSession {
    fn run(&self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>> {
        let n = batch.n_rows;
        Ok((0..n)
            .map(|r| SessionOutput {
                logits: self.logits.get(r).cloned().unwrap_or_default(),
                act_logits: self.act_logits.get(r).cloned().unwrap_or_default(),
            })
            .collect())
    }
}

/// A word-free stand-in tokenizer for tests, mirroring the upstream `_FakeTok`:
/// ids keyed by character code.
#[cfg(test)]
#[doc(hidden)]
pub(crate) struct FakeTok;

#[cfg(test)]
impl Tokenizer for FakeTok {
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32> {
        // ids = [10 + (ord(c) % 40) for c in text]
        let mut ids: Vec<u32> = text.chars().map(|c| 10 + (c as u32 % 40)).collect();
        if truncation && let Some(max_length) = max_length {
            ids.truncate(max_length);
        }
        ids
    }
    fn mask_token(&self) -> &str {
        "[M]"
    }
    fn mask_token_id(&self) -> u32 {
        3
    }
    fn cls_token_id(&self) -> u32 {
        1
    }
    fn sep_token_id(&self) -> u32 {
        2
    }
    fn pad_token_id(&self) -> u32 {
        0
    }
}

/// A bare agent assembled from parts, mirroring the upstream
/// `ONNXAgent.__new__(ONNXAgent)` test technique: no load path runs. The hook
/// fields take the class defaults (no hooks, raise on error, concurrent, no
/// timeout) unless a test overrides them through the public fields.
#[cfg(test)]
#[doc(hidden)]
pub(crate) fn bare_agent(
    cfg: Map<String, Value>,
    tok: Box<dyn Tokenizer>,
    session: Box<dyn SessionRunner>,
    temperature: [f64; 3],
    temperature_by_options: HashMap<String, f64>,
    lang_temperatures: HashMap<String, LangTemperatures>,
) -> OnnxAgent {
    OnnxAgent {
        model_id: "stub".to_string(),
        cfg,
        tok,
        session,
        temperature_raw: vec![json!(1.0), json!(1.0), json!(1.0)],
        temperature_by_options_raw: Map::new(),
        temperature,
        temperature_by_options,
        lang_temperatures,
        revision: None,
        hooks: Vec::new(),
        hooks_raise: true,
        hooks_concurrent: true,
        hooks_timeout: None,
        hooks_lock: None,
    }
}

/// The upstream-shaped positional `load`, kept for callers that mirror the
/// Python `laya.load(...)` call site argument-for-argument. Deprecated: the
/// stable surface is [`crate::decision::model::ModelKind`] +
/// [`LoadOptions`](crate::decision::model::LoadOptions) through
/// [`crate::laya::facade::load`], which passes the same values by name.
#[deprecated(
    since = "0.1.0",
    note = "use cosh_onnx::laya::facade::load with ModelKind + LoadOptions"
)]
#[allow(clippy::too_many_arguments)]
pub fn load(
    model_id_or_path: &str,
    onnx_path: &str,
    subfolder: Option<&str>,
    revision: Option<&str>,
    expected_sha256: Option<&HashMap<String, String>>,
    lang_temperatures: Option<&Map<String, Value>>,
    hooks: Vec<crate::hooks::SharedHook>,
    on_predict_start: Option<crate::hooks::PredictHook>,
    on_predict_end: Option<crate::hooks::PredictHook>,
    hooks_raise: bool,
    hooks_concurrent: bool,
    hooks_timeout: Option<f64>,
) -> Result<OnnxAgent> {
    load_agent(
        model_id_or_path,
        onnx_path,
        subfolder,
        revision,
        expected_sha256,
        lang_temperatures,
        hooks,
        on_predict_start,
        on_predict_end,
        hooks_raise,
        hooks_concurrent,
        hooks_timeout,
        None,
    )
}

#[cfg(test)]
mod tests;
