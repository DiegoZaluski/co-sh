# `ast::parse` — grammar parsers

The simplest module in the crate: one function per language, each returning
the tree-sitter grammar as an ast-grep `TSLanguage`.

```rust,ignore
pub fn language_rust() -> TSLanguage;
pub fn language_python() -> TSLanguage;
pub fn language_javascript() -> TSLanguage;
// … one per supported language
```

Every `SupportLang` variant in [`lang`](lang.md) dispatches to exactly one
of these functions (via the `LanguageExt` impl), so you almost never call
them directly. They exist as the single place where each grammar crate is
mapped into ast-grep's `TSLanguage` type — `tree_sitter_rust::LANGUAGE.into()`,
`tree_sitter_python::LANGUAGE.into()`, and so on (with a few special cases:
Dockerfile uses `tree_sitter_dockerfile::language()`, PHP the
`LANGUAGE_PHP_ONLY` variant, TypeScript/TSX their respective
`LANGUAGE_TYPESCRIPT`/`LANGUAGE_TSX`, OCaml `LANGUAGE_OCAML`, XML
`LANGUAGE_XML`).

The full list mirrors the `SupportLang` enum: Astro, Bash, C, Cmake, Cpp,
CSharp, Dart, Clojure, Css, Diff, Dockerfile, EmacsLisp, Elixir, Erlang,
Fortran, Go, Graphql, Haskell, Hcl, Html, Ini, Java, JavaScript, Json,
Just, Julia, Kotlin, Lua, Make, Markdown, Nix, ObjC, Ocaml, Odin, Php,
Powershell, Proto, Python, R, Regex, Ruby, Rust, Scala, Solidity, Sql,
Starlark, Svelte, Swift, Toml, Tsx, TypeScript, Tlaplus, Verilog, Vue, Xml,
Yaml, Zig.

To operate on a raw `TSLanguage` yourself (outside the `SupportLang`
registry), these functions are the direct access point.

---

## Summary

- `ast::parse` provides one function per language that returns the tree-sitter grammar as an ast-grep `TSLanguage`.
- Every `SupportLang` variant dispatches to exactly one of these functions via the `LanguageExt` implementation.
- These functions are the single mapping point from grammar crates to ast-grep's `TSLanguage` type.
- Most use the standard `LANGUAGE.into()` pattern; special cases exist for Dockerfile, PHP, TypeScript/TSX, OCaml, and XML.
- Direct use is rare — callers typically use `SupportLang` which dispatches automatically — but these functions exist for direct `TSLanguage` access when needed.
