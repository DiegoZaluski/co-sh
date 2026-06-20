//! Tests for recursive vs non-recursive directory discovery.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::super::{SkillAction, SkillOutput, SkillSchema, execute};
use super::common::{TempDir, list_schema};

#[test]
fn non_recursive_does_not_discover_nested_skills() {
    let dir = TempDir::new();
    let path = dir.path();

    let top = path.join("top");
    std::fs::create_dir_all(&top).unwrap();
    std::fs::write(top.join("SKILL.md"), "---\nname: top\n---\ntop body").unwrap();

    let nested = path.join("nested").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("SKILL.md"), "---\nname: deep\n---\ndeep body").unwrap();

    let out = execute(&list_schema(path)).expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"top"), "top-level skill should be found");
            assert!(
                !names.contains(&"deep"),
                "nested skill should NOT be found in non-recursive mode"
            );
        }
        _ => panic!("expected List output"),
    }
}

#[test]
fn recursive_discovery_finds_nested_skills() {
    let dir = TempDir::new();
    let path = dir.path();

    let nested = path.join("team").join("internal");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("SKILL.md"),
        "---\nname: internal-tools\n---\ninternal body",
    )
    .unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::List,
        recursive: true,
        ..list_schema(path)
    })
    .expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert!(
                names.contains(&"internal-tools"),
                "nested skill should be found in recursive mode"
            );
        }
        _ => panic!("expected List output"),
    }
}

#[cfg(unix)]
#[test]
fn recursive_discovery_handles_symlink_loop() {
    let dir = TempDir::new();
    let path = dir.path();

    // Real skill under the root.
    let skill_dir = path.join("my-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "---\nname: my-skill\n---\nbody").unwrap();

    // Symlink loop: skill-a/back -> root (creates a cycle).
    symlink(path, skill_dir.join("back")).unwrap();

    // Must not hang.
    let out = execute(&SkillSchema {
        action: SkillAction::List,
        recursive: true,
        ..list_schema(path)
    })
    .expect("recursive discovery should handle symlink loops");

    match out {
        SkillOutput::List { skills } => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names.len(), 1, "only one real skill should be found");
            assert!(names.contains(&"my-skill"), "real skill must be found");
        }
        _ => panic!("expected List output"),
    }
}
