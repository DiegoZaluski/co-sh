//! `cosh-uninstall` — standalone uninstaller for the cosh CLI.
//!
//! A separate binary (shipped alongside `cosh` in the release archives) so
//! the running `cosh` image can be deleted directly on every platform — only
//! this uninstaller's own image needs the Windows rename-and-defer trick.
//! The whole flow (reason prompt, telemetry, cleanup) lives in
//! `cosh::uninstall`; this file is just the entry point.
//!
//! `cosh-uninstall --dry-run` previews the exact same flow without deleting
//! anything or sending telemetry.

#[tokio::main]
async fn main() {
    // try_init, never init: a logging failure (unwritable scratch dir, etc.)
    // must not abort the process before the cleanup — `cosh::uninstall`'s
    // documented invariant is that nothing panics before the uninstall runs.
    let _ = cosh::util::logger::try_init("uninstall");
    let dry_run = std::env::args().skip(1).any(|a| a == "--dry-run");
    let consented = cosh::uninstall::consented_in_config();
    let code = cosh::uninstall::run(consented, dry_run).await;
    std::process::exit(code);
}
