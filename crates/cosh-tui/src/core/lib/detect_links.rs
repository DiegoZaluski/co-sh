use crate::core::lib::styled_text::TextChunk;

#[derive(Debug, Clone)]
pub struct SimpleHighlight(pub usize, pub usize, pub String);

const URL_SCOPES: &[&str] = &["markup.link.url", "string.special.url"];

#[must_use]
pub fn detect_links(
    chunks: Vec<TextChunk>,
    content: &str,
    highlights: &[SimpleHighlight],
) -> Vec<TextChunk> {
    let mut ranges: Vec<(usize, usize, String)> = Vec::new();

    for (i, highlight) in highlights.iter().enumerate() {
        let SimpleHighlight(start, end, group) = highlight;
        if !URL_SCOPES.contains(&group.as_str()) {
            continue;
        }

        let url = &content[*start..*end];
        ranges.push((*start, *end, url.to_string()));

        for j in (0..i).rev() {
            let SimpleHighlight(label_start, label_end, prev) = &highlights[j];
            if prev == "markup.link.label" {
                ranges.push((*label_start, *label_end, url.to_string()));
                break;
            }
            if !prev.starts_with("markup.link") {
                break;
            }
        }
    }

    if ranges.is_empty() {
        return chunks;
    }

    let mut content_pos = 0;
    let mut result = chunks;

    for chunk in &mut result {
        if chunk.text.len() <= 1 {
            continue;
        }

        let idx = content[content_pos..].find(&chunk.text);
        let Some(found_idx) = idx else {
            continue;
        };
        let idx = content_pos + found_idx;

        for (range_start, range_end, url) in &ranges {
            if idx < *range_end && idx + chunk.text.len() > *range_start {
                chunk.link = Some(crate::core::lib::styled_text::UrlLink { url: url.clone() });
                break;
            }
        }

        content_pos = idx + chunk.text.len();
    }

    result
}
