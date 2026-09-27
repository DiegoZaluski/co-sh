//! Opt-in embedding shortlist for high-cardinality choice questions.
//!
//! Choice options share one `head_max_len` budget, so a large label set
//! leaves only a few tokens per label. [`shortlist_choice`] embeds the state
//! and each option with a caller-supplied [`EmbedFn`], keeps the top `k`, and
//! runs a single predict on that reduced criteria set.
//!
//! Divergences (intentional):
//! - `embed_fn_from_agent` is not ported: it imports torch and mean-pools
//!   `agent.model.encoder` directly, which is exactly the torch-backed
//!   surface this port excludes. The ONNX graph in this crate bundles the
//!   encoder with the decision head, so no equivalent hook exists.
//! - The upstream tests' torch-tensor returns (`detach().float().cpu()…`)
//!   collapse to plain row matrices here: an [`EmbedFn`] already returns
//!   `Vec<Vec<f64>>`, and `_embeddings`' NaN/±Inf cleaning runs on it.
//! - `k`/`maxsize` are typed integers, so the "bool / float / str is not an
//!   int" ValueError branches are unrepresentable and are not raised.
//! - `predict` kwargs forwarding (`model=` on a `Router`) has no seam in the
//!   [`crate::decision::model::PredictRunner`] trait; the runner decides how its
//!   own options reach it.
//! - Upstream's `_call_predict` prefers `predict` and falls back to
//!   `system_one` (`getattr(agent, "system_one", None)`), raising
//!   `TypeError("agent must provide predict or system_one")` when the agent
//!   provides neither. In the Rust port a caller only has an `impl
//!   PredictRunner`, which *is* the predict seam — the fallback is the
//!   trivial `AgentLike::predict == system_one` identity (the alias lives
//!   in `laya/agent.rs`, mirrored by `Router::system_one` in `laya/router.rs`),
//!   so no second method is plumbed through the
//!   trait. The two upstream tests exercising the fallback
//!   ("system_one/used when predict is absent",
//!   "system_one/criteria length is k") are therefore not ported verbatim;
//!   the tests that exist here cover the same shortlisting behavior through
//!   the one seam the trait exposes.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::decision::question::{json_type_name, render_options, serialize_state};
use crate::error::{Error, Result};
use crate::pycompat::{py_repr_str, py_repr_value, py_str};
use crate::decision::model::PredictRunner;

/// `DEFAULT_SHORTLIST_K`.
pub const DEFAULT_SHORTLIST_K: usize = 20;

/// The `embed_fn(texts) -> (len(texts), dim)` seam, already normalised to
/// plain float rows. Implementations may raise through [`Error`].
///
/// `Send + Sync` so a wrapped embedder can be shared between threads exactly
/// as upstream's `cached_embed_fn` promises ("safe to share between
/// threads").
pub trait EmbedFn: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>>;
}

impl<F> EmbedFn for F
where
    F: Fn(&[String]) -> Result<Vec<Vec<f64>>> + Send + Sync,
{
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        self(texts)
    }
}

/// `shortlist_choice`: return the top-`k` choice labels for `state`.
///
/// `embed_fn` is called once, with the query text first and then one string
/// per option in criteria order. Option strings match `render_options` for a
/// choice question.
///
/// When `k` is at least the number of labels, every label is returned in its
/// original order and `embed_fn` is not called.
///
/// Ties keep the earlier label. A zero vector scores 0 and does not outrank a
/// label that came before it.
pub fn shortlist_choice(
    state: &Value,
    criteria: &Value,
    embed_fn: &dyn EmbedFn,
    k: usize,
    instructions: Option<&str>,
) -> Result<Vec<Value>> {
    let (labels, _scores, _passthrough, _n) = rank(state, criteria, embed_fn, k, instructions)?;
    Ok(labels)
}

