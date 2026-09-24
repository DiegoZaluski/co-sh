//! Severity contract for sub-agent code-review reports.
//!
//! A code-review sub-agent task (`code_review: true` on
//! [`SubAgentCallInput`](crate::subagent::types::SubAgentCallInput)) carries
//! a prompt contract: the FINAL REPORT — the sub-agent's last message, the
//! one written after its final tool call and the only text returned to the
//! caller (see [`crate::subagent::closure`]) — must START with an HTML
//! comment header declaring the outcome: `<!-- severity: green -->`,
//! `<!-- severity: yellow -->` (minor issues / bad practice at most) or
//! `<!-- severity: red -->` (something critical was found).
//!
//! The marker lives on the final report — not on the turn's first message —
//! for a logical reason: the agent can only declare the review outcome once
//! it has DONE the analysis. Earlier narration (progress notes between tool
//! calls) belongs to the live TUI timeline and is never shown to the caller,
//! so coloring the turn by the first line would tint the box from a message
//! written before any work happened.
//!
//! An HTML comment was chosen deliberately: it is a shape every model
//! already knows how to produce, it is inert in markdown rendering, and it
//! survives copy-through without being reformatted.
//!
//! The header is CONSUMED by the client: it is stripped from the rendered
//! report and only drives the sub-agent box color (green/yellow/red). A
//! report without a header (or a non-review task) leaves the box's neutral
//! per-agent color untouched.

use serde::{Deserialize, Serialize};

/// The review outcome declared by the severity header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// All good — at most cosmetic details.
    Green,
    /// Minor issues / bad practice at most.
    Yellow,
    /// Something critical was found.
    Red,
}

impl Severity {
    /// Parse the DSL keyword (case-insensitive). Returns `None` for any
    /// other word — an unknown severity is treated as "no header", leaving
    /// the box color untouched, rather than guessing.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "green" => Some(Self::Green),
            "yellow" => Some(Self::Yellow),
            "red" => Some(Self::Red),
            _ => None,
        }
    }
}

/// The prompt contract appended to the input when `code_review` is set. The
/// marker must open the FINAL REPORT — the sub-agent's last message, written
/// after its final tool call, which is the only text returned to the caller
/// ([`crate::subagent::closure`]). Earlier narration is not the report and
/// carries no marker.
pub(crate) const SEVERITY_CONTRACT: &str = "\n\n---\nREPORT FORMAT CONTRACT (mandatory): your final report — your LAST message, the one you write AFTER your final tool calls, which is the only text returned to the caller — must START with an HTML comment header declaring the review outcome, on its own first line, exactly one of:\n<!-- severity: green -->   (all good — at most cosmetic details)\n<!-- severity: yellow --> (minor issues or bad practice found, nothing critical)\n<!-- severity: red -->    (something critical was found)\nDo NOT put the header on earlier progress messages: intermediate narration between tool calls is shown live but is not your report. The header is consumed by the client tooling and never shown; everything after it is the report itself. Do not put anything before the header.";

/// Extract the severity from the first line of a report and return
/// `(severity, report_without_the_header)`.
///
/// The header must be the report's first non-empty line and must match
/// `<!-- severity: WORD -->` (whitespace inside the comment is tolerated;
/// the match is case-insensitive). Anything else — no comment, wrong word,
/// header not on the first line — means "no severity": the report is
/// returned untouched with `None`.
pub fn extract_severity(report: &str) -> (Option<Severity>, &str) {
    // Tolerate leading blank lines before the header.
    let trimmed = report.trim_start_matches(['\n', '\r', ' ', '\t']);
    let Some(first_line_end) = trimmed.find('\n') else {
        // Single-line report: the whole thing is the candidate line. With a
        // header the body is empty by definition; without one the report is
        // returned UNTOUCHED (dropping the only line would lose the text).
        return match parse_header_line(trimmed) {
            Some(severity) => (Some(severity), ""),
            None => (None, report),
        };
    };
    let (first_line, after) = trimmed.split_at(first_line_end);
    match parse_header_line(first_line) {
        Some(severity) => (
            Some(severity),
            // CRLF outputs leave a leading `\r` after splitting on `\n`
            // (and Windows bodies may pad blank lines with `\r`): strip
            // both so the body never starts with stray carriage returns.
            after.trim_start_matches(['\n', '\r']),
        ),
        None => (None, report),
    }
}

