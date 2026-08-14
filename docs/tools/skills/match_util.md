# `match_util` — glob matching helpers

Two small matching helpers power the rest of the module. They are public so
the sibling modules can share them, but they are **engine internals** — the
[`Skills`](skills.md#the-skills-wrapper) wrapper's `match_skills` is the
user-facing API that uses them.

```rust,ignore
pub fn globs_match_any(globs: &[String], paths: &[String]) -> bool
pub fn name_matches_glob(pattern: &str, name: &str) -> bool
```

## `globs_match_any` — path globs (for `match_skills`)

Returns `true` when **any** glob in `globs` matches **any** path in `paths`.
This is the matcher behind the `Match` action:

- Globs are compiled with the project's standard glob engine (`cosh_sdk`'s
  `compile_glob`), so patterns like `**/*.rs` and `src/**` behave exactly as
  they do in the [`find`](../find/find.md) tools.
- A glob that fails to compile is skipped (`Ok`-only matching); a list that
  is empty or contains no compiling pattern matches nothing.

This is why a skill declares `globs: ["**/*.sql"]` and then matches any
workspace containing a `.sql` file — one call to `globs_match_any` decides it.

## `name_matches_glob` — name patterns (for `ignore`/`include`)

A deliberately **simple** matcher for skill *names*, used by the discovery
filters. It is not a full glob engine — just enough for human-readable name
patterns:

| Pattern | Matches |
|---|---|
| `git` | exactly `git` |
| `*` | anything |
| `deprecated-*` | any name starting with `deprecated-` (`.` is matched literally) |
| `a*b` | names with `a`, anything, `b` |

Exact match first, then a `*`-based regex fallback. Anything without a `*`
behaves as an exact-name test.

---

## When to use which

- **Paths on disk** (workspace files vs. skill globs) → `globs_match_any`.
- **Skill names** (filtering `ignore`/`include` lists) → `name_matches_glob`.

The distinction is intentional: path globs need the full engine (`**`,
character classes, brace expansion); name filters only ever need `*`
wildcards, and keeping them simple makes the filter lists predictable.
