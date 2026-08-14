# The `skills` module: discovering and serving `SKILL.md` capability packs

`skills` manages *skills* — self-contained Markdown capability packs (a
`SKILL.md` plus optional asset files) that teach the agent how to do
something. The module discovers skills from caller-supplied sources, lists
and reads them, reads their asset files, and matches them against the active
workspace so the right skills can be auto-activated.

| Tool | What it does |
|---|---|
| [`skills_list`](actions.md#list) | List every discovered skill (name + description). |
| [`skills_read`](actions.md#read) | Read a skill's full content: metadata plus body with frontmatter stripped. |
| [`skills_read_asset`](actions.md#read-asset) | Read one asset file from a skill's directory (path-traversal blocked). |
| [`skills_match_skills`](actions.md#match) | Return skills whose globs match the active file paths, or that are `always_apply`. |

The design matches the other cosh modules: a builder-style wrapper —
[`Skills`](#the-skills-wrapper) — holds the configuration, and each
[`SkillAction`](types.md#skillaction) is a dedicated method on it. The
configuration carries **all** policy: there are no hardcoded paths, names, or
filters in the module.

---

## The `Skills` wrapper

```rust,ignore
use cosh_tools::skills::{Skills, SkillSource};

let skills = Skills::new()
    .sources(vec![SkillSource::Directory { path: "/home/user/.skills".into() }])
    .recursive(true);

skills.list()?;                              // all skills
skills.read("my-skill")?;                    // one skill's full content
skills.read_asset("my-skill", "templates/foo.txt")?;
skills.match_skills(vec!["src/main.rs".into()])?;
```

Configure the sources and filters once, then call the operations. All four
take no per-call configuration — like the other cosh tools, the wrapper is
the single point of setup. `Skills::default()` is `Skills::new()`.

The builder methods:

| Method | Meaning | Default |
|---|---|---|
| `sources(Vec<SkillSource>)` | Ordered list of places to look for skills. First source wins on name collision. | empty |
| `recursive(bool)` | Scan directory sources recursively (skills at any depth) instead of only the immediate subdirectories. | `false` |
| `ignore(Vec<String>)` | Glob-style name patterns to exclude (e.g. `"deprecated-*"`). | empty |
| `include(Vec<String>)` | Allowlist — when non-empty, only skills whose exact name appears here are returned. | empty |

---

## What a skill is

A *skill* is a directory containing a `SKILL.md` file. Its YAML-like
frontmatter declares the skill's name, description, matching globs, and
whether it should always apply; the rest of the file is the skill's body —
the instructions the agent follows. Asset files (templates, reference
material) live next to `SKILL.md` in the same directory. The full format is
documented on the [frontmatter page](frontmatter.md).

A skill's **name** is the `name:` frontmatter field, falling back to the
directory name when absent. This matters for the filters and for `read`:
every operation addresses skills by name.

## The two source kinds

| Source | What it provides |
|---|---|
| `SkillSource::Directory { path }` | A local directory whose subdirectories contain `SKILL.md`. Non-recursive: only the **immediate** subdirectories are scanned. Recursive: any subdirectory at any depth. |
| `SkillSource::Embedded { skill }` | One in-memory skill supplied directly by the caller (no filesystem access). |

Directory sources are the normal case. Embedded sources are for callers that
want to inject skills programmatically — note that embedded skills have no
directory, so `read_asset` does not work for them
(`"read_asset is not supported for embedded skills"`).

---

## Discovery: cache, dedup, filters

Every operation starts by **discovering** skills from the sources, then
applies the filters. Three behaviors shape what you get back (detailed on the
[discovery page](discover.md)):

- **Caching.** Discovered skills are cached per source for the life of the
  process, so repeated calls do not re-scan the filesystem. A directory
  source is keyed by its canonicalized path.
- **First source wins.** If two sources contain a skill with the same name,
  only the first (in `sources` order) is kept; the duplicate is dropped with
  a warning log.
- **Filters apply to names.** `ignore` patterns remove skills whose name
  matches; a non-empty `include` keeps only the listed names. Both are
  applied after discovery, before any output is built.

---

## The harness and Ask mode

The harness constructs `Skills::new()` **without sources** — skill sources
must be configured by the caller before the tools return anything. In the
current harness this means `skills_list` etc. return empty results; the API
exists so a host application can wire its own skill directories.

All four skill tools are read-only, so they are exposed in Ask mode as well
as Build/Yolo mode — the model can always inspect available skills.

---

## Summary

- A skill is a directory with a `SKILL.md`; configure `Skills` once with
  sources + filters, then call `list` / `read` / `read_asset` /
  `match_skills`.
- Two source kinds: `Directory` (scanned, optionally recursively) and
  `Embedded` (in-memory). First source wins on name collision; results are
  cached per source.
- `match_skills` auto-activates skills by glob patterns or `always_apply`.
- Skill assets are sandboxed: `read_asset` rejects absolute paths, `..`, and
  any resolution outside the skill directory.

Next: the [data model](types.md), then the
[actions](actions.md), [discovery](discover.md), and
[frontmatter](frontmatter.md) pages.
