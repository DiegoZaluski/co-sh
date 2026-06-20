//! Tests for the `list` action: count, field selection, directory-name fallback.

use super::super::{SkillOutput, execute};
use super::common::{TempDir, list_schema, make_skills, single_skill};

#[test]
fn list_returns_correct_count() {
    let dir = TempDir::new();
    let path = dir.path();
    make_skills(
        path,
        &[
            ("alpha", "description: Alpha skill\n", "Alpha body"),
            ("beta", "description: Beta skill\n", "Beta body"),
            ("gamma", "description: Gamma skill\n", "Gamma body"),
        ],
    );

    let out = execute(&list_schema(path)).expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 3);
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"alpha"));
            assert!(names.contains(&"beta"));
            assert!(names.contains(&"gamma"));
        }
        _ => panic!("expected List output"),
    }
}

#[test]
fn list_returns_only_name_and_description() {
    let dir = TempDir::new();
    single_skill(
        dir.path(),
        "my-tool",
        "description: A handy tool\n",
        "Secret content",
    );

    let out = execute(&list_schema(dir.path())).expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "my-tool");
            assert_eq!(skills[0].description, "A handy tool");
        }
        _ => panic!("expected List output"),
    }
}

#[test]
fn list_uses_directory_name_when_frontmatter_has_no_name() {
    let dir = TempDir::new();
    let skill_dir = dir.path().join("no-name-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: No name in frontmatter\n---\nBody here",
    )
    .unwrap();

    let out = execute(&list_schema(dir.path())).expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "no-name-skill");
        }
        _ => panic!("expected List output"),
    }
}
