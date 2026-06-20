//! Schema-driven skill discovery, loading, and serving.
//!
//! Discovers `SKILL.md` capability packs from caller-supplied sources
//! (directories or embedded data) and exposes them via a typed schema.

pub mod actions;
pub mod discover;
pub mod frontmatter;
pub mod match_util;
pub mod types;

#[cfg(test)]
mod test;

pub use actions::execute;
pub use types::{
    EmbeddedSkill, SkillAction, SkillContent, SkillError, SkillInfo, SkillOutput, SkillSchema,
    SkillSource,
};
