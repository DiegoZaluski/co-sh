//! Tests for the `match` action: glob hits, always-apply, empty paths.

use super::super::{SkillAction, SkillOutput, SkillSchema, execute};
use super::common::{TempDir, list_schema, make_skills, single_skill};

#[test]
fn match_returns_skill_whose_globs_hit() {
    let dir = TempDir::new();
    let path = dir.path();
    make_skills(
        path,
        &[
            (
                "rust",
                "description: Rust help\nglobs:\n  - \"**/*.rs\"\n",
                "rust body",
            ),
            (
                "python",
                "description: Python help\nglobs:\n  - \"**/*.py\"\n",
                "python body",
            ),
        ],
    );

    let out = execute(&SkillSchema {
        action: SkillAction::Match,
        match_paths: vec!["src/main.rs".into(), "README.md".into()],
        ..list_schema(path)
    })
    .expect("match should succeed");

    match out {
        SkillOutput::Match { matched } => {
            assert_eq!(matched.len(), 1);
            assert_eq!(matched[0].name, "rust");
        }
        _ => panic!("expected Match output"),
    }
}

#[test]
fn match_returns_always_apply_skills_even_without_glob_hit() {
    let dir = TempDir::new();
    let path = dir.path();
    make_skills(
        path,
        &[
            (
                "always-on",
                "description: Always applied\nalwaysApply: true\n",
                "always body",
            ),
            (
                "conditional",
                "description: Conditional\nglobs:\n  - \"**/*.rs\"\n",
                "cond body",
            ),
        ],
    );

    let out = execute(&SkillSchema {
        action: SkillAction::Match,
        match_paths: vec!["readme.txt".into()],
        ..list_schema(path)
    })
    .expect("match should succeed");

    match out {
        SkillOutput::Match { matched } => {
            let names: Vec<&str> = matched.iter().map(|s| s.name.as_str()).collect();
            assert!(
                names.contains(&"always-on"),
                "always-on should match unconditionally"
            );
            assert!(
                !names.contains(&"conditional"),
                "conditional should not match .txt files"
            );
        }
        _ => panic!("expected Match output"),
    }
}

#[test]
fn match_returns_only_name_and_description_not_body() {
    let dir = TempDir::new();
    single_skill(
        dir.path(),
        "secret",
        "description: Secret stuff\nglobs:\n  - \"**/*.rs\"\n",
        "TOP SECRET",
    );

    let out = execute(&SkillSchema {
        action: SkillAction::Match,
        match_paths: vec!["a.rs".into()],
        ..list_schema(dir.path())
    })
    .expect("match should succeed");

    match out {
        SkillOutput::Match { matched } => {
            assert_eq!(matched.len(), 1);
            assert_eq!(matched[0].name, "secret");
            assert_eq!(matched[0].description, "Secret stuff");
        }
        _ => panic!("expected Match output"),
    }
}

#[test]
fn match_with_empty_paths_returns_empty() {
    let dir = TempDir::new();
    single_skill(
        dir.path(),
        "any",
        "description: Any\nglobs:\n  - \"*\"\n",
        "body",
    );

    let out = execute(&SkillSchema {
        action: SkillAction::Match,
        match_paths: Vec::new(),
        ..list_schema(dir.path())
    })
    .expect("match should succeed");

    match out {
        SkillOutput::Match { matched } => {
            assert!(matched.is_empty());
        }
        _ => panic!("expected Match output"),
    }
}
