//! The laya model core: everything specific to the laya System 1 decision
//! engine and its published checkpoints.
//!
//! Generic machinery lives outside this module (`pycompat`, `runtime`,
//! `decision`, `hooks`, `lang`, `shortlist`, `hub`); laya consumes it like
//! any future model would. Only this module may name the Hub repos, the
//! pinned revisions or the graph input signatures of the checkpoints.
//!
//! Public surface: [`Router`] (multi-checkpoint routing), [`RouteDecision`],
//! the question `presets` and the option types. The concrete agent
//! (`OnnxAgent`, `load`) is crate-internal — build models through the crate
//! facade.

pub mod agent;
pub mod checkpoints;
pub mod email;
pub mod facade;
pub mod presets;
pub mod router;
