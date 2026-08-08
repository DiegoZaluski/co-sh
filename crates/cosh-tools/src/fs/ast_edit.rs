//! AST-driven file edit engine — `ast_edit` powered by the `cosh_sdk::ast`
//! (ast-grep) structural matching engine.
//!
//! This is the second, *structural* editing strategy behind
//! [`Fs::edit`](crate::fs::Fs::edit) (the other being the hashline replace
//! engine in [`edit`](crate::fs::edit)). Instead of anchoring edits on line
//! ranges or content hashes, it rewrites the file's **tree-sitter syntax
//! tree** via ast-grep:
//!
//! - a pattern (`pat`) is parsed and matched structurally against the file's
//!   syntax tree;
//! - the matched subtree's source text is replaced by a template (`out`).
//!
//! Patterns support ast-grep style metavariables (`$NAME` for one node,
//! `$$$NAME` for zero-or-more siblings). Metavariable identity is enforced by
//! the engine: the same metavariable must capture identical text in every
//! occurrence for a match to succeed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ast_grep_core::source::Edit;
use ast_grep_core::tree_sitter::LanguageExt;
use cosh_sdk::ast::{apply_edits, compile_search_patterns, is_supported_file, resolve_language};
use cosh_sdk::find::{FileType, GlobOptions, glob};
use cosh_sdk::hashline::{
    diff::structured_patch,
    format,
    fs::{DiskFilesystem, Filesystem},
};
use cosh_sdk::rollback;
use cosh_sdk::tree_sitter::tree_sitter;

use crate::util::path_guard::assert_editable_file;

use super::edit::EditResult;
use super::types::{AstEditOp, FsAstEdit, FsMetadata};

/// Outcome of rewriting one file.
struct FileEditOutcome {
    path: String,
    file_hash: String,
    header: String,
    first_changed_line: Option<u32>,
    warnings: Vec<String>,
    diff: Option<String>,
    touched: bool,
}

// Structural rewriting

/// Remap an `anchor` (each byte of the current text → the original byte it
/// came from) across a set of non-overlapping edits, splicing replacement
/// bytes onto the mapping of their start byte. Mirrors the dedup/ordering of
/// [`apply_edits`] so the two stay in agreement.
fn splice_anchor(anchor: &[usize], edits: &[Edit<String>]) -> Vec<usize> {
    let mut sorted: Vec<&Edit<String>> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        a.position
            .cmp(&b.position)
            .then(a.deleted_length.cmp(&b.deleted_length))
            .then(a.inserted_text.cmp(&b.inserted_text))
    });
    sorted.dedup_by(|a, b| {
        a.position == b.position
            && a.deleted_length == b.deleted_length
            && a.inserted_text == b.inserted_text
    });
    let mut out = anchor.to_vec();
    for e in sorted.iter().rev() {
        let start = e.position;
        let end = e.position.saturating_add(e.deleted_length);
        // An insertion at the very end of the file has `start == out.len()`; map
        // it onto the last byte's origin rather than silently to `0` (which
        // would misreport a later `first_changed_line`).
        //
        // The `unwrap_or(0)` branch only fires for a fully empty anchor (nothing
        // left to trace after a previous op deleted the whole file). A byte
        // created at offset 0 of an empty buffer has no predecessor source byte;
        // mapping it to origin 0 (the very start of the file) is the only
        // sensible degenerate choice and reports line 1, which is correct.
        let orig = out.get(start).or_else(|| out.last()).copied().unwrap_or(0);
        out.splice(start..end, std::iter::repeat_n(orig, e.inserted_text.len()));
    }
    out
}

/// Line count of the prefix `[0..byte]` (used to compute `first_changed_line`).
fn newlines_before(text: &str, byte: usize) -> u32 {
    let end = byte.min(text.len());
    text[..end].bytes().filter(|&b| b == b'\n').count() as u32
}

// Target path resolution

/// Default hard cap on the number of files edited in one call.
pub const DEFAULT_MAX_FILES: usize = 1000;

fn has_glob_chars(p: &str) -> bool {
    p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'{'))
}

fn is_supported(path: &Path) -> bool {
    is_supported_file(path, None)
}

