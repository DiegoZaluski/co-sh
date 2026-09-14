//! GitHub release check and in-app update pipeline.
//!
//! At boot a background task queries the GitHub releases API for the latest
//! published release. When its tag is newer than the running binary, the
//! home banner announces `"🎉 Vx.y.z Now available"` with a changelog link
//! and an "Update" button.

use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────────────
// DEV MOCK TOGGLE ── flip this to see the banner without a real release
// ─────────────────────────────────────────────────────────────────────────────
/// When `true`, the release check returns a FAKE release (see
/// [`mock_release`]) without touching the network, and the update pipeline
/// is NOT run (it is only logged) — the TUI still closes and must be
/// reopened manually.
///
/// Set to `false` (or remove) for real behavior: the GitHub API check and
/// the actual install pipeline, which closes the TUI, installs and
/// relaunches cosh.
const MOCK_UPDATE_BANNER: bool = true;

/// Fake release shown while [`MOCK_UPDATE_BANNER`] is on. The running
/// version is 0.1.0, so 0.1.1 is correctly detected as an update.
fn mock_release() -> LatestRelease {
    LatestRelease {
        tag: "v0.1.1".to_string(),
        version: "0.1.1".to_string(),
        url: format!("https://github.com/{REPO}/releases/tag/v0.1.1"),
    }
}
// ─────────────────────────────────────────────────────────────────────────────

/// GitHub repo the releases are published under (matches `download.sh`).
pub const REPO: &str = "DiegoZaluski/cosh";

/// Defense-in-depth for values that come from the GitHub API response and
/// end up on a shell command line or in a URL: accept only plain release
/// tags (`v0.1.1`, `1.2.3-rc1+meta`). Anything else aborts the pipeline.
fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// Raw git tag as published (e.g. `v0.1.1`).
    pub tag: String,
    /// Version without the `v` prefix (e.g. `0.1.1`).
    pub version: String,
    /// Release page URL (changelog). Falls back to the releases list when
    /// the API does not provide one.
    pub url: String,
}

/// Messages from the background release check into the UI thread.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// Boot check finished. `None` = up to date or the check failed
    /// (offline, rate limit) — in both cases the banner stays hidden.
    CheckFinished(Option<LatestRelease>),
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    draft: Option<bool>,
    prerelease: Option<bool>,
}

