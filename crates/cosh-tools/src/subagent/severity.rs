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
///
/// The explicit `code_review` flag wins, but it is NOT trusted blindly:
/// session evidence showed the orchestrating model routinely forgets to set
/// it on review dispatches, so the contract was never appended and the
/// sub-agent never produced the `<!-- severity: ... -->` header (the box
/// stayed untinted — the reported "agent ignores the DSL"). When the flag
/// is absent, a conservative heuristic infers a review task from the input
/// itself and appends the contract anyway; the header is consumed (stripped
/// and never shown) so a false positive costs the task nothing but a
/// harmless first line, and the extraction treats a report without one as
/// "no severity".
pub fn with_severity_contract(input: &str, code_review: bool) -> String {
    if is_review_task(input, code_review) {
        format!("{input}{SEVERITY_CONTRACT}")
    } else {
        input.to_string()
    }
}

/// Whether this dispatch is a code-review task: the explicit `code_review`
/// flag, or the same conservative heuristic [`with_severity_contract`]
/// applies when the caller forgot the flag. Both the contract append (input
/// side) and the header enforcement (report side) must ask THIS so a task
/// that got the contract is also the task whose report is enforced.
pub fn is_review_task(input: &str, code_review: bool) -> bool {
    code_review || looks_like_code_review(input)
}

/// Enforce the severity header on a FINISHED review report.
///
/// The prompt contract asks the sub-agent to open its final report with the
/// `<!-- severity: ... -->` header, but prompting is not enforcement — a
/// model can end its report without one (or bury it after narration, which
/// `extract_severity` then ignores). On a completed review turn the header
/// is MANDATORY: it is the only color signal the report box gets. When the
/// report lacks a valid first-line header, the client INJECTS the header
/// that best matches the report's content ([`infer_severity`]) — the DSL is
/// honored even when the sub-agent ignored it. Reports that already carry
/// the header pass through untouched, and non-review tasks are never
/// touched. The header is never RENDERED (the visible report strips it);
/// on the external path the enforced text is what the orchestrator's tool
/// call returns, but as an inert HTML comment it is inert transcript
/// metadata, not displayable text.
pub fn enforce_severity_header(report: &str, is_review: bool) -> String {
    if !is_review || report.trim().is_empty() || extract_severity(report).0.is_some() {
        return report.to_string();
    }
    let word = match infer_severity(report) {
        Severity::Green => "green",
        Severity::Yellow => "yellow",
        Severity::Red => "red",
    };
    format!("<!-- severity: {word} -->\n{report}")
}

/// Words that flip a NEARBY finding keyword into "absent": a review saying
/// "no critical findings" is CLEAN, not red. Only the three tokens before
/// the keyword are considered — enough to reach a negation separated by
/// short filler ("nothing is a major concern"), short enough that ordinary
/// prose does not extend the shadow across sentences.
const NEGATIONS: [&str; 6] = ["no", "not", "never", "none", "without", "nothing"];

/// Best-effort severity inference from a review report's own findings, used
/// only when the sub-agent failed to declare the header. Scans for the
/// finding vocabulary the review protocol prescribes — CRITICAL findings are
/// red, MAJOR findings (or an explicit "changes required" verdict) are
/// yellow, anything else reads as green. The scan is token-based (word
/// boundaries), so "majority" does not read as "major", and a negation right
/// before a keyword ("no critical findings", "no changes required") marks
/// the finding ABSENT instead of present: a false RED on a clean report is
/// the most misleading failure this inference could produce, so cleanliness
/// outranks coverage. The header is consumed by the client either way, so a
/// mis-inference costs a tint shade, never text.
fn infer_severity(report: &str) -> Severity {
    let tokens: Vec<String> = report
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let negated = |i: usize| {
        tokens[..i]
            .iter()
            .rev()
            .take(3)
            .any(|t| NEGATIONS.contains(&t.as_str()))
    };
    if tokens
        .iter()
        .enumerate()
        .any(|(i, t)| t == "critical" && !negated(i))
    {
        return Severity::Red;
    }
    if tokens.iter().enumerate().any(|(i, t)| {
        (t == "major"
            || (t == "changes" && tokens.get(i + 1).is_some_and(|next| next == "required")))
            && !negated(i)
    }) {
        return Severity::Yellow;
    }
    Severity::Green
}

