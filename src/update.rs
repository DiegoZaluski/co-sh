//! GitHub release check and in-app update pipeline.
//!
//! At boot a background task queries the GitHub releases API for the latest
//! published release. When its tag is newer than the running binary the UI
//! announces the update (session footer hint, update toast, home banner)
//! and offers both the in-app pipeline and the manual install command.

use serde::Deserialize;

/// GitHub repo the releases are published under (matches `download.sh`).
pub const REPO: &str = "DiegoZaluski/co-sh";

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

/// Latest published release, regardless of whether it is newer than the
/// running build.
///
/// `cosh update` needs the tag even when it equals (or trails) the current
/// version: the user may be pinning it deliberately. The boot check uses
/// [`fetch_latest_release`], which applies the "is there anything new"
/// filter on top of this one.
pub async fn fetch_latest_tag() -> Option<LatestRelease> {
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
    Some(LatestRelease {
        version: version_from_tag(&tag),
        url: release
            .html_url
            .unwrap_or_else(|| format!("https://github.com/{REPO}/releases")),
        tag,
    })
}

/// Query the GitHub releases API for the latest published release.
/// Returns `None` on any network/parse error (the banner simply stays
/// hidden — a failed check must never degrade the app) and when the running
/// version is already current.
pub async fn fetch_latest_release() -> Option<LatestRelease> {
    let release = fetch_latest_tag().await?;
    is_update_available(env!("CARGO_PKG_VERSION"), &release.tag).then_some(release)
}

/// Normalize a user-supplied version (`0.2.3`, `v0.2.3`, `v0.2.3-rc1`) into
/// the release-tag form (`v0.2.3`).
///
/// Mirrors the installers' own check (`^v?[0-9]+\.[0-9]+\.[0-9]+(-.*)?$`), so
/// a value that passes here is one `download.sh`/`download.ps1` accepts —
/// the tag is then pinned into the installer URL and `COSH_VERSION`.
pub fn normalize_tag(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let version = trimmed.strip_prefix('v').unwrap_or(trimmed);
    // The pre-release suffix is left to `is_valid_tag` below: only the
    // numeric core is checked here.
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    if [major, minor, patch]
        .iter()
        .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let tag = format!("v{version}");
    is_valid_tag(&tag).then_some(tag)
}

/// The manual update command pinned to `latest_tag`, shown in the update
/// toast so the user can update by hand from their own terminal (the ⧉
/// button copies it).
///
/// It is the CLI's own command — the same code path the in-app updater runs,
/// so there is exactly one documented way to install a release and no
/// OS-specific shell one-liner to keep in sync with the installers.
pub fn manual_update_command(latest_tag: &str) -> Option<String> {
    if !is_valid_tag(latest_tag) {
        log::warn!("manual update command: invalid release tag {latest_tag:?}");
        return None;
    }
    Some(format!("cosh update {latest_tag}"))
}

/// Fire-and-forget the in-app update: start this same binary as
/// `cosh update <tag> --yes --relaunch`.
///
/// The command line is the single implementation of "install a release"
/// (`crate::cli::update`): it downloads the pinned installer script, runs it
/// with `COSH_VERSION`/`COSH_VARIANT` set, and — with `--relaunch` — starts
/// cosh again on the new image. This function only starts that process
/// detached (own process group on Unix, no console window on Windows) and
/// returns so the caller can close the TUI: the terminal visibly closes and
/// reopens when the update lands. Errors (failed spawn, invalid tag) are just
/// logged; there is no in-app retry surface.
pub fn spawn_update_pipeline(latest_tag: &str) {
    // The tag lands on a shell command line and inside download URLs:
    // validate before anything runs (defense-in-depth; the release check
    // already rejects malformed tags).
    if !is_valid_tag(latest_tag) {
        log::error!("update pipeline: invalid release tag {latest_tag:?}");
        return;
    }

    let Some(current_exe) = std::env::current_exe().ok() else {
        log::error!("update pipeline: cannot resolve the running executable path");
        return;
    };

    let mut command = std::process::Command::new(&current_exe);
    command
        .arg("update")
        .arg(latest_tag)
        .arg("--yes")
        .arg("--relaunch")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Detach from the terminal the TUI is about to restore: its own process
    // group on Unix (no Ctrl-C fan-out while the installer runs), and no
    // console window on Windows. CREATE_NO_WINDOW only: DETACHED_PROCESS is
    // mutually exclusive with it and the combined bitmask can make
    // CreateProcess fail outright.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    match command.spawn() {
        Ok(child) => log::info!(
            "update pipeline spawned (pid {}, target {latest_tag}); closing cosh — `cosh update` relaunches it when done",
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
    fn normalize_tag_pins_the_release_form() {
        assert_eq!(normalize_tag("0.2.3"), Some("v0.2.3".into()));
        assert_eq!(normalize_tag("v0.2.3"), Some("v0.2.3".into()));
        assert_eq!(normalize_tag(" v0.2.3-rc1 "), Some("v0.2.3-rc1".into()));
    }

    #[test]
    fn normalize_tag_rejects_what_the_installer_would_not_accept() {
        for raw in ["", "0.2", "0.2.3.4", "v0.2.x", "latest", "0.2.3; rm -rf /"] {
            assert_eq!(normalize_tag(raw), None, "must reject {raw:?}");
        }
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
