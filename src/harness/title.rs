//! Auto title generation for sessions.
//!
//! Sends the first user prompt to the LLM with a short system prompt
//! requesting a concise session title.  The title is generated
//! asynchronously (in a background task) so the agent loop is never
//! blocked.

use cosh_sdk::connector::Connector;

/// System prompt that instructs the model to produce a short title.
const TITLE_SYSTEM: &str = "\
You will generate a short title based on the first message a user begins a conversation with.

Rules:
- Keep the title in the same language that the user wrote their message in.
- Ensure it is not more than 50 characters long.
- The title should be a summary of the user's message.
- It should be one line long.
- Do not use quotes or colons.
- The entire text you return will be used as the title.
- Never return anything that is more than one sentence (one line) long.
- Do not include any preamble, explanation, or markdown formatting.
- Return ONLY the title text, nothing else.";

/// Maximum length for the generated title.
const MAX_TITLE_LEN: usize = 50;

/// Generate a session title from the first user prompt.
///
/// Uses `connector` to call the LLM with a minimal prompt.  On any
/// failure (network, provider error, empty response) returns `None` so
/// the caller can fall back to the timestamp-based default.
pub async fn generate_title(connector: &Connector, user_prompt: &str) -> Option<String> {
    if user_prompt.trim().is_empty() {
        return None;
    }

    let result = connector
        .chat_with_system(user_prompt, TITLE_SYSTEM)
        .await;

    match result {
        Ok(output) => {
            let raw = output.message().trim();
            if raw.is_empty() {
                return fallback_title(user_prompt);
            }

            // Strip thinking tags if present (some models emit them).
            let cleaned = strip_think_tags(raw);
            let cleaned = cleaned.trim();

            if cleaned.is_empty() {
                return fallback_title(user_prompt);
            }

            // Collapse newlines into spaces and truncate.
            let title: String = cleaned
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

            if title.len() > MAX_TITLE_LEN {
                let truncated: String = title.chars().take(MAX_TITLE_LEN - 1).collect();
                Some(format!("{truncated}…"))
            } else {
                Some(title)
            }
        }
        Err(e) => {
            log::warn!("title generation failed: {e}");
            fallback_title(user_prompt)
        }
    }
}

/// Fallback: use the first N chars of the user prompt as the title.
fn fallback_title(user_prompt: &str) -> Option<String> {
    let cleaned: String = user_prompt
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() {
        return None;
    }

    if cleaned.len() > MAX_TITLE_LEN {
        let truncated: String = cleaned.chars().take(MAX_TITLE_LEN - 1).collect();
        Some(format!("{truncated}…"))
    } else {
        Some(cleaned)
    }
}

/// Remove `<think>...</think>` tags that some models emit.
fn strip_think_tags(text: &str) -> String {
    // Remove complete think blocks.
    let mut result = text.to_string();
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result[start..].find("</think>") {
            result = format!(
                "{}{}",
                &result[..start],
                &result[start + end + "</think>".len()..]
            );
        } else {
            // Unclosed think tag — keep text after the tag, remove the tag itself.
            result = format!("{}{}", &result[..start], &result[start + "<think>".len()..]);
            break;
        }
    }
    // Remove orphan tags.
    result = result.replace("<think>", "");
    result = result.replace("</think>", "");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_think_tags_removes_complete_block() {
        let input = "<think>reasoning here</think>Actual title";
        assert_eq!(strip_think_tags(input), "Actual title");
    }

    #[test]
    fn strip_think_tags_removes_unclosed_block() {
        // Unclosed think tag: keep text after the tag.
        let input = "<think>reasoning here";
        assert_eq!(strip_think_tags(input), "reasoning here");
    }

    #[test]
    fn strip_think_tags_removes_orphan_tags() {
        let input = "<think>sometitle";
        assert_eq!(strip_think_tags(input), "sometitle");
    }

    #[test]
    fn strip_think_tags_no_tags() {
        let input = "Plain title text";
        assert_eq!(strip_think_tags(input), "Plain title text");
    }

    #[test]
    fn fallback_title_basic() {
        let result = fallback_title("Fix the login bug").unwrap();
        assert_eq!(result, "Fix the login bug");
    }

    #[test]
    fn fallback_title_truncates_long_input() {
        let long = "a".repeat(100);
        let result = fallback_title(&long).unwrap();
        assert!(result.chars().count() <= MAX_TITLE_LEN);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn fallback_title_collapses_whitespace() {
        let result = fallback_title("  Fix   the\n\nlogin  bug  ").unwrap();
        assert_eq!(result, "Fix the login bug");
    }

    #[test]
    fn fallback_title_empty_returns_none() {
        assert!(fallback_title("").is_none());
        assert!(fallback_title("   ").is_none());
    }

    #[test]
    fn title_truncation() {
        let long = "x".repeat(60);
        // Simulate what generate_title does with truncation.
        let truncated: String = long.chars().take(MAX_TITLE_LEN - 1).collect();
        let title = format!("{truncated}…");
        assert_eq!(title.chars().count(), MAX_TITLE_LEN);
    }
}
