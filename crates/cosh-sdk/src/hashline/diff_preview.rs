//! Re-number a unified diff that uses the `+<lineNum>|content` /
//! `-<lineNum>|content` / ` <lineNum>|content` line format into a compact
//! preview that anchors every line to its post-edit position. Added lines,
//! removed lines, and context lines all end up with a hashline-style anchor
//! so a follow-up edit can reuse them directly.
//!
//! This is intentionally decoupled from the diff producer: anything that
//! emits the `<sign><lineNum>|<content>` shape works.
use super::types::{CompactDiffOptions, CompactDiffPreview};

pub fn build_compact_diff_preview(
    diff: &str,
    _options: Option<CompactDiffOptions>,
) -> CompactDiffPreview {
    let lines: Vec<&str> = if diff.is_empty() {
        Vec::new()
    } else {
        diff.split('\n').collect()
    };

    let mut added_lines: u32 = 0;
    let mut removed_lines: u32 = 0;

    // External diff producers number `+` lines with the post-edit line number,
    // `-` lines with the pre-edit line number, and context lines with the
    // pre-edit line number. To emit fresh line numbers usable for follow-up
    // edits, convert context-line numbers to post-edit positions by tracking
    // the running offset (added so far - removed so far) as we walk the diff.
    let formatted: Vec<String> = lines
        .iter()
        .map(|line| {
            let kind = line.chars().next();
            match kind {
                Some('+') | Some('-') | Some(' ') => {}
                _ => return line.to_string(),
            }
            let kind = kind.unwrap();

            let body = &line[1..];
            let sep = match body.find('|') {
                Some(pos) => pos,
                None => return line.to_string(),
            };

            let content = &body[sep + 1..];

            match kind {
                '+' => {
                    added_lines += 1;
                    format!("+{}:{}", &body[..sep], content)
                }
                '-' => {
                    removed_lines += 1;
                    format!("-{}:{}", &body[..sep], content)
                }
                _ => {
                    let line_number: u32 = body[..sep].parse().unwrap_or(0);
                    let offset = added_lines as i64 - removed_lines as i64;
                    let new_line_number = (line_number as i64 + offset) as u32;
                    format!(" {}:{}", new_line_number, content)
                }
            }
        })
        .collect();

    CompactDiffPreview {
        preview: formatted.join("\n"),
        added_lines,
        removed_lines,
    }
}
