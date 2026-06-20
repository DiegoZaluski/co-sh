//! Tests for the `read` action: frontmatter stripping, missing skills, missing name.

use super::super::{SkillAction, SkillError, SkillOutput, SkillSchema, execute};
use super::common::{TempDir, list_schema, single_skill};

#[test]
fn read_strips_frontmatter() {
    let dir = TempDir::new();
    single_skill(
        dir.path(),
        "sql",
        "description: SQL helper\n",
        "SELECT * FROM users;",
    );

    let out = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: Some("sql".into()),
        ..list_schema(dir.path())
    })
    .expect("read should succeed");

    match out {
        SkillOutput::Read { skill } => {
            assert_eq!(skill.info.name, "sql");
            assert_eq!(skill.info.description, "SQL helper");
            assert_eq!(skill.body, "SELECT * FROM users;");
        }
        _ => panic!("expected Read output"),
    }
}

#[test]
fn read_returns_error_for_nonexistent_skill() {
    let dir = TempDir::new();
    single_skill(dir.path(), "alpha", "", "body");

    let result = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: Some("nonexistent".into()),
        ..list_schema(dir.path())
    });

    assert!(matches!(result, Err(SkillError::NotFound(_))));
}

#[test]
fn read_requires_skill_name() {
    let dir = TempDir::new();

    let result = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: None,
        ..list_schema(dir.path())
    });

    assert!(matches!(result, Err(SkillError::InvalidAction(_))));
}
