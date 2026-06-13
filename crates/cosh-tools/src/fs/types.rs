use std::path::Path;

/// A single read specification.
pub struct Target<'a> {
    pub path: &'a str,
    pub line: Option<usize>,
    pub symbol: Option<&'a str>,
}

/// One or more file read operations.
///
/// ```ignore
/// ReadFile { read: vec![
///     Target { path: "src/main.rs", line: Some(5), symbol: None },
///     Target { path: "src/lib.rs", line: None, symbol: Some("run") },
/// ] }
/// ```
pub struct ReadFile<'a> {
    pub read: Vec<Target<'a>>,
}

pub struct TargetFile<'a> {
    pub text: &'a str,
    pub path: &'a str,
}

pub struct WriteAllFile<'a> {
    pub write: Vec<TargetFile<'a>>,
}

#[derive(Debug, Clone)]
pub struct FsMetadata<'a> {
    pub root: &'a Path,
    pub write_path_allowlist: Option<Vec<&'a Path>>,
    pub write_path_blocklist: Option<Vec<&'a Path>>,
}

// ___
#[derive(Debug, Clone, Default)]
pub struct EditTarget<'a> {
    pub path: &'a str,
    pub file_hash: &'a str,
    pub ops: &'a str,
}
#[derive(Debug, Clone, Default)]
pub struct EditFile<'a> {
    pub edit: Vec<EditTarget<'a>>,
}

//___
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) enum FsGuard {
    Allowed,
    Denied,
    Mismatch(String),
}

impl<'a> FsMetadata<'a> {
    #[allow(dead_code)]
    pub(crate) fn fs_guard(&self, path: &str) -> FsGuard {
        let path = Path::new(path);

        let blocked = self
            .write_path_blocklist
            .as_ref()
            .is_some_and(|list| list.iter().any(|&fs| fs == path || path.starts_with(fs)));
        let allowed = self
            .write_path_allowlist
            .as_ref()
            .is_some_and(|list| list.contains(&path));
        let in_cwd = path.starts_with(self.root);

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
}
