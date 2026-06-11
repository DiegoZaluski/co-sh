use super::types::FsMetadata;
use std::path::Path;

pub(crate) enum FsGuard {
    Allowed,
    Denied,
    Mismatch(String),
}

pub(crate) fn fs_guard(allows: FsMetadata, path: &str) -> FsGuard {
    let path = Path::new(path);

    let blocked = allows
        .write_path_blocklist
        .as_ref()
        .is_some_and(|list| list.iter().any(|&fs| fs == path || path.starts_with(fs)));
    let allowed = allows
        .write_path_allowlist
        .as_ref()
        .is_some_and(|list| list.contains(&path));
    let in_cwd = path.starts_with(allows.root);

    if blocked && allowed {
        return FsGuard::Mismatch(
            "Security Alert: path is in both blocklist and allowlist.".to_string(),
        );
    }
    if blocked {
        return FsGuard::Denied;
    }
    if !in_cwd && !allowed {
        return FsGuard::Denied;
    }

    FsGuard::Allowed
}
