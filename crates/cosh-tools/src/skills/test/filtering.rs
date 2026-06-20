//! Tests for `ignore` and `include` schema filters.

use super::super::{SkillAction, SkillOutput, SkillSchema, execute};
use super::common::{TempDir, list_schema, make_skills};

#[test]
fn ignore_excludes_matching_skills() {
    let dir = TempDir::new();
    let path = dir.path();
    make_skills(
        path,
        &[
            ("keep", "description: Keep me\n", ""),
            ("skip", "description: Skip me\n", ""),
        ],
    );

    let out = execute(&SkillSchema {
        action: SkillAction::List,
        ignore: vec!["skip".into()],
        ..list_schema(path)
    })
    .expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "keep");
        }
        _ => panic!("expected List output"),
    }
}

#[test]
fn include_restricts_to_named_skills() {
    let dir = TempDir::new();
    let path = dir.path();
    make_skills(
        path,
        &[
            ("a", "description: A\n", ""),
            ("b", "description: B\n", ""),
            ("c", "description: C\n", ""),
        ],
    );

    let out = execute(&SkillSchema {
        action: SkillAction::List,
        include: vec!["a".into(), "c".into()],
        ..list_schema(path)
    })
    .expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 2);
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"a"));
            assert!(names.contains(&"c"));
            assert!(!names.contains(&"b"));
        }
        _ => panic!("expected List output"),
    }
}