fn collect_supported_files(metadata: &FsMetadata, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        // Never follow symlinks: a symlinked directory could escape the root or
        // form a cycle, and a symlinked file would be rewritten through the
        // link. `file_type` is lstat-based, so this also skips broken links.
        if ft.is_symlink() {
            continue;
        }
        let p = entry.path();
        if ft.is_dir() {
            // Parity with the glob walker: skip VCS and vendored subtrees.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == ".git" || name == "node_modules" {
                continue;
            }
            collect_supported_files(metadata, &p, out);
        } else if ft.is_file()
            && let Ok(abs) = metadata.fs_guard(&p.to_string_lossy())
            && is_supported(&abs)
            && !out.contains(&abs)
        {
            out.push(abs);
        }
    }
}

/// Expand a glob relative to `root` into validated, supported file paths.
fn expand_glob(metadata: &FsMetadata, root: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let res = glob(GlobOptions {
        pattern: pattern.to_string(),
        path: root.to_string_lossy().to_string(),
        file_type: Some(FileType::File),
        recursive: None,
        hidden: None,
        max_results: None,
        gitignore: Some(true),
        cache: None,
        sort_by_mtime: None,
        include_node_modules: None,
        timeout_ms: None,
        on_match: None,
    })
    .map_err(|e| format!("glob `{pattern}` failed: {e}"))?;
    let mut v = Vec::new();
    for m in res.matches {
        if let Ok(abs) = metadata.fs_guard(&m.path)
            && is_supported(&abs)
            && !v.contains(&abs)
        {
            v.push(abs);
        }
    }
    Ok(v)
}

/// Resolve the `paths` argument (files, directories, or globs) into validated,
/// deduplicated, supported file paths, capped at `max_files`.
///
/// Returns the file list and whether the cap was reached (some requested files
/// were dropped). Nonexistent explicit paths are silently skipped: an absent
/// path simply yields nothing.
fn resolve_target_files(
    metadata: &FsMetadata,
    root: &Path,
    paths: &[String],
    max_files: usize,
) -> Result<(Vec<PathBuf>, bool), String> {
    let mut out = Vec::new();
    for p in paths {
        if has_glob_chars(p) {
            out.extend(expand_glob(metadata, root, p)?);
        } else {
            let abs = metadata
                .fs_guard(p)
                .map_err(|e| format!("permission denied for `{p}`: {e}"))?;
            if !abs.exists() {
                continue;
            }
            if abs.is_dir() {
                collect_supported_files(metadata, &abs, &mut out);
            } else if is_supported(&abs) && !out.contains(&abs) {
                out.push(abs);
            }
        }
    }
    out.sort();
    out.dedup();
    let limit_reached = out.len() > max_files;
    if limit_reached {
        out.truncate(max_files);
    }
    Ok((out, limit_reached))
}

// Rewriting a single file

