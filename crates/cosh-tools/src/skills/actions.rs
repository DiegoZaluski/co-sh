//! Action implementations for the skill reader tool.
//!
//! Each variant of [`SkillAction`](super::types::SkillAction) has a dedicated
//! handler that calls into the discovery layer and assembles the output.

use std::fs;

use crate::skills::discover::discover_skills;
use crate::skills::match_util::globs_match_any;
use crate::skills::types::{
    SkillAction, SkillContent, SkillError, SkillInfo, SkillOutput, SkillSchema,
};
use crate::util::path_guard::validate_asset_path;

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

    let resolved = match validate_asset_path(&raw.base_dir, asset_path) {
        Ok(p) => p,
        Err(msg) if msg.starts_with("asset not found") => {
            return Err(SkillError::NotFound(msg));
        }
        Err(msg) => {
            return Err(SkillError::PathTraversal(msg));
        }
    };

    let content = fs::read_to_string(&resolved)
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
