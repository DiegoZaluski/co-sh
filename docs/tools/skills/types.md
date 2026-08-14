# `skills` data types: sources, outputs, and errors

The skills module's types split into four groups: the output payloads
(`SkillInfo`, `SkillContent`, `SkillOutput`), the source types
(`SkillSource`, `EmbeddedSkill`), the schema that carries all policy
(`SkillSchema`, `SkillAction`), and the error enum. The output and error
types implement `Serialize`; the input types additionally derive
`Deserialize` + `JsonSchema` and double as the tool argument schemas.

---

## Output types

### `SkillInfo` — lightweight metadata

```rust
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}
```

Name + description only — the "card" for a skill. `list` and `match_skills`
return these; the body is deliberately not included (listing 50 skills must
not dump 50 bodies into context).

### `SkillContent` — the full skill

```rust
pub struct SkillContent {
    pub info: SkillInfo,
    pub body: String,   // SKILL.md body with frontmatter stripped
}
```

Returned by `read`: the metadata plus the instructions body.

### `SkillOutput` — the discriminated result

```rust
pub enum SkillOutput {
    List      { skills: Vec<SkillInfo> },
    Read      { skill: SkillContent },
    ReadAsset { content: String },
    Match     { matched: Vec<SkillInfo> },
}
```

One variant per action, so the caller can match on which action it
requested.

---

## Source types

### `SkillSource`

```rust
pub enum SkillSource {
    Directory { path: String },
    Embedded  { skill: EmbeddedSkill },
}
```

Where to look for skills. `Directory` points at a parent directory whose
subdirectories contain `SKILL.md` files; `Embedded` provides one skill
directly in memory. The ordering of the `sources` vector matters — the first
source wins on name collision.

### `EmbeddedSkill`

```rust
pub struct EmbeddedSkill {
    pub name: String,
    pub description: String,
    /// Full SKILL.md content, including frontmatter and body.
    pub content: String,
    pub globs: Vec<String>,
    pub always_apply: bool,
}
```

The in-memory equivalent of a skill directory. `content` is parsed like a
real `SKILL.md`; fields present in the embedded content's frontmatter
override the struct fields (see [frontmatter](frontmatter.md)). Embedded
skills have no directory, so `read_asset` rejects them.

---

## The schema and action

### `SkillAction`

```rust
pub enum SkillAction { List, Read, ReadAsset, Match }
```

The four operations, as data. Each variant maps to one
[action](actions.md).

### `SkillSchema` — the full policy

```rust
pub struct SkillSchema {
    pub action: SkillAction,
    pub sources: Vec<SkillSource>,   // ordered; first wins on name collision
    pub skill_name: Option<String>,  // target for Read / ReadAsset
    pub asset_path: Option<String>,  // relative sub-path for ReadAsset
    pub match_paths: Vec<String>,    // active file paths for Match
    pub recursive: bool,
    pub ignore: Vec<String>,
    pub include: Vec<String>,        // non-empty = allowlist
}
```

The module has **zero hardcoded policy**: every path, name, and filter is
carried in the schema. The [`Skills`](skills.md#the-skills-wrapper) wrapper
builds one of these per call from its configuration — callers normally never
construct one by hand, but the free form exists for direct engine use.

---

## Input types

`SkillsReadInput { name }`, `SkillsReadAssetInput { name, asset_path }`, and
`SkillsMatchInput { match_paths }` are thin wrappers matching the tool call
arguments — one field per argument, deserialized straight from JSON.

---

## `SkillError`

```rust
pub enum SkillError {
    NotFound(String),        // skill, asset, or asset file missing
    PathTraversal(String),   // asset path escapes the skill directory
    InvalidAction(String),   // missing argument, or unsupported combo
    InvalidSource(String),   // directory source unusable
}
```

The single error type. The variants tell the caller *which kind* of problem
occurred — a missing skill (`NotFound`) is a normal "nothing here" case,
while `PathTraversal` is a security rejection. The `Display` messages are
meant to be actionable (`"skill not found: foo"`).
