# `language` — mapping paths to grammars

`detect_language(path)` maps a file path to the tree-sitter
[`Language`] for its extension:

```rust,ignore
pub fn detect_language(path: &str) -> Option<Language>
```

Returns `None` for unknown extensions (callers treat that as "unsupported
language" and skip parsing). Detection is case-insensitive on the filename
for `Dockerfile`, and extension-based for everything else.

## Supported extensions

| Grammar | Extensions |
|---|---|
| JavaScript | `js`, `jsx`, `mjs`, `cjs` |
| C# | `cs` |
| Astro | `astro` |
| Bash | `bash`, `bats`, `cgi`, `command`, `env`, `fcgi`, `ksh`, `sh`, `tmux`, `tool`, `zsh` |
| C | `c`, `h` |
| CMake | `cmake` |
| C++ | `cc`, `hpp`, `cpp`, `c++`, `hh`, `cxx`, `cu`, `ino` |
| Dart | `dart` |
| Clojure | `clj`, `cljs`, `cljc`, `edn` |
| CSS | `css`, `scss` |
| Diff | `diff`, `patch` |
| Elisp | `el` |
| Elixir | `ex`, `exs` |
| Erlang | `erl`, `hrl` |
| Fortran | `f90`, `F90`, `f95`, `F95`, `f03`, `F03`, `f08`, `F08` |
| Go | `go` |
| GraphQL | `graphql`, `gql` |
| Haskell | `hs`, `lhs` |
| HCL | `hcl`, `tf`, `tfvars` |
| HTML | `html`, `htm`, `xhtml` |
| INI | `ini`, `cfg`, `conf`, `properties` |
| Java | `java` |
| JSON | `json` |
| Julia | `jl` |
| Kotlin | `kt`, `ktm`, `kts` |
| Lua | `lua` |
| Make | `mk`, `mak` |
| Markdown | `md`, `markdown`, `mdx` |
| Nix | `nix` |
| Objective-C | `m` |
| OCaml | `ml` |
| Odin | `odin` |
| PHP | `php` |
| PowerShell | `ps1`, `psm1` |
| Protobuf | `proto` |
| Python | `py`, `py3`, `pyi`, `bzl` |
| R | `r` |
| Ruby | `rb`, `rbw`, `gemspec` |
| Rust | `rs` |
| Scala | `scala`, `sc`, `sbt` |
| Solidity | `sol` |
| SQL | `sql` |
| Starlark | `star` |
| Svelte | `svelte` |
| Swift | `swift` |
| TOML | `toml` |
| TLA+ | `tla` |
| TypeScript | `ts` (TS), `tsx` (TSX) |
| Verilog | `verilog` |
| Vue | `vue` |
| XML | `xml` |
| YAML | `yaml`, `yml` |
| Zig | `zig`, `zon` |

Plus the special case: a file literally named `Dockerfile` (any case) maps
to the Dockerfile grammar.

The extension set deliberately mirrors the ast-grep registry in
[`crate::ast`](../ast/ast.md) — kept independent so the low-level parse
cache does not depend on the structural-matching engine.
