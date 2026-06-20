//! Shared helpers for skill tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::{SkillAction, SkillSchema, SkillSource};

// Temp directory (no external dependency)

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let pid = std::process::id();
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/cosh_skills_test_{pid}_{n}"));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// Test fixtures

/// Create skills in a parent directory.
///
/// Each entry is `(name, frontmatter_overrides, body)`.
/// `frontmatter_overrides` is appended between the `---` markers (after
/// `name: {name}\n`).
pub fn make_skills(parent: &Path, skills: &[(&str, &str, &str)]) {
    for (name, frontmatter, body) in skills {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        let content = format!("---\nname: {name}\n{frontmatter}---\n{body}");
        fs::write(dir.join("SKILL.md"), &content).unwrap();
    }
}

/// Create a single-skill directory.
pub fn single_skill(parent: &Path, name: &str, frontmatter: &str, body: &str) {
    let dir = parent.join(name);
    fs::create_dir_all(&dir).unwrap();
    let content = format!("---\nname: {name}\n{frontmatter}---\n{body}");
    fs::write(dir.join("SKILL.md"), &content).unwrap();
}

/// Build a minimal List schema that targets a directory source.
pub fn list_schema(path: &Path) -> SkillSchema {
    SkillSchema {
        action: SkillAction::List,
        sources: vec![SkillSource::Directory {
            path: path.to_string_lossy().into_owned(),
        }],
        skill_name: None,
        asset_path: None,
        match_paths: Vec::new(),
        recursive: false,
        ignore: Vec::new(),
        include: Vec::new(),
    }
}