/// `predict_shortlist`: shortlist each choice question, then call predict
/// once (upstream: "call ``predict`` or ``system_one`` once" — in this port
/// the predict seam is the only seam, see the module divergences).
///
/// Non-choice questions are forwarded unchanged. A choice whose label count
/// is `<= k` is forwarded unchanged and does not call `embed_fn`. The
/// caller's `questions` map is not mutated.
///
/// The returned value is the model result plus a `shortlist` entry.
/// Probabilities on a shortlisted choice are over the kept labels only.
/// `shortlist[qid]` holds `labels` (rank order), `scores` (cosine, or `null`
/// when nothing was dropped), `k`, `n`, and `passthrough`.
pub fn predict_shortlist(
    runner: &mut impl PredictRunner,
    state: &Value,
    questions: &Map<String, Value>,
    embed_fn: &dyn EmbedFn,
    k: usize,
) -> Result<Value> {
    let checked = check_k(k)?;
    let mut reduced: Map<String, Value> = Map::new();
    let mut meta: Map<String, Value> = Map::new();
    for (qid, qdef) in questions {
        let is_choice = qdef.is_object() && qdef.get("type").and_then(Value::as_str) == Some("choice");
        if !is_choice {
            reduced.insert(qid.clone(), qdef.clone());
            continue;
        }
        let Some(criteria) = qdef.get("criteria") else {
            return Err(Error::Value(format!(
                "question {} is a choice but has no criteria",
                py_repr_str(qid)
            )));
        };
        let (labels, scores, passthrough, n) = rank(
            state,
            criteria,
            embed_fn,
            checked,
            qdef.get("instructions").and_then(Value::as_str),
        )?;
        let mut entry = Map::new();
        entry.insert("labels".to_string(), Value::Array(labels.clone()));
        entry.insert(
            "scores".to_string(),
            match scores {
                Some(scores) => json_f64_list(&scores),
                None => Value::Null,
            },
        );
        entry.insert("k".to_string(), Value::from(checked));
        entry.insert("n".to_string(), Value::from(n));
        entry.insert("passthrough".to_string(), Value::Bool(passthrough));
        meta.insert(qid.clone(), Value::Object(entry));
        if passthrough {
            reduced.insert(qid.clone(), qdef.clone());
            continue;
        }
        let mut updated = qdef.clone();
        updated["criteria"] = subset_criteria(criteria, &labels)?;
        reduced.insert(qid.clone(), updated);
    }

    let result = runner.predict(state, &reduced)?;
    if !result.is_object() {
        return Err(Error::Value(format!(
            "predict/system_one must return a dict, got {}",
            json_type_name(&result)
        )));
    }
    let mut out = result;
    out["shortlist"] = Value::Object(meta);
    Ok(out)
}

/// `cached_embed_fn`: cache `embed_fn` output per input string, under an LRU
/// bound.
///
/// Lookups are exact string matches. Texts missing from the cache are
/// deduplicated and embedded in a single `embed_fn` call, so a cold cache
/// costs the same number of batched calls as the unwrapped function. Nothing
/// is cached when `embed_fn` raises or returns a bad shape.
///
/// The wrapper is safe to share between threads: the lock covers only cache
/// reads and writes, never the embedding call. `cache_info()` reports
/// `size`, `maxsize`, `hits` and `misses`; `cache_clear()` empties it. Clear
/// the cache if the model or weights behind `embed_fn` change.
pub fn cached_embed_fn(
    embed_fn: impl EmbedFn + 'static,
    maxsize: usize,
) -> Result<CachedEmbedFn> {
    if maxsize < 1 {
        // Upstream `if isinstance(maxsize, bool) or not isinstance(maxsize,
        // int) or maxsize < 1: raise ValueError("maxsize must be a positive
        // integer, got %r")`. The typed usize makes the bool/str cases
        // unrepresentable; zero stays reachable.
        return Err(Error::Value(format!(
            "maxsize must be a positive integer, got {}",
            maxsize
        )));
    }
    Ok(CachedEmbedFn {
        embed_fn: Box::new(embed_fn),
        state: Mutex::new(CacheState {
            rows: HashMap::new(),
            order: VecDeque::new(),
            counts: Counts { hits: 0, misses: 0 },
            maxsize,
        }),
    })
}

pub struct CachedEmbedFn {
    embed_fn: Box<dyn EmbedFn>,
    state: Mutex<CacheState>,
}

struct Counts {
    hits: u64,
    misses: u64,
}

struct CacheState {
    rows: HashMap<String, Vec<f64>>,
    order: VecDeque<String>,
    counts: Counts,
    maxsize: usize,
}

impl CachedEmbedFn {
    /// `cached.cache_info()`.
    pub fn cache_info(&self) -> Map<String, Value> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut info = Map::new();
        info.insert("size".to_string(), Value::from(state.rows.len()));
        info.insert("maxsize".to_string(), Value::from(state.maxsize));
        info.insert("hits".to_string(), Value::from(state.counts.hits));
        info.insert("misses".to_string(), Value::from(state.counts.misses));
        info
    }

    /// `cached.cache_clear()`.
    pub fn cache_clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.rows.clear();
        state.order.clear();
        state.counts = Counts { hits: 0, misses: 0 };
    }
}

