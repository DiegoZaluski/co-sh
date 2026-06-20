//! YAML-like frontmatter parser for `SKILL.md` files.
//!
//! Handles the subset needed by the skill standard: single-line values, block
//! scalars (`|`), indented lists, and quoted strings.

use crate::skills::types::Frontmatter;

/// Parse YAML-like frontmatter between `---` delimiters.
///
/// Returns `(Frontmatter, body)` where `body` is everything after the closing
/// `---`. If no closing `---` is found, the body is empty. If no `---` at all,
/// the entire content is treated as body and default frontmatter is returned.
#[must_use]
pub fn parse_frontmatter(content: &str) -> (Frontmatter, String) {
    let content = content.trim_start();

    if !content.starts_with("---") {
        return (Frontmatter::default(), content.to_owned());
    }

    let after_first = content
        .lines()
        .next()
        .map_or("", |first| &content[first.len()..]);

    let mut closing_pos: Option<usize> = None;
    let mut body_start: usize = after_first.len();

    if let Some(pos) = after_first.find("\n---") {
        let after_newline = &after_first[pos + 1..];
        let line_end = after_newline.find('\n').unwrap_or(after_newline.len());
        if after_newline[..line_end].trim() == "---" {
            closing_pos = Some(pos + 1);
            body_start = pos + 1 + line_end;
        }
    }

    let frontmatter_str = match closing_pos {
        Some(pos) => &after_first[..pos],
        None => after_first,
    };

    let body = match closing_pos {
        Some(_) => after_first[body_start..].trim().to_owned(),
        None => String::new(),
    };

    let fm = parse_frontmatter_lines(frontmatter_str);
    (fm, body)
}

fn parse_frontmatter_lines(input: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;

    let mut in_block_scalar = false;
    let mut block_scalar_key: Option<&str> = None;
    let mut block_lines: Vec<String> = Vec::new();
    let mut in_list = false;
    let mut list_key: Option<&str> = None;
    let mut list_items: Vec<String> = Vec::new();

    while i < lines.len() {
        let line = lines[i];

        if in_block_scalar {
            if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
                block_lines.push(line.to_owned());
                i += 1;
                continue;
            }
            finalize_block_scalar(&mut fm, block_scalar_key.take(), &block_lines);
            block_lines.clear();
            in_block_scalar = false;
            continue;
        }

        if in_list {
            if line.trim_start().starts_with("- ") || line.trim_start() == "-" {
                let item = line
                    .trim_start()
                    .strip_prefix("- ")
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_owned();
                list_items.push(item);
                i += 1;
                continue;
            }
            finalize_list(&mut fm, list_key.take(), &list_items);
            list_items.clear();
            in_list = false;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        if let Some((key, value)) = trimmed.split_once(':') {
            let k = key.trim();
            let v = value.trim();

            match v {
                "|" => {
                    in_block_scalar = true;
                    block_scalar_key = Some(k);
                    block_lines.clear();
                    i += 1;
                    continue;
                }
                "" => {
                    in_list = true;
                    list_key = Some(k);
                    list_items.clear();
                    i += 1;
                    continue;
                }
                _ => {
                    let unquoted = v.trim_matches('"').trim_matches('\'').to_owned();
                    match k {
                        "name" => fm.name = Some(unquoted),
                        "description" => fm.description = Some(unquoted),
                        "alwaysApply" | "always_apply" => fm.always_apply = unquoted == "true",
                        _ => {}
                    }
                }
            }
        }

        i += 1;
    }

    if in_block_scalar {
        finalize_block_scalar(&mut fm, block_scalar_key.take(), &block_lines);
    }
    if in_list {
        finalize_list(&mut fm, list_key.take(), &list_items);
    }

    fm
}

fn finalize_block_scalar(fm: &mut Frontmatter, key: Option<&str>, lines: &[String]) {
    if let Some("description") = key {
        let value = if lines.is_empty() {
            String::new()
        } else {
            let min_indent = lines
                .iter()
                .filter(|l| !l.is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            lines
                .iter()
                .map(|l| {
                    if l.len() >= min_indent {
                        l[min_indent..].to_owned()
                    } else {
                        l.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        fm.description = Some(value);
    }
}

fn finalize_list(fm: &mut Frontmatter, key: Option<&str>, items: &[String]) {
    if let Some("globs") = key {
        fm.globs = items.to_vec();
    }
}
