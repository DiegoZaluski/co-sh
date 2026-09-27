//! `laya/presets.py`: ready-to-use question presets for common production
//! decision workflows.
//!
//! A pure data module: five functions, each returning a questions dict in the
//! public shape (`type` / `instructions` / `criteria`). Values are
//! [`serde_json::Value`]s with insertion-ordered maps (the workspace enables
//! serde_json `preserve_order`), so key order matches Python dict order
//! exactly — the order `Agent._to_internal` and `render_options` read.
//!
//! `email_questions` lives in [`crate::laya::email`] now: it is defined once
//! there and `laya::presets` re-exports it, so both import paths keep
//! answering the same questions.

pub mod guard;
pub mod moderation;
pub mod router;
pub mod triage;

use serde_json::{Map, Value};

/// The public questions dict shape: question id -> definition.
pub type Questions = Map<String, Value>;

pub(crate) fn q(
    qtype: &str,
    instructions: &str,
    criteria: Option<Value>,
) -> Value {
    let mut def = Map::new();
    def.insert("type".to_string(), Value::String(qtype.to_string()));
    def.insert(
        "instructions".to_string(),
        Value::String(instructions.to_string()),
    );
    if let Some(criteria) = criteria {
        def.insert("criteria".to_string(), criteria);
    }
    Value::Object(def)
}

/// `email_questions` is defined in [`crate::laya::email`] (the questions are
/// the email workflow's, so they travel with the email domain) and
/// re-exported here so the upstream import path keeps working.
pub use crate::laya::email::email_questions;
pub use crate::laya::presets::guard::guard_questions;
pub use crate::laya::presets::moderation::moderation_questions;
pub use crate::laya::presets::router::router_questions;
pub use crate::laya::presets::triage::triage_questions;
