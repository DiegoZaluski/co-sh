//! Schema-driven skill discovery, loading, and serving.
//!
//! Discovers `SKILL.md` capability packs from caller-supplied sources
//! (directories or embedded data) and exposes them via a typed schema.
//!
//! [`Skills`] wraps the schema and action dispatch in a single builder —
//! configure sources once and call the desired operation.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::skills::{Skills, SkillSource};
//!
//! let s = Skills::new()
//!     .recursive(true)
//!     .sources(vec![SkillSource::Directory { path: "/skills".into() }]);
//!
//! s.list();
//! s.read("my-skill");
//! ```

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
    SkillSource, SkillsMatchInput, SkillsReadAssetInput, SkillsReadInput,
};

use crate::ToolDescription;

/// Shared-state wrapper for skill tool operations.
///
/// Holds the discovery configuration (sources, recursion, filters) and
/// exposes each [`SkillAction`] variant as a dedicated method.
pub struct Skills {
    sources: Vec<SkillSource>,
    recursive: bool,
    ignore: Vec<String>,
    include: Vec<String>,

    /// MCP Tool description for `list`.
    pub description_list: ToolDescription,
    /// MCP Tool description for `read`.
    pub description_read: ToolDescription,
    /// MCP Tool description for `read_asset`.
    pub description_read_asset: ToolDescription,
    /// MCP Tool description for `match_skills`.
    pub description_match_skills: ToolDescription,
}

impl Default for Skills {
    fn default() -> Self {
        Self::new()
    }
}

impl Skills {
    /// Create a new `Skills` with no sources and all filters empty.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            recursive: false,
            ignore: Vec::new(),
            include: Vec::new(),
            description_list: serde_json::json!({
                "name": "skills_list",
                "description": concat!(
                    "List all available skills from the configured sources. ",
                    "Returns skill names and descriptions for every skill found ",
                    "across all sources, respecting ignore/include filters and ",
                    "recursion settings."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }),
            description_read: serde_json::json!({
                "name": "skills_read",
                "description": concat!(
                    "Read the full content of a named skill. Retrieves the ",
                    "skill's metadata (name, description) and body content from ",
                    "the first source that contains it (first-source-wins on ",
                    "name collision)."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Name of the skill to read"
                        }
                    },
                    "required": ["name"]
                }
            }),
            description_read_asset: serde_json::json!({
                "name": "skills_read_asset",
                "description": concat!(
                    "Read an asset file from a named skill's directory. ",
                    "Asset paths must be relative and must not traverse outside ",
                    "the skill directory (path traversal is blocked). Returns ",
                    "the raw asset content."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Name of the skill that owns the asset"
                        },
                        "asset_path": {
                            "type": "string",
                            "description": "Relative path to the asset file within the skill directory"
                        }
                    },
                    "required": ["name", "asset_path"]
                }
            }),
            description_match_skills: serde_json::json!({
                "name": "skills_match_skills",
                "description": concat!(
                    "Match skills against a set of active file paths. Skills ",
                    "whose glob patterns match at least one path, or that have ",
                    "`always_apply` set to true, are returned. Useful for ",
                    "auto-activating relevant skills based on the current ",
                    "workspace context."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "match_paths": {
                            "type": "array",
                            "description": "File paths to match against skill glob patterns",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["match_paths"]
                }
            }),
        }
    }

    /// Set the ordered list of skill sources (first source wins on name collision).
    #[must_use]
    pub fn sources(mut self, sources: Vec<SkillSource>) -> Self {
        self.sources = sources;
        self
    }

    /// Whether to scan directories recursively for skills.
    #[must_use]
    pub fn recursive(mut self, v: bool) -> Self {
        self.recursive = v;
        self
    }

    /// Glob-style skill name exclusions.
    #[must_use]
    pub fn ignore(mut self, patterns: Vec<String>) -> Self {
        self.ignore = patterns;
        self
    }

    /// Allowlist — when non-empty, only skills whose name appears here are returned.
    #[must_use]
    pub fn include(mut self, names: Vec<String>) -> Self {
        self.include = names;
        self
    }

    /// Build a [`SkillSchema`] from the shared state.
    fn schema(&self, action: SkillAction) -> SkillSchema {
        SkillSchema {
            action,
            sources: self.sources.clone(),
            skill_name: None,
            asset_path: None,
            match_paths: Vec::new(),
            recursive: self.recursive,
            ignore: self.ignore.clone(),
            include: self.include.clone(),
        }
    }

    /// List all available skills.
    ///
    /// # Errors
    ///
    /// Returns `SkillError` if a source cannot be read.
    pub fn list(&self) -> Result<SkillOutput, SkillError> {
        execute(&self.schema(SkillAction::List))
    }

    /// Read the full content of a named skill.
    ///
    /// # Errors
    ///
    /// Returns `SkillError::NotFound` if the skill does not exist in any source.
    pub fn read(&self, name: impl Into<String>) -> Result<SkillOutput, SkillError> {
        let mut schema = self.schema(SkillAction::Read);
        schema.skill_name = Some(name.into());
        execute(&schema)
    }

    /// Read an asset file from a named skill's directory.
    ///
    /// # Errors
    ///
    /// Returns `SkillError::NotFound` if the skill or asset does not exist,
    /// or `SkillError::PathTraversal` if the path escapes the skill directory.
    pub fn read_asset(
        &self,
        name: impl Into<String>,
        asset_path: impl Into<String>,
    ) -> Result<SkillOutput, SkillError> {
        let mut schema = self.schema(SkillAction::ReadAsset);
        schema.skill_name = Some(name.into());
        schema.asset_path = Some(asset_path.into());
        execute(&schema)
    }

    /// Match skills against a set of active file paths.
    ///
    /// Skills whose glob patterns match at least one path (or that have
    /// `always_apply` set) are returned.
    ///
    /// # Errors
    ///
    /// Returns `SkillError` if a source cannot be read.
    pub fn match_skills(&self, match_paths: Vec<String>) -> Result<SkillOutput, SkillError> {
        let mut schema = self.schema(SkillAction::Match);
        schema.match_paths = match_paths;
        execute(&schema)
    }
}