async fn rewrite_file(abs: &Path, ops: &[AstEditOp]) -> Result<FileEditOutcome, String> {
    let fs = DiskFilesystem::new();
    let path_str = abs.to_string_lossy().to_string();

    // Never rewrite a file that declares itself machine-generated. This fires
    // even for bulk directory/glob rewrites, where the model may never have
    // opened the file and its change would be lost on the next generation run.
    assert_editable_file(abs).map_err(|e| format!("failed to edit `{path_str}`: {e}"))?;

    let original = fs
        .read_text(&path_str)
        .await
        .map_err(|e| format!("failed to read `{path_str}`: {e}"))?;
    let lang = resolve_language(None, abs)
        .map_err(|e| format!("unsupported language for `{path_str}`: {e}"))?;

    // A zero-byte file has no tree to rewrite. Returning here also avoids
    // indexing the empty `anchor` below, which an ast-grep pattern (`$A`) can
    // otherwise hit with a zero-length root match.
    if original.is_empty() {
        let hash = format::compute_file_hash("");
        return Ok(FileEditOutcome {
            path: path_str.clone(),
            file_hash: hash.clone(),
            header: format::format_hashline_header(&path_str, &hash),
            first_changed_line: None,
            warnings: vec!["no AST matches found".to_string()],
            diff: None,
            touched: false,
        });
    }

    let mut text = original.clone();
    let mut anchor: Vec<usize> = (0..original.len()).collect();
    let mut total = 0usize;
    let mut first_change_orig: Option<usize> = None;
    let mut issues: Vec<String> = Vec::new();

    for op in ops {
        // Compile every pattern ast-grep derives for this op (a normal pattern
        // plus, for Rust, a statement-level contextual pattern — see
        // `compile_search_patterns`). Each derived pattern is applied in
        // sequence with a re-parse in between.
        let patterns = match compile_search_patterns(&op.pat, lang) {
            Ok(ps) => ps,
            Err(e) => {
                issues.push(format!("unable to parse pattern `{}`: {e}", op.pat));
                continue;
            }
        };
        let mut ast = lang.ast_grep(&text);
        for pattern in patterns {
            let edits = ast.root().replace_all(pattern, op.out.as_str());
            if edits.is_empty() {
                continue;
            }
            match apply_edits(ast.root().text().as_ref(), &edits) {
                Ok(updated) => {
                    for e in &edits {
                        // Guard against an empty `anchor` (a prior op may have
                        // deleted the whole file); the degenerate origin 0 keeps
                        // the line report stable instead of panicking.
                        let orig_start = anchor
                            .get(e.position.min(anchor.len().saturating_sub(1)))
                            .copied()
                            .unwrap_or(0);
                        if first_change_orig.is_none_or(|fc| orig_start < fc) {
                            first_change_orig = Some(orig_start);
                        }
                    }
                    anchor = splice_anchor(&anchor, &edits);
                    text = updated;
                    total += edits.len();
                    ast = lang.ast_grep(&text);
                }
                Err(e) => issues.push(format!("rewrite `{}` skipped: {e}", op.pat)),
            }
        }
    }

    if total == 0 {
        issues.push("no AST matches found".to_string());
    }

    let touched = text != original;
    if touched {
        let _ = rollback::record(&path_str, &original);
        fs.write_text(&path_str, &text)
            .await
            .map_err(|e| format!("failed to write `{path_str}`: {e}"))?;
        tree_sitter().invalidate(&path_str);
        let _ = rollback::record(&path_str, &text);
    }

    let first_changed_line = first_change_orig.map(|b| newlines_before(&original, b) + 1);
    let hash = format::compute_file_hash(&text);
    let header = format::format_hashline_header(&path_str, &hash);
    let diff = if touched {
        Some(structured_patch(&original, &text, 3).to_unified_diff(&path_str, &path_str))
    } else {
        None
    };

    Ok(FileEditOutcome {
        path: path_str,
        file_hash: hash,
        header,
        first_changed_line,
        warnings: issues,
        diff,
        touched,
    })
}

// Public entry point

/// Apply AST rewrites (`ast_edit`) across the resolved `paths`.
///
/// Returns one [`EditResult`] per file that was actually modified by at least
/// one rewrite. Files that produced only warnings (a pattern failed to parse,
/// or no match was found) are reported with `diff: None` so the model still
/// sees the parse-error / no-match feedback instead of silence. Searched files
/// with no changes and no warnings are not reported.
///
/// # Errors
///
/// Returns an error string when a path is denied by the guards, a glob fails,
/// a file cannot be read, or a file cannot be written back.
pub async fn ast_edit(metadata: FsMetadata, tg: FsAstEdit) -> Result<Vec<EditResult>, String> {
    let root = metadata.root.clone();
    if tg.paths.is_empty() {
        return Err("AST edit requires at least one `paths` entry".to_string());
    }
    if tg.ops.is_empty() {
        return Err("AST edit requires at least one `ops` entry".to_string());
    }
    let mut seen_pats = HashSet::new();
    for (i, op) in tg.ops.iter().enumerate() {
        if op.pat.trim().is_empty() {
            return Err(format!("`ops[{i}].pat` must be a non-empty pattern"));
        }
        if !seen_pats.insert(op.pat.as_str()) {
            return Err(format!("duplicate rewrite pattern `{}`", op.pat));
        }
    }
    let max_files = tg.max_files.unwrap_or(DEFAULT_MAX_FILES);
    let (files, limit_reached) = resolve_target_files(&metadata, &root, &tg.paths, max_files)?;
    if files.is_empty() && !limit_reached {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for f in files {
        let outcome = rewrite_file(&f, &tg.ops).await?;
        if outcome.touched || !outcome.warnings.is_empty() {
            results.push(EditResult {
                path: outcome.path,
                file_hash: outcome.file_hash,
                header: outcome.header,
                first_changed_line: outcome.first_changed_line,
                warnings: outcome.warnings,
                diff: outcome.diff,
            });
        }
    }

    if limit_reached {
        let note = format!("AST edit limit reached ({max_files} files); narrow `paths`");
        match results.last_mut() {
            Some(last) => last.warnings.push(note),
            None => results.push(EditResult {
                path: String::new(),
                file_hash: String::new(),
                header: String::new(),
                first_changed_line: None,
                warnings: vec![note],
                diff: None,
            }),
        }
    }
    Ok(results)
}