/// `<!-- severity: WORD -->` on a single line, case-insensitive, whitespace
/// tolerant. Returns `None` for anything else.
fn parse_header_line(line: &str) -> Option<Severity> {
    let line = line.trim();
    let inner = line.strip_prefix("<!--")?.strip_suffix("-->")?;
    let inner = inner.trim();
    let word = inner.strip_prefix("severity:")?;
    Severity::parse(word)
}

/// Append the severity contract to a task input (review tasks only).
pub fn with_severity_contract(input: &str, code_review: bool) -> String {
    if code_review {
        format!("{input}{SEVERITY_CONTRACT}")
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_three_keywords_case_insensitive() {
        assert_eq!(Severity::parse("green"), Some(Severity::Green));
        assert_eq!(Severity::parse("YELLOW"), Some(Severity::Yellow));
        assert_eq!(Severity::parse("  Red "), Some(Severity::Red));
        assert_eq!(Severity::parse("orange"), None);
        assert_eq!(Severity::parse(""), None);
    }

    #[test]
    fn extract_reads_the_header_and_strips_it() {
        let report = "<!-- severity: red -->\n\n## Critical\n\nBug found.";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, Some(Severity::Red));
        assert_eq!(rest, "## Critical\n\nBug found.");
    }

    #[test]
    fn extract_tolerates_leading_blank_lines_and_whitespace() {
        let report = "\n\n  <!--severity: GREEN-->  \nbody";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, Some(Severity::Green));
        assert_eq!(rest, "body");
    }

    /// CRLF reports: the header line's trailing `\r` is already trimmed by
    /// `parse_header_line`, but the split leaves `"\r\n"` before the body —
    /// the body must not start with stray carriage returns.
    #[test]
    fn extract_strips_crlf_blank_lines_after_the_header() {
        let report = "<!-- severity: red -->\r\n\r\n## Critical\r\nBug found.";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, Some(Severity::Red));
        assert!(rest.starts_with("## Critical"));
        assert!(!rest.starts_with('\r'));
    }

    #[test]
    fn extract_without_header_returns_the_report_untouched() {
        let report = "## Findings\n\nnothing wrong";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, None);
        // Bit-identical round trip: no header means no mutation.
        assert_eq!(rest, report);
    }

    #[test]
    fn extract_ignores_headers_not_on_the_first_line() {
        let report = "## Report\n<!-- severity: red -->\nbody";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, None);
        assert_eq!(rest, report);
    }

    #[test]
    fn extract_ignores_unknown_severity_words() {
        let report = "<!-- severity: purple -->\nbody";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, None);
        assert_eq!(rest, report);
    }

    #[test]
    fn extract_handles_single_line_header_only_report() {
        let report = "<!-- severity: yellow -->";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, Some(Severity::Yellow));
        assert_eq!(rest, "");
    }

    /// Regression: a single-line report WITHOUT a header must come back
    /// untouched — an earlier version returned an empty body, dropping the
    /// only line of the report.
    #[test]
    fn extract_keeps_a_single_line_report_without_header() {
        let report = "just one line of findings";
        let (severity, rest) = extract_severity(report);
        assert_eq!(severity, None);
        assert_eq!(rest, report);
    }

    #[test]
    fn contract_is_appended_only_for_review_tasks() {
        let plain = with_severity_contract("do the task", false);
        assert_eq!(plain, "do the task");
        assert!(!plain.contains("severity"));

        let review = with_severity_contract("review this PR", true);
        assert!(review.starts_with("review this PR"));
        assert!(review.contains("<!-- severity: green -->"));
        assert!(review.contains("<!-- severity: yellow -->"));
        assert!(review.contains("<!-- severity: red -->"));
    }
}
