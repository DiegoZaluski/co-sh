//! Smart truncation for large tool outputs heading to the model context.
//!
//! Strategy (head / middle / tail):
//!
//! - keep the **head** — the command's intent, headers, early errors;
//! - keep the **tail** — the final lines and the exit status;
//! - when the output exceeds the token budget, the **middle** is written
//!   verbatim to a scratch log at `<OS temp>/cosh/<content-hash>.log` and
//!   replaced in the model-facing text with a structured notice pointing at
//!   the file. The model can then read the omitted section back gradually
//!   with `fs_read` (using `line_range`) or `find_grep` — no re-execution,
//!   no information loss.
//!
//! The TUI is unaffected: it receives the full output as a live stream via
//! `HarnessEvent::ToolOutput` before this truncation runs, so the user always
//! sees everything.

use std::path::PathBuf;

use xxhash_rust::xxh64::xxh64;

use crate::util::estimate_tokens;

/// Token budget for the model-facing version of a single `bash_run` output.
///
/// Above this, the output is truncated (head + tail kept, middle → scratch
/// log). Estimated with the generic OpenAI `cl100k_base` tokenizer
/// ([`estimate_tokens`]) — the de-facto standard for cross-model estimation.
pub const MAX_TOOL_OUTPUT_TOKENS: usize = 3000;

/// Token budget kept from the beginning of the output.
///
/// Slightly larger than the tail: the beginning carries the command's intent,
/// headers, and the first errors, which are usually the most informative.
const HEAD_TOKEN_BUDGET: usize = 2000;

/// Token budget kept from the end of the output (final lines + exit status).
const TAIL_TOKEN_BUDGET: usize = 1000;

// Soft-cap note: the model-facing text may exceed `MAX_TOOL_OUTPUT_TOKENS` by
// a small margin — the per-line budget rounding plus the ~100-token notice.
// This is intentional: the budget guards against runaway context growth, not
// against a few dozen extra tokens.

/// Result of [`truncate_tool_output`].
pub struct TruncatedOutput {
    /// The model-facing text: verbatim when small, head + notice + tail when truncated.
    pub text: String,
    /// Path of the scratch log holding the full middle, when truncated.
    pub log_path: Option<PathBuf>,
    /// Whether the output was truncated.
    pub truncated: bool,
}

/// Truncate `output` for the model context using the head / middle / tail
/// strategy.
///
/// Outputs at or under [`MAX_TOOL_OUTPUT_TOKENS`] pass through verbatim.
/// Larger outputs keep whole lines from the start and end of the output
/// within the head/tail budgets; the middle is written to the scratch log and
/// replaced with a notice pointing at it.
#[must_use]
pub fn truncate_tool_output(output: &str) -> TruncatedOutput {
    if estimate_tokens(output) <= MAX_TOOL_OUTPUT_TOKENS {
        return TruncatedOutput {
            text: output.to_string(),
            log_path: None,
            truncated: false,
        };
    }

    // Index lines once as (byte offset of line start, line including '\n').
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut pos = 0usize;
    for piece in output.split_inclusive('\n') {
        lines.push((pos, piece));
        pos += piece.len();
    }

    // Head: keep whole lines from the start while they fit the budget.
    let mut head_end = 0usize;
    let mut head_tokens = 0usize;
    for (start, line) in &lines {
        let tokens = estimate_tokens(line);
        if head_tokens + tokens > HEAD_TOKEN_BUDGET {
            break;
        }
        head_tokens += tokens;
        head_end = start + line.len();
    }

    // Tail: keep whole lines from the end while they fit the budget.
    let mut tail_start = output.len();
    let mut tail_tokens = 0usize;
    for (start, line) in lines.iter().rev() {
        let tokens = estimate_tokens(line);
        if tail_tokens + tokens > TAIL_TOKEN_BUDGET {
            break;
        }
        tail_tokens += tokens;
        tail_start = *start;
    }

    // Degenerate case: a handful of giant lines exceed both budgets and the
    // head/tail regions overlap (e.g. a single minified line larger than the
    // whole budget). Fall back to proportional byte slicing — cutting mid-line
    // is unavoidable for a single line larger than the budget.
    let (head_end, tail_start) = if head_end < tail_start {
        (head_end, tail_start)
    } else {
        let len = output.len();
        let head_bytes = (HEAD_TOKEN_BUDGET * 4).min(len / 2);
        let tail_keep = (TAIL_TOKEN_BUDGET * 4).min(len / 2);
        let head = output.floor_char_boundary(head_bytes);
        let tail = output.floor_char_boundary(len - tail_keep);
        if head < tail {
            (head, tail)
        } else {
            // Pathologically small for its token count — give up gracefully.
            return TruncatedOutput {
                text: output.to_string(),
                log_path: None,
                truncated: false,
            };
        }
    };

    let middle = &output[head_end..tail_start];
    let middle_lines = middle.lines().count();
    let middle_bytes = middle.len();
    let middle_tokens = estimate_tokens(middle);

    let log_path = write_middle_log(middle);
    let notice = match &log_path {
        Some(path) => truncation_notice(
            middle_lines,
            middle_bytes,
            middle_tokens,
            &path.display().to_string(),
        ),
        None => format!(
            "\n════════════════════════════════════════════\n\
             [OUTPUT TRUNCATED]\n\
             The middle of this output ({middle_lines} lines, {middle_bytes} bytes, ~{middle_tokens} tokens)\n\
             was removed to conserve context and could not be saved to a log file.\n\
             If you need the middle, re-run the command and redirect its output to a file.\n\
             ════════════════════════════════════════════\n"
        ),
    };

    TruncatedOutput {
        text: format!("{}{}{}", &output[..head_end], notice, &output[tail_start..]),
        log_path,
        truncated: true,
    }
}

