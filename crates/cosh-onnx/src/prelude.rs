//! The one-import surface: `use cosh_onnx::prelude::*;`
//!
//! Everything a caller of the laya core regularly names, and nothing a
//! backend implementor needs. Deeper imports stay available: the modules are
//! all `pub`, and this prelude is a convenience, not a boundary.

pub use crate::decision::confidence::{answer_confidence, confidence_from_probs, ece_score};
pub use crate::decision::model::{AgentLike, DecisionModel, LoadOptions, ModelKind, Prediction};
pub use crate::decision::question::{QTYPES, render_options, to_internal};
pub use crate::decision::schema::{DecisionResult, decide};
pub use crate::error::{Error, Result};
pub use crate::hooks::{Hook, PredictContext, PredictHook};
pub use crate::lang::{analyse as detect_language, detect_script, is_english};
pub use crate::laya::email::{clean_email_body, email_state};
pub use crate::laya::facade::load;
pub use crate::laya::presets::{
    email_questions, guard_questions, moderation_questions, router_questions, triage_questions,
};
pub use crate::laya::router::{ModelSpec, RouteDecision, RouteOptions, Router, RouterOptions};
pub use crate::shortlist::{EmbedFn, cached_embed_fn, predict_shortlist, shortlist_choice};
