# `discover` — finding skills and applying filters

Every action starts by discovering skills from the configured
[`SkillSource`](types.md#skillsource)s. This page is about that pipeline:
what gets scanned, how recursion works, the per-source cache, and the
name-collision rules.

```rust,ignore
pub fn discover_skills(schema: &SkillSchema) -> Result<Vec<RawSkill>, SkillError>
```

`discover_skills` iterates the sources **in order**, discovers each one's
skills, filters them, and deduplicates — producing the list every action then
works from.

---

## Directory sources

A `Directory` source is a parent directory whose subdirectories contain
`SKILL.md` files. The scan is canonicalized first (so the cache key and the
containment checks are stable against symlinked paths), and an unreadable or
non-directory source is a hard `InvalidSource` error.

### Non-recursive (default)

Only the **immediate** subdirectories of the source are inspected; a
subdirectory is a skill iff it contains a `SKILL.md` file. Deeper nesting is
ignored:

```text
/home/user/.skills/
├── git/            ← skill (has SKILL.md)
├── docker/         ← skill (has SKILL.md)
└── archive/
    └── old-tool/   ← NOT a skill (too deep)
```

### Recursive

With `recursive(true)`, the whole tree under the source is walked: every
directory at any depth that contains a `SKILL.md` becomes a skill, and the
walk continues below it (a skill directory can also contain nested skill
directories). Cycles are impossible in a real filesystem but the walk keeps a
visited set regardless.

In both modes the skill's **name** comes from the `name:` frontmatter field,
falling back to the directory name.

---

## Embedded sources

An `Embedded` source is a single in-memory skill, produced directly from the
[`EmbeddedSkill`](types.md#embeddedskill) value. Its `content` is parsed like
a real `SKILL.md`: frontmatter fields in the content take precedence over the
struct fields (`name`, `description`), `globs` from the content are used when
non-empty (else the struct's), and `always_apply` is OR-ed. The skill's
"directory" is empty, which is why `read_asset` cannot work for it.

---

## The discovery cache

Discovered skills are cached **per source** for the lifetime of the process:

- The cache key is the canonicalized source path (`dir:/abs/path`) or
  `embedded:<name>`.
- On a cache hit the stored skills are cloned; on a miss the source is
  scanned, cached, and returned.
- The cache is never invalidated: if a `SKILL.md` changes on disk mid-session
  (or a skill directory is added or removed), the module keeps serving the
  snapshot from the first scan of that source.

This is what makes repeated `skills_*` calls cheap in a long session — the
filesystem is only touched once per source. It also means new skills require
a new process (or a source path change) to appear.

## Dedup: first source wins

After discovery, skills are merged across sources in `sources` order. If two
sources contribute a skill with the **same name**, the first occurrence wins
and the later one is dropped with a warning log:

```
duplicate skill name 'git' from source 'dir:/home/user/extra' — keeping first occurrence from 'dir:/home/user/.skills'
```

Order your `sources` by priority: the most authoritative source first.

---

## Filters

The two filters apply to skill **names** after discovery:

| Filter | Rule |
|---|---|
| `ignore` | Glob-style patterns; a skill whose name matches any pattern is dropped. Patterns support `*` wildcards (`"deprecated-*"`) plus exact names and the catch-all `"*"`. |
| `include` | Allowlist of exact names. **Empty = everything passes**; non-empty = only listed names survive. |

Both are simple name matchers (not path globs) — see
[match_util](match_util.md). Filters apply uniformly across all four actions,
so a skill excluded here is invisible to `list`, `read`, and `match` alike.
