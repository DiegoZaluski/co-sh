//! Tests for deduplication: first-source-wins on name collision.

use super::super::{SkillAction, SkillOutput, SkillSchema, SkillSource, execute};
use super::common::{TempDir, single_skill};

#[test]
fn dedup_first_source_wins() {
    let dir_a = TempDir::new();
    let dir_b = TempDir::new();

    single_skill(
        dir_a.path(),
        "dup",
        "description: First source\n",
        "first body",
    );
    single_skill(
        dir_b.path(),
        "dup",
        "description: Second source\n",
        "second body",
    );

    let out = execute(&SkillSchema {
        action: SkillAction::List,
        sources: vec![
            SkillSource::Directory {
                path: dir_a.path().to_string_lossy().into_owned(),
            },
            SkillSource::Directory {
                path: dir_b.path().to_string_lossy().into_owned(),
            },
        ],
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        recursive: false,
        ignore: Vec::new(),
        include: Vec::new(),
    })
    .expect("list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1, "only one copy of 'dup' should appear");
            assert_eq!(skills[0].description, "First source");
        }
        _ => panic!("expected List output"),
    }
}
