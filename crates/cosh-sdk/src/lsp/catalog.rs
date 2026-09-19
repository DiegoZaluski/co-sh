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
    /// Whether the server understands the *whole* workspace and should be
    /// rooted at its outermost project directory instead of the nearest one.
    /// Workspace-aware servers (rust-analyzer analyzes an entire Cargo
    /// workspace from any member) would otherwise be spawned once per
    /// nested `Cargo.toml`, multiplying indexing load by the member count.
    pub workspace_aware: bool,
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
        workspace_aware: true,
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
        workspace_aware: false,
    },
    ServerSpec {
        name: "zls",
        command: "zls",
        args: &[],
        filenames: &[],
        extensions: &[".zig", ".zon"],
        root_markers: &["build.zig", ".git"],
        workspace_aware: false,
    },
    // ── Web / scripting ─────────────────────────────────────────────────
    ServerSpec {
        name: "gopls",
        command: "gopls",
        args: &[],
        filenames: &[],
        extensions: &[".go"],
        root_markers: &["go.work", "go.mod", ".git"],
        workspace_aware: false,
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
        workspace_aware: false,
    },
    ServerSpec {
        name: "typescript-language-server",
        command: "typescript-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
        root_markers: &["package.json", "tsconfig.json", "jsconfig.json", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "ruby-lsp",
        command: "ruby-lsp",
        args: &[],
        filenames: &[],
        extensions: &[".rb", ".erb", ".rake", ".gemspec"],
        root_markers: &["Gemfile", "Rakefile", ".ruby-version", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "php-actor",
        command: "php-actor",
        args: &["language-server"],
        filenames: &[],
        extensions: &[".php"],
        root_markers: &["composer.json", "composer.lock", ".git"],
        workspace_aware: false,
    },
    // ── BEAM ────────────────────────────────────────────────────────────
    ServerSpec {
        name: "elixir-ls",
        command: "elixir-ls",
        args: &[],
        filenames: &[],
        extensions: &[".ex", ".exs"],
        root_markers: &["mix.exs", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "erlang-ls",
        command: "erlang_ls",
        args: &[],
        filenames: &[],
        extensions: &[".erl", ".hrl"],
        root_markers: &["rebar.config", "erlang_ls.config", ".git"],
        workspace_aware: false,
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
        workspace_aware: false,
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
        workspace_aware: false,
    },
    ServerSpec {
        name: "clojure-lsp",
        command: "clojure-lsp",
        args: &[],
        filenames: &[],
        extensions: &[".clj", ".cljs", ".cljc", ".edn"],
        root_markers: &["deps.edn", "project.clj", "shadow-cljs.edn", ".git"],
        workspace_aware: false,
    },
    // ── Functional ──────────────────────────────────────────────────────
    ServerSpec {
        name: "haskell-language-server",
        command: "haskell-language-server-wrapper",
        args: &["--lsp"],
        filenames: &[],
        extensions: &[".hs", ".lhs"],
        root_markers: &["stack.yaml", "cabal.project", "*.cabal", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "ocamllsp",
        command: "ocamllsp",
        args: &[],
        filenames: &[],
        extensions: &[".ml", ".mli"],
        root_markers: &["dune-project", "Makefile", ".git"],
        workspace_aware: false,
    },
    // ── Shell / config ──────────────────────────────────────────────────
    ServerSpec {
        name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        filenames: &[],
        extensions: &[".sh", ".bash", ".zsh"],
        root_markers: &[],
        workspace_aware: false,
    },
    ServerSpec {
        name: "lua-language-server",
        command: "lua-language-server",
        args: &["--lsp"],
        filenames: &[],
        extensions: &[".lua"],
        root_markers: &[".luarc.json", ".luacheckrc", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "taplo",
        command: "taplo",
        args: &["lsp", "stdio"],
        filenames: &[],
        extensions: &[".toml"],
        root_markers: &["Cargo.toml", "pyproject.toml", ".git"],
        workspace_aware: false,
    },
    ServerSpec {
        name: "yaml-language-server",
        command: "yaml-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".yml", ".yaml"],
        root_markers: &[],
        workspace_aware: false,
    },
    ServerSpec {
        name: "marksman",
        command: "marksman",
        args: &[],
        filenames: &[],
        extensions: &[".md", ".markdown"],
        root_markers: &[],
        workspace_aware: false,
    },
    // ── Web markup ──────────────────────────────────────────────────────
    ServerSpec {
        name: "vscode-html-language-server",
        command: "vscode-html-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".html", ".htm"],
        root_markers: &[],
        workspace_aware: false,
    },
    ServerSpec {
        name: "vscode-css-language-server",
        command: "vscode-css-language-server",
        args: &["--stdio"],
        filenames: &[],
        extensions: &[".css", ".scss", ".less"],
        root_markers: &[],
        workspace_aware: false,
    },
    ServerSpec {
        name: "dockerfile-language-server-nodejs",
        command: "dockerfile-language-server-nodejs",
        args: &["--stdio"],
        filenames: &["dockerfile"],
        extensions: &[".dockerfile"],
        root_markers: &["Dockerfile", "docker-compose.yml", ".git"],
        workspace_aware: false,
    },
    // ── Nix ─────────────────────────────────────────────────────────────
    ServerSpec {
        name: "nixd",
        command: "nixd",
        args: &[],
        filenames: &[],
        extensions: &[".nix"],
        root_markers: &["flake.nix", "shell.nix", "default.nix", ".git"],
        workspace_aware: false,
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
        for extension in command_extensions() {
            let with_extension = dir.join(format!("{command}.{extension}"));
            if is_executable_file(&with_extension) {
                return Some(with_extension);
            }
        }
    }
    None
}

/// Executable extensions probed after the bare command name.
///
/// Windows CreateProcess appends `PATHEXT` entries (default `.COM`, `.EXE`,
/// `.BAT`, `.CMD`) when no extension is given; PATH discovery must mirror
/// that or every catalog entry resolves to `None` on Windows. `PATHEXT` is
/// read when present (order preserved, entries normalized), the documented
/// default substituted otherwise.
#[cfg(windows)]
fn command_extensions() -> Vec<String> {
    const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_owned());
    raw.split(';')
        .filter_map(|entry| {
            let entry = entry.trim();
            (!entry.is_empty()).then(|| entry.trim_start_matches('.').to_ascii_lowercase())
        })
        .collect()
}

#[cfg(not(windows))]
fn command_extensions() -> Vec<String> {
    Vec::new()
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

    /// RAII replacement of `PATH` for the duration of one test; restores the
    /// previous value on drop.
    ///
    /// Every caller MUST carry `#[serial_test::serial]`. Other tests that
    /// read `PATH` (all of them only make *negative* lookups of unique
    /// command names) stay correct under a temporary mutation window; any
    /// future test asserting a *positive* lookup on the ambient PATH must
    /// also take `#[serial_test::serial]`. Windows-only: unix builds never
    /// mutate `PATH`, keeping the helper dead-code clean there.
    #[cfg(windows)]
    struct ScopedPath {
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(windows)]
    impl ScopedPath {
        fn set<I>(dirs: I) -> Self
        where
            I: IntoIterator,
            I::Item: AsRef<std::ffi::OsStr>,
        {
            let previous = std::env::var_os("PATH");
            // SAFETY: test-only; `#[serial_test::serial]` on the callers
            // keeps this single-threaded for the duration of the guard.
            unsafe {
                std::env::set_var(
                    "PATH",
                    std::env::join_paths(dirs).expect("joinable PATH entries"),
                );
            }
            Self { previous }
        }
    }

    #[cfg(windows)]
    impl Drop for ScopedPath {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                // SAFETY: same single-threaded test context as `set`.
                unsafe { std::env::set_var("PATH", previous) };
            }
        }
    }

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
    #[cfg(unix)]
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

    /// Windows discovery must resolve `command` → `command.exe` via the
    /// PATHEXT-equivalent probe; a bare `dir.join(command)` finds nothing.
    #[test]
    #[cfg(windows)]
    #[serial_test::serial]
    fn lookup_on_path_finds_windows_executable_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fake-server-xyz.exe"), b"").unwrap();

        let guard = ScopedPath::set(std::iter::once(dir.path()));
        let found = lookup_on_path("fake-server-xyz");
        let absent = lookup_on_path("definitely-not-a-real-binary-xyz");
        drop(guard);

        assert_eq!(
            found,
            Some(dir.path().join("fake-server-xyz.exe")),
            "PATH lookup must append the Windows executable extension"
        );
        assert!(absent.is_none());
    }

    /// A command already carrying an executable extension is found verbatim,
    /// and a directory hit does not pass for a file.
    #[test]
    #[cfg(windows)]
    #[serial_test::serial]
    fn lookup_on_path_verbatim_and_directory_edge_cases() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fake-server-abc.exe"), b"").unwrap();
        std::fs::create_dir(dir.path().join("fake-dir-server.exe")).unwrap();

        let guard = ScopedPath::set(std::iter::once(dir.path()));
        let verbatim = lookup_on_path("fake-server-abc.exe");
        let directory_is_not_file = lookup_on_path("fake-dir-server");
        drop(guard);

        assert_eq!(verbatim, Some(dir.path().join("fake-server-abc.exe")));
        assert!(directory_is_not_file.is_none());
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
