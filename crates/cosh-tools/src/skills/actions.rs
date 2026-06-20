//! Action implementations for the skill reader tool.
//!
//! Each variant of [`SkillAction`](super::types::SkillAction) has a dedicated
//! handler that calls into the discovery layer and assembles the output.

use std::fs;
use std::path::{Component, Path};

use crate::skills::discover::discover_skills;
use crate::skills::match_util::globs_match_any;
use crate::skills::types::{
    SkillAction, SkillContent, SkillError, SkillInfo, SkillOutput, SkillSchema,
};

/// Execute a skill tool action.
///
/// # Errors
/// Returns `SkillError` on any failure (missing skill, path traversal, etc.).
pub fn execute(schema: &SkillSchema) -> Result<SkillOutput, SkillError> {
    match schema.action {
        SkillAction::List => action_list(schema),
        SkillAction::Read => action_read(schema),
        SkillAction::ReadAsset => action_read_asset(schema),
        SkillAction::Match => action_match(schema),
    }
}

fn action_list(schema: &SkillSchema) -> Result<SkillOutput, SkillError> {
    let skills = discover_skills(schema)?;
    let infos: Vec<SkillInfo> = skills
        .into_iter()
        .map(|s| SkillInfo {
            name: s.name,
            description: s.description,
        })
        .collect();
    Ok(SkillOutput::List { skills: infos })
}

fn action_read(schema: &SkillSchema) -> Result<SkillOutput, SkillError> {
    let name = schema
        .skill_name
        .as_deref()
        .ok_or_else(|| SkillError::InvalidAction("read requires a skill_name".into()))?;

    let skills = discover_skills(schema)?;
    let raw = skills
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| SkillError::NotFound(name.into()))?;

    Ok(SkillOutput::Read {
        skill: SkillContent {
            info: SkillInfo {
                name: raw.name,
                description: raw.description,
            },
            body: raw.body,
        },
    })
}

fn action_read_asset(schema: &SkillSchema) -> Result<SkillOutput, SkillError> {
    let name = schema
        .skill_name
        .as_deref()
        .ok_or_else(|| SkillError::InvalidAction("read_asset requires a skill_name".into()))?;
    let asset_path = schema
        .asset_path
        .as_deref()
        .ok_or_else(|| SkillError::InvalidAction("read_asset requires an asset_path".into()))?;

    let skills = discover_skills(schema)?;
    let raw = skills
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| SkillError::NotFound(name.into()))?;

    if raw.base_dir.as_os_str().is_empty() {
        return Err(SkillError::InvalidAction(
            "read_asset is not supported for embedded skills".into(),
        ));
    }

    let requested = Path::new(asset_path);
    if requested.is_absolute() {
        return Err(SkillError::PathTraversal(
            "absolute paths are not allowed".into(),
        ));
    }
    if requested.components().any(|c| c == Component::ParentDir) {
        return Err(SkillError::PathTraversal(
            "path must not contain '..' components".into(),
        ));
    }

    let base_canon = raw
        .base_dir
        .canonicalize()
        .map_err(|e| SkillError::PathTraversal(format!("cannot canonicalize base dir: {e}")))?;
    let resolved = base_canon.join(asset_path);
    let resolved_canon = resolved
        .canonicalize()
        .map_err(|e| SkillError::NotFound(format!("asset not found: {e}")))?;

    if !resolved_canon.starts_with(&base_canon) {
        return Err(SkillError::PathTraversal(
            "resolved path escapes the skill directory".into(),
        ));
    }

    let content = fs::read_to_string(&resolved_canon)
        .map_err(|e| SkillError::NotFound(format!("cannot read asset: {e}")))?;

    Ok(SkillOutput::ReadAsset { content })
}

fn action_match(schema: &SkillSchema) -> Result<SkillOutput, SkillError> {
    if schema.match_paths.is_empty() {
        return Ok(SkillOutput::Match {
            matched: Vec::new(),
        });
    }

    let skills = discover_skills(schema)?;

    let matched: Vec<SkillInfo> = skills
        .into_iter()
        .filter(|s| {
            if s.always_apply {
                return true;
            }
            if s.globs.is_empty() {
                return false;
            }
            globs_match_any(&s.globs, &schema.match_paths)
        })
        .map(|s| SkillInfo {
            name: s.name,
            description: s.description,
        })
        .collect();

    Ok(SkillOutput::Match { matched })
}
