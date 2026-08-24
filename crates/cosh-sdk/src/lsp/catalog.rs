//! Curated language-server catalog and discovery helpers.
//!
//! Matching is by exact file extension (lowercase, dot included) plus
//! root-marker walk-up — the loose heuristics that made crush start the wrong
//! server for a language (#1751) are exactly what this avoids.
//!
//! Servers are expected on `PATH`; there is no auto-install. Missing binaries
//! soft-skip at spawn time, so the catalog can be generous without penalty.
//!
//! Sources for launch args and root markers: nvim-lspconfig defaults,
//  cross-checked against each server's own documentation.

use std::path::{Path, PathBuf};

/// One catalog entry: how to launch a language server and when it applies.
#[derive(Clone, Copy, Debug)]
pub struct ServerSpec {
    /// Canonical name (`rust-analyzer`, `gopls`, …). Manager key component.
    pub name: &'static str,
    /// Executable looked up on `PATH`.
    pub command: &'static str,
    /// Fixed launch arguments.
    pub args: &'static [&'static str],
    /// File extensions served, lowercase with leading dot (`".rs"`).
    pub extensions: &'static [&'static str],
    /// Files/directories identifying the project root, checked walking up
    /// from the touched file toward the workspace root. Empty = always use
    /// the workspace root.
    pub root_markers: &'static [&'static str],
}

impl ServerSpec {
    /// Whether this spec claims files with the given extension.
    pub fn handles_extension(&self, extension: &str) -> bool {
        let extension = extension.to_ascii_lowercase();
        self.extensions
            .iter()
            .any(|candidate| *candidate == extension)
    }

    /// Resolve the binary path against `PATH`. Returns `None` when absent or
    /// when no entry is an executable regular file.
    pub fn resolve_binary(&self) -> Option<PathBuf> {
        lookup_on_path(self.command)
    }
}

/// The built-in catalog. Order matters only aesthetically; matching collects
/// every applicable spec, not just the first.
pub const CATALOG: &[ServerSpec] = &[
    // ── Systems ──────────────────────────────────────────────────────────
    ServerSpec {
        name: "rust-analyzer",
        command: "rust-analyzer",
        args: &[],
        extensions: &[".rs"],
        root_markers: &["Cargo.toml", "rust-toolchain.toml", ".git"],
    },
    ServerSpec {
        name: "clangd",
        command: "clangd",
        args: &[],
        extensions: &[".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx"],
        root_markers: &[
            "compile_commands.json",
            "compile_flags.txt",
            "CMakeLists.txt",
            "Makefile",
            ".git",
        ],
    },
    ServerSpec {
        name: "zls",
        command: "zls",
        args: &[],
        extensions: &[".zig", ".zon"],
        root_markers: &["build.zig", ".git"],
    },
    // ── Web / scripting ─────────────────────────────────────────────────
    ServerSpec {
        name: "gopls",
        command: "gopls",
        args: &[],
        extensions: &[".go"],
        root_markers: &["go.work", "go.mod", ".git"],
    },
    ServerSpec {
        name: "pyright",
        command: "pyright-langserver",
        args: &["--stdio"],
        extensions: &[".py", ".pyi"],
        root_markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            ".git",
        ],
    },
    ServerSpec {
        name: "typescript-language-server",
        command: "typescript-language-server",
        args: &["--stdio"],
        extensions: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
        root_markers: &["package.json", "tsconfig.json", "jsconfig.json", ".git"],
    },
    ServerSpec {
        name: "ruby-lsp",
        command: "ruby-lsp",
        args: &[],
        extensions: &[".rb", ".erb", ".rake", ".gemspec"],
        root_markers: &["Gemfile", "Rakefile", ".ruby-version", ".git"],
    },
    ServerSpec {
        name: "php-actor",
        command: "php-actor",
        args: &["language-server"],
        extensions: &[".php"],
        root_markers: &["composer.json", "composer.lock", ".git"],
    },
    // ── BEAM ────────────────────────────────────────────────────────────
    ServerSpec {
        name: "elixir-ls",
        command: "elixir-ls",
        args: &[],
        extensions: &[".ex", ".exs"],
        root_markers: &["mix.exs", ".git"],
    },
    ServerSpec {
        name: "erlang-ls",
        command: "erlang_ls",
        args: &[],
        extensions: &[".erl", ".hrl"],
        root_markers: &["rebar.config", "erlang_ls.config", ".git"],
    },
    // ── JVM ─────────────────────────────────────────────────────────────
    ServerSpec {
        name: "jdtls",
        command: "jdtls",
        args: &[],
        extensions: &[".java"],
        root_markers: &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            ".git",
        ],
    },
    ServerSpec {
        name: "kotlin-language-server",
        command: "kotlin-language-server",
        args: &[],
        extensions: &[".kt", ".kts"],
        root_markers: &[
            "build.gradle.kts",
            "build.gradle",
            "settings.gradle",
            "pom.xml",
            ".git",
        ],
    },
    ServerSpec {
        name: "clojure-lsp",
        command: "clojure-lsp",
        args: &[],
        extensions: &[".clj", ".cljs", ".cljc", ".edn"],
        root_markers: &["deps.edn", "project.clj", "shadow-cljs.edn", ".git"],
    },
    // ── Functional ──────────────────────────────────────────────────────
    ServerSpec {
        name: "haskell-language-server",
        command: "haskell-language-server-wrapper",
        args: &["--lsp"],
        extensions: &[".hs", ".lhs"],
        root_markers: &["stack.yaml", "cabal.project", "*.cabal", ".git"],
    },
    ServerSpec {
        name: "ocamllsp",
        command: "ocamllsp",
        args: &[],
        extensions: &[".ml", ".mli"],
        root_markers: &["dune-project", "Makefile", ".git"],
    },
    // ── Shell / config ──────────────────────────────────────────────────
    ServerSpec {
        name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        extensions: &[".sh", ".bash", ".zsh"],
        root_markers: &[],
    },
    ServerSpec {
        name: "lua-language-server",
        command: "lua-language-server",
        args: &["--lsp"],
        extensions: &[".lua"],
        root_markers: &[".luarc.json", ".luacheckrc", ".git"],
    },
    ServerSpec {
        name: "taplo",
        command: "taplo",
        args: &["lsp", "stdio"],
        extensions: &[".toml"],
        root_markers: &["Cargo.toml", "pyproject.toml", ".git"],
    },
    ServerSpec {
        name: "yaml-language-server",
        command: "yaml-language-server",
        args: &["--stdio"],
        extensions: &[".yml", ".yaml"],
        root_markers: &[],
    },
    ServerSpec {
        name: "marksman",
        command: "marksman",
        args: &[],
        extensions: &[".md", ".markdown"],
        root_markers: &[],
    },
    // ── Web markup ──────────────────────────────────────────────────────
    ServerSpec {
        name: "vscode-html-language-server",
        command: "vscode-html-language-server",
        args: &["--stdio"],
        extensions: &[".html", ".htm"],
        root_markers: &[],
    },
    ServerSpec {
        name: "vscode-css-language-server",
        command: "vscode-css-language-server",
        args: &["--stdio"],
        extensions: &[".css", ".scss", ".less"],
        root_markers: &[],
    },
    ServerSpec {
        name: "dockerfile-language-server-nodejs",
        command: "dockerfile-language-server-nodejs",
        args: &["--stdio"],
        extensions: &["dockerfile", ".dockerfile"],
        root_markers: &["Dockerfile", "docker-compose.yml", ".git"],
    },
    // ── Nix ─────────────────────────────────────────────────────────────
    ServerSpec {
        name: "nixd",
        command: "nixd",
        args: &[],
        extensions: &[".nix"],
        root_markers: &["flake.nix", "shell.nix", "default.nix", ".git"],
    },
];

