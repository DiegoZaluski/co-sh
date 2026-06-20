//! Tests for embedded (in-memory) skill sources — no filesystem needed.

use super::super::{EmbeddedSkill, SkillAction, SkillOutput, SkillSchema, SkillSource, execute};

fn embedded_schema(skill: EmbeddedSkill) -> SkillSchema {
    SkillSchema {
        action: SkillAction::List,
        sources: vec![SkillSource::Embedded { skill }],
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        recursive: false,
        ignore: Vec::new(),
        include: Vec::new(),
    }
}

#[test]
fn embedded_source_works_without_filesystem() {
    let out = execute(&embedded_schema(EmbeddedSkill {
        name: "in-memory".into(),
        description: "No filesystem needed".into(),
        content: "---\nname: in-memory\ndescription: From embedded\n---\nEmbedded body".into(),
        globs: Vec::new(),
        always_apply: false,
    }))
    .expect("embedded list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "in-memory");
            assert_eq!(skills[0].description, "From embedded");
        }
        _ => panic!("expected List output"),
    }
}

#[test]
fn embedded_skill_read_returns_body() {
    let out = execute(&SkillSchema {
        action: SkillAction::Read,
        sources: vec![SkillSource::Embedded {
            skill: EmbeddedSkill {
                name: "emb".into(),
                description: "Embedded".into(),
                content: "---\nname: emb\ndescription: Embedded skill\n---\nInline content here"
                    .into(),
                globs: Vec::new(),
                always_apply: false,
            },
        }],
        skill_name: Some("emb".into()),
        ..embedded_schema(EmbeddedSkill {
            name: String::new(),
            description: String::new(),
            content: String::new(),
            globs: Vec::new(),
            always_apply: false,
        })
    })
    .expect("embedded read should succeed");

    match out {
        SkillOutput::Read { skill } => {
            assert_eq!(skill.info.name, "emb");
            assert_eq!(skill.info.description, "Embedded skill");
            assert_eq!(skill.body, "Inline content here");
        }
        _ => panic!("expected Read output"),
    }
}

#[test]
fn embedded_skill_without_frontmatter_uses_provided_fields() {
    let out = execute(&embedded_schema(EmbeddedSkill {
        name: "no-fm".into(),
        description: "Fallback desc".into(),
        content: "Just raw content, no frontmatter".into(),
        globs: vec!["**/*.txt".into()],
        always_apply: true,
    }))
    .expect("embedded list should succeed");

    match out {
        SkillOutput::List { skills } => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "no-fm");
            assert_eq!(skills[0].description, "Fallback desc");
        }
        _ => panic!("expected List output"),
    }
}
