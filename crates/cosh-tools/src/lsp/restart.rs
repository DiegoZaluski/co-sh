//! `lsp_restart` engine: targeted or workspace-wide server restart.

use std::path::PathBuf;

use cosh_sdk::lsp::ClientKey;

use super::{support::Deps, types::RestartInput, types::RestartOutput};

/// Stop the server(s) serving `file_path` (or all running servers), then warm
/// them back up so the agent's next query does not pay the spawn latency.
fn key_names(keys: &[cosh_sdk::lsp::ClientKey]) -> String {
    keys.iter()
        .map(|key| key.server.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn run_restart(deps: &Deps<'_>, input: &RestartInput) -> Result<RestartOutput, String> {
    let keys: Vec<ClientKey> = match &input.file_path {
        Some(file_path) => {
            let path: PathBuf = deps.manager.root().join(file_path);
            deps.manager
                .matches_for_file(&path)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        }
        None => deps
            .manager
            .states()
            .into_iter()
            .map(|(key, _)| key)
            .collect(),
    };

    if keys.is_empty() {
        return Ok(RestartOutput {
            restarted: Vec::new(),
        });
    }

    let mut restarted = Vec::new();
    for key in &keys {
        deps.manager.stop_client(key).await;
        restarted.push(key.server.clone());
    }

    // Warm a scoped restart back up so the next query is instant — including
    // reopening the file (didOpen) on the fresh server, matching what the
    // tool description promises. Workspace-wide restarts respawn lazily;
    // warming every language here would undo the lazy-start guarantees.
    if let Some(file_path) = &input.file_path {
        let path: PathBuf = deps.manager.root().join(file_path);
        if let Ok(handles) = deps.manager.ensure_for_file(&path).await {
            for handle in &handles {
                if let Err(err) = handle.touch_file(&path).await {
                    log::warn!(
                        "restart of `{}` could not reopen `{}`: {err}",
                        key_names(&keys),
                        path.display()
                    );
                }
            }
        }
    }

    Ok(RestartOutput { restarted })
}
