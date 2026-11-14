//! `cosh update` — install the latest (or a pinned) release.
//!
//! The work is delegated to the release installer (`download.sh` /
//! `download.ps1`), exactly as a manual install would: it knows the archive
//! layout, the DLLs and the uninstaller that ships next to the binary, and it
//! is versioned WITH the release it installs. What this module adds is the
//! standard command line around it — version resolution, confirmation,
//! `COSH_VERSION`/`COSH_VARIANT` pinned to the running build, and the exit
//! status.
//!
//! The installer is downloaded into a private temp file and executed directly
//! instead of being piped through a shell: no tag or path is ever
//! interpolated into a command line.

use std::io::{IsTerminal, Write};
use std::process::Command;

use cosh::update::{REPO, fetch_latest_tag, is_update_available, normalize_tag};

use crate::cli::VERSION;
use crate::cli::style::Palette;

/// Options of `cosh update`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateOptions {
    /// Release to install (`v0.2.3` or `0.2.3`). `None` targets the latest
    /// published release.
    pub target: Option<String>,
    /// `--yes`: skip the confirmation prompt.
    pub assume_yes: bool,
    /// Internal flag set by the in-app updater: start the TUI again once the
    /// install lands. A user running `cosh update` from a shell stays in it.
    pub relaunch: bool,
}

/// Run the command and return the process exit code.
pub async fn run(options: UpdateOptions) -> i32 {
    let palette = Palette::load();

    let target = match resolve_target(&options, &palette).await {
        Ok(Some(tag)) => tag,
        // "already up to date" was reported; nothing to do.
        Ok(None) => return 0,
        Err(code) => return code,
    };

    match confirm(&palette, &target, options.assume_yes) {
        Confirmation::Accepted => {}
        Confirmation::Declined => {
            println!("{}", palette.muted("Cancelled."));
            return 1;
        }
        Confirmation::RequiresYes => {
            eprintln!(
                "{}",
                palette.error(
                    "refusing to update without confirmation — pass --yes for a non-interactive run"
                )
            );
            return 2;
        }
    }

    println!(
        "{}",
        palette.body(&format!(
            "Updating cosh v{VERSION} → {target} ({} variant)",
            variant()
        ))
    );

    let code = match install(&target).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}", palette.error(&error));
            return 1;
        }
    };

    if code != 0 {
        eprintln!(
            "{}",
            palette.error(&format!(
                "the installer exited with status {code} — cosh was left as it was"
            ))
        );
        return code;
    }

    if options.relaunch {
        if let Err(error) = relaunch() {
            eprintln!("{}", palette.error(&error));
            return 1;
        }
    } else {
        println!(
            "{}",
            palette.success(&format!(
                "cosh {target} installed — start it again to use the new version."
            ))
        );
    }
    0
}

/// Resolve the release tag to install. `Ok(None)` = already up to date.
async fn resolve_target(
    options: &UpdateOptions,
    palette: &Palette,
) -> Result<Option<String>, i32> {
    if let Some(raw) = &options.target {
        return match normalize_tag(raw) {
            Some(tag) => Ok(Some(tag)),
            None => {
                eprintln!(
                    "{}",
                    palette.error(&format!(
                        "invalid version '{raw}' — expected a release like v0.2.3"
                    ))
                );
                Err(2)
            }
        };
    }

    println!("{}", palette.muted("Checking the latest release…"));
    let Some(latest) = fetch_latest_tag().await else {
        eprintln!(
            "{}",
            palette.error(
                "could not reach the GitHub releases API — pass a version explicitly, e.g. `cosh update v0.2.3`"
            )
        );
        return Err(1);
    };
    if !is_update_available(VERSION, &latest.tag) {
        println!(
            "{}",
            palette.success(&format!(
                "cosh v{VERSION} is already up to date (latest: {})",
                latest.tag
            ))
        );
        return Ok(None);
    }
    Ok(Some(latest.tag))
}

enum Confirmation {
    Accepted,
    Declined,
    RequiresYes,
}

fn confirm(palette: &Palette, target: &str, assume_yes: bool) -> Confirmation {
    if assume_yes {
        return Confirmation::Accepted;
    }
    if !std::io::stdin().is_terminal() {
        return Confirmation::RequiresYes;
    }

    println!(
        "{}",
        palette.body(&format!(
            "This replaces the running cosh (v{VERSION}) with {target}."
        ))
    );
    print!("{} ", palette.body("Continue? [y/N]"));
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Confirmation::Declined;
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Confirmation::Accepted,
        _ => Confirmation::Declined,
    }
}

/// Download the pinned installer and run it with inherited stdio, so its
/// progress and errors land in the user's terminal.
async fn install(target: &str) -> Result<i32, String> {
    let script_name = if cfg!(windows) {
        "download.ps1"
    } else {
        "download.sh"
    };
    let url = format!("https://raw.githubusercontent.com/{REPO}/{target}/{script_name}");
    let response = reqwest::get(&url)
        .await
        .map_err(|error| format!("could not download the installer ({url}): {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "could not download the installer ({url}): HTTP {}",
            response.status()
        ));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("could not read the installer ({url}): {error}"))?;

    // Process-unique name: a predictable shared path would let a local
    // process swap the script between the download and its execution.
    let extension = if cfg!(windows) { "ps1" } else { "sh" };
    let script = std::env::temp_dir().join(format!(
        "cosh-update-{}.{extension}",
        std::process::id()
    ));
    std::fs::write(&script, &body)
        .map_err(|error| format!("could not write {}: {error}", script.display()))?;

    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File");
        command
    } else {
        Command::new("bash")
    };
    command
        .arg(&script)
        .env("COSH_VERSION", target)
        .env("COSH_VARIANT", variant());

    let status = command.status();
    let _ = std::fs::remove_file(&script);

    match status {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error) => Err(format!("could not run the installer: {error}")),
    }
}

/// The release variant this binary was built from — the installers default to
/// `slim`, which would silently drop local RAG from an `embed` build.
fn variant() -> &'static str {
    if cfg!(feature = "embed") {
        "slim-embed"
    } else {
        "slim"
    }
}

/// Start cosh again on the freshly installed image (in-app update only).
fn relaunch() -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not resolve the running executable: {error}"))?;
    let mut command = Command::new(&executable);
    // The in-app updater runs without a console on Windows; the relaunched
    // TUI needs one of its own to paint into.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cosh was updated but could not be started again: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variant_matches_the_build() {
        // A slim-embed release must keep installing slim-embed: the installer
        // defaults to slim, which would silently drop local RAG.
        #[cfg(feature = "embed")]
        assert_eq!(variant(), "slim-embed");
        #[cfg(not(feature = "embed"))]
        assert_eq!(variant(), "slim");
    }
}
