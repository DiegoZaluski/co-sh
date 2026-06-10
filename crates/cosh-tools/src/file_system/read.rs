//! TODO:
//!     1. Add search by function name.
//!     2. Add support for searching across multiple files.
//!     3. Create module documentation strings.
use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    normalize,
    types::BlockSpan,
};

/// Configuration for reading a file in hashline format.
pub struct ReadFile<'a, 'b> {
    pub path: &'a str,
    pub function: Option<&'b str>,
    pub line: Option<usize>,
}

/// Read a file and format it as a hashline section header + numbered lines.
///
/// The output is a string like:
/// ```text
/// ¶src/main.rs#A3F2
/// 1:fn main() {
/// 2:    println!("hello");
/// 3:}
/// ```
pub fn read(search: ReadFile) -> Result<String, String> {
    let fs = DiskFilesystem::new();
    let file_text = fs.read_text(search.path).map_err(|e| e.to_string())?;

    // Strip BOM and normalize line endings to LF.
    // The hash must be computed over the normalized text to match
    // what the patcher expects when validating snapshot tags.
    let bom_result = normalize::strip_bom(&file_text);
    let normalized = normalize::normalize_to_lf(&bom_result.text);

    let hash = format::compute_file_hash(&normalized);
    let header = format::format_hashline_header(search.path, &hash);
    let body = format::format_numbered_lines(&normalized, 1);

    if let Some(line) = search.line {
        let ts = cosh_sdk::syntax::syntax();
        let offset = ts
            .resolve_block(search.path, body.as_str(), line.try_into().unwrap())
            .unwrap();
        let body = extract_block(body.as_str(), &offset);
        return Ok(format!("{}\n{}", header, body));
    }

    Ok(format!("{}\n{}", header, body))
}

fn extract_block(text: &str, span: &BlockSpan) -> String {
    let mut line_num = 1u32;
    let mut block = std::string::String::new();

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

#[test]
fn test() {
    let read_file = ReadFile {
        path: "/home/inky/cosh/crates/cosh-sdk/src/hashline/tokenizer.rs",
        line: Some(5),
        function: None,
    };

    let result = read(read_file);
    println!("{:?}", result)
}
