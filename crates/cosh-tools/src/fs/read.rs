//! Read file(s) and format the output as hashline sections.
//!
//! Each [`ReadFile`] carries one or more [`Target`] entries.  A target can
//! request the whole file, a syntactic block at a given line, or a definition
//! block matching a name (symbol, struct, class, …).  When the name search
//! targets a directory the entire tree is walked recursively.
use std::path::Path;

use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    normalize,
    types::BlockSpan,
};

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

/// Run every target in `config` and return hashline-formatted output.
///
/// Each target produces a hashline section (`¶path#hash` + numbered lines).
/// When `symbol` is set the output is limited to the syntactic block of
/// the matching definition.  If `path` is a directory and `symbol` is set,
/// all supported source files under that directory are searched.
pub fn read(config: ReadFile) -> Result<String, String> {
    let fs = DiskFilesystem::new();
    let mut outputs: Vec<String> = Vec::new();

    for target in &config.read {
        let result = read_target(&fs, target)?;
        outputs.push(result);
    }

    Ok(outputs.join("\n"))
}

fn read_target(fs: &DiskFilesystem, target: &Target) -> Result<String, String> {
    if let Some(name) = target.symbol {
        return search_symbol(fs, target.path, name);
    }

    if Path::new(target.path).is_dir() {
        return Err(format!(
            "cannot read directory `{}` without a symbol filter",
            target.path
        ));
    }

    let text = read_normalized(fs, target.path)?;
    let hash = format::compute_file_hash(&text);
    let header = format::format_hashline_header(target.path, &hash);
    let body = format::format_numbered_lines(&text, 1);

    if let Some(line) = target.line {
        let ts = cosh_sdk::syntax::syntax();
        let ln = line
            .try_into()
            .map_err(|e| format!("cannot convert line {}: {}", line, e))?;
        let span = ts
            .resolve_block(target.path, &body, ln)
            .ok_or_else(|| format!("could not resolve block at line {}", line))?;
        let block = extract_block(&body, &span);
        return Ok(format!("{}\n{}", header, block));
    }

    Ok(format!("{}\n{}", header, body))
}

fn search_symbol(fs: &DiskFilesystem, path: &str, name: &str) -> Result<String, String> {
    let paths = if Path::new(path).is_dir() {
        Ok(collect_source_files(Path::new(path)))
    } else if cosh_sdk::syntax::language::detect_language(path).is_some() {
        Ok(vec![path.to_string()])
    } else {
        Err(format!(
            "no tree-sitter grammar available for `{path}`; use `line` targeting or read the whole file instead"
        ))
    }?;

    let ts = cosh_sdk::syntax::syntax();
    let mut results: Vec<String> = Vec::new();

    for p in &paths {
        let text = read_normalized(fs, p)?;
        if let Some(span) = ts.resolve_symbol(p, &text, name) {
            let hash = format::compute_file_hash(&text);
            let header = format::format_hashline_header(p, &hash);
            let body = format::format_numbered_lines(&text, 1);
            let block = extract_block(&body, &span);
            results.push(format!("{}\n{}", header, block));
        }
    }

    if results.is_empty() {
        return Err(format!("symbol `{name}` not found in `{path}`"));
    }

    Ok(results.join("\n"))
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
                if cosh_sdk::syntax::language::detect_language(&s).is_some() {
                    files.push(s);
                }
            }
        }
    }
    files
}

fn read_normalized(fs: &DiskFilesystem, path: &str) -> Result<String, String> {
    let file_text = fs.read_text(path).map_err(|e| e.to_string())?;
    let bom_result = normalize::strip_bom(&file_text);
    Ok(normalize::normalize_to_lf(&bom_result.text))
}

fn extract_block(text: &str, span: &BlockSpan) -> String {
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
