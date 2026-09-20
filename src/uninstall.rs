//! `cosh-uninstall` — self-removal of the cosh CLI (Linux, macOS, Windows).
//!
//! Lives in its OWN binary (`src/bin/cosh_uninstall.rs`, shipped alongside
//! `cosh` by the release workflow and the download scripts): a separate
//! process can delete the running `cosh` image directly — on Windows the
//! main binary is unlinked while this uninstaller runs, and only the
//! uninstaller's own image needs the rename-and-defer trick.
//!
//! Flow (deliberately simple):
//!
//! 1. Ask ONCE, in plain text, why the user is uninstalling. The answer is
//!    optional — pressing Enter skips it. No suggestions, no ratings.
//! 2. Cleanup phase 1: remove the `cosh` binary plus the cosh profile
//!    directories (config, cache, and the data dir's non-telemetry entries).
//! 3. Telemetry: enqueue the `uninstall` event and flush the queue. The event
//!    queue LIVES inside the data dir (`data_dir/telemetry`), so this MUST
//!    happen before phase 4 or the event would be deleted with its own queue.
//! 4. Cleanup phase 2: remove the telemetry queue directory and the data dir
//!    itself. If the queue cannot be removed, the data dir is kept so the
//!    pending events can still reach a later flush.
//!
//! Every removal is best-effort: a failure is reported on stderr but the
//! uninstall never aborts half-way, and nothing here can panic.

use std::io::Write as _;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::telemetry::Telemetry;
use crate::telemetry::events::{
    AppVersion, EventEnvelope, EventPayload, OccurredAt, UninstallPayload,
};
use crate::telemetry::schema::EventType;

/// The single, direct uninstall question. Deliberately bare: no reason
/// suggestions, no multiple choice — the user types freely or presses Enter.
const REASON_PROMPT: &str = concat!(
    "Before cosh is removed, would you tell us why you are uninstalling?\n",
    "(optional — press Enter to skip)\n> "
);

/// Directory name of the telemetry queue inside the platform data dir.
/// MUST stay in sync with [`crate::telemetry::queue_dir`].
const TELEMETRY_DIR_NAME: &str = "telemetry";

/// Run the uninstall flow. `consented_in_config` is the persisted telemetry
/// consent flag (same source `Telemetry::resolve` expects).
///
/// With `dry_run` (cosh-uninstall --dry-run) NOTHING is deleted and NO
/// telemetry is sent: the same prompt is shown, and every phase runs in a
/// report-only mode that lists exactly what a real run would remove —
/// binaries, profile directories and which keyring credentials exist. The
/// return value is still the process exit code (always `0`).
pub async fn run(consented_in_config: bool, dry_run: bool) -> i32 {
    // 1. Ask why (optional; Enter skips). Same prompt in both modes.
    let reason = ask_reason();

    if dry_run {
        report_dry_run(reason.as_deref());
        return 0;
    }

    // 2. Cleanup phase 1: binaries + profile dirs + OS keyring credentials
    //    (the telemetry queue survives until after the flush).
    let binary_removed = remove_binaries();
    let data_removed = remove_profile_dirs();
    let keyring_removed = remove_keyring_credentials();

    // 3. Telemetry: enqueue + flush BEFORE the queue's own directory dies.
    //    The queue may only be deleted when nothing pending is left in it.
    let telemetry = Telemetry::resolve(consented_in_config);
    let queue_flushed = if telemetry.enabled() {
        enqueue_and_flush(
            &telemetry,
            reason.as_deref(),
            binary_removed,
            data_removed,
            keyring_removed,
        )
        .await
    } else {
        // Opted out: no event was (or may be) enqueued by us, and honoring
        // the user's choice includes discarding whatever lingers.
        true
    };

    // 4. Cleanup phase 2: telemetry queue + data dir.
    let queue_removed = queue_flushed && remove_telemetry_and_data_dir();

    print_summary(binary_removed, data_removed, keyring_removed, queue_removed);
    0
}

