use super::*;

/// All compaction phases use muted gray to indicate system messages.
pub(super) fn compaction_color(theme: &Theme) -> RGBA {
    theme.text_muted
}

/// The chat line text for the LLM "Summarizing" box. `now` is the current
/// wall-clock millis: while the stopwatch line is running the elapsed number
/// ticks every frame (the moving number IS the activity signal — no spinner
/// needed); when it finalizes the same line freezes.
pub(super) fn compaction_line(part: &CompactionPart, now: u64) -> String {
    let elapsed = part
        .elapsed_ms
        .unwrap_or_else(|| now.saturating_sub(part.started_at));
    // Millisecond precision — the second counter flips visibly.
    let secs = elapsed as f64 / 1000.0;
    format!("llm compaction · {secs:.3}")
}

/// The chat line for a [`Part::Install`] (the decision-model weights
/// install). Rendered with the SAME bar component as the header budget bar
/// (`render_budget_bar` from `component::budget`) so the two read as one
/// component. While the download runs the elapsed time ticks live; the
/// terminal event freezes the line.
pub(super) fn install_line(part: &InstallPart, now: u64) -> String {
    // The model's name ("installing Laya ▓▓▓…"); an empty field (sessions
    // persisted before the name existed) falls back to the generic label.
    let subject = if part.model.is_empty() {
        "decision model"
    } else {
        &part.model
    };
    // 0% until the first file announces its size; the mirror's big graph
    // dominates the total, so the bar fills in one visible sweep.
    let pct = (part.bytes_done.min(part.bytes_total))
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(part.bytes_total))
        .unwrap_or(0) as u8;
    let mb_done = part.bytes_done / (1024 * 1024);
    let mb_total = part.bytes_total / (1024 * 1024);
    if part.is_running() {
        let elapsed = now.saturating_sub(part.started_at);
        let secs = elapsed as f64 / 1000.0;
        let bar = crate::component::budget::render_budget_bar(pct);
        format!("installing {subject} {bar} {pct:>3}% · {mb_done}/{mb_total} MB · {secs:.1}s")
    } else if part.failed == Some(true) {
        let bar = crate::component::budget::render_budget_bar(pct);
        format!("installing {subject} {bar} failed")
    } else {
        // The frozen byte counts can LAG the Finished event (the last
        // Progress update is throttled), so the completion branch forces
        // the bar to full instead of trusting them — the label reads 100%,
        // the bar must match.
        let bar = crate::component::budget::render_budget_bar(100);
        format!("installed {subject} {bar} 100%")
    }
}

/// Max body lines of the COLLAPSED "Summarizing" box. When the streamed
/// summary exceeds it, only the LAST lines stay visible — the top lines
/// visually leave the box (an LRU-like scroll-up effect, nothing is actually
/// removed). Expanded, the box grows to fit the whole text.
pub(super) const SUMMARIZING_COLLAPSED_LINES: u16 = 8;

/// Vertical padding inside the box: 1 blank row above the title and 1 below
/// the last content row, so the content never touches the box edges.
pub(super) const SUMMARIZING_PAD_V: u16 = 1;

/// Stable expand/collapse key for a "Summarizing" box: one per LLM-compaction
/// line, keyed by its `started_at`.
pub(super) fn summarizing_id(part: &CompactionPart) -> String {
    format!("summarize-{}", part.started_at)
}
/// The rendered height of the "Summarizing" box. The body is laid out with the
/// SAME markdown algorithm as the render ([`estimate_height`], pulldown_cmark),
/// so the allocated height always matches the drawn body exactly. Collapsed: a
/// fixed preview of the LAST [`SUMMARIZING_COLLAPSED_LINES`] rows plus a hint
/// row when the body overflows; expanded: the whole markdown body. Every case
/// adds [`SUMMARIZING_PAD_V`] rows of padding above the title and below the
/// content.
pub(super) fn summarizing_height(part: &CompactionPart, expanded: bool, max_w: u16) -> u16 {
    let wrap_w = max_w.saturating_sub(3).max(1);
    let body_h = estimate_height(&part.text, wrap_w).max(1);
    let pad = SUMMARIZING_PAD_V * 2;
    if expanded {
        body_h.saturating_add(1).saturating_add(pad) // pad + title + full body + pad
    } else if body_h > SUMMARIZING_COLLAPSED_LINES {
        SUMMARIZING_COLLAPSED_LINES
            .saturating_add(2)
            .saturating_add(pad) // pad + title + preview + hint + pad
    } else {
        body_h.saturating_add(1).saturating_add(pad)
    }
}
