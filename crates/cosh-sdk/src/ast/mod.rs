//! AST-aware structural search and rewrite powered by ast-grep.
//!
//! Vendored from oh-my-pi's `pi-ast`/`pi-natives` (ast-grep based), stripped
//! of the N-API/JS surface. Exposes the matching engine behind the AST edit
//! tool of `cosh-tools`:
//!
//! - [`lang`] — the [`SupportLang`] registry: 60+ tree-sitter grammars, alias
//!   resolution, and extension inference;
//! - [`parse`] — the tree-sitter grammar parsers behind every language;
//! - [`ops`] — pattern compilation, structural matching, rewriting, and edit
//!   application (non-overlapping, dedupe, overlap-rejection).
//!
//! The type [`SupportLang`] implements `ast-grep-core`'s [`Language`] trait,
//! so any helper that accepts a `SupportLang` can compile patterns and run
//! them against source via `ast_grep_core::Language::ast_grep`.

pub mod lang;
pub mod ops;
pub mod parse;

pub use lang::SupportLang;
pub use ops::{
    AstError, AstMatch, AstMatchStrictness, CompiledRewrite, MatchedFile, apply_edits,
    collect_matched_files, collect_matches, compile_pattern, compile_rewrite_rules,
    compile_search_patterns, has_glob_syntax, is_supported_file, resolve_language,
    resolve_strictness, resolve_supported_lang, rewrite_source, supported_lang_list,
};