impl EmbedFn for CachedEmbedFn {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        // `"" if text is None else str(text)`: the Rust surface receives
        // strings already, so the keys are the texts themselves.
        let keys: Vec<String> = texts.to_vec();
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut found: HashMap<String, Vec<f64>> = HashMap::new();
        let missing: Vec<String>;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut hits = 0u64;
            for key in &keys {
                if let Some(row) = state.rows.get(key).cloned() {
                    // move_to_end: touch the entry as most recently used.
                    if let Some(pos) = state.order.iter().position(|k| k == key) {
                        state.order.remove(pos);
                        state.order.push_back(key.clone());
                    }
                    found.insert(key.clone(), row);
                    hits += 1;
                }
            }
            state.counts.hits += hits;
            state.counts.misses += (keys.len() - hits as usize) as u64;
            // deduplicated missing keys, in first-seen order
            let mut seen = Vec::new();
            for key in &keys {
                if !found.contains_key(key) && !seen.contains(key) {
                    seen.push(key.clone());
                }
            }
            missing = seen;
        }
        if !missing.is_empty() {
            let fresh = embeddings(&*self.embed_fn, &missing)?;
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for (key, row) in missing.iter().zip(fresh) {
                state.rows.insert(key.clone(), row.clone());
                if let Some(pos) = state.order.iter().position(|k| k == key) {
                    state.order.remove(pos);
                }
                state.order.push_back(key.clone());
                while state.rows.len() > state.maxsize {
                    if let Some(oldest) = state.order.pop_front() {
                        state.rows.remove(&oldest);
                    }
                }
                found.insert(key.clone(), row.clone());
            }
        }
        Ok(keys
            .iter()
            .map(|key| found.get(key).cloned().expect("every key was filled"))
            .collect())
    }
}