/// The persisted telemetry consent flag from `setup.json` (DEFAULT ON —
/// opt-out, same semantics as the TUI's private `Setup` loader, which this
/// standalone binary cannot see). Honors the same `COSH_CONFIG_DIR` override
/// the TUI uses so tests and power users get identical behavior. The
/// effective consent still goes through `Telemetry::resolve` (env override +
/// CI hard-off) — this is only the user's persisted choice.
pub fn consented_in_config() -> bool {
    let config_dir = match std::env::var("COSH_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => {
            match directories::ProjectDirs::from("", "", "cosh") {
                Some(proj) => proj.config_dir().to_path_buf(),
                // No resolvable config dir → no persisted opt-out → default ON.
                None => return true,
            }
        }
    };
    let Ok(content) = std::fs::read_to_string(config_dir.join("setup.json")) else {
        return true; // missing config → default ON (opt-out)
    };
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v.get("telemetry").and_then(serde_json::Value::as_bool))
        .unwrap_or(true) // legacy/corrupt file: the TUI's serde(default) is ON
}

/// Report-only pass for `--dry-run`: lists exactly what a real run would
/// remove. Strictly read-only — existence checks and keyring READS only
/// (`get_password`), never a delete, and no telemetry is enqueued or sent.
fn report_dry_run(reason: Option<&str>) {
    say("DRY RUN — nothing was removed. A real run would:");
    match reason {
        Some(answer) => say(format!("  - record your feedback: \"{answer}\"").as_str()),
        None => say("  - skip the (empty) feedback answer"),
    }

    // Binaries: the sibling pair the download scripts install.
    if let Ok(self_exe) = std::env::current_exe() {
        let main = self_exe.with_file_name(cosh_binary_name());
        say(if main.exists() {
            "  - remove the main cosh binary:"
        } else {
            "  - (main cosh binary already absent — nothing to remove at)"
        });
        say(format!("        {}", main.display()).as_str());
        say(format!("  - remove this uninstaller: {}", self_exe.display()).as_str());
    } else {
        say("  - remove the cosh binaries (location undeterminable in this run)");
    }

    // Profile directories.
    if let Some(dirs) = project_dirs() {
        say("  - remove the cosh profile directories:");
        for dir in [dirs.config_dir(), dirs.cache_dir(), dirs.data_dir()] {
            let state = if dir.exists() { "exists" } else { "absent " };
            say(format!("        [{state}] {}", dir.display()).as_str());
        }
    } else {
        say("  - remove the cosh profile directories (location undeterminable)");
    }

    // Keyring credentials: READ-ONLY check (get_password), never delete.
    let mut stored = 0usize;
    say("  - remove these provider API keys from the OS credential store:");
    for (provider, env_var) in cosh_sdk::connector::known_providers_with_env() {
        let has_key = keyring::Entry::new(cosh_sdk::connector::COSH_SERVICE, env_var)
            .ok()
            .and_then(|e| e.get_password().ok())
            .is_some();
        if has_key {
            stored += 1;
            say(format!("        [stored] {provider} ({env_var})").as_str());
        }
    }
    if stored == 0 {
        say("        (none found — nothing to clean)");
    }

    // Telemetry note (nothing is sent in dry-run, even with consent on).
    if Telemetry::resolve(consented_in_config()).enabled() {
        say("  - send the uninstall telemetry event (skipped in dry-run)");
    }
    say("Dry run complete — your installation is untouched. Run without --dry-run to uninstall.");
}

/// Print the one-shot question and read a single line from stdin. `None` on
/// EOF, read error, or an empty (Enter-only) answer.
fn ask_reason() -> Option<String> {
    // Inline prompt: ends in "> ", so it must NOT get an automatic newline.
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(REASON_PROMPT.as_bytes());
    let _ = out.flush();
    drop(out);
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
    }
}

