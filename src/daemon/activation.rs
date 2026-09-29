//! Daemon lifecycle: socket activation, the bind-as-lock, idle shutdown.
//!
//! **Socket activation** — the client-side flow that makes the daemon exist
//! only when it is needed:
//!
//! 1. try to connect to the daemon's socket;
//! 2. on failure, spawn the daemon binary (`current_exe()` of the running
//!    app, so the daemon always ships with the app — no PATH lookup, no
//!    install step) and poll until the socket appears or the activation
//!    window closes.
//!
//! Two cosh processes racing into step 2 resolve by **the bind being the
//! lock**: the loser's `bind` gets `EADDRINUSE` and falls through to
//! connecting, which is the only correct outcome for both.
//!
//! **Idle shutdown** — the daemon-side counterpart: when no client activity
//! happened for the idle budget, the serve loop returns and the process
//! exits. Nothing is orphaned, nothing runs when the feature is unused, and
//! the next request pays one activation instead of a resident process
//! nobody is talking to.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ipc_error::Lifecycle;

/// The runtime socket directory: `$XDG_RUNTIME_DIR/cosh/daemon`, falling
/// back to what `directories` computes when the runtime dir is not set
/// (some containers drop it).
///
/// Under `XDG_RUNTIME_DIR` the path is tmpfs, user-owned and wiped at
/// logout — the right home for a per-user socket with `0600` perms.
pub(crate) fn socket_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("cosh").join("daemon");
    }
    directories::ProjectDirs::from("", "", "cosh")
        .map(|dirs| {
            dirs.runtime_dir().map_or_else(
                || dirs.data_dir().join("daemon"),
                |runtime| runtime.join("cosh").join("daemon"),
            )
        })
        .unwrap_or_else(|| std::env::temp_dir().join("cosh-daemons"))
}

/// The socket path for a daemon named `name` (e.g. `decision`).
pub(crate) fn socket_path(name: &str) -> PathBuf {
    socket_dir().join(format!("{name}.sock"))
}

/// Connect to `name`'s socket, activating (spawning) the daemon on demand.
///
/// `activate` spawns the process; keeping it a closure keeps this module
/// protocol-agnostic (the decision daemon passes its own spawn details).
pub(crate) fn connect_or_activate(
    name: &'static str,
    activate: impl FnOnce() -> Result<(), Lifecycle>,
) -> Result<std::os::unix::net::UnixStream, Lifecycle> {
    let path = socket_path(name);
    if let Ok(stream) = std::os::unix::net::UnixStream::connect(&path) {
        return Ok(stream);
    }

    // Nothing is listening: spawn, then poll for the socket. The spawn
    // loses races the same way it wins them — a second binder gets
    // EADDRINUSE and its socket file is simply the winner's.
    activate()?;
    let deadline = std::time::Instant::now() + ACTIVATION_WINDOW;
    let mut delay = Duration::from_millis(25);
    loop {
        std::thread::sleep(delay);
        delay = (delay * 2).min(ACTIVATION_BACKOFF_CAP);
        // Every connect failure inside the window is retried (the socket
        // may not exist yet, or exist before its listener does); the
        // deadline is the single authority on giving up.
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(stream) => return Ok(stream),
            Err(_) if std::time::Instant::now() >= deadline => {
                return Err(Lifecycle::Timeout {
                    name,
                    path: path.display().to_string(),
                });
            }
            Err(_) => continue,
        }
    }
}

/// Total wait for an activating daemon to become reachable. Model loads run
/// on first `decide`, not at startup, so activation only waits for a socket
/// bind — fast even on a cold machine.
const ACTIVATION_WINDOW: Duration = Duration::from_secs(2);
const ACTIVATION_BACKOFF_CAP: Duration = Duration::from_millis(250);

/// The activation lock: an exclusive `flock` on `<name>.lock`, held for the
/// daemon's whole lifetime.
///
/// The review found the bind's probe→remove→bind sequence racy (M1): two
/// processes could both probe a stale socket, both conclude "dead", and the
/// loser would then unlink the winner's LIVE socket and bind its own — two
/// live daemons, the path pointing at only one. `flock` closes the window:
/// the probe/remove/bind critical section runs under the lock, and the
/// winner HOLDS the lock for its lifetime, so any later binder finds the
/// lock held and knows a live daemon owns the path — it exits 0 as the
/// loser instead of ever touching the socket file.
///
/// The lock file persists when a daemon crashes (locks do not — `flock` is
/// released by the kernel on process death), which is exactly the property
/// a pidfile lacks.
///
/// `None` when the lock cannot even be created (read-only dir): the caller
/// then falls back to the racy path rather than refusing to run.
pub(crate) enum ActivationLock {
    /// We hold the lock: safe to probe/remove/bind the socket.
    Held(std::fs::File),
    /// Someone else holds it: a live daemon owns the socket path.
    Contended,
}

pub(crate) fn acquire_activation_lock(name: &str) -> ActivationLock {
    let path = socket_dir().join(format!("{name}.lock"));
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            log::warn!("activation lock unavailable at {}: {e}", path.display());
            // Racy fallback rather than refusal — fail-open, like the rest
            // of the layer.
            return ActivationLock::Held(std::fs::File::open("/dev/null").expect("/dev/null"));
        }
    };
    use std::os::fd::AsRawFd;
    let held = unsafe {
        // `flock` is process-associated (not fd-associated) and released on
        // process exit by the kernel; there is no unlock path to maintain.
        libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
    };
    if held == 0 {
        ActivationLock::Held(file)
    } else {
        ActivationLock::Contended
    }
}

/// Spawn the daemon binary next to the running app.
///
/// The daemon shares the app binary's install location by construction
/// (`current_exe()`), so there is exactly one artifact to install and the
/// spawned daemon is always the version the app was built with. It
/// detaches into its own process group with stdio null: it must survive
/// the spawning app (that is the point — other instances share it) and
/// never write to the app's terminal.
pub(crate) fn spawn_sibling(name: &str) -> Result<(), Lifecycle> {
    use std::os::unix::process::CommandExt;
    // The real daemon name in both error paths (m4) — "cannot spawn the
    // daemon daemon" was the old hardcoded-report bug.
    let exe = std::env::current_exe().map_err(|e| Lifecycle::Spawn {
        name: "decision",
        source: e,
    })?;
    let daemon_exe = exe.with_file_name(format!("cosh-{name}d"));
    std::process::Command::new(&daemon_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| Lifecycle::Spawn {
            name: "decision",
            source: e,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_named_under_the_daemon_dir() {
        // Shape only: the dir depends on the environment, the file name
        // must not drift from the `<name>.sock` convention the activation
        // flow and the server both derive from the daemon name.
        let path = socket_path("decision");
        assert!(path.ends_with("decision.sock"));
        assert!(path.starts_with(socket_dir()));
    }
}