/// `_rank`: shared by `shortlist_choice` and `predict_shortlist`.
/// Returns `(labels, scores, passthrough, n)`.
#[allow(clippy::type_complexity)]
fn rank(
    state: &Value,
    criteria: &Value,
    embed_fn: &dyn EmbedFn,
    k: usize,
    instructions: Option<&str>,
) -> Result<(Vec<Value>, Option<Vec<f64>>, bool, usize)> {
    let checked = check_k(k)?;
    let items = criteria_items(criteria)?;
    let n = items.len();
    let keys: Vec<Value> = items.iter().map(|(key, _)| key.clone()).collect();
    if checked >= n {
        return Ok((keys, None, true, n));
    }
    let query = query_text(state, instructions)?;
    let option_texts = option_texts(&items)?;
    let mut texts = vec![query];
    texts.extend(option_texts);
    let matrix = embeddings(embed_fn, &texts)?;
    let sims = cosine(&matrix[0], &matrix[1..]);
    // `np.argsort(-sims, kind="mergesort")[:checked]`: stable descending
    // order, ties keep the earlier label.
    let mut order: Vec<usize> = (0..sims.len()).collect();
    order.sort_by(|&a, &b| {
        sims[b]
            .partial_cmp(&sims[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order.truncate(checked);
    let labels = order.iter().map(|&i| keys[i].clone()).collect();
    let scores = order.iter().map(|&i| sims[i]).collect();
    Ok((labels, Some(scores), false, n))
}

/// `_check_k`.
fn check_k(k: usize) -> Result<usize> {
    // `k < 1` is the only representable failure of
    // `isinstance(k, bool) or not isinstance(k, int) or k < 1`.
    if k < 1 {
        return Err(Error::Value(format!(
            "k must be a positive integer, got {}",
            k
        )));
    }
    Ok(k)
}

/// `_criteria_items`: `(key, value)` pairs with the upstream validation.
fn criteria_items(criteria: &Value) -> Result<Vec<(Value, Option<&Value>)>> {
    let items: Vec<(Value, Option<&Value>)> = match criteria {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), Some(v)))
            .collect(),
        Value::Array(list) => list.iter().map(|item| (item.clone(), None)).collect(),
        other => {
            return Err(Error::Value(format!(
                "choice criteria must be a dict or list, got {}",
                json_type_name(other)
            )))
        }
    };
    if items.is_empty() {
        return Err(Error::Value(
            "choice criteria must contain at least one option".to_string(),
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    for (key, _value) in &items {
        let repr = py_repr_value(key);
        if seen.contains(&repr) {
            return Err(Error::Value(format!(
                "choice criteria label {} is duplicated",
                repr
            )));
        }
        seen.push(repr);
    }
    Ok(items)
}

/// `_option_texts`: one string per option, matching `render_options`.
fn option_texts(items: &[(Value, Option<&Value>)]) -> Result<Vec<String>> {
    let mut crit = Map::new();
    for (key, value) in items {
        // `_option_texts` builds `crit = {key: value ...}`; a list criterion's
        // key can be any scalar and upstream `render_options` stringifies it,
        // so the key goes through `py_str` here (JSON map keys are always
        // strings, so dict criteria pass through unchanged). Two labels that
        // stringify alike would collapse in the map and trip the `could not
        // render every choice option` guard below — the same guard upstream
        // raises when rendering loses an option.
        let key_str = py_str(key);
        crit.entry(key_str)
            .or_insert_with(|| value.cloned().unwrap_or(Value::Null));
    }
    let rendered = render_options(&serde_json::json!({
        "t": "choice",
        "ins": "",
        "crit": crit,
    }))?;
    // `piece if isinstance(piece, str) else str(piece)`: render_options
    // already returns strings.
    if rendered.len() != items.len() {
        return Err(Error::Value(
            "could not render every choice option".to_string(),
        ));
    }
    Ok(rendered)
}

/// `_query_text`: serialized state, optionally prefixed with the
/// instructions.
fn query_text(state: &Value, instructions: Option<&str>) -> Result<String> {
    let body = serialize_state(state);
    match instructions {
        None | Some("") => Ok(body),
        Some(instructions) => Ok(format!("{}\n{}", instructions, body)),
    }
}

/// `_subset_criteria`.
fn subset_criteria(criteria: &Value, labels: &[Value]) -> Result<Value> {
    match criteria {
        Value::Object(map) => {
            let mut out = Map::new();
            for label in labels {
                let key = label.as_str().expect("dict criteria keys are strings");
                out.insert(key.to_string(), map.get(key).cloned().unwrap_or(Value::Null));
            }
            Ok(Value::Object(out))
        }
        Value::Array(_) => Ok(Value::Array(labels.to_vec())),
        // `criteria_items` already rejected every other shape.
        _ => Ok(Value::Array(labels.to_vec())),
    }
}

/// `_embeddings`: run `embed_fn`, validate the shape, clean non-finite
/// entries.
fn embeddings(embed_fn: &dyn EmbedFn, texts: &[String]) -> Result<Vec<Vec<f64>>> {
    let raw = embed_fn.embed(texts)?;
    let rows = raw.len();
    let cols = raw.first().map(Vec::len).unwrap_or(0);
    if rows != texts.len() || cols < 1 || raw.iter().any(|r| r.len() != cols) {
        return Err(Error::Value(format!(
            "embed_fn must return an array of shape ({}, dim), got {}",
            texts.len(),
            py_shape_repr(rows, cols)
        )));
    }
    // `np.nan_to_num(..., nan=0.0, posinf=0.0, neginf=0.0)`.
    Ok(raw
        .into_iter()
        .map(|row| row.into_iter().map(|v| if v.is_finite() { v } else { 0.0 }).collect())
        .collect())
}

/// Python tuple repr of a 2-D numpy shape: `(3, 4)`, and `(0,)` for the
/// single-dimension empty array `np.asarray([])` produces.
fn py_shape_repr(rows: usize, cols: usize) -> String {
    if rows == 0 && cols == 0 {
        "(0,)".to_string()
    } else {
        format!("({}, {})", rows, cols)
    }
}

/// `_cosine`: cosine similarity of the query against every doc row; zero
/// norms score 0, results clip to [-1, 1].
fn cosine(query: &[f64], docs: &[Vec<f64>]) -> Vec<f64> {
    let qn = norm(query);
    if qn == 0.0 || docs.is_empty() {
        return vec![0.0; docs.len()];
    }
    docs.iter()
        .map(|doc| {
            let dn = norm(doc);
            let denom = dn * qn;
            if denom > 0.0 {
                let dot: f64 = doc.iter().zip(query).map(|(d, q)| d * q).sum();
                (dot / denom).clamp(-1.0, 1.0)
            } else {
                0.0
            }
        })
        .collect()
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// A `Vec<f64>` as a JSON array.
fn json_f64_list(values: &[f64]) -> Value {
    Value::Array(values.iter().map(|v| serde_json::json!(v)).collect())
}

#[cfg(test)]
mod tests;
