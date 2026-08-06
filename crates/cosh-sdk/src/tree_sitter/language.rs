//! Map file paths to tree-sitter [`Language`] objects.
//!
//! The extension set mirrors the languages registered in the ast-grep
//! integration (`crate::ast`). It is kept independent so the low-level parse
//! cache (`tree_sitter`) does not depend on the structural-matching engine.
use tree_sitter::Language;

#[must_use]
pub fn detect_language(path: &str) -> Option<Language> {
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if file_name.eq_ignore_ascii_case("dockerfile") {
        return Some(tree_sitter_dockerfile::language());
    }

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    Some(match ext {
        // Legacy direct mappings (JS, C#) retained verbatim.
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),

        // Shared with the ast-grep registry.
        "astro" => tree_sitter_astro::LANGUAGE.into(),
        "bash" | "bats" | "cgi" | "command" | "env" | "fcgi" | "ksh" | "sh" | "tmux" | "tool"
        | "zsh" => tree_sitter_bash::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cmake" => tree_sitter_cmake::LANGUAGE.into(),
        "cc" | "hpp" | "cpp" | "c++" | "hh" | "cxx" | "cu" | "ino" => {
            tree_sitter_cpp::LANGUAGE.into()
        }
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "clj" | "cljs" | "cljc" | "edn" => tree_sitter_clojure::LANGUAGE.into(),
        "css" | "scss" => tree_sitter_css::LANGUAGE.into(),
        "diff" | "patch" => tree_sitter_diff::LANGUAGE.into(),
        "el" => tree_sitter_elisp::LANGUAGE.into(),
        "ex" | "exs" => tree_sitter_elixir::LANGUAGE.into(),
        "erl" | "hrl" => tree_sitter_erlang::LANGUAGE.into(),
        "f90" | "F90" | "f95" | "F95" | "f03" | "F03" | "f08" | "F08" => {
            tree_sitter_fortran::LANGUAGE.into()
        }
        "go" => tree_sitter_go::LANGUAGE.into(),
        "graphql" | "gql" => tree_sitter_graphql::LANGUAGE.into(),
        "hs" | "lhs" => tree_sitter_haskell::LANGUAGE.into(),
        "hcl" | "tf" | "tfvars" => tree_sitter_hcl::LANGUAGE.into(),
        "html" | "htm" | "xhtml" => tree_sitter_html::LANGUAGE.into(),
        "ini" | "cfg" | "conf" | "properties" => tree_sitter_ini::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "jl" => tree_sitter_julia::LANGUAGE.into(),
        "kt" | "ktm" | "kts" => tree_sitter_kotlin::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "mk" | "mak" => tree_sitter_make::LANGUAGE.into(),
        "md" | "markdown" | "mdx" => tree_sitter_md::LANGUAGE.into(),
        "nix" => tree_sitter_nix::LANGUAGE.into(),
        "m" => tree_sitter_objc::LANGUAGE.into(),
        "ml" => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        "odin" => tree_sitter_odin::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP_ONLY.into(),
        "ps1" | "psm1" => tree_sitter_powershell::LANGUAGE.into(),
        "proto" => tree_sitter_proto::LANGUAGE.into(),
        "py" | "py3" | "pyi" | "bzl" => tree_sitter_python::LANGUAGE.into(),
        "r" => tree_sitter_r::LANGUAGE.into(),
        "rb" | "rbw" | "gemspec" => tree_sitter_ruby::LANGUAGE.into(),
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "scala" | "sc" | "sbt" => tree_sitter_scala::LANGUAGE.into(),
        "sol" => tree_sitter_solidity::LANGUAGE.into(),
        "sql" => tree_sitter_sql::LANGUAGE.into(),
        "star" => tree_sitter_starlark::LANGUAGE.into(),
        "svelte" => tree_sitter_svelte::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "tla" => tree_sitter_tlaplus::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "verilog" => tree_sitter_verilog::LANGUAGE.into(),
        "vue" => tree_sitter_vue::LANGUAGE.into(),
        "xml" => tree_sitter_xml::LANGUAGE_XML.into(),
        "yaml" | "yml" => tree_sitter_yaml::LANGUAGE.into(),
        "zig" | "zon" => tree_sitter_zig::LANGUAGE.into(),
        _ => return None,
    })
}
