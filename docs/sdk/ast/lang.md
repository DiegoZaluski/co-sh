# `ast::lang` — the `SupportLang` registry

`SupportLang` is a `Copy` enum of every grammar the module can operate on —
60+ entries spanning the tree-sitter grammars the crate links. Each variant
implements ast-grep's [`Language`] trait, so any helper that accepts a
`SupportLang` can compile patterns and run them against source.

## Getting a language

```rust,ignore
pub fn from_alias(value: &str) -> Option<SupportLang>   // case-insensitive, trimmed
pub fn from_path(path: &Path) -> Option<SupportLang>    // extension / filename inference
pub fn all_langs() -> &'static [SupportLang]
pub fn sorted_aliases() -> &'static [&'static str]      // for error messages
pub fn canonical_name(self) -> &'static str             // stable lowercase key
```

- **Aliases** are generous: `"py"`, `"python"`, `"c++"`, `"cxx"`,
  `"objective-c"`, `"systemverilog"`, `"bazel"` (→ Starlark), `"terraform"`
  (→ HCL), `"c#"` (→ CSharp), `"docker"` (→ Dockerfile), … The full map is
  in `lang.rs` (`LANG_ALIASES`).
- **Extension inference** covers the standard extension sets plus special
  filenames: `Makefile`/`makefile`/`GNUmakefile` → Make, `Justfile` →
  Just, `CMakeLists.txt` → CMake, `Dockerfile*`/`Containerfile` →
  Dockerfile, `.emacs` → EmacsLisp, and the extensionless shell rc files
  (`.zshrc`, `bashrc`, `profile`, `kshrc`, …) → Bash.

## Grammar variants and metavariable handling

Not every grammar accepts `$NAME` as a valid identifier, so the module
classifies each language into one of three implementations:

1. **Stub languages** — the grammar accepts `$` natively, so patterns need
   no preprocessing: `Astro`, `Bash`, `Clojure`, `Dart`, `Diff`,
   `EmacsLisp`, `Graphql`, `Java`, `JavaScript`, `Json`, `Lua`, `Markdown`,
   `Regex`, `Scala`, `Solidity`, `Svelte`, `Toml`, `Tsx`, `TypeScript`,
   `Vue`, `Xml`, `Yaml`.
2. **Expando languages** — the grammar rejects `$` as an identifier, so the
   module rewrites patterns with an *expando character* (a rarely-used
   Unicode char) and translates the results back. Most use `µ` (`Rust`,
   `Go`, `Swift`, `Kotlin`, `Python`, `Ruby`, `Php`, `Haskell`, `Erlang`,
   `Sql`, `Elixir`, `Dockerfile`, `Hcl`, `Ini`, `Just`, `Ocaml`,
   `Powershell`, `Proto`, `R`, `Make`, `Starlark`, `Odin`, `Julia`,
   `Verilog`, `Tlaplus`, `Zig`, `CSharp`, `Cmake`), a few use `𐀀` (`C`,
   `Cpp`, `Fortran`, `ObjC`), and `Css` and `Nix` use `_`. A detail you
   never need to know as a user, since the module hides it.
3. **Html** — a custom implementation with **injection support**: `<script>`
   and `<style>` contents are extracted and matched with their own
   languages (`js`/`ts`/`tsx`/`css`/`scss`/`less`/`stylus`/`coffee`).

This is invisible from the outside: `SupportLang::Rust` and
`SupportLang::JavaScript` both just work with `$MSG` patterns.

## The language list

`Astro, Bash, C, Cmake, Cpp, CSharp, Dart, Clojure, Css, Diff, Dockerfile,
EmacsLisp, Elixir, Erlang, Fortran, Go, Graphql, Haskell, Hcl, Html, Ini,
Java, JavaScript, Json, Just, Julia, Kotlin, Lua, Make, Markdown, Nix,
ObjC, Ocaml, Odin, Php, Powershell, Proto, Python, R, Regex, Ruby, Rust,
Scala, Solidity, Sql, Starlark, Svelte, Swift, Toml, Tlaplus, Tsx,
TypeScript, Verilog, Vue, Xml, Yaml, Zig`.

The per-language `Language` implementations live here; the parser functions
they dispatch to live in [`parse`](parse.md).