/// Best-effort line write to stdout: a closed/broken pipe (`cosh uninstall |
/// head`) is IGNORED — `print!`/`println!` would panic, and a closed stdout
/// must never abort an uninstall.
fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Delete the main `cosh` binary and this uninstaller's own image.
///
/// The download scripts install BOTH binaries side by side in the same
/// directory (`cosh` + `cosh-uninstall`, `.exe` on Windows). Because this is
/// a separate process:
///
/// - The main `cosh` binary is unlinked DIRECTLY on every platform (nothing
///   is running from it while the uninstaller executes).
/// - This uninstaller's own image: Unix can unlink a running executable;
///   Windows cannot, so it is renamed to `<name>.old` (renaming a running
///   image IS allowed), immediate deletion is attempted, and any failure is
///   handed to a detached PowerShell that runs after this process exits.
fn remove_binaries() -> bool {
    let Ok(self_exe) = std::env::current_exe() else {
        return false;
    };
    let main = self_exe.with_file_name(cosh_binary_name());
    // Main binary FIRST: the uninstaller's own image must never be the
    // reason `cosh` survives. A missing main binary (never installed,
    // already removed, or a differently-named manual copy) is a success —
    // there is nothing left to clean.
    let main_removed = !main.exists() || std::fs::remove_file(&main).is_ok();
    if !main_removed {
        eprintln!("warning: could not remove {}", main.display());
    }
    let self_removed = remove_own_image(&self_exe);
    main_removed && self_removed
}

/// File name of the main cosh binary, as installed next to this uninstaller.
fn cosh_binary_name() -> &'static str {
    if cfg!(windows) {
        "cosh.exe"
    } else {
        "cosh"
    }
}

/// `<name>.old` sibling of `exe` (append, never replace the `.exe` suffix).
#[cfg(windows)]
fn old_binary_path(exe: &Path) -> PathBuf {
    let mut os = exe.as_os_str().to_os_string();
    os.push(".old");
    PathBuf::from(os)
}

/// Remove THIS uninstaller's own image (platform-specific, see
/// [`remove_binaries`]).
#[cfg(not(windows))]
fn remove_own_image(exe: &Path) -> bool {
    std::fs::remove_file(exe).is_ok()
}

#[cfg(windows)]
fn remove_own_image(exe: &Path) -> bool {
    let old = old_binary_path(exe);
    // Clear stale leftovers from a previous interrupted uninstall.
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(exe, &old).is_err() {
        return false;
    }
    if std::fs::remove_file(&old).is_ok() {
        return true;
    }
    // The image lock survives the rename: schedule the delete for after this
    // process exits. Detached and silent; best-effort by design.
    let script = format!(
        "Start-Sleep -Seconds 1; Remove-Item -Force -LiteralPath '{}' -ErrorAction SilentlyContinue",
        old.display().to_string().replace('\'', "''")
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .spawn();
    true
}

/// Platform profile directories for cosh (the SAME resolution the telemetry
/// module and the rest of the app use).
fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "cosh")
}

/// Best-effort recursive removal: a missing path counts as removed, a failure
/// is reported but never aborts the caller.
fn remove_tree_best_effort(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let ok = if path.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        std::fs::remove_file(path).is_ok()
    };
    if !ok {
        eprintln!("warning: could not remove {}", path.display());
    }
    ok
}

/// Remove the config and cache dirs, plus everything inside the data dir
/// EXCEPT the telemetry queue (which still has to be flushed). Returns `true`
/// only when every target was removed (or never existed).
fn remove_profile_dirs() -> bool {
    let Some(dirs) = project_dirs() else {
        return false;
    };
    let mut ok = true;
    for dir in [dirs.config_dir(), dirs.cache_dir()] {
        ok &= remove_tree_best_effort(dir);
    }
    let data = dirs.data_dir();
    if data.is_dir() && let Ok(entries) = std::fs::read_dir(data) {
        for entry in entries.flatten() {
            if entry.file_name() == TELEMETRY_DIR_NAME {
                continue; // flushed and removed in phase 2
            }
            ok &= remove_tree_best_effort(&entry.path());
        }
    }
    ok
}

