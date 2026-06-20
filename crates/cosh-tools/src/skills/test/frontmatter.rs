//! Tests for frontmatter edge cases: block scalars, missing delimiters, no frontmatter.

use super::super::{SkillAction, SkillOutput, SkillSchema, execute};
use super::common::{TempDir, list_schema};

#[test]
fn frontmatter_with_block_scalar_description() {
    let dir = TempDir::new();
    let path = dir.path();
    let skill_dir = path.join("pg");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: postgres\ndescription: |\n  Know-how for working with\n  PostgreSQL schemas and queries.\nglobs:\n  - \"**/*.sql\"\n---\nActual skill body",
    )
    .unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: Some("postgres".into()),
        ..list_schema(path)
    })
    .expect("read should succeed");

    match out {
        SkillOutput::Read { skill } => {
            assert_eq!(skill.info.name, "postgres");
            assert_eq!(
                skill.info.description,
                "Know-how for working with\nPostgreSQL schemas and queries."
            );
            assert_eq!(skill.body, "Actual skill body");
        }
        _ => panic!("expected Read output"),
    }
}

#[test]
fn frontmatter_without_closing_delimiter_returns_empty_body() {
    let dir = TempDir::new();
    let path = dir.path();
    let skill_dir = path.join("broken");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: broken\ndescription: No closing\n",
    )
    .unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: Some("broken".into()),
        ..list_schema(path)
    })
    .expect("read should succeed");

    match out {
        SkillOutput::Read { skill } => {
            assert_eq!(skill.info.name, "broken");
            assert_eq!(skill.info.description, "No closing");
            assert!(skill.body.is_empty());
        }
        _ => panic!("expected Read output"),
    }
}

#[test]
fn no_frontmatter_returns_full_content_as_body() {
    let dir = TempDir::new();
    let path = dir.path();
    let skill_dir = path.join("raw");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "Just raw markdown content\nNo frontmatter here.",
    )
    .unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::Read,
        skill_name: Some("raw".into()),
        ..list_schema(path)
    })
    .expect("read should succeed");

    match out {
        SkillOutput::Read { skill } => {
            assert_eq!(skill.info.name, "raw");
            assert_eq!(skill.info.description, "");
            assert_eq!(
                skill.body,
                "Just raw markdown content\nNo frontmatter here."
            );
        }
        _ => panic!("expected Read output"),
    }
}
