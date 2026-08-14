# `highlight` — syntax highlighting spans

A small, self-contained highlighter: given source text and an explicit
language name, it returns the byte ranges of tokens worth coloring.

```rust,ignore
pub fn highlight(source: &str, lang: &str) -> Option<Vec<HighlightSpan>>
```

Unlike the rest of the module, `highlight` takes a **language name** (not a
path) and does **not** use the LRU cache — it is a one-shot parse for
rendering snippets (code blocks in markdown, terminal output).

## Language names

`lang` is matched against a fixed set of names. Note the **JS family
collapse**: `javascript`, `js`, `jsx`, `mjs`, `cjs`, `json`,
`typescript`, `ts`, `tsx`, `php`, `lua`, and `dart` all use the JavaScript
grammar (with the JS query). Matching is **exact** — a leading/trailing
space (`" rust"`, `"rust "`) returns `None` (this mirrors how a markdown
renderer passes code-fence languages and rejects stray whitespace).

| Language names | Grammar |
|---|---|
| `rust`, `rs` | Rust |
| `python`, `py` | Python |
| `javascript`, `js`, `jsx`, `mjs`, `cjs`, `json`, `typescript`, `ts`, `tsx`, `php`, `lua`, `dart` | JavaScript |
| `c#`, `csharp`, `cs`, `c`, `h`, `cpp`, `c++`, `cxx`, `hpp`, `objectivec`, `objc`, `m`, `mm` | C# |
| `go`, `golang` | Go |
| `java`, `scala`, `groovy` | Java |
| `haskell`, `hs`, `lhs` | Haskell |
| `swift` | Swift |
| `zig`, `zon` | Zig |
| `kotlin`, `kt`, `kts` | Kotlin |

Unknown names return `None`.

## Categories and queries

The output is a sorted list of `HighlightSpan { start, end, category }`
(byte offsets into `source`):

```rust
pub enum HighlightCategory { Keyword, String, Comment, Type, Function, Number, Builtin }
```

Each language has a hand-written query:

- **Rust, Python, JavaScript** get full queries: keywords (literals and
  structural words), strings, comments, types, numbers, function
  definitions *and* call sites, and builtins (`true`/`false`/`null`/
  `this`/`self`/…).
- **The remaining languages** (C#, Go, Java, Scala, Haskell, Swift, Zig,
  Kotlin, C/C++, Objective-C, …) share a minimal fallback query covering
  just strings, comments, types, and numbers.

Spans are sorted by `start`; overlapping captures are possible (e.g. a
keyword inside a comment) — consumers decide how to resolve them.

## Example

```rust,ignore
let spans = tree_sitter::highlight::highlight("fn main() { let x = 1; }", "rust");
for span in spans.unwrap() {
    let text = &src[span.start..span.end];
    println!("{:?}: {text:?}", span.category);
}
// Keyword: "fn", Keyword: "let", Number: "1", ...
```
