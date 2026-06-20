//! Tests for the `read_asset` action: sandbox enforcement, valid reads, embedded rejection.

use super::super::{
    EmbeddedSkill, SkillAction, SkillError, SkillOutput, SkillSchema, SkillSource, execute,
};
use super::common::{TempDir, list_schema, single_skill};

#[test]
fn read_asset_path_traversal_rejected() {
    let dir = TempDir::new();
    single_skill(dir.path(), "safe", "description: Safe skill\n", "body");

    let result = execute(&SkillSchema {
        action: SkillAction::ReadAsset,
        asset_path: Some("../../../etc/passwd".into()),
        skill_name: Some("safe".into()),
        ..list_schema(dir.path())
    });

    assert!(
        matches!(result, Err(SkillError::PathTraversal(_))),
        "expected PathTraversal error, got: {result:?}"
    );
}

#[test]
fn read_asset_absolute_path_rejected() {
    let dir = TempDir::new();
    single_skill(dir.path(), "safe", "description: Safe skill\n", "body");

    let result = execute(&SkillSchema {
        action: SkillAction::ReadAsset,
        asset_path: Some("/etc/passwd".into()),
        skill_name: Some("safe".into()),
        ..list_schema(dir.path())
    });

    assert!(matches!(result, Err(SkillError::PathTraversal(_))));
}

#[test]
fn read_asset_valid_file_succeeds() {
    let dir = TempDir::new();
    let path = dir.path();
    single_skill(path, "docs", "description: Docs\n", "main body");

    std::fs::write(path.join("docs").join("README.md"), "Hello from README").unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::ReadAsset,
        asset_path: Some("README.md".into()),
        skill_name: Some("docs".into()),
        ..list_schema(path)
    })
    .expect("read_asset should succeed");

    match out {
        SkillOutput::ReadAsset { content } => {
            assert_eq!(content, "Hello from README");
        }
        _ => panic!("expected ReadAsset output"),
    }
}

#[test]
fn read_asset_nonexistent_file_returns_not_found() {
    let dir = TempDir::new();
    single_skill(dir.path(), "docs", "description: Docs\n", "body");

    let result = execute(&SkillSchema {
        action: SkillAction::ReadAsset,
        asset_path: Some("missing.txt".into()),
        skill_name: Some("docs".into()),
        ..list_schema(dir.path())
    });

    assert!(matches!(result, Err(SkillError::NotFound(_))));
}

#[test]
fn read_asset_embedded_skill_fails() {
    let result = execute(&SkillSchema {
        action: SkillAction::ReadAsset,
        sources: vec![SkillSource::Embedded {
            skill: EmbeddedSkill {
                name: "embedded".into(),
                description: "Embedded".into(),
                content: "---\nname: embedded\n---\nbody".into(),
                globs: Vec::new(),
                always_apply: false,
            },
        }],
        skill_name: Some("embedded".into()),
        asset_path: Some("file.txt".into()),
        ..SkillSchema {
            action: SkillAction::ReadAsset,
            sources: Vec::new(),
            skill_name: None,
            asset_path: None,
            match_paths: Vec::new(),
            recursive: false,
            ignore: Vec::new(),
            include: Vec::new(),
        }
    });

    assert!(matches!(result, Err(SkillError::InvalidAction(_))));
}
