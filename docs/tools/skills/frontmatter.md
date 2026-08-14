# `frontmatter` — the `SKILL.md` format

A skill's `SKILL.md` starts with a small YAML-like frontmatter block between
`---` delimiters, followed by the skill body. The module ships its own
purpose-built parser for this format — deliberately a subset of YAML, enough
for the fields the skill standard needs:

```markdown
---
name: git
description: Work effectively with git
globs:
  - "**/*.rs"
alwaysApply: true
---
# Git skill

The body — the actual instructions the agent follows.
```

The four recognized fields:

| Field | Meaning | Accepted forms |
|---|---|---|
| `name` | The skill's name (addressed by all tools). Falls back to the directory name. | `name: git` or `name: "git"` |
| `description` | One-line summary shown in `list`/`match` results. | `description: …` or a `\|` block scalar |
| `globs` | Glob patterns that activate the skill in `match_skills`. | an indented list: `globs:` + `- "…"` items |
| `alwaysApply` / `always_apply` | When `true`, the skill always matches regardless of globs. | `alwaysApply: true` (anything else = false) |

Everything after the closing `---` is the **body**. `read` returns the body
with the frontmatter stripped; `list`/`match` never include bodies at all.

---

## Parsing rules

The parser is line-based and forgiving, and only understands the subset
above:

- **CRLF is normalized to LF** first, so Windows-written skill files parse
  correctly.
- **No opening `---`:** the whole content is treated as body, with default
  (empty) frontmatter. The skill still exists — its name falls back to the
  directory name and its description is empty.
- **No closing `---`:** the frontmatter fields up to the end of the file are
  parsed, and the body is **empty**. (A malformed file gives you metadata but
  no instructions.)
- **Unrecognized fields are ignored**, as are comments (`# …`) and blank
  lines inside the frontmatter.
- **Quoted values** have their surrounding quotes stripped.
- **`description: |` block scalars** collect the following indented lines,
  dedented to their common indentation and joined with newlines — useful for
  multi-sentence descriptions.

A few formats are *not* supported: inline lists (`globs: ["a", "b"]`),
multi-line quoted scalars, and YAML anchors. If a field does not parse, it is
simply left unset.

---

## Where the fields land

For a directory skill, the parsed frontmatter is merged with the directory:

- `name` → the skill name (else: directory name)
- `description` → `SkillInfo::description` (else: empty)
- `globs` → used by `match_skills`
- `always_apply` → matched unconditionally by `match_skills`

For an [embedded skill](discover.md#embedded-sources), the content's
frontmatter takes precedence over the `EmbeddedSkill` struct fields, and
`globs`/`always_apply` from the content are used when present.

---

## Example

```markdown
---
name: postgres
description: |
  Know-how for working with
  PostgreSQL schemas and queries.
globs:
  - "**/*.sql"
  - "**/migrations/**"
---
Connect with `psql`, then run EXPLAIN on slow queries…
```

Parsed, this yields name `postgres`, description
`"Know-how for working with\nPostgreSQL schemas and queries."`, globs
`["**/*.sql", "**/migrations/**"]`, `always_apply: false`, and the body
starting at `Connect with psql…`. In `match_skills`, it matches whenever an
active path is a `.sql` file or lives under a `migrations/` directory.
