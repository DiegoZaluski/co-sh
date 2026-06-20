//! Tests for error cases: invalid source directory, empty sources.

use super::super::{SkillAction, SkillError, SkillSchema, SkillSource, execute};

#[test]
fn invalid_source_directory_returns_error() {
    let result = execute(&SkillSchema {
        action: SkillAction::List,
        sources: vec![SkillSource::Directory {
            path: "/tmp/nonexistent_cosh_skills_test".into(),
        }],
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        recursive: false,
        ignore: Vec::new(),
        include: Vec::new(),
    });

    assert!(matches!(result, Err(SkillError::InvalidSource(_))));
}

#[test]
fn list_with_no_sources_is_empty() {
    let out = execute(&SkillSchema {
        action: SkillAction::List,
        sources: Vec::new(),
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        recursive: false,
        ignore: Vec::new(),
        include: Vec::new(),
    })
    .expect("list with no sources should succeed");

    match out {
        super::super::SkillOutput::List { skills } => {
            assert!(skills.is_empty());
        }
        _ => panic!("expected List output"),
    }
}