/// Phase 2: remove the telemetry queue directory, then the data dir itself.
/// If the queue cannot be removed its events are kept for a later flush —
/// the data dir stays with it.
fn remove_telemetry_and_data_dir() -> bool {
    let Some(dirs) = project_dirs() else {
        return false;
    };
    let data = dirs.data_dir();
    if remove_tree_best_effort(&data.join(TELEMETRY_DIR_NAME)) {
        if data.is_dir() {
            let empty = std::fs::read_dir(data).is_ok_and(|mut it| it.next().is_none());
            if empty {
                let _ = std::fs::remove_dir(data);
            }
        }
        true
    } else {
        false
    }
}

/// Enqueue the `uninstall` event and flush the queue. Returns `true` when the
/// queue has NO pending events left (flush succeeded, or nothing was ever
/// enqueued) — only then may the queue directory be deleted. Best-effort end
/// to end: no entropy, no version validation or no ingest config → nothing is
/// sent and the uninstall continues regardless.
async fn enqueue_and_flush(
    telemetry: &Telemetry,
    reason: Option<&str>,
    binary_removed: bool,
    data_removed: bool,
    keyring_removed: bool,
) -> bool {
    let Some(install_id) = crate::telemetry::install_id() else {
        return true; // no install id → no event was enqueued
    };
    let Some(version) = AppVersion::validate(env!("CARGO_PKG_VERSION")) else {
        return true; // same: no envelope, nothing pending from us
    };
    let payload = UninstallPayload::new(
        &install_id,
        reason,
        binary_removed,
        data_removed,
        keyring_removed,
    );
    let Some(envelope) = EventEnvelope::new(
        EventType::Uninstall,
        &version,
        None,
        &install_id,
        None,
        OccurredAt::now(),
        EventPayload::Uninstall(payload),
    ) else {
        return true; // construction failed → nothing enqueued
    };
    if !telemetry.enqueue(&envelope) {
        log::warn!("telemetry: uninstall event not enqueued (disabled or invalid)");
        return true;
    }
    if let Some(config) = crate::telemetry::sink::SinkConfig::from_env() {
        telemetry.flush(&config).await;
    }
    // The authoritative answer: after the flush attempt, does the queue still
    // hold events (ours re-queued on a transient failure, or older pending
    // ones)? Non-empty → keep the queue so they can reach a later flush.
    telemetry.queue().pending_count() == 0
}

/// Remove the cosh PROVIDER API keys from the OS credential store (macOS
/// Keychain, Windows Credential Manager, Linux Secret Service/gnome-keyring).
/// The TUI stores provider keys under the service `"cosh"`, one entry per
/// known provider (the entry "user" is the provider's env-var name). Scope
/// note: MCP server credentials (`mcp:<server>` users under the same
/// service) are NOT covered — server names are dynamic and not enumerable
/// from the static provider table; removing them would require the (now
/// deleted) MCP config. A missing or already-deleted entry counts as
/// removed; real failures are aggregated into ONE warning and make the
/// overall result `false` (surfaced in the uninstall summary).
fn remove_keyring_credentials() -> bool {
    let mut failures = 0usize;
    for (_provider, env_var) in cosh_sdk::connector::known_providers_with_env() {
        let removed = match keyring::Entry::new(cosh_sdk::connector::COSH_SERVICE, env_var) {
            Ok(entry) => match entry.delete_credential() {
                Ok(()) => true,
                // NoEntry is the normal case for providers whose key was
                // never stored (env-var-only users) — nothing to clean.
                Err(keyring::Error::NoEntry) => true,
                Err(_) => false,
            },
            Err(_) => false,
        };
        if !removed {
            failures += 1;
        }
    }
    if failures > 0 {
        // One aggregated warning instead of one line per provider — a locked
        // or headless keyring would otherwise print 26 near-identical lines.
        eprintln!(
            "warning: could not remove {failures} API key(s) from the OS credential store"
        );
    }
    failures == 0
}

