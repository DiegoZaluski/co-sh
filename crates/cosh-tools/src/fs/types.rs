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