/// Conservative heuristic for review tasks the caller forgot to flag.
///
/// Matches only unmistakable review phrasing ("code review", the VERDICT
/// protocol used by this repo's own review dispatches, and
/// review-only/re-review wording). It deliberately does NOT match generic
/// "review the changes" prose — a false positive is cheap (the header is
/// stripped from the rendered report) but appending a report-format
/// contract to an unrelated implementation task would still be noise.
fn looks_like_code_review(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.contains("code review")
        || lower.contains("re-review")
        || lower.contains("review only")
        || lower.contains("review-only")
        || lower.contains("verdict:")
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

    /// Regression (live session evidence): the orchestrating model routinely
    /// dispatches code reviews WITHOUT setting `code_review: true`, so the
    /// contract was never appended and the sub-agent never emitted the
    /// `<!-- severity: ... -->` header — the box stayed untinted ("agent
    /// ignores the DSL"). The heuristic must catch those unflagged review
    /// dispatches while leaving ordinary implementation tasks untouched.
    #[test]
    fn unflagged_review_dispatches_get_the_contract_via_heuristic() {
        // Real unflagged dispatch shapes from the live session / review flow.
        for input in [
            "Faça um CODE REVIEW do último commit deste repositório. Rode git show e leia o código.",
            "Faça um RE-REVIEW do commit mais recente deste repositório.",
            "Code review task (review only — do NOT modify anything).",
            "You are a code reviewer. Review ONLY; modify nothing. End with VERDICT: ACCEPT.",
        ] {
            let out = with_severity_contract(input, false);
            assert!(
                out.starts_with(input),
                "the input text itself must stay untouched"
            );
            assert!(
                out.contains("<!-- severity: red -->"),
                "unflagged review dispatch must get the contract: {input:?}"
            );
        }

        // Ordinary implementation tasks stay contract-free.
        for input in [
            "Fix the failing test in src/lib.rs and run the suite.",
            "review the changes and apply the refactor to the module",
            "Add a caching layer to the renderer.",
        ] {
            let out = with_severity_contract(input, false);
            assert_eq!(out, input, "non-review task must not get the contract");
        }
    }

    /// The explicit flag wins even when the input carries no review wording
    /// (the caller knows the task kind better than the heuristic).
    #[test]
    fn the_explicit_flag_always_appends_the_contract() {
        let out = with_severity_contract("audit the crate thoroughly", true);
        assert!(out.contains("<!-- severity: green -->"));
    }

    /// `is_review_task` must answer exactly what `with_severity_contract`
    /// asked when it decided to append: the same task that got the contract
    /// is the task whose report gets the header enforced.
    #[test]
    fn is_review_task_agrees_with_the_contract_append() {
        assert!(is_review_task("plain task", true));
        assert!(is_review_task(
            "Faça um CODE REVIEW do último commit.",
            false
        ));
        assert!(!is_review_task("plain task", false));
        assert!(!is_review_task(
            "review the changes and apply the refactor",
            false
        ));
    }

    /// Enforcement (the point of the fix): a review report WITHOUT the
    /// header gets one injected — inferred from its own findings — so the
    /// box tint works even when the sub-agent ignored the DSL.
    #[test]
    fn a_headerless_review_report_gets_the_header_injected() {
        // The report's text is untouched: the injected header is PREPENDED,
        // never merged into the body.
        let report = "## Findings\n\n- MAJOR: the parser drops comments.\n";
        let out = enforce_severity_header(report, true);
        assert!(out.starts_with("<!-- severity: yellow -->\n"));
        assert!(out.ends_with(report));
        assert_eq!(extract_severity(&out).0, Some(Severity::Yellow));
    }

    /// The inference maps the report vocabulary to the three severities.
    #[test]
    fn the_inference_reads_the_findings_vocabulary() {
        let word = |report: &str| {
            let out = enforce_severity_header(report, true);
            extract_severity(&out).0
        };
        assert_eq!(
            word("CRITICAL: use-after-free in the render loop."),
            Some(Severity::Red)
        );
        assert_eq!(
            word("MAJOR: the lock is held across the await point."),
            Some(Severity::Yellow)
        );
        assert_eq!(
            word("VERDICT: CHANGES REQUIRED (naming only)."),
            Some(Severity::Yellow)
        );
        assert_eq!(
            word("All checks pass; only cosmetic notes remain."),
            Some(Severity::Green)
        );
    }

    /// Regression (review round 2): the scan is token-based and
    /// negation-aware. A CLEAN report must never be tinted red/yellow —
    /// that is the loudest mis-color this inference could produce.
    #[test]
    fn the_inference_never_colors_a_clean_report_by_substring_or_negation() {
        let word = |report: &str| {
            let out = enforce_severity_header(report, true);
            extract_severity(&out).0
        };
        // "majority" is not a MAJOR finding (substring false positive).
        assert_eq!(
            word("For the majority of the changes the code follows the conventions."),
            Some(Severity::Green)
        );
        // Negated findings are ABSENT, not present.
        assert_eq!(
            word("No critical findings; no major issues were raised."),
            Some(Severity::Green)
        );
        assert_eq!(
            word("Nothing is a major concern; VERDICT: no changes required."),
            Some(Severity::Green)
        );
        assert_eq!(
            word("There is without any critical defect in the patch."),
            Some(Severity::Green)
        );
        // A real finding still wins even after earlier negated prose.
        assert_eq!(
            word("No critical findings in module A. CRITICAL: data loss in module B."),
            Some(Severity::Red)
        );
        // The negation shadow does not extend across intervening prose.
        assert_eq!(
            word("No blockers this round. MAJOR: the parser drops comments."),
            Some(Severity::Yellow)
        );
    }

    /// An empty (or whitespace-only) report is never minted into a
    /// header-only report by a hypothetical caller that skipped the
    /// call-site guard.
    #[test]
    fn an_empty_report_is_never_enforced() {
        assert_eq!(enforce_severity_header("", true), "");
        assert_eq!(enforce_severity_header("   \n  ", true), "   \n  ");
    }

    /// A report that ALREADY carries a valid header passes through
    /// untouched — enforcement never overrides the sub-agent's own verdict.
    #[test]
    fn a_report_with_a_header_passes_through_untouched() {
        let report = "<!-- severity: red -->\nCRITICAL: data loss.\n";
        assert_eq!(enforce_severity_header(report, true), report);
    }

    /// Non-review tasks are never touched, even when their text mentions
    /// findings-like vocabulary (a build log can contain "CRITICAL").
    #[test]
    fn a_non_review_report_is_never_touched() {
        let report = "CRITICAL: the build log shows a warning.\n";
        assert_eq!(enforce_severity_header(report, false), report);
    }
}
