//! Path helpers for resolving skill sources across shell environments.
//!
//! On Windows the harness is frequently launched from Git Bash / MSYS, whose
//! `HOME` uses POSIX drive syntax (`/c/Users/...`). Native Rust filesystem
//! calls do not understand that spelling — `Path::new("/c/Users/rooti/.skills")
//! .is_dir()` is false on Windows — so the `~/.skills` default source was
//! silently dropped and the `skills_*` tools reported an empty list.
//!
//! These helpers convert MSYS-style paths to their native Windows spelling
//! and provide a `HOME` resolution that falls back to `USERPROFILE` when
//! `HOME` does not point at a real directory.

/// Convert an MSYS-style POSIX drive path (`/c/Users/...`) to its native
/// Windows spelling (`C:/Users/...`).
///
/// Pure and platform-independent: returns `None` unless the string matches
/// exactly `/<drive-letter>/...`. Paths like `/usr/local`, `//server/share`,
/// relative paths, and already-native `C:/...` paths are left untouched
/// (forward slashes are kept — they are valid on Windows).
///
/// Note: on Windows `/<letter>/...` is therefore ALWAYS interpreted as MSYS
/// drive syntax — it cannot denote a root-relative `C:\<letter>\...` path.
/// This matches MSYS semantics but is an inherent heuristic.
#[must_use]
pub fn msys_to_windows(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b'/' && bytes[1].is_ascii_alphabetic() {
        let drive = (bytes[1] as char).to_ascii_uppercase();
        Some(format!("{drive}:{}", &path[2..]))
    } else {
        None
    }
}

/// Normalize `path` to a native filesystem spelling.
///
/// On Windows, MSYS-style drive paths (`/c/...`) are rewritten to `C:/...`;
/// everything else — and every path on non-Windows — passes through
/// unchanged.
#[must_use]
pub fn normalize_shell_path(path: &str) -> String {
    #[cfg(windows)]
    {
        msys_to_windows(path).unwrap_or_else(|| path.to_string())
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// Best-effort home directory resolution for skill-source defaults.
///
/// Prefers `HOME` (normalizing MSYS spellings on Windows) when it points at
/// an existing directory, then falls back to `USERPROFILE` — always set on
/// Windows, which covers the common broken case of an MSYS `HOME` inherited
/// from a Git Bash parent process. Returns `None` when neither resolves.
#[must_use]
pub fn home_dir() -> Option<String> {
    if let Some(h) = std::env::var_os("HOME") {
        // Lossy on invalid UTF-8 (unpaired surrogates): the is_dir probe
        // then fails and resolution degrades safely to USERPROFILE.
        let h = normalize_shell_path(&h.to_string_lossy());
        if std::path::Path::new(&h).is_dir() {
            return Some(h);
        }
    }
    if let Some(up) = std::env::var_os("USERPROFILE") {
        let up = up.to_string_lossy().into_owned();
        if std::path::Path::new(&up).is_dir() {
            return Some(up);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{msys_to_windows, normalize_shell_path};

    #[test]
    fn msys_drive_paths_convert_with_uppercase_drive() {
        assert_eq!(
            msys_to_windows("/c/Users/rooti/.skills").as_deref(),
            Some("C:/Users/rooti/.skills")
        );
        assert_eq!(
            msys_to_windows("/d/data/skills").as_deref(),
            Some("D:/data/skills")
        );
        // Single-letter drive: input `/m/x`, output `M:/x`.
        assert_eq!(msys_to_windows("/m/x"), Some("M:/x".to_string()));
    }

    #[test]
    fn non_msys_paths_are_left_untouched() {
        assert_eq!(msys_to_windows("/usr/local"), None);
        assert_eq!(msys_to_windows("//server/share"), None);
        assert_eq!(msys_to_windows("C:/Users/rooti"), None);
        assert_eq!(msys_to_windows("C:\\Users\\rooti"), None);
        assert_eq!(msys_to_windows("relative/path"), None);
        assert_eq!(msys_to_windows(""), None);
        assert_eq!(msys_to_windows("/c"), None);
        assert_eq!(msys_to_windows("/1/invalid-drive"), None);
    }

    #[cfg(windows)]
    #[test]
    fn normalize_rewrites_msys_paths_only_on_windows() {
        assert_eq!(
            normalize_shell_path("/c/Users/rooti/.skills"),
            "C:/Users/rooti/.skills"
        );
        assert_eq!(normalize_shell_path("/usr/local"), "/usr/local");
        assert_eq!(normalize_shell_path("D:/data"), "D:/data");
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_is_identity_off_windows() {
        assert_eq!(
            normalize_shell_path("/c/Users/rooti/.skills"),
            "/c/Users/rooti/.skills"
        );
    }
}
