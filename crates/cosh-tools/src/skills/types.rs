//! All public and internal types for the skill reader tool.

use std::path::PathBuf;

use serde::Serialize;

// Public types

/// Lightweight metadata returned in list / match results and system prompts.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// Full skill content returned by the `read` action.
#[derive(Debug, Clone, Serialize)]
pub struct SkillContent {
    pub info: SkillInfo,
    /// SKILL.md body with frontmatter stripped.
    pub body: String,
}

/// Where to look for skills.
#[derive(Debug, Clone)]
pub enum SkillSource {
    /// A directory on the local filesystem that contains skill subdirectories.
    Directory { path: String },
    /// An in-memory skill provided directly by the caller.
    Embedded { skill: EmbeddedSkill },
}

/// An inline skill that does not require filesystem access.
#[derive(Debug, Clone)]
pub struct EmbeddedSkill {
    pub name: String,
    pub description: String,
    /// Full SKILL.md content, including frontmatter and body.
    pub content: String,
    pub globs: Vec<String>,
    pub always_apply: bool,
}

/// Actions the tool can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAction {
    List,
    Read,
    ReadAsset,
    Match,
}

/// The schema passed by the caller — carries all policy.
///
/// Zero hardcoded paths, names, or policies. Everything is supplied here.
#[derive(Debug, Clone)]
pub struct SkillSchema {
    pub action: SkillAction,
    /// Ordered list of sources. First source wins on name collision.
    pub sources: Vec<SkillSource>,
    /// Target skill name for `read` / `read_asset`.
    pub skill_name: Option<String>,
    /// Relative sub-path inside a skill directory for `read_asset`.
    pub asset_path: Option<String>,
    /// Active file paths to match skill globs against.
    pub match_paths: Vec<String>,
    /// Whether to scan directories recursively.
    pub recursive: bool,
    /// Glob-style skill name exclusions.
    pub ignore: Vec<String>,
    /// Allowlist — when non-empty, only skills whose name appears here are
    /// returned. Empty means all skills pass.
    pub include: Vec<String>,
}

/// Output of a skill tool invocation.
#[derive(Debug, Clone, Serialize)]
pub enum SkillOutput {
    List { skills: Vec<SkillInfo> },
    Read { skill: SkillContent },
    ReadAsset { content: String },
    Match { matched: Vec<SkillInfo> },
}

/// Error type for skill operations.
#[derive(Debug)]
pub enum SkillError {
    NotFound(String),
    PathTraversal(String),
    InvalidAction(String),
    InvalidSource(String),
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillError::NotFound(msg) => write!(f, "skill not found: {msg}"),
            SkillError::PathTraversal(msg) => write!(f, "path traversal detected: {msg}"),
            SkillError::InvalidAction(msg) => write!(f, "invalid action: {msg}"),
            SkillError::InvalidSource(msg) => write!(f, "invalid source: {msg}"),
        }
    }
}

impl std::error::Error for SkillError {}

// Internal types (shared across sub-modules)

/// Parsed frontmatter metadata from a SKILL.md file.
#[derive(Debug, Default)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub globs: Vec<String>,
    pub always_apply: bool,
}

/// A discovered skill before dedup and output conversion.
#[derive(Debug, Clone)]
pub struct RawSkill {
    pub source_key: String,
    pub name: String,
    pub description: String,
    /// Directory containing SKILL.md (base for asset resolution).
    pub base_dir: PathBuf,
    /// Body with frontmatter stripped.
    pub body: String,
    pub globs: Vec<String>,
    pub always_apply: bool,
}
