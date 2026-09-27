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

use serde_json::{json, Map, Value};

use crate::decision::confidence::{
    answer_confidence, clamp_temperature_default, confidence_from_probs, temp_bucket, TEMP_MAX,
    TEMP_MIN,
};
use crate::decision::question::{
    check_question, qtype_code, render_options, serialize_state, to_internal,
};
use crate::decision::sequence::{build_sequence, BuildOptions};
use crate::error::{Error, Result};
use crate::hub::{resolve_revision, snapshot_revision, verify_digests};
use crate::pycompat::{py_float, py_g, py_repr_str, py_repr_value, round4};
use crate::runtime::batch::{collate_items, CollateItem};
#[cfg(test)]
use crate::runtime::batch::CollatedBatch;
use crate::runtime::session::{ort_error, OrtSession, SessionRunner};
#[cfg(test)]
use crate::runtime::session::SessionOutput;
use crate::runtime::tokenizer::{fix_tokenizer_config, HfTokenizer, Tokenizer};

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
) -> Result<OnnxAgent> {
    let mut resolved_revision: Option<String> = None;
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
        // snapshot_download restricted to the files the runtime reads.
        // Cache-first: hf-hub 0.5 has no offline switch, so a fully cached
        // repo is served straight from the cache directory — the same
        // no-network behaviour upstream gets from HF_HUB_OFFLINE=1.
        let resolved = resolve_revision(model_id_or_path, revision);
        let repo = match &resolved {
            Some(r) => hf_hub::Repo::with_revision(
                model_id_or_path.to_string(),
                hf_hub::RepoType::Model,
                r.clone(),
            ),
            None => hf_hub::Repo::new(model_id_or_path.to_string(), hf_hub::RepoType::Model),
        };
        let prefix = subfolder.map(|s| format!("{}/", s)).unwrap_or_default();
        let config_rel = format!("{}rl_agent_config.json", prefix);
        let cache_repo = hf_hub::Cache::from_env().repo(repo.clone());
        let cached_cfg = cache_repo.get(&config_rel);
        let cached_tok = cache_repo.get(&format!("{}tokenizer/tokenizer.json", prefix));
        if let (Some(cfg_file), Some(_)) = (cached_cfg, cached_tok) {
            // The snapshot root: the config sits at
            // <cache>/models--*/snapshots/<sha>[/<sub>]/rl_agent_config.json.
            let mut dir = cfg_file.parent().unwrap_or(Path::new(".")).to_path_buf();
            if !prefix.is_empty() {
                dir = dir.parent().unwrap_or(Path::new(".")).to_path_buf();
            }
            resolved_revision = snapshot_revision(&dir).or_else(|| resolved.clone());
            dir
        } else {
            // Network path. hf-hub reads the token from the cache token file
            // only; layer the HF_TOKEN environment variable over it the way
            // huggingface_hub does, for gated repositories.
            let mut builder = hf_hub::api::sync::ApiBuilder::from_env();
            if let Ok(token) = std::env::var("HF_TOKEN")
                && !token.is_empty()
            {
                builder = builder.with_token(Some(token));
            }
            let api = builder
                .build()
                .map_err(|e| Error::Runtime(format!("cosh-onnx: hub client failed: {}", e)))?;
            let repo_api = api.repo(repo);
            let info = repo_api
                .info()
                .map_err(|e| Error::Runtime(format!("cosh-onnx: could not resolve {}: {}", model_id_or_path, e)))?;
            // Pin every download to the revision the listing came from, so a
            // stale `refs/main` pointer cannot mix files from two revisions.
            let pinned = api.repo(hf_hub::Repo::with_revision(
                model_id_or_path.to_string(),
                hf_hub::RepoType::Model,
                info.sha.clone(),
            ));
            let wanted = |name: &str| -> bool {
                let Some(stripped) = name.strip_prefix(&prefix) else {
                    return false;
                };
                stripped == "rl_agent_config.json"
                    || stripped.starts_with("tokenizer/")
                    || stripped.starts_with("encoder/")
            };
            let mut cfg_file: Option<PathBuf> = None;
            for sib in &info.siblings {
                if !wanted(&sib.rfilename) {
                    continue;
                }
                let path = pinned
                    .get(&sib.rfilename)
                    .map_err(|e| Error::Runtime(format!("cosh-onnx: could not download {}: {}", sib.rfilename, e)))?;
                if sib.rfilename == config_rel {
                    cfg_file = Some(path);
                }
            }
            let cfg_file = cfg_file.ok_or_else(|| {
                Error::Runtime(format!(
                    "Incompatible model: {} does not contain 'rl_agent_config.json'.",
                    py_repr_str(model_id_or_path)
                ))
            })?;
            // The snapshot root: the config sits at
            // <cache>/models--*/snapshots/<sha>[/<sub>]/rl_agent_config.json.
            let mut dir = cfg_file.parent().unwrap_or(Path::new(".")).to_path_buf();
            if !prefix.is_empty() {
                dir = dir.parent().unwrap_or(Path::new(".")).to_path_buf();
            }
            resolved_revision = Some(info.sha);
            dir
        }
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

    // Verify integrity before any file in the checkpoint is parsed or executed.
    verify_digests(&model_dir, expected_sha256, Some(Path::new(onnx_path)))?;

    let cfg_path = model_dir.join("rl_agent_config.json");
    if !cfg_path.exists() {
        return Err(Error::Runtime(format!(
            "Incompatible model: {} does not contain 'rl_agent_config.json'.",
            py_repr_str(model_id_or_path)
        )));
    }
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).map_err(|e| {
        Error::Runtime(format!("cosh-onnx: could not read {}: {}", cfg_path.display(), e))
    })?)
    .map_err(|e| Error::Runtime(format!("cosh-onnx: {} is not valid JSON: {}", cfg_path.display(), e)))?;
    let cfg = cfg
        .as_object()
        .cloned()
        .ok_or_else(|| Error::Runtime("cosh-onnx: rl_agent_config.json must be a JSON object".to_string()))?;

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
    let session = ort::session::Session::builder()
        .map_err(ort_error)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(ort_error)?
        .with_execution_providers([ort::ep::CPU::default().build()])
        .map_err(ort_error)?
        .commit_from_file(onnx_path)
        .map_err(ort_error)?;

    let mut agent = OnnxAgent {
        model_id: model_id_or_path.to_string(),
        cfg,
        tok: Box::new(tok),
        session: Box::new(OrtSession { session }),
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
                    self.temperature_by_options
                        .get(k)
                        .copied()
                        .unwrap_or(1.0),
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
            rejected.push(format!("{}={} -> {}", name, py_repr_value(&raw), py_g(applied)));
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
        &mut self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
        per_call: &crate::hooks::PerCall,
    ) -> Result<Value> {
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
            let hook_state = ctx
                .states
                .first()
                .cloned()
                .unwrap_or_else(|| state.clone());
            match self.infer(&hook_state, &ctx.questions, lang, ctx.max_len, ctx.head_max_len) {
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
                log::warn!("cosh-onnx: hook on_error failed while handling {}: {}", exc, hook_exc);
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
        &mut self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
        per_call: &crate::hooks::PerCall,
    ) -> Result<Value> {
        self.system_one(state, questions, lang, max_len, head_max_len, per_call)
    }

    /// `ONNXAgent._infer`: validate, build one row per question, run a single
    /// forward pass, and decode the typed answers.
    pub fn infer(
        &mut self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value> {
        let ids: Vec<String> = questions.keys().cloned().collect();
        if ids.is_empty() {
            return Ok(json!({
                "model": "laya-rl-agent-onnx",
                "answers": {},
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }));
        }
        for qid in &ids {
            check_question(qid, &questions[qid])?;
        }
        let max_len = max_len
            .or_else(|| self.cfg.get("max_len").and_then(Value::as_u64).map(|v| v as usize))
            .unwrap_or(512);
        let head_max_len = head_max_len
            .or_else(|| {
                self.cfg
                    .get("head_max_len")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize)
            })
            .unwrap_or(192);

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
            // moved into `internals` for the decode loop.
            let q = to_internal(&questions[qid])?;
            let q_value = Value::Object(q);
            let opts = BuildOptions {
                max_len,
                head_max_len,
                option_order: None,
                truncate_left,
                state_ids: Some(&state_ids),
            };
            let (seq, markers) = build_sequence(&*self.tok, state, &q_value, &opts)?;
            if markers.len() != render_options(&q_value)?.len() {
                return Err(Error::Value(format!(
                    "question {} options exceed head_max_len={}",
                    py_repr_str(qid),
                    head_max_len
                )));
            }
            let qt = qtype_code(
                q_value["t"].as_str().unwrap_or_default(),
            )
            .unwrap_or_else(|| {
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

        let pad_id = self.tok.pad_token_id();
        let batch = collate_items(&[items], pad_id)
            .ok_or_else(|| Error::Value("collate_items returned nothing for a non-empty batch".to_string()))?;
        let outputs = self.session.run(&batch)?;
        let n_tokens: u32 = batch.attention_mask.iter().flatten().sum();

        let mut answers = Map::new();
        for (r, qid) in ids.iter().enumerate() {
            let q = &internals[r];
            // Number of option markers for this row (the logit width k).
            let k = batch.marker_mask[r].iter().filter(|b| **b).count();
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
            let logits = &outputs[r].logits;
            let z: Vec<f64> = logits.iter().take(k).map(|v| *v as f64 / t_scale).collect();
            let p = softmax(&z);

            let conf_score = round4(confidence_from_probs(&p, k));
            // `answer_confidence` is the calibrated max(p) confidence, reported
            // on every question type so a caller can gate across types on one
            // number.
            let ans_conf = round4(answer_confidence(&p, k));
            let act = softmax(&outputs[r].act_logits.iter().map(|v| *v as f64).collect::<Vec<_>>());
            let mut ext = Map::new();
            ext.insert("act_probability".into(), json!(round4(act.first().copied().unwrap_or(0.0))));

            let answer = match q["t"].as_str().unwrap_or_default() {
                "choice" => {
                    let keys: Vec<String> =
                        q["crit"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
                    let mut probabilities = Map::new();
                    for (kk, v) in keys.iter().zip(&p) {
                        probabilities.insert(kk.clone(), json!(round4(*v)));
                    }
                    let choice = keys
                        .get(argmax(&p))
                        .cloned()
                        .unwrap_or_default();
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
            };
            answers.insert(qid.clone(), answer);
        }

        Ok(json!({
            "model": "laya-rl-agent-onnx",
            "answers": answers,
            "usage": {"input_tokens": n_tokens, "output_tokens": 0}
        }))
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
    fn predict(&mut self, state: &Value, questions: &Map<String, Value>) -> Result<Value> {
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
        &mut self,
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
    fn run(&mut self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>> {
        let n = batch.input_ids.len();
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
    )
}

#[cfg(test)]
mod tests;
