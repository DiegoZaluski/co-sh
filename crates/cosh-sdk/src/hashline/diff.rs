//! Wrapper around the `similar` crate mirroring the Node.js `diff` package API.
//! Used for structured unified-diff creation and application (3-way merge) in
//! snapshot recovery.
use similar::{ChangeTag, TextDiff};

/// A unified-diff patch, analogous to the object returned by
/// `Diff.structuredPatch()` in the Node.js `diff` package.
#[derive(Debug, Clone)]
pub struct Patch {
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// Build a `Patch` describing the changes from `old` to `new`,
/// with `context` lines of surrounding context per hunk.
///
/// Equivalent to `Diff.structuredPatch("file", "file", old, new, "", "", { context })`.
#[must_use]
pub fn structured_patch(old: &str, new: &str, context: usize) -> Patch {
    let diff = TextDiff::from_lines(old, new);
    let groups = diff.grouped_ops(context);

    let mut hunks = Vec::with_capacity(groups.len());

    for group in &groups {
        let old_start = group
            .first()
            .map_or(1, |op| op.old_range().start + 1);
        let new_start = group
            .first()
            .map_or(1, |op| op.new_range().start + 1);

        let mut lines = Vec::new();
        let mut old_count = 0usize;
        let mut new_count = 0usize;

        for change in group.iter().flat_map(|op| diff.iter_changes(op)) {
            let tag = change.tag();
            let value = change.value();
            let value = value
                .strip_suffix("\r\n")
                .or_else(|| value.strip_suffix('\n'))
                .unwrap_or(value);

            let (prefix, old_step, new_step) = match tag {
                ChangeTag::Equal => (" ", 1, 1),
                ChangeTag::Delete => ("-", 1, 0),
                ChangeTag::Insert => ("+", 0, 1),
            };

            lines.push(format!("{prefix}{value}"));
            old_count += old_step;
            new_count += new_step;
        }

        hunks.push(Hunk {
            old_start,
            old_lines: old_count,
            new_start,
            new_lines: new_count,
            lines,
        });
    }

    Patch { hunks }
}

/// Apply `patch` to `target`. Returns `None` when context lines do not match
/// exactly (fuzz factor = 0).
///
/// Equivalent to `Diff.applyPatch(target, patch, { fuzzFactor: 0 })`.
#[must_use]
pub fn apply_patch(target: &str, patch: &Patch) -> Option<String> {
    let target_lines: Vec<&str> = target.split('\n').collect();
    let mut output: Vec<String> = Vec::new();
    let mut pos: usize = 0;
    let target_len = target_lines.len();

    for hunk in &patch.hunks {
        let hunk_start = hunk.old_start.saturating_sub(1);
        while pos < hunk_start && pos < target_len {
            output.push(target_lines[pos].to_string());
            pos += 1;
        }

        for line in &hunk.lines {
            let prefix = line.chars().next()?;
            let content = &line[1..];

            match prefix {
                ' ' => {
                    if pos >= target_len || target_lines[pos] != content {
                        return None;
                    }
                    output.push(target_lines[pos].to_string());
                    pos += 1;
                }
                '-' => {
                    if pos >= target_len || target_lines[pos] != content {
                        return None;
                    }
                    pos += 1;
                }
                '+' => {
                    output.push(content.to_string());
                }
                _ => return None,
            }
        }
    }

    while pos < target_len {
        output.push(target_lines[pos].to_string());
        pos += 1;
    }

    Some(output.join("\n"))
}
