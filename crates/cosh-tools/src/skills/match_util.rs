//! Glob-matching helpers for the skill tool.

use cosh_sdk::find::compile_glob;

/// Check whether at least one of the glob patterns matches at least one path.
#[must_use]
pub fn globs_match_any(globs: &[String], paths: &[String]) -> bool {
    globs.iter().any(|g| {
        compile_glob(g, true).is_ok_and(|glob_set| paths.iter().any(|p| glob_set.is_match(p)))
    })
}

/// Simple glob-like match for skill name filtering in ignore/include lists.
///
/// Supports `*` wildcard and exact matching. Not a full glob engine — just
/// enough for human-readable name patterns.
#[must_use]
pub fn name_matches_glob(pattern: &str, name: &str) -> bool {
    if pattern == name || pattern == "*" {
        return true;
    }
    if pattern.contains('*') {
        let re_pattern = format!("^{}$", pattern.replace('.', "\\.").replace('*', ".*"));
        if let Ok(re) = regex::Regex::new(&re_pattern) {
            return re.is_match(name);
        }
    }
    false
}
