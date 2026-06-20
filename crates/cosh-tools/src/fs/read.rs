//! Read file(s) and format the output as hashline sections.
//!
//! Each call to [`read`] carries one or more [`Target`] entries.  A target can
//! request the whole file, a syntactic block at a given line, or a definition
//! block matching a name (symbol, struct, class, …).  When the name search
//! targets a directory the entire tree is walked recursively.
use super::types::{FsRead, Target};

use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    normalize,
    types::BlockSpan,
};
use cosh_sdk::rollback;
use std::path::Path;

#[derive(Debug)]
pub struct ReadResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub content: String,
    pub warnings: Option<String>,
}

/// Run every target and return hashline-formatted results.
///
/// Each target produces one or more [`ReadResult`] entries. Errors and
/// incomplete reads are recorded as warnings inside each result instead of
/// aborting the entire operation.
pub async fn read(config: &FsRead, targets: Vec<Target<'_>>) -> Vec<ReadResult> {
    let _ = config;
    let fs = DiskFilesystem::new();
    let mut results: Vec<ReadResult> = Vec::new();

    for target in targets {
        results.extend(read_target(fs.clone(), target).await);
    }

    results
}

async fn read_target(fs: DiskFilesystem, target: Target<'_>) -> Vec<ReadResult> {
    if let Some(name) = target.symbol {
        return search_symbol(&fs, target.path, name).await;
    }

    if Path::new(target.path).is_dir() {
        return vec![ReadResult {
            path: target.path.to_string(),
            file_hash: String::new(),
            header: String::new(),
            content: String::new(),
            warnings: Some(format!(
                "cannot read directory `{path}` without a `symbol` filter. \
                 When `path` is a directory, a `symbol` (e.g., a function or struct name) must be \
                 provided so the tool searches for matching definitions across all supported source \
                 files in that tree. \
                 To read entire files, list each file path explicitly in the `read` array with no \
                 `line` or `symbol` fields.",
                path = target.path
            )),
        }];
    }

    let text = match read_normalized(&fs, target.path).await {
        Ok(t) => t,
        Err(e) => {
            return vec![ReadResult {
                path: target.path.to_string(),
                file_hash: String::new(),
                header: String::new(),
                content: String::new(),
                warnings: Some(e),
            }];
        }
    };

    let _ = rollback::record(target.path, &text);

    let hash = format::compute_file_hash(&text);
    let header = format::format_hashline_header(target.path, &hash);
    let body = format::format_numbered_lines(&text, 1);

    if let Some(line) = target.line {
        let ts = cosh_sdk::tree_sitter::tree_sitter();
        let ln: u32 = match line.try_into() {
            Ok(l) => l,
            Err(e) => {
                return vec![ReadResult {
                    path: target.path.to_string(),
                    file_hash: hash,
                    header,
                    content: body,
                    warnings: Some(format!("cannot convert line {line}: {e}")),
                }];
            }
        };
        match ts.resolve_block(target.path, &body, ln) {
            Some(span) => {
                let block = extract_block(&body, span);
                vec![ReadResult {
                    path: target.path.to_string(),
                    file_hash: hash,
                    header,
                    content: block,
                    warnings: None,
                }]
            }
            None => vec![ReadResult {
                path: target.path.to_string(),
                file_hash: hash,
                header,
                content: body,
                warnings: Some(format!(
                    "could not resolve a syntactic block starting at line {line} in `{path}`. \
                         Possible causes: the line does not begin a valid block (e.g. fn, struct, \
                         impl, enum, trait, mod), the line number exceeds the file length, or the \
                         line falls inside a string or comment. \
                         Try a different line number or read the whole file instead.",
                    path = target.path
                )),
            }],
        }
    } else {
        vec![ReadResult {
            path: target.path.to_string(),
            file_hash: hash,
            header: header.clone(),
            content: format!("{header}\n{body}"),
            warnings: None,
        }]
    }
}

async fn search_symbol(fs: &DiskFilesystem, path: &str, name: &str) -> Vec<ReadResult> {
    let paths = match if Path::new(path).is_dir() {
        Ok(collect_source_files(Path::new(path)))
    } else if cosh_sdk::tree_sitter::language::detect_language(path).is_some() {
        Ok(vec![path.to_string()])
    } else {
        Err(format!(
            "no tree-sitter grammar available for `{path}`; use `line` targeting or read the whole file instead"
        ))
    } {
        Ok(p) => p,
        Err(e) => {
            return vec![ReadResult {
                path: path.to_string(),
                file_hash: String::new(),
                header: String::new(),
                content: String::new(),
                warnings: Some(e),
            }];
        }
    };

    let ts = cosh_sdk::tree_sitter::tree_sitter();
    let mut results: Vec<ReadResult> = Vec::new();

    for p in &paths {
        let text = match read_normalized(fs, p).await {
            Ok(t) => t,
            Err(e) => {
                results.push(ReadResult {
                    path: p.clone(),
                    file_hash: String::new(),
                    header: String::new(),
                    content: String::new(),
                    warnings: Some(e),
                });
                continue;
            }
        };
        if let Some(span) = ts.resolve_symbol(p, &text, name) {
            let _ = rollback::record(p, &text);
            let hash = format::compute_file_hash(&text);
            let header = format::format_hashline_header(p, &hash);
            let body = format::format_numbered_lines(&text, 1);
            let block = extract_block(&body, span);
            results.push(ReadResult {
                path: p.clone(),
                file_hash: hash,
                header: header.clone(),
                content: format!("{header}\n{block}"),
                warnings: None,
            });
        }
    }

    if results.is_empty() {
        return vec![ReadResult {
            path: path.to_string(),
            file_hash: String::new(),
            header: String::new(),
            content: String::new(),
            warnings: Some(format!(
                "symbol `{name}` not found in `{path}`. \
                 Verify the symbol name is spelled exactly as defined in source code. \
                 If `{path}` is a directory, it may contain no files with a supported \
                 tree-sitter grammar. \
                 Try using `line` targeting to read specific sections, or read the whole \
                 file to inspect its contents.",
            )),
        }];
    }

    results
}

/// Recursively collects files within a specified directory.
///
/// Returns only files with a supported Tree-sitter grammar.
fn collect_source_files(path: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                files.extend(collect_source_files(&p));
            } else if p.is_file() {
                let s = p.to_string_lossy().to_string();
                if cosh_sdk::tree_sitter::language::detect_language(&s).is_some() {
                    files.push(s);
                }
            }
        }
    }
    files
}

async fn read_normalized(fs: &DiskFilesystem, path: &str) -> Result<String, String> {
    let file_text = fs.read_text(path).await.map_err(|e| e.to_string())?;
    let bom_result = normalize::strip_bom(&file_text);
    Ok(normalize::normalize_to_lf(&bom_result.text))
}

fn extract_block(text: &str, span: BlockSpan) -> String {
    let mut line_num = 1u32;
    let mut block = String::new();

    for ch in text.chars() {
        if line_num >= span.start && line_num <= span.end {
            block.push(ch);
        }
        if ch == '\n' {
            line_num += 1;
            if line_num > span.end {
                break;
            }
        }
    }
    if block.ends_with('\n') {
        block.pop();
    }

    block
}
