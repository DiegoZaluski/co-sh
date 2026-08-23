//! Curated language-server catalog and discovery helpers.
//!
//! v1 ships a deliberately small table covering the core development
//! languages; expansion (~15–30 servers) happens only after the whole engine
//! is validated. Matching is by exact file extension (lowercase, dot
//! included) plus root-marker walk-up — the loose heuristics that made crush
//! start the wrong server for a language (#1751) are exactly what this avoids.
//!
//! Servers are expected on `PATH`; there is no auto-install. Users override
//! per server through configuration later (manager wiring).

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
    ServerSpec {
        name: "rust-analyzer",
        command: "rust-analyzer",
        args: &[],
        extensions: &[".rs"],
        root_markers: &["Cargo.toml", "rust-toolchain.toml", ".git"],
    },
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
        ] {
            assert!(
                CATALOG.iter().any(|spec| spec.name == name),
                "{name} missing from dev catalog"
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
