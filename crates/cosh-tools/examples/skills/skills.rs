//! Demonstrate `skills`: discovering `SKILL.md` capability packs. Builds a
//! scratch skills directory and exercises `list`, `read`, `read_asset` (with
//! the path-traversal sandbox), `match_skills` (globs + always_apply), the
//! ignore/include filters, an embedded in-memory source, and the error
//! cases.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example skills
//! ```

use cosh_tools::skills::{EmbeddedSkill, SkillOutput, Skills, SkillSource};

fn show_out(label: &str, out: &SkillOutput) {
    println!("== {label} ==");
    match out {
        SkillOutput::List { skills } => {
            for s in skills {
                println!("  - {}: {}", s.name, s.description);
            }
            if skills.is_empty() {
                println!("  (no skills)");
            }
        }
        SkillOutput::Read { skill } => {
            println!("  name: {}", skill.info.name);
            println!("  description: {}", skill.info.description);
            println!("  body:");
            for line in skill.body.lines() {
                println!("    {line}");
            }
        }
        SkillOutput::ReadAsset { content } => {
            println!("  content: {content:?}");
        }
        SkillOutput::Match { matched } => {
            for s in matched {
                println!("  - {}: {}", s.name, s.description);
            }
            if matched.is_empty() {
                println!("  (nothing matched)");
            }
        }
    }
    println!();
}

fn show_err(label: &str, err: cosh_tools::skills::SkillError) {
    println!("== {label} ==");
    println!("  error: {err}\n");
}

fn main() {
    // A scratch skills directory: two glob-driven skills, one always-on
    // skill, and one skill that declares neither globs nor always_apply.
    let dir = std::env::temp_dir().join("cosh-skills-example");
    let _ = std::fs::remove_dir_all(&dir);
    for (name, fm, body) in [
        (
            "rust",
            "description: Rust help\nglobs:\n  - \"**/*.rs\"\n",
            "Help with Rust: run `cargo test` after changes.",
        ),
        (
            "sql",
            "description: SQL help\nglobs:\n  - \"**/*.sql\"\n  - \"**/migrations/**\"\n",
            "Help with SQL: EXPLAIN slow queries.",
        ),
        (
            "always-on",
            "description: Applied everywhere\nalwaysApply: true\n",
            "General advice that always applies.",
        ),
        (
            "no-globs",
            "description: Has no activation rule\n",
            "This skill can never auto-activate.",
        ),
    ] {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), format!("---\nname: {name}\n{fm}---\n{body}\n"))
            .unwrap();
    }
    // An asset file inside the rust skill.
    std::fs::create_dir_all(dir.join("rust").join("templates")).unwrap();
    std::fs::write(
        dir.join("rust").join("templates").join("main.rs"),
        "fn main() {}\n",
    )
    .unwrap();

    let skills = Skills::new().sources(vec![SkillSource::Directory {
        path: dir.to_string_lossy().into_owned(),
    }]);

    // 1. List everything discovered.
    show_out("1. list", &skills.list().unwrap());

    // 2. Read one skill: body with the frontmatter stripped.
    show_out("2. read rust", &skills.read("rust").unwrap());

    // 3. Read an asset from a skill directory.
    show_out(
        "3. read_asset rust templates/main.rs",
        &skills.read_asset("rust", "templates/main.rs").unwrap(),
    );

    // 4. The asset sandbox: `..` escapes and absolute paths are rejected.
    show_err(
        "4a. read_asset with .. traversal",
        skills
            .read_asset("rust", "../../../etc/passwd")
            .expect_err("traversal must fail"),
    );
    show_err(
        "4b. read_asset with absolute path",
        skills
            .read_asset("rust", "/etc/passwd")
            .expect_err("absolute path must fail"),
    );

    // 5. Match against active workspace paths: only the rust skill's globs
    //    hit `src/main.rs`; always-on matches unconditionally; no-globs
    //    never matches.
    show_out(
        "5. match_skills on [src/main.rs]",
        &skills
            .match_skills(vec!["src/main.rs".into(), "README.md".into()])
            .unwrap(),
    );
    show_out(
        "5b. match_skills on [schema.sql]",
        &skills.match_skills(vec!["db/schema.sql".into()]).unwrap(),
    );

    // 6. Filters: ignore pattern and include allowlist.
    let filtered = Skills::new()
        .sources(vec![SkillSource::Directory {
            path: dir.to_string_lossy().into_owned(),
        }])
        .ignore(vec!["always-*".into()]);
    show_out("6a. list with ignore [always-*]", &filtered.list().unwrap());

    let allowlisted = Skills::new()
        .sources(vec![SkillSource::Directory {
            path: dir.to_string_lossy().into_owned(),
        }])
        .include(vec!["rust".into()]);
    show_out("6b. list with include [rust]", &allowlisted.list().unwrap());

    // 7. Embedded in-memory source (no filesystem access).
    let embedded = Skills::new().sources(vec![SkillSource::Embedded {
        skill: EmbeddedSkill {
            name: "inline".into(),
            description: "Built-in skill".into(),
            content: "---\nname: inline\ndescription: From the struct\n---\nInline body.\n".into(),
            globs: vec!["**/*.toml".into()],
            always_apply: false,
        },
    }]);
    show_out("7a. embedded list", &embedded.list().unwrap());
    show_out(
        "7b. embedded match on [Cargo.toml]",
        &embedded.match_skills(vec!["Cargo.toml".into()]).unwrap(),
    );
    show_err(
        "7c. embedded read_asset (no directory)",
        embedded
            .read_asset("inline", "x.txt")
            .expect_err("embedded skills have no assets"),
    );

    // 8. Errors: unknown skill, missing source directory.
    show_err("8a. read of unknown skill", skills.read("nope").unwrap_err());
    show_err(
        "8b. missing source directory",
        Skills::new()
            .sources(vec![SkillSource::Directory {
                path: "/nonexistent/skills".into(),
            }])
            .list()
            .unwrap_err(),
    );

    std::fs::remove_dir_all(&dir).ok();
}
