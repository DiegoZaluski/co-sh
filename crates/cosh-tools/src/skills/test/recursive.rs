//! Tests for recursive vs non-recursive directory discovery.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::super::{SkillAction, SkillOutput, SkillSchema, SkillSource, execute};
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

// Canonicalize + starts_with check now rejects symlinks pointing outside
// the intended base directory.

#[cfg(unix)]
#[test]
fn discover_gap_symlink_outside_base() {
    let real_base = TempDir::new();
    let trap_base = TempDir::new();

    // Put a skill inside trap_base
    let real_skill = trap_base.path().join("secret-skill");
    fs::create_dir_all(&real_skill).unwrap();
    fs::write(
        real_skill.join("SKILL.md"),
        "---\nname: secret-skill\n---\nhidden content",
    )
    .unwrap();

    // Symlink inside real_base that points to trap_base
    let symlink_dir = real_base.path().join("poisoned-link");
    symlink(trap_base.path(), &symlink_dir).unwrap();

    let out = execute(&SkillSchema {
        action: SkillAction::List,
        recursive: true,
        sources: vec![SkillSource::Directory {
            path: real_base.path().to_string_lossy().into_owned(),
        }],
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        ignore: Vec::new(),
        include: Vec::new(),
    })
    .expect("discovery should succeed");

    match out {
        SkillOutput::List { skills } => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert!(
                !names.contains(&"secret-skill"),
                "canonicalize must reject symlink traversal: found secret-skill: {names:?}"
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
