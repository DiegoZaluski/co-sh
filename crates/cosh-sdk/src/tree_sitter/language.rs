//! Map file paths to tree-sitter [`Language`] objects.
use tree_sitter::Language;

#[must_use]
pub fn detect_language(path: &str) -> Option<Language> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    Some(match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "hs" | "lhs" => tree_sitter_haskell::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "zig" | "zon" => tree_sitter_zig::LANGUAGE.into(),
        "kt" | "kts" => tree_sitter_kotlin::LANGUAGE.into(),
        _ => return None,
    })
}
