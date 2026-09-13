//! Editor resolution for the file explorer.
//!
//! A configured editor (Settings → Editor) always wins; otherwise the first
//! available of `nvim` → `vim` → `nano` is used. When nothing can be found
//! the explorer stays navigation-only: launching silently does nothing.

/// Editor fallback chain, in priority order (VS Code-style: capable TUI
/// editors first, `nano` as the universally-installed floor).
pub const FALLBACK_EDITORS: [&str; 3] = ["nvim", "vim", "nano"];

/// Check whether an editor is launchable.
///
/// A configured editor may be a full command ("nvim", "code -w") or an
/// absolute path: the first whitespace-separated token must name an
/// executable in `$PATH` (or be an executable file itself).
fn command_exists(command: &str) -> bool {
    let Some(bin) = command.split_whitespace().next() else {
        return false;
    };
    if bin.is_empty() {
        return false;
    }
    let path = std::path::Path::new(bin);
    if path.is_absolute() || bin.contains('/') {
        return path.is_file();
    }
    // Bare name: search $PATH.
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(bin);
            candidate.is_file() && is_executable(&candidate)
        })
    })
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Resolve the editor command for a configured `editor` setting.
///
/// `Some(command)` means "try to launch this"; `None` means no usable
/// editor was found (the caller must silently do nothing).
pub fn resolve_editor_command(configured: &str) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        // A configured editor is used as-is — even when its binary is not
        // found here, launching may still work (shell alias, PATH change
        // since startup). The launch failure itself is silent.
        return Some(configured.to_string());
    }
    FALLBACK_EDITORS
        .iter()
        .find(|name| command_exists(name))
        .map(|name| (*name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_editor_wins_even_if_not_found() {
        assert_eq!(
            resolve_editor_command("my-fancy-editor"),
            Some("my-fancy-editor".into())
        );
        // Used as-is (outer trim only): launch failures stay silent, so
        // there is nothing to validate against $PATH here.
        assert_eq!(
            resolve_editor_command("  nvim  -u NONE "),
            Some("nvim  -u NONE".to_string())
        );
    }

    #[test]
    fn empty_command_falls_back_to_the_chain() {
        // Whitespace-only configured command == no configuration: the
        // fallback chain decides. It must resolve to a chain member when
        // one exists, and to None only when none are installed.
        let resolved = resolve_editor_command("   ");
        if FALLBACK_EDITORS.iter().any(|name| command_exists(name)) {
            assert!(matches!(resolved, Some(cmd) if FALLBACK_EDITORS.contains(&cmd.as_str())));
        } else {
            assert_eq!(resolved, None);
        }
    }

    /// Environment-dependent: whatever fallback editors exist on this
    /// machine, the resolved command must be one of the chain (or None on
    /// a machine with none of them installed).
    #[test]
    fn fallback_resolves_to_first_available_editor() {
        match resolve_editor_command("") {
            Some(cmd) => assert!(FALLBACK_EDITORS.contains(&cmd.as_str())),
            None => {
                // Legitimate only when NONE of the fallback editors exist.
                assert!(!FALLBACK_EDITORS
                    .iter()
                    .any(|name| command_exists(name)));
            }
        }
    }
}