/// Strip an optional leading `v` from a release tag.
fn version_from_tag(tag: &str) -> String {
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn strip_meta(v: &str) -> &str {
    let v = v.split('+').next().unwrap_or(v);
    v.split('-').next().unwrap_or(v)
}

/// Compare two dotted versions numerically component by component.
/// Missing components count as 0 (`0.2` > `0.1.9`). Non-numeric components
/// compare as 0 via the parse fallback. Build metadata (`+...`) and
/// pre-release suffixes (`-...`) are stripped from both sides so a dev
/// build like `0.1.0+abc` still receives `0.1.1`.
fn is_newer(current: &str, candidate: &str) -> bool {
    let cur = strip_meta(current);
    let cand = strip_meta(candidate);

    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|c| c.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let cur_parts = parse(cur);
    let cand_parts = parse(cand);

    let len = cur_parts.len().max(cand_parts.len());
    for i in 0..len {
        let a = cur_parts.get(i).copied().unwrap_or(0);
        let b = cand_parts.get(i).copied().unwrap_or(0);
        if b != a {
            return b > a;
        }
    }
    false
}

/// Whether a released `tag` should be advertised over the running version.
pub fn is_update_available(current_version: &str, tag: &str) -> bool {
    is_newer(current_version, &version_from_tag(tag))
}

/// Query the GitHub releases API for the latest published release.
/// Returns `None` on any network/parse error (the banner simply stays
/// hidden — a failed check must never degrade the app).
pub async fn fetch_latest_release() -> Option<LatestRelease> {
    // DEV MOCK: skip the network entirely while the toggle is on.
    if MOCK_UPDATE_BANNER {
        return Some(mock_release());
    }

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent(concat!("cosh/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let release: GhRelease = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;

    let tag = release.tag_name?;
    if !is_valid_tag(&tag) {
        log::warn!("ignoring release with malformed tag: {tag:?}");
        return None;
    }
    if release.draft.unwrap_or(false) || release.prerelease.unwrap_or(false) {
        return None;
    }
    let version = version_from_tag(&tag);
    if !is_update_available(env!("CARGO_PKG_VERSION"), &version) {
        return None;
    }
    Some(LatestRelease {
        url: release
            .html_url
            .unwrap_or_else(|| format!("https://github.com/{REPO}/releases")),
        version,
        tag,
    })
}

/// Fire-and-forget the OS install pipeline (the documented manual install:
/// `curl … | bash` / `download.ps1`) pinned to `latest_tag`.
///
/// The script downloads the release, replaces the binary in place and
/// RELAUNCHES cosh when it finishes. This function only spawns it (fully
/// detached: own process group, no console window) and returns immediately —
/// the caller then closes the TUI, and the pipeline's relaunch brings the
/// updated app back up. No UI feedback is needed: the terminal visibly
/// closes and reopens. Errors (failed spawn, invalid tag) are just logged;
/// there is no in-app retry surface.
pub fn spawn_update_pipeline(latest_tag: &str) {
    // DEV MOCK: nothing to run — the real pipeline closes the TUI and
    // relaunches it, so the mock only logs; the caller still closes the app.
    if MOCK_UPDATE_BANNER {
        log::info!("[MOCK] update pipeline for {latest_tag} (would close the TUI and relaunch)");
        return;
    }

    // The tag lands on a shell command line and inside download URLs:
    // validate before anything runs (defense-in-depth; the release check
    // already rejects malformed tags).
    if !is_valid_tag(latest_tag) {
        log::error!("update pipeline: invalid release tag {latest_tag:?}");
        return;
    }

    // Path of the running binary: the pipeline relaunches it after the
    // install replaces it on disk.
    let Some(current_exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
    else {
        log::error!("update pipeline: cannot resolve the running executable path");
        return;
    };

    let result = if cfg!(windows) {
        // Windows: curl the pinned PowerShell installer to a private temp
        // file, run it, record its exit code in an env var BEFORE the
        // unconditional cleanup `del` (a trailing cleanup command would
        // otherwise become cmd's exit status and mask a failure), relaunch
        // cosh, then exit with the recorded code. CREATE_NO_WINDOW only:
        // DETACHED_PROCESS is mutually exclusive with it and the combined
        // bitmask can make CreateProcess fail outright.
        #[cfg(windows)]
        {
            let script_url =
                format!("https://raw.githubusercontent.com/{REPO}/{latest_tag}/download.ps1");
            // Process-unique temp name: a predictable shared path would let
            // a local process pre-create or swap the script between the
            // download and its execution under -ExecutionPolicy Bypass.
            let script =
                std::env::temp_dir().join(format!("cosh-update-{}.ps1", std::process::id()));
            let cmd = format!(
                "curl.exe -fsSL -o \"{}\" \"{script_url}\" && powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" & set COSH_UPDATE_RC=!ERRORLEVEL! & del /q \"{}\" & start \"\" \"{current_exe}\" & exit /b !COSH_UPDATE_RC!",
                script.display(),
                script.display(),
                script.display(),
            );
            use std::os::windows::process::CommandExt;
            std::process::Command::new("cmd")
                // /V:ON enables DELAYED variable expansion: without it cmd
                // expands %VAR% at PARSE time (the whole one-liner is read
                // at once), so the recorded code would be the shell's
                // startup errorlevel (0) instead of the install's exit
                // code. With /V:ON, !VAR! expands at execution time.
                .args(["/V:ON", "/C", &cmd])
                .creation_flags(0x0800_0000)
                .spawn()
        }
        #[cfg(not(windows))]
        {
            let _ = current_exe;
            Err(std::io::Error::other("windows-only path"))
        }
    } else {
        // Unix: run the pinned installer with pipefail (a pipeline's exit
        // status is its LAST command — without pipefail a failed download
        // would feed empty stdin to bash, exit 0 and report success), then
        // relaunch cosh on the freed terminal.
        #[cfg(unix)]
        {
            let script_url =
                format!("https://raw.githubusercontent.com/{REPO}/{latest_tag}/download.sh");
            let script = format!(
                "set -o pipefail; curl -fsSL {script_url} | COSH_VERSION={latest_tag} bash; exec {current_exe}"
            );
            use std::os::unix::process::CommandExt;
            std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .process_group(0)
                .spawn()
        }
        #[cfg(not(unix))]
        {
            let _ = current_exe;
            Err(std::io::Error::other("unix-only path"))
        }
    };

    match result {
        Ok(child) => log::info!(
            "update pipeline spawned (pid {}, target {latest_tag}); closing cosh — the pipeline relaunches it when done",
            child.id()
        ),
        Err(err) => log::error!("failed to start update pipeline: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_tag_accepts_only_plain_tags() {
        assert!(is_valid_tag("v0.1.1"));
        assert!(is_valid_tag("1.2.3-rc1"));
        assert!(!is_valid_tag(""));
        assert!(!is_valid_tag("v1; rm -rf /"));
        assert!(!is_valid_tag("v1 $(echo pwn)"));
        assert!(!is_valid_tag("v1\n0.2"));
    }

    #[test]
    fn version_from_tag_strips_v() {
        assert_eq!(version_from_tag("v0.1.1"), "0.1.1");
        assert_eq!(version_from_tag("1.2.3"), "1.2.3");
    }

    #[test]
    fn is_newer_compares_numerically() {
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(is_newer("0.1", "0.1.1"));
        // Build metadata on the running version must not mask an update.
        assert!(is_newer("0.1.0+abc", "0.1.1"));
    }

    #[test]
    fn is_update_available_uses_semver() {
        assert!(is_update_available("0.1.0", "v0.1.1"));
        assert!(!is_update_available("0.1.1", "v0.1.1"));
        assert!(!is_update_available("0.2.0", "v0.1.9"));
    }
}
