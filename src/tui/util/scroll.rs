pub fn collapse_tool_output(output: &str, max_lines: usize, max_chars: usize) -> Collapsed {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines && output.chars().count() <= max_chars {
        return Collapsed {
            output: output.to_string(),
            overflow: false,
        };
    }

    let preview = lines[..max_lines.min(lines.len())].join("\n");
    if preview.chars().count() > max_chars {
        let truncated: String = preview.chars().take(max_chars.saturating_sub(1)).collect();
        Collapsed {
            output: truncated + "…",
            overflow: true,
        }
    } else {
        let mut result = preview;
        result.push('\n');
        result.push('…');
        Collapsed {
            output: result,
            overflow: true,
        }
    }
}

pub struct Collapsed {
    pub output: String,
    pub overflow: bool,
}