/// Post-uninstall report: what was removed and the ONE manual step that may
/// remain (the PATH entry the install scripts printed instructions for).
/// Every line goes through [`say`] — a closed stdout must never panic here.
fn print_summary(
    binary_removed: bool,
    data_removed: bool,
    keyring_removed: bool,
    queue_removed: bool,
) {
    if binary_removed && data_removed && keyring_removed && queue_removed {
        say("cosh was uninstalled. Thank you for trying it!");
        return;
    }
    say("cosh uninstall finished with warnings:");
    if !binary_removed {
        say("  - the cosh binary could not be removed automatically");
    }
    if !data_removed {
        say("  - some cosh data/config directories could not be removed");
    }
    if !keyring_removed {
        say("  - some API keys could not be removed from the OS credential store");
    }
    if !queue_removed {
        say("  - the telemetry queue was kept (pending events) — remove it later");
    }
    say("If your PATH still references the cosh install dir, remove that entry manually.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn old_binary_path_appends_suffix() {
        let p = PathBuf::from("/opt/bin/cosh.exe");
        assert_eq!(old_binary_path(&p), PathBuf::from("/opt/bin/cosh.exe.old"));
        let p = PathBuf::from("/opt/bin/cosh");
        assert_eq!(old_binary_path(&p), PathBuf::from("/opt/bin/cosh.old"));
    }

    #[test]
    fn remove_tree_best_effort_handles_missing_and_nested() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing");
        assert!(remove_tree_best_effort(&missing));
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).expect("create");
        std::fs::write(nested.join("f"), b"x").expect("write");
        assert!(remove_tree_best_effort(&dir.path().join("a")));
        assert!(!dir.path().join("a").exists());
    }

    #[test]
    fn project_dirs_resolves_or_fails_cleanly() {
        // Either resolution works or it returns None — never a panic. The
        // important property is that it is the SAME resolution telemetry
        // uses (same ProjectDirs call), asserted implicitly by construction.
        let dirs = project_dirs();
        if let Some(d) = dirs {
            assert!(!d.data_dir().as_os_str().is_empty());
            assert!(!d.config_dir().as_os_str().is_empty());
        }
    }

    #[test]
    fn cosh_binary_name_matches_platform() {
        if cfg!(windows) {
            assert_eq!(cosh_binary_name(), "cosh.exe");
        } else {
            assert_eq!(cosh_binary_name(), "cosh");
        }
    }

    /// `COSH_CONFIG_DIR` is process-global state: env-touching tests serialize
    /// on this lock so parallel test threads cannot race each other's value.
    static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn consented_in_config_reads_telemetry_flag() {
        let _guard = ENV_LOCK.lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("cosh-config");
        std::fs::create_dir_all(&config).expect("mkdir");
        let path = config.join("setup.json");
        // SAFETY (edition 2024): single-threaded with respect to env mutation
        // via ENV_LOCK; the value is restored (removed) before returning.
        unsafe { std::env::set_var("COSH_CONFIG_DIR", &config) };

        // Missing setup.json → default ON (opt-out).
        assert!(consented_in_config());

        // Explicit opt-out must win.
        std::fs::write(&path, br#"{"telemetry": false}"#).expect("write");
        assert!(!consented_in_config());

        // Explicit on.
        std::fs::write(&path, br#"{"telemetry": true}"#).expect("write");
        assert!(consented_in_config());

        // Legacy file without the flag → serde(default) semantics are ON.
        std::fs::write(&path, br#"{"other": 1}"#).expect("write");
        assert!(consented_in_config());

        // Corrupt JSON → default ON (same as the TUI's unwrap_or_default).
        std::fs::write(&path, b"not json").expect("write");
        assert!(consented_in_config());

        unsafe { std::env::remove_var("COSH_CONFIG_DIR") };
    }
}
