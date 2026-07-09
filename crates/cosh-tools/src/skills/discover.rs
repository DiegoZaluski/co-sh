//! Skill discovery — scans directories or embedded sources for SKILL.md files
//! and caches results so repeated calls don't re-scan the filesystem.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::skills::frontmatter::parse_frontmatter;
use crate::skills::types::{EmbeddedSkill, RawSkill, SkillError, SkillSchema, SkillSource};

// Cache

/// Per-source discovery cache keyed by a source identifier.
static DISCOVERY_CACHE: LazyLock<Mutex<HashMap<String, Vec<RawSkill>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_key_for_source(source: &SkillSource) -> String {
    match source {
        SkillSource::Directory { path } => Path::new(path).canonicalize().map_or_else(
            |_| format!("dir:{path}"),
            |canon| format!("dir:{}", canon.display()),
        ),
        SkillSource::Embedded { skill } => format!("embedded:{}", skill.name),
    }
}

// Public API

/// Discover all skills from the schema's sources, applying ignore/include
/// filters and dedup (first source wins).
///
/// # Errors
/// Returns `SkillError::InvalidSource` when a directory source does not exist
/// or cannot be read.
#[allow(clippy::significant_drop_tightening)]
pub fn discover_skills(schema: &SkillSchema) -> Result<Vec<RawSkill>, SkillError> {
    let mut all: Vec<RawSkill> = Vec::new();
    let mut seen_names: HashMap<String, usize> = HashMap::new();
    let mut cache = DISCOVERY_CACHE
        .lock()
        .map_err(|e| SkillError::InvalidSource(format!("cache lock error: {e}")))?;

    for source in &schema.sources {
        let key = cache_key_for_source(source);
        let raw_skills: Vec<RawSkill> = if let Some(cached) = cache.get(&key) {
            cached.clone()
        } else {
            let discovered = match source {
                SkillSource::Directory { path } => discover_from_directory(path, schema.recursive)?,
                SkillSource::Embedded { skill } => {
                    vec![raw_skill_from_embedded(skill, &key)]
                }
            };
            cache.insert(key.clone(), discovered.clone());
            discovered
        };

        for raw in raw_skills {
            if schema
                .ignore
                .iter()
                .any(|pattern| super::match_util::name_matches_glob(pattern, &raw.name))
            {
                continue;
            }
            if !schema.include.is_empty() && !schema.include.contains(&raw.name) {
                continue;
            }
            if let Some(&idx) = seen_names.get(&raw.name) {
                log::warn!(
                    "duplicate skill name '{name}' from source '{key}' \
                     — keeping first occurrence from '{first_key}'",
                    name = raw.name,
                    first_key = all[idx].source_key,
                );
                continue;
            }
            seen_names.insert(raw.name.clone(), all.len());
            all.push(raw);
        }
    }

    Ok(all)
}

// Directory discovery

fn discover_from_directory(path: &str, recursive: bool) -> Result<Vec<RawSkill>, SkillError> {
    let root = Path::new(path);
    let root = root
        .canonicalize()
        .map_err(|e| SkillError::InvalidSource(format!("cannot canonicalize path: {e}")))?;
    if !root.is_dir() {
        return Err(SkillError::InvalidSource(format!(
            "not a directory: {path}"
        )));
    }

    let source_key = format!("dir:{}", root.display());

    let mut skill_dirs: Vec<PathBuf> = Vec::new();

    if recursive {
        collect_skill_dirs_recursive(&root, &mut skill_dirs)?;
    } else {
        let entries = fs::read_dir(&root)
            .map_err(|e| SkillError::InvalidSource(format!("cannot read directory: {e}")))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| SkillError::InvalidSource(format!("read_dir error: {e}")))?;
            let path = entry.path();
            let Ok(canon_entry) = path.canonicalize() else {
                continue;
            };
            if !canon_entry.starts_with(&root) {
                continue;
            }
            if canon_entry.is_dir() {
                let skill_path = canon_entry.join("SKILL.md");
                if skill_path.is_file() {
                    skill_dirs.push(canon_entry);
                }
            }
        }
    }

    let mut skills = Vec::new();
    for skill_dir in &skill_dirs {
        if let Some(raw) = load_skill_md(skill_dir, &source_key) {
            skills.push(raw);
        }
    }

    Ok(skills)
}

fn collect_skill_dirs_recursive(root: &Path, result: &mut Vec<PathBuf>) -> Result<(), SkillError> {
    let root = root.to_path_buf();
    let mut stack: Vec<PathBuf> = vec![root.clone()];
    let mut visited: HashSet<PathBuf> = HashSet::new();

    while let Some(current) = stack.pop() {
        let canon = current
            .canonicalize()
            .map_err(|e| SkillError::InvalidSource(format!("cannot canonicalize path: {e}")))?;
        if !visited.insert(canon.clone()) {
            continue;
        }
        let entries = fs::read_dir(&canon)
            .map_err(|e| SkillError::InvalidSource(format!("cannot read directory: {e}")))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| SkillError::InvalidSource(format!("read_dir error: {e}")))?;
            let path = entry.path();
            let Ok(canon_entry) = path.canonicalize() else {
                continue;
            };
            if !canon_entry.starts_with(&root) {
                continue;
            }
            if canon_entry.is_dir() {
                if canon_entry.join("SKILL.md").is_file() {
                    result.push(canon_entry.clone());
                }
                stack.push(canon_entry);
            }
        }
    }

    Ok(())
}

fn load_skill_md(skill_dir: &Path, source_key: &str) -> Option<RawSkill> {
    let content = fs::read_to_string(skill_dir.join("SKILL.md")).ok()?;

    let (fm, body) = parse_frontmatter(&content);
    let name = fm.name.clone().unwrap_or_else(|| {
        skill_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    Some(RawSkill {
        source_key: source_key.to_owned(),
        name,
        description: fm.description.unwrap_or_default(),
        base_dir: skill_dir.to_path_buf(),
        body,
        globs: fm.globs,
        always_apply: fm.always_apply,
    })
}

// Embedded source

fn raw_skill_from_embedded(skill: &EmbeddedSkill, source_key: &str) -> RawSkill {
    let (fm, body) = parse_frontmatter(&skill.content);
    let name = fm.name.unwrap_or_else(|| skill.name.clone());
    let description = fm
        .description
        .clone()
        .unwrap_or_else(|| skill.description.clone());
    let globs = if fm.globs.is_empty() {
        skill.globs.clone()
    } else {
        fm.globs
    };
    let always_apply = fm.always_apply || skill.always_apply;

    RawSkill {
        source_key: source_key.to_owned(),
        name,
        description,
        base_dir: PathBuf::new(),
        body: if body.is_empty() {
            skill.content.clone()
        } else {
            body
        },
        globs,
        always_apply,
    }
}