/// Build the structured notice that replaces the middle of a truncated output.
fn truncation_notice(
    omitted_lines: usize,
    omitted_bytes: usize,
    omitted_tokens: usize,
    path: &str,
) -> String {
    format!(
        "\n════════════════════════════════════════════\n\
         [OUTPUT TRUNCATED]\n\
         The middle of this output was removed to conserve context.\n\
         omitted: {omitted_lines} lines, {omitted_bytes} bytes (~{omitted_tokens} tokens)\n\
         The FULL output is saved at: {path}\n\
         Read it in parts with fs_read (targets: [{{\"path\": \"{path}\", \"line_range\": \"50-100\"}}])\n\
         or search it with find_grep (path: \"{path}\", line_range: \"1-100\").\n\
         ════════════════════════════════════════════\n"
    )
}

/// The harness scratch directory (`<OS temp>/cosh`) for truncated-output logs.
///
/// Uses the same constant as the path guard's scratch exemption, so the writer
/// and the guards can never drift apart — the model can always read the logs
/// back with `fs_read` / `find_grep` without permission friction.
#[must_use]
pub fn scratch_log_dir() -> PathBuf {
    std::env::temp_dir().join(cosh_tools::util::path_guard::HARNESS_SCRATCH_DIR)
}

/// Write `middle` verbatim to `<scratch>/<xxh64(middle)>.log`.
///
/// The name is a content hash, so identical middles deduplicate to a single
/// file. Returns `None` when the directory cannot be created or the write
/// fails (truncation still proceeds, just without a recoverable log).
fn write_middle_log(middle: &str) -> Option<PathBuf> {
    let dir = scratch_log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{:016x}.log", xxh64(middle.as_bytes(), 0)));
    std::fs::write(&path, middle).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic multi-line output with UNIQUE lines (indexed), guaranteed
    /// to cost many tokens (≈ 10k+) while letting tests tell head/middle/tail
    /// apart by line index.
    fn big_output() -> String {
        let mut out = String::new();
        for i in 0..800 {
            out.push_str(&format!(
                "line {i}: abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\n"
            ));
        }
        out
    }

    #[test]
    fn small_output_passes_through_untouched() {
        let out = "hello world\n";
        let result = truncate_tool_output(out);
        assert!(!result.truncated);
        assert_eq!(result.text, out);
        assert!(result.log_path.is_none());
    }

    #[test]
    fn empty_output_passes_through() {
        let result = truncate_tool_output("");
        assert!(!result.truncated);
        assert_eq!(result.text, "");
    }

    #[test]
    fn huge_output_is_truncated_with_notice_and_log() {
        let out = big_output();
        let result = truncate_tool_output(&out);

        assert!(result.truncated, "huge output must be truncated");
        assert!(
            result.text.len() < out.len(),
            "truncated text must be smaller"
        );

        // Head and tail preserved at line boundaries (cuts never split a line).
        assert!(result.text.starts_with("line 0:"), "head must be kept");
        assert!(
            result.text.ends_with("ABCDEFGHIJKLMNOPQRSTUVWXYZ\n"),
            "tail must be kept, got: {:?}",
            &result.text[result.text.len().saturating_sub(60)..]
        );
        assert!(
            result.text.contains("line 799:"),
            "final line must be in the tail"
        );

        // Notice present and actionable.
        assert!(
            result.text.contains("[OUTPUT TRUNCATED]"),
            "notice must be present"
        );
        assert!(
            result.text.contains("omitted:"),
            "notice must quantify the omission"
        );

        // Log written verbatim and recoverable. Lines are unique, so the saved
        // middle (distinct indices) cannot appear inside the kept head/tail.
        let path = result
            .log_path
            .expect("log must be written for a truncated output");
        let saved = std::fs::read_to_string(&path).expect("log must be readable");
        assert!(!saved.is_empty());
        assert!(
            out.contains(&saved),
            "log content must be a verbatim substring of the output"
        );
        assert!(
            !result.text.contains(&saved),
            "the saved middle must not appear in the truncated text"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identical_output_deduplicates_to_same_log() {
        let out = big_output();
        let a = truncate_tool_output(&out);
        let b = truncate_tool_output(&out);
        assert_eq!(
            a.log_path, b.log_path,
            "content hash must deduplicate identical middles"
        );
        if let Some(path) = a.log_path {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn single_giant_line_is_still_truncated() {
        // One line (no '\n') larger than the whole budget → the line-boundary
        // walk degenerates; the fallback must still produce head + notice + tail.
        let out = "abcd efgh ijkl mnop qrst uvwx yz12 3456 7890 ".repeat(700);
        assert_eq!(out.lines().count(), 1, "sanity: a single giant line");
        let result = truncate_tool_output(&out);
        assert!(
            result.truncated,
            "a giant single line must still be truncated"
        );
        assert!(result.text.contains("[OUTPUT TRUNCATED]"));
        assert!(result.text.contains("saved at:"));
        assert!(result.log_path.is_some());
        if let Some(path) = result.log_path {
            let saved = std::fs::read_to_string(&path).expect("log must be readable");
            assert!(out.contains(&saved));
            let _ = std::fs::remove_file(path);
        }
    }
}
