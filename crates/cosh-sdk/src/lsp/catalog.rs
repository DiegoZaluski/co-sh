//! Curated language-server catalog and discovery helpers.
//!
//! Matching is by exact file extension (lowercase, dot included) or by
//! exact extensionless file name (`Dockerfile`), plus root-marker walk-up — the loose heuristics that made crush start the wrong
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
    /// Extensionless file names claimed by this spec, lowercase (`"dockerfile"`).
    pub filenames: &'static [&'static str],
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

    /// Whether this spec claims a file by its (case-insensitive) name.
    /// Covers extensionless files such as `Dockerfile`.
    pub fn handles_file_name(&self, file_name: &str) -> bool {
        let file_name = file_name.to_ascii_lowercase();
        self.filenames
            .iter()
            .any(|candidate| *candidate == file_name)
    }

    /// Whether this spec claims `path`, by extension or by exact file name.
    pub fn handles(&self, path: &Path) -> bool {
        if path
            .file_name()
            .is_some_and(|file_name| self.handles_file_name(&file_name.to_string_lossy()))
        {
            return true;
        }
        path.extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .is_some_and(|ext| self.handles_extension(&ext))
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
        filenames: &[],
        extensions: &[".rs"],
        root_markers: &["Cargo.toml", "rust-toolchain.toml", ".git"],
    },
    ServerSpec {
        name: "clangd",
        command: "clangd",
        args: &[],
        filenames: &[],
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
        filenames: &[],
        extensions: &[".zig", ".zon"],
        root_markers: &["build.zig", ".git"],
    },
    // ── Web / scripting ─────────────────────────────────────────────────
    ServerSpec {
        name: "gopls",
        command: "gopls",
        args: &[],
        filenames: &[],
        extensions: &[".go"],
        root_markers: &["go.work", "go.mod", ".git"],
    },
    ServerSpec {
        name: "pyright",
        command: "pyright-langserver",
        args: &["--stdio"],
        filenames: &[],
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
        filenames: &[],
        extensions: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
        root_markers: &["package.json", "tsconfig.json", "jsconfig.json", ".git"],
    },
    ServerSpec {
        name: "ruby-lsp",
        command: "ruby-lsp",
        args: &[],
        filenames: &[],
        extensions: &[".rb", ".erb", ".rake", ".gemspec"],
        root_markers: &["Gemfile", "Rakefile", ".ruby-version", ".git"],
    },
    ServerSpec {
        name: "php-actor",
        command: "php-actor",
        args: &["language-server"],
        filenames: &[],
        extensions: &[".php"],
        root_markers: &["composer.json", "composer.lock", ".git"],
    },
    // ── BEAM ────────────────────────────────────────────────────────────
    ServerSpec {
        name: "elixir-ls",
        command: "elixir-ls",
        args: &[],
        filenames: &[],
        extensions: &[".ex", ".exs"],
        root_markers: &["mix.exs", ".git"],
    },
    ServerSpec {
        name: "erlang-ls",
        command: "erlang_ls",
        args: &[],
        filenames: &[],
        extensions: &[".erl", ".hrl"],
        root_markers: &["rebar.config", "erlang_ls.config", ".git"],
    },
    // ── JVM ─────────────────────────────────────────────────────────────
    ServerSpec {
        name: "jdtls",
        command: "jdtls",
        args: &[],
        filenames: &[],
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
        filenames: &[],
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
        filenames: &[],
        extensions: &[".clj", ".cljs", ".cljc", ".edn"],
        root_markers: &["deps.edn", "project.clj", "shadow-cljs.edn", ".git"],
    },
    // ── Functional ──────────────────────────────────────────────────────
    ServerSpec {
        name: "haskell-language-server",
        command: "haskell-language-server-wrapper",
        args: &["--lsp"],
        filenames: &[],
        extensions: &[".hs", ".lhs"],
        root_markers: &["stack.yaml", "cabal.project", "*.cabal", ".git"],
    },
    ServerSpec {
        name: "ocamllsp",
        command: "ocamllsp",
        args: &[],
        filenames: &[],
        extensions: &[".ml", ".mli"],
        root_markers: &["dune-project", "Makefile", ".git"],
    },
    // ── Shell / config ──────────────────────────────────────────────────
    ServerSpec {
        name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        filenames: &[],
        extensions: &[".sh", ".bash", ".zsh"],
        root_markers: &[],
    },
    ServerSpec {
        name: "lua-language-server",
        command: "lua-language-server",
        args: &["--lsp"],
        filenames: &[],
        extensions: &[".lua"],
        root_markers: &[".luarc.json", ".luacheckrc", ".git"],
    },
    ServerSpec {
        name: "taplo",
        command: "taplo",
        args: &["lsp", "stdio"],
        filenames: &[],
        extensions: &[".toml"],
        root_markers: &["Cargo.toml", "pyproject.toml", ".git"],
    },
    ServerSpec {
        name: "yaml-language-server",
        command: "yaml-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".yml", ".yaml"],
        root_markers: &[],
    },
    ServerSpec {
        name: "marksman",
        command: "marksman",
        args: &[],
        filenames: &[],
        extensions: &[".md", ".markdown"],
        root_markers: &[],
    },
    // ── Web markup ──────────────────────────────────────────────────────
    ServerSpec {
        name: "vscode-html-language-server",
        command: "vscode-html-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".html", ".htm"],
        root_markers: &[],
    },
    ServerSpec {
        name: "vscode-css-language-server",
        command: "vscode-css-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".css", ".scss", ".less"],
        root_markers: &[],
    },
    ServerSpec {
        name: "dockerfile-language-server-nodejs",
        command: "dockerfile-language-server-nodejs",
        args: &["--stdio"],
        filenames: &["dockerfile"],
        extensions: &[".dockerfile"],
        root_markers: &["Dockerfile", "docker-compose.yml", ".git"],
    },
    // ── Nix ─────────────────────────────────────────────────────────────
    ServerSpec {
        name: "nixd",
        command: "nixd",
        args: &[],
        filenames: &[],
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

    #[test]
    fn handles_matches_extensionless_files_by_name() {
        let docker = CATALOG
            .iter()
            .find(|s| s.name == "dockerfile-language-server-nodejs")
            .unwrap();
        assert!(docker.handles(Path::new("Dockerfile")));
        assert!(docker.handles(Path::new("dockerfile")));
        assert!(docker.handles(Path::new("/a/b/DOCKERFILE")));
        assert!(docker.handles(Path::new("build.dockerfile")));
        assert!(!docker.handles(Path::new("Dockerfile.bak")));
        assert!(!docker.handles(Path::new("docker-compose.yml")));
    }

    #[test]
    fn handles_survives_non_utf8_file_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let docker = CATALOG
            .iter()
            .find(|s| s.name == "dockerfile-language-server-nodejs")
            .unwrap();
        let weird = Path::new(OsStr::from_bytes(b"doc\xffuments/Dockerfile"));
        assert!(docker.handles(weird));

        let rust = CATALOG.iter().find(|s| s.name == "rust-analyzer").unwrap();
        let weird_rs = Path::new(OsStr::from_bytes(b"proj\xffct.rs"));
        assert!(
            rust.handles(weird_rs),
            "lossy extension matching preserved for non-UTF-8 stems"
        );
    }

    #[test]
    fn every_spec_filename_is_lowercase_and_extensionless() {
        for spec in CATALOG {
            for name in spec.filenames {
                assert!(
                    *name == name.to_ascii_lowercase() && !name.contains('.'),
                    "`{}` filename `{name}` must be lowercase without extension",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn handles_by_extension_only_when_no_name_match() {
        let rust = CATALOG.iter().find(|s| s.name == "rust-analyzer").unwrap();
        assert!(rust.handles(Path::new("src/lib.rs")));
        assert!(!rust.handles(Path::new("rs")));
    }
}
