//! The laya implementation of the crate facade: [`load`] builds a
//! [`DecisionModel`] for a [`ModelKind`].
//!
//! [`ModelKind`] names a checkpoint the way the Router does (`english`,
//! `multilingual`, `typed-decisions`); this module resolves those names to
//! the laya hub repos and subfolders — the only place outside `checkpoints`
//! that maps a kind to laya artifacts. Revisions stay opt-in, exactly as
//! upstream: an explicit `revision` in the options wins, otherwise the Hub
//! default is used so existing offline caches keep working; the reviewed
//! SHAs live in [`crate::laya::checkpoints`] for callers that pin.

use crate::decision::model::{DecisionModel, LoadOptions, ModelKind};
use crate::error::Result;
use crate::laya::agent::{self, OnnxAgent};
use crate::laya::router::BUNDLE_REPO;
use serde_json::{Map, Value};
use std::sync::Arc;

/// `laya.load`: build one checkpoint into a shared [`DecisionModel`].
///
/// ```ignore
/// let model = cosh_onnx::load(ModelKind::Multilingual, &LoadOptions::default())?;
/// let prediction = model.decide(&state, &questions, None, None, None)?;
/// ```
///
/// A named [`ModelKind`] addresses the bundled repo (`convaiinnovations/laya`
/// with the matching subfolder; the English root has none). A
/// [`ModelKind::Custom`] passes a raw repo id, a bundled `(repo, subfolder)`
/// pair, or a local checkpoint directory straight to the loader. In every
/// case the ONNX graph is `onnx_path` (default `"laya.onnx"`, the name the
/// upstream export script writes), resolved against the checkpoint
/// directory.
///
/// The returned handle is cheap to clone and shares one model: concurrent
/// [`DecisionModel::decide`] calls serialise on the single forward pass,
/// exactly as they do through the Router.
pub fn load(kind: ModelKind, options: &LoadOptions) -> Result<Arc<dyn DecisionModel>> {
    let (repo, subfolder) = resolve(kind);
    let model_id = match &subfolder {
        Some(sub) => format!("{repo}/{sub}"),
        None => repo.clone(),
    };
    let onnx_path = options
        .onnx_path
        .clone()
        .unwrap_or_else(|| "laya.onnx".to_string());
    let agent = agent::load_agent(
        &repo,
        &onnx_path,
        subfolder.as_deref(),
        options.revision.as_deref(),
        options.expected_sha256.as_ref(),
        options.lang_temperatures.as_ref(),
        options.hooks.clone(),
        options.on_predict_start.clone(),
        options.on_predict_end.clone(),
        options.hooks_raise.unwrap_or(true),
        options.hooks_concurrent.unwrap_or(true),
        options.hooks_timeout,
        options.intra_op_threads,
    )?;
    Ok(Arc::new(OnnxModel {
        agent: Arc::new(agent),
        model_id,
    }))
}

/// Resolve a kind to `(repo, subfolder)` for the loader: the named kinds map
/// to the bundle repo (the same specs the Router's default model table
/// carries), `Custom` passes through.
fn resolve(kind: ModelKind) -> (String, Option<String>) {
    match kind {
        ModelKind::English => (BUNDLE_REPO.to_string(), None),
        ModelKind::Multilingual => (BUNDLE_REPO.to_string(), Some("multilingual".to_string())),
        ModelKind::TypedDecisions => (BUNDLE_REPO.to_string(), Some("typed-decisions".to_string())),
        ModelKind::Custom { repo, subfolder } => (repo, subfolder),
    }
}

/// The ONNX agent behind the facade handle: the shared-agent handle the
/// Router uses, exposed through the [`DecisionModel`] surface. An `Arc`
/// clone shares one agent with no outer lock — a prediction reads immutable
/// state and drives interior-mutable seams, so `decide` calls run
/// concurrently (the forward pass itself serialises inside `OrtSession`).
struct OnnxModel {
    agent: Arc<OnnxAgent>,
    model_id: String,
}

impl DecisionModel for OnnxModel {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn revision(&self) -> Option<String> {
        self.agent.revision.clone()
    }

    fn decide(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value> {
        self.agent.system_one(
            state,
            questions,
            lang,
            max_len,
            head_max_len,
            &crate::hooks::PerCall::default(),
        )
    }
}