/// Look a command up on `PATH` without shelling out.
///
/// Mirrors `which(1)` semantics closely enough for discovery: entries are
/// tried in order, first executable regular file wins.
///
/// Precondition: `command` is a bare file name — every catalog entry satisfies
/// this, and commands containing path separators would be joined onto each
/// PATH entry and never found.
pub fn lookup_on_path(command: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    // Windows resolves PATHEXT lazily; existence of the plain file is close
    // enough for discovery, and spawn errors surface precisely afterwards.
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_extension_is_case_insensitive_and_exact() {
        let rust = CATALOG.iter().find(|s| s.name == "rust-analyzer").unwrap();
        assert!(rust.handles_extension(".rs"));
        assert!(rust.handles_extension(".RS"));
        assert!(
            !rust.handles_extension("rs"),
            "extension must include the dot"
        );
        assert!(!rust.handles_extension(".rlib"));
    }

    #[test]
    fn dev_catalog_covers_core_languages() {
        for name in [
            "rust-analyzer",
            "gopls",
            "pyright",
            "typescript-language-server",
            "clangd",
            "bash-language-server",
            "elixir-ls",
            "zls",
        ] {
            assert!(
                CATALOG.iter().any(|spec| spec.name == name),
                "{name} missing from catalog"
            );
        }
    }

    #[test]
    fn catalog_has_at_least_20_entries() {
        assert!(
            CATALOG.len() >= 20,
            "catalog has {} entries, expected >= 20",
            CATALOG.len()
        );
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for spec in CATALOG {
            assert!(
                seen.insert(spec.name),
                "duplicate server name `{}`",
                spec.name
            );
        }
    }

    #[test]
    fn every_spec_has_at_least_one_extension() {
        for spec in CATALOG {
            assert!(
                !spec.extensions.is_empty(),
                "`{}` has no extensions",
                spec.name
            );
        }
    }

    #[test]
    fn lookup_on_path_finds_a_real_binary() {
        // `sh` exists on any unix CI.
        #[cfg(unix)]
        assert!(lookup_on_path("sh").is_some());
        assert!(lookup_on_path("definitely-not-a-real-binary-xyz").is_none());
    }
}
