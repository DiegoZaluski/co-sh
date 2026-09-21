//! `computer_apps` — list running desktop applications and their PIDs.
//!
//! Thin adapter over [`xa11y::App::list`]; every xa11y call is blocking
//! (platform accessibility APIs are synchronous) so it runs on tokio's
//! blocking pool.
use xa11y::{App, AppExt};

use super::types::{AppInfo, AppsOutput};

/// List running desktop applications visible to the OS accessibility tree.
///
/// The result is a point-in-time snapshot in platform enumeration order;
/// the entry actually holding the system foreground is flagged.
///
/// # Errors
///
/// Returns `Err` when the platform accessibility API is unreachable
/// (missing macOS Accessibility permission, no AT-SPI2 bus on Linux, …).
pub async fn apps() -> Result<AppsOutput, String> {
    tokio::task::spawn_blocking(|| {
        let list = App::list().map_err(|e| format!("computer_apps: {e}"))?;
        let apps: Vec<AppInfo> = list
            .iter()
            .map(|app| AppInfo {
                name: app.name.clone(),
                pid: app.pid,
                foreground: app.is_foreground(),
            })
            .collect();
        let count = apps.len();
        Ok(AppsOutput { apps, count })
    })
    .await
    .map_err(|e| format!("computer_apps: blocking task failed: {e}"))?
}
