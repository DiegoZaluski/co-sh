//! Ported tests: `tests/test_shortlist.py` and `tests/test_shortlist_cosine.py`.
//!
//! Offline: no checkpoint and no Hub download. A fake `embed_fn` supplies
//! vectors; `predict` is a mock (`Recorder`), so the decision model is never
//! constructed.
//!
//! Documented omissions against the upstream file:
//! - the module-introspection checks (`laya.shortlist_choice is …`,
//!   `inspect.getsource(Agent.system_one)` contains no "shortlist") pin
//!   Python module wiring, not behaviour; the Rust module structure makes
//!   them trivially true.
//! - `k`/`maxsize` are typed `usize`, so the "bool / float / str is not an
//!   int" error branches are unrepresentable; `k=0` and `maxsize=0` stay
//!   testable.
//! - `questions` is a typed map, so "questions not a dict" cannot occur.
//! - the torch-tensor branch of `_embeddings` (`detach().float().cpu()…`)
//!   collapses to plain rows: an [`EmbedFn`] already returns floats.
//! - kwargs forwarding (`model="english"`) has no seam in `PredictRunner`;
//!   the Recorder records `(state, questions)` only.
//! - `None` texts normalise to `""` upstream; the Rust surface receives
//!   strings, so only the `""` half of that check is ported.
//! - cache rows are stored as `f64` (upstream `float32`); every compared
//!   value is exactly representable, so the assertions are unchanged.
//! - `embed_fn_from_agent` is not ported (torch), so its test block is out
//!   of scope with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::{
    cached_embed_fn, cosine, predict_shortlist, shortlist_choice, EmbedFn, DEFAULT_SHORTLIST_K,
};
use crate::decision::model::PredictRunner;
use crate::decision::question::{render_options, serialize_state, to_internal};
use crate::error::{Error, Result};

/// A shared handle over a `TableEmbed` so the calls log survives being moved
/// into `cached_embed_fn`.
struct SharedEmbed(Arc<TableEmbed>);

impl EmbedFn for SharedEmbed {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        self.0.embed(texts)
    }
}

// Vectors: query [1, 0]. alpha and delta tie at cosine 1; gamma is 0.6; beta
// is 0. Stable order must keep alpha ahead of delta.
fn vec_table(extra: &[(&str, Vec<f64>)]) -> HashMap<String, Vec<f64>> {
    let mut v: HashMap<String, Vec<f64>> = [
        ("pay me", vec![1.0, 0.0]),
        ("alpha", vec![1.0, 0.0]),
        ("beta", vec![0.0, 1.0]),
        ("gamma", vec![0.6, 0.8]),
        ("delta", vec![1.0, 0.0]),
        ("zero", vec![0.0, 0.0]),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, vec) in extra {
        v.insert((*k).to_string(), vec.clone());
    }
    v
}

fn criteria_value() -> Value {
    json!({"alpha": null, "beta": "", "gamma": "mid", "delta": "same"})
}

// option texts follow render_options: bare key when value is None or ""
fn option_texts_table() -> Vec<(&'static str, Vec<f64>)> {
    vec![
        ("alpha", vec![1.0, 0.0]),
        ("beta", vec![0.0, 1.0]),
        ("gamma: mid", vec![0.6, 0.8]),
        ("delta: same", vec![1.0, 0.0]),
    ]
}

struct TableEmbed {
    vectors: HashMap<String, Vec<f64>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl TableEmbed {
    fn new(vectors: HashMap<String, Vec<f64>>) -> Self {
        Self { vectors, calls: Mutex::new(Vec::new()) }
    }

    fn from_pairs(pairs: &[(&'static str, Vec<f64>)]) -> Self {
        Self::new(pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect())
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EmbedFn for TableEmbed {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(texts.to_vec());
        let missing: Vec<&String> = texts.iter().filter(|t| !self.vectors.contains_key(*t)).collect();
        if !missing.is_empty() {
            panic!("unexpected texts {missing:?}");
        }
        Ok(texts.iter().map(|t| self.vectors[t].clone()).collect())
    }
}

/// `BoomEmbed`: must never run (k >= n pass-through).
struct BoomEmbed;

impl EmbedFn for BoomEmbed {
    fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f64>>> {
        panic!("embed_fn should not run when k >= n")
    }
}

/// Stand-in for Agent: records the questions handed to predict.
struct Recorder {
    calls: Vec<(Value, Map<String, Value>)>,
}

impl Recorder {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }
}

fn answers(questions: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (qid, qdef) in questions {
        let is_choice = qdef.is_object() && qdef.get("type").and_then(Value::as_str) == Some("choice");
        if !is_choice {
            continue;
        }
        let keys: Vec<Value> = match qdef.get("criteria") {
            Some(Value::Object(map)) => map.keys().map(|k| json!(k)).collect(),
            Some(Value::Array(list)) => list.clone(),
            _ => continue,
        };
        out.insert(
            qid.clone(),
            json!({"type": "choice", "choice": keys[0]}),
        );
    }
    json!({ "model": "fake", "answers": out })
}

impl PredictRunner for Recorder {
    fn predict(&mut self, state: &Value, questions: &Map<String, Value>) -> Result<Value> {
        self.calls.push((state.clone(), questions.clone()));
        Ok(answers(questions))
    }
}

fn shortlist(
    state: &Value,
    criteria: &Value,
    embed: &dyn EmbedFn,
    k: usize,
) -> Result<Vec<Value>> {
    shortlist_choice(state, criteria, embed, k, None)
}


fn full_criteria() -> (Value, Value) {
    // sentinel: the value object identity is preserved through the subset
    let sentinel = json!({"desc": "payments"});
    let full = json!({
        "billing": sentinel,
        "tech": "bugs",
        "sales": null,
        "other": "misc",
    });
    (full, sentinel)
}

fn full_vectors() -> Vec<(&'static str, Vec<f64>)> {
    vec![
        ("Which desk?\nI was charged twice", vec![1.0, 0.0]),
        ("billing: {\"desc\": \"payments\"}", vec![0.0, 1.0]),
        ("tech: bugs", vec![1.0, 0.0]),
        ("sales", vec![0.2, 0.2]),
        ("other: misc", vec![0.0, 1.0]),
    ]
}

fn score_question() -> Value {
    json!({"type": "score", "instructions": "How urgent?", "criteria": ["low", "mid", "high", "now"]})
}

fn noul_question() -> Value {
    json!({"type": "noul", "instructions": "Is a refund requested?"})
}

mod cache;
mod cosine;
mod criteria;
mod errors;
mod predict;
mod topk;
