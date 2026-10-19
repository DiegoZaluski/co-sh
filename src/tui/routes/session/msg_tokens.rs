use super::*;

/// Generous in-memory budget for the per-message render cache (cells +
/// text regions). Once the total exceeds this, the oldest entries outside a
/// guard around the viewport are evicted LRU-style — the transcript on disk
/// keeps the full session, so scroll-back simply re-renders the evicted
/// message. Bounding this cache keeps the TUI fluid on very long sessions.
///
/// This bounds the ACTIVE cache set only. The parked width snapshot (see
/// [`WidthCacheSnapshot`], kept so a left-panel toggle restores the other
/// width's caches in O(1)) holds its own copy — gated at capture/swap time to
/// the low-water mark below, so while a snapshot is parked the peak is at
/// most `RENDER_CACHE_BUDGET + RENDER_CACHE_LOW_WATER`.
pub(super) const RENDER_CACHE_BUDGET: usize = 64 * 1024 * 1024; // 64 MB

/// Eviction stops once the cache is back under this water mark, so a small
/// scroll doesn't immediately re-trigger eviction (anti-thrash).
pub(super) const RENDER_CACHE_LOW_WATER: usize = 48 * 1024 * 1024; // 48 MB

/// Row margin (in content rows) above/below the viewport that is never
/// evicted — ordinary scrolling within a few screens stays re-render-free.
pub(super) const RENDER_CACHE_GUARD_ROWS: i32 = 400;

/// Convert a `ToolStatus` to a single byte for hashing.
pub(super) fn status_to_u8(status: &ToolStatus) -> u8 {
    match status {
        ToolStatus::Running => 0,
        ToolStatus::Completed => 1,
        ToolStatus::Failed(_) => 2,
    }
}

/// Combined hash of part metadata (lengths, statuses, counts).
/// Used to detect streaming/tool-status changes without full re-parse.
pub(super) fn msg_change_token(msg: &Message) -> u64 {
    hash_parts(msg)
}

/// True when the message contains a still-running compaction line — its
/// stopwatch ticks every frame, so the per-message cell cache must be
/// bypassed (the stored part data does not change while running, only the
/// rendered elapsed time does).
pub(super) fn msg_has_running_compaction(msg: &Message) -> bool {
    msg.parts
        .iter()
        .any(|p| matches!(p, Part::Compaction(c) if c.is_running()))
}

/// True when the message contains a still-running install line — same
/// contract as the compaction stopwatch: the part data does not change
/// while running, only the rendered elapsed time does, so the per-message
/// cell cache must be bypassed for the counter to tick every frame.
pub(super) fn msg_has_running_install(msg: &Message) -> bool {
    msg.parts
        .iter()
        .any(|p| matches!(p, Part::Install(c) if c.is_running()))
}

/// Indices of messages that can still change in place, so their change token
/// genuinely needs re-checking: those holding a `Running` tool (its output
/// grows / its status transitions) or a `Running` compaction (it finalizes
/// once). Every other message is immutable once set, so re-hashing its full
/// content every frame would pay O(whole transcript) per frame for nothing.
/// The scan is a cheap O(parts) status pass — no content hashing.
pub(super) fn mutable_msg_indices(session: &crate::types::Session) -> Vec<usize> {
    session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.parts.iter().any(|p| {
                matches!(p, Part::Tool(t) if matches!(t.status, ToolStatus::Running))
                    || matches!(p, Part::Compaction(c) if c.is_running())
                    || matches!(p, Part::Install(c) if c.is_running())
            })
        })
        .map(|(i, _)| i)
        .collect()
}

pub(super) fn msg_content_token(
    msg: &Message,
    config_token: u64,
    max_w: u16,
    tool_state: &ToolRenderState,
) -> u64 {
    let mut h = hash_parts(msg);
    // Expansion toggles must invalidate the per-message render cache. The
    // `version` counter is incremented on every expand/collapse toggle, so
    // hashing it (O(1)) is sufficient — `ensure_height_caches_fresh` already
    // forces a full cache rebuild (clearing `msg_cache_tokens`) whenever the
    // version changes, so the per-part expansion state never needs to be
    // hashed individually.
    h = h.wrapping_mul(31).wrapping_add(tool_state.version);
    h = h.wrapping_mul(31).wrapping_add(config_token);
    h = h.wrapping_mul(31).wrapping_add(max_w as u64);
    h
}
