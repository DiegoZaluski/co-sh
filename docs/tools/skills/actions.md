# `skills` actions — what each tool does

All four operations share one flow: `execute` dispatches on the
[`SkillAction`](types.md#skillaction), the skills are
[discovered](discover.md) from the configured sources, filters are applied,
and the matching output is assembled. The
[`Skills`](skills.md#the-skills-wrapper) wrapper methods (`list`, `read`,
`read_asset`, `match_skills`) are thin conveniences over
`execute(&SkillSchema)`.

---

## `List`

Returns a `SkillOutput::List` with the name + description of **every**
discovered skill (after filters):

```rust,ignore
let out = skills.list()?;              // SkillOutput::List
match out {
    SkillOutput::List { skills } => { /* ... */ }
    _ => unreachable!(),
}
```

- Reads every source, so an unreadable directory source is an error here —
  `SkillError::InvalidSource` (`"cannot canonicalize path: …"` /
  `"not a directory: …"` / `"cannot read directory: …"`).
- No skill is ever "not found" in a list — an empty result just means no
  skills matched the sources and filters.

## `Read`

Returns the full content of one named skill — metadata plus the body with
frontmatter stripped:

```rust,ignore
let out = skills.read("my-skill")?;    // SkillOutput::Read
```

- The name is matched **exactly** against discovered skill names (after
  filters — a skill excluded by `ignore`/`include` is not readable).
- **Errors:** `SkillError::NotFound` (`"skill not found: my-skill"`) when no
  discovered skill has that name. (An internal guard also rejects a call
  with no name at all as `InvalidAction` — the wrapper always supplies one.)

## `Read Asset`

Returns the raw content of one asset file inside a skill's directory:

```rust,ignore
let out = skills.read_asset("my-skill", "templates/request.json")?;
// SkillOutput::ReadAsset { content }
```

The asset path is **sandboxed** to the skill directory:

| Requested path | Result |
|---|---|
| A relative path to an existing file inside the skill dir | `ReadAsset { content }` |
| Absolute path (`/etc/passwd`) | `SkillError::PathTraversal` |
| Any `..` component (`../…`) | `SkillError::PathTraversal` |
| Resolves (even via symlink) outside the skill dir | `SkillError::PathTraversal` |
| File does not exist | `SkillError::NotFound` (`"asset not found: …"`) |
| Skill does not exist | `SkillError::NotFound` |
| Embedded skill (no directory) | `SkillError::InvalidAction` (`"read_asset is not supported for embedded skills"`) |

The sandbox works by canonicalizing both the skill directory and the
resolved path and checking containment — which is also what catches symlink
escapes.

## `Match`

Returns the skills that should be **active** for the current workspace —
those whose glob patterns match at least one of the supplied paths, or that
opt out of matching entirely:

```rust,ignore
let out = skills.match_skills(vec!["src/main.rs".into(), "README.md".into()])?;
// SkillOutput::Match { matched }
```

A skill is included when any of these holds:

1. **`always_apply: true`** in its frontmatter — matched unconditionally,
   whatever the paths are.
2. **At least one glob hits** — the skill's `globs` list is matched against
   the paths using the project's standard glob engine (`**` works).
3. **No globs and not `always_apply`** — never matches (a skill must declare
   *something* to be auto-activated).

Behavioral notes:

- An **empty `match_paths`** short-circuits to an empty result without even
  discovering skills — there is nothing to match against.
- Like `List`, the result carries only `SkillInfo` (name + description), not
  bodies — `Match` is for *deciding what to load*, and you `read` the chosen
  skills afterwards.
- Filters (`ignore`/`include`) apply here too: an excluded skill cannot be
  matched.

---

## Error summary

| Situation | Error |
|---|---|
| Source directory missing / unreadable / not a directory | `InvalidSource` |
| Skill name not found (`read` / `read_asset`) | `NotFound` |
| Asset file missing | `NotFound` |
| Asset path escapes the skill dir (absolute, `..`, symlink out) | `PathTraversal` |
| `read_asset` on an embedded skill | `InvalidAction` |
