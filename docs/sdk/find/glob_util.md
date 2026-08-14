# `find::glob_util` — glob pattern helpers

Shared pattern utilities used by both [`glob`](glob.md) and [`grep`](grep.md)
(when `grep.glob` is set). If you only call `glob` / `grep`, you never need
this module directly — but it is public because it is genuinely useful on its
own for building patterns.

```rust,ignore
pub fn build_glob_pattern(glob: &str, recursive: bool) -> String
pub fn compile_glob(glob: &str, recursive: bool) -> Result<GlobSet, String>
pub fn try_compile_glob(glob: Option<&str>, recursive: bool) -> Result<Option<GlobSet>, String>
```

## `build_glob_pattern` — the normalization rules

Turns a raw pattern string into a compiled-ready one:

1. **Backslashes become forward slashes** — `"src\\**\\*.ts"` → `"src/**/*.ts"`
   (Windows-style input works everywhere).
2. **Recursive prefix** — when `recursive` is true and the pattern has no
   `/`, does not start with `**`, and is not an *exact brace union*, it gets
   `"**/"` prepended: `"*.ts"` → `"**/*.ts"`. An exact brace union like
   `"{alpha.txt,beta.txt}"` (braces only, no wildcards) is treated as a list
   of literal filenames and left alone.
3. **Unclosed braces are closed** — `"*.{ts,tsx"` → `"*.{ts,tsx}"`, and nested
   cases like `"{a,{b,c}"` → `"{a,{b,c}}"`. This is a deliberate tolerance:
   LLMs occasionally emit patterns without the closing `}`.

## `compile_glob` — pattern → matcher

Compiles a single pattern string into a `globset::GlobSet` using
`build_glob_pattern` first, with `literal_separator(true)` — meaning `*`
never crosses a `/` boundary. Errors are prefixed `Invalid glob pattern: ...`
or `Failed to build glob matcher: ...`.

## `try_compile_glob` — the optional variant

Same as `compile_glob`, but takes `Option<&str>` and returns `Ok(None)` when
the input is `None`, empty, or whitespace-only — exactly the shape grep needs
for its optional `glob` filter.

---

Next: [grep — searching by content](grep.md).
