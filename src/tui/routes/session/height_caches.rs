use super::*;

impl SessionView {
    /// Ensure height caches match the current session. Performs a full rebuild
    /// if terminal width changed or config changed. If only message count grew
    /// (new messages appended), extends the cache incrementally without touching
    /// existing entries. Otherwise, incrementally updates only the last message
    /// if its content changed (streaming — tool status, output, reasoning, text).
    /// Returns cache update details so callers can clear render caches and
    /// distinguish remote content growth from local expand/collapse layout.
    pub(super) fn ensure_height_caches_fresh(
        &mut self,
        session: &crate::types::Session,
        max_w: u16,
        config: &TuiConfig,
        mode: HeightCacheMode,
        streaming: bool,
    ) -> HeightCacheUpdate {
        let config_tok = config_token(config);
        let session_changed = self.last_session_id.as_deref() != Some(session.id.as_str());
        // Streaming pins are session-scoped: message ids (`msg-N`) are not
        // unique across sessions, so a pin carried over a switch could bind
        // to an unrelated message in the new session.
        if session_changed {
            self.last_streamed_msg = None;
        }
        let tool_state_changed = self.last_tool_state_version != self.tool_state.version;
        let cache_stale = session_changed
            || tool_state_changed
            || self.msg_height_cache.len() != session.messages.len()
            || self.cache_max_w != max_w
            || self.cache_config_token != config_tok;

        // Mouse hit-testing between frames (`update_hover` / `handle_mouse`):
        // the cursor must be mapped against the layout the last render
        // PAINTED. When the caches are structurally fresh, re-estimating
        // content here would mutate the shared caches and desynchronize the
        // hover/click geometry from the screen (the highlight used to jump
        // onto the block above a freshly injected queued message while the
        // agent streamed). Structural staleness still rebuilds below — the
        // old layout cannot be mapped onto different content safely.
        if mode == HeightCacheMode::PaintedOnly && !cache_stale {
            return HeightCacheUpdate::default();
        }

        if cache_stale {
            // ── Width-snapshot swap: restore a previously-rendered width ─────
            // A left-panel toggle changes ONLY `cache_max_w`; the caches for
            // the width being re-entered were parked as a snapshot when the
            // rebuild for the opposite width happened. If that snapshot still
            // describes this exact session/config/expansion/message-count/
            // width, swap it in O(1) instead of re-estimating every message and
            // re-rendering the visible viewport from scratch — the ~60-80ms
            // toggle frame (→ a second on a real 10h transcript) that froze the
            // header animation. The outgoing active set was in steady state
            // this frame, so it is healthy and gets parked back in the snapshot
            // slot for the next toggle. Streaming does not disturb this: the
            // streaming message cache re-validates by width every frame, and a
            // message whose content changed re-renders itself because its
            // per-message cell token no longer matches the restored cache.
            if let Some(mut snap) = self.width_cache_snapshot.take()
                && !session_changed
                && !tool_state_changed
                && snap.session_id == session.id
                && snap.msg_len == session.messages.len()
                && snap.config_tok == config_tok
                && snap.tool_state_version == self.tool_state.version
                && snap.cache_max_w == max_w
            {
                snap.swap_with_view(self);
                // The reversed snapshot slot now holds the outgoing set;
                // keep it only if its cached cells are small enough that
                // snapshot + active stay within the render-cache budget
                // (48MB low-water per set, mirroring `prune_render_cache`).
                self.width_cache_snapshot =
                    (snap.msg_cache_bytes <= RENDER_CACHE_LOW_WATER).then_some(snap);
                log::debug!("[PERF] msg_height_cache: width-snapshot swap to max_w={max_w}");
                // Sticky-bottom parity with the old full-rebuild path: the
                // restored set's `last_content_height` matches its restored
                // total, so the render-time `total_height !=
                // last_content_height` guard would NOT fire and a sticky user
                // would be left a wrap-delta above the bottom on every swap
                // frame. Bumping the stamp forces that same recalc this frame
                // (with the fresh viewport height), exactly like a full
                // rebuild used to.
                self.last_content_height = self.last_content_height.wrapping_sub(1);
                return HeightCacheUpdate {
                    full_rebuild: false,
                    tool_state_changed: false,
                };
            }
            // An invalid snapshot was `take`n and dropped (its validation keys
            // no longer describe this session/config/expansion/messages/width)
            // or no snapshot existed — either way, continue to the ordinary
            // rebuild/extend path below.

            let config_or_width_changed =
                self.cache_max_w != max_w || self.cache_config_token != config_tok;
            let count_grew =
                !config_or_width_changed && session.messages.len() > self.msg_height_cache.len();

            if config_or_width_changed || tool_state_changed || !count_grew || session_changed {
                // Full rebuild: config/width changed, or count decreased
                let _start = Instant::now();
                // Park the outgoing set as a snapshot so a toggle back to this
                // width is an O(1) swap instead of a second full rebuild. Only
                // worth keeping when the set is healthy for the CURRENT
                // session: non-empty, covers the whole transcript (a stale
                // length would never re-validate), was built under the same
                // session and tool-expansion version (the version counter is
                // monotonic, so an older version can never match again), and is
                // small enough that the snapshot never doubles the
                // render-cache memory.
                if !session_changed
                    && !tool_state_changed
                    && !self.msg_height_cache.is_empty()
                    && self.msg_height_cache.len() == session.messages.len()
                    && self.msg_cache_bytes <= RENDER_CACHE_LOW_WATER
                {
                    self.width_cache_snapshot = Some(Box::new(WidthCacheSnapshot::capture(self)));
                }
                self.msg_height_cache.clear();
                self.part_heights_cache.clear();
                // Parallel cold build (rayon): every message's height
                // estimation is pure and independent — `estimate_part_height`
                // reads only `tool_state` (immutable here) plus message data,
                // and the markdown estimator memoizes its work in global
                // `Mutex`-guarded LRU caches. On a unique-content transcript
                // (the real 10-hour case) this is the single dominant cost of
                // the first reflow at a new width — ~645ms sequential for 800
                // messages, ~145ms across 12 cores — so splitting it across
                // cores is the one change that moves the user-visible "first
                // open" wall. The per-frame incremental paths below stay
                // sequential: they touch a handful of messages, where thread
                // dispatch would cost more than it saves. Small transcripts
                // stay sequential too: below ~32 messages the per-message work
                // (often a single line) is cheaper than the rayon task
                // dispatch it would pay for.
                if session.messages.len() >= 32 {
                    let (part_hs_vec, msg_h_vec): (Vec<Vec<u16>>, Vec<i32>) = session
                        .messages
                        .par_iter()
                        .map(|m| {
                            let part_hs: Vec<u16> = m
                                .parts
                                .iter()
                                .map(|p| {
                                    Self::estimate_part_height(
                                        p,
                                        max_w,
                                        config,
                                        &m.role,
                                        &self.tool_state,
                                    )
                                })
                                .collect();
                            let msg_h = Self::render_message_height(
                                m,
                                max_w,
                                config,
                                Some(&part_hs),
                                &self.tool_state,
                            );
                            (part_hs, msg_h)
                        })
                        .unzip();
                    self.part_heights_cache = part_hs_vec;
                    self.msg_height_cache = msg_h_vec;
                } else {
                    for m in session.messages.iter() {
                        let part_hs: Vec<u16> = m
                            .parts
                            .iter()
                            .map(|p| {
                                Self::estimate_part_height(
                                    p,
                                    max_w,
                                    config,
                                    &m.role,
                                    &self.tool_state,
                                )
                            })
                            .collect();
                        let msg_h = Self::render_message_height(
                            m,
                            max_w,
                            config,
                            Some(&part_hs),
                            &self.tool_state,
                        );
                        self.part_heights_cache.push(part_hs);
                        self.msg_height_cache.push(msg_h);
                    }
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.msg_change_tokens = session.messages.iter().map(msg_change_token).collect();
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                self.prev_mutable_msgs = mutable_msg_indices(session);
                self.cached_total_height = self.rebuild_prefix_y();
                // When the session, expansion state, terminal width, or config
                // changed, the previous actual_total_height was computed for a
                // DIFFERENT layout and is NOT comparable to the new cached
                // height. Using .max() would preserve the old, larger value,
                // inflating max_scroll. This allows scroll_y to point beyond
                // the actual content, pushing all messages above the viewport.
                //
                // Only preserve the previous actual_total_height via .max()
                // when the layout is unchanged: during streaming, the rendered
                // height may slightly exceed the cache estimate, and .max()
                // prevents a scroll gap below the streaming message.
                if config_or_width_changed || tool_state_changed || session_changed {
                    self.actual_total_height = self.cached_total_height;
                } else {
                    self.actual_total_height =
                        self.actual_total_height.max(self.cached_total_height);
                }

                // When switching to a different session, scroll to the bottom
                // so the user sees the latest messages without manual scrolling.
                if session_changed {
                    self.scroll_y = self.cached_total_height;
                    self.has_manual_scroll = false;
                    self.is_sticky_bottom = true;
                    self.scroll_accumulator_y = 0.0;
                }

                log::debug!(
                    "[PERF] msg_height_cache: cold_build={}us msgs={}",
                    _start.elapsed().as_micros(),
                    session.messages.len()
                );
                HeightCacheUpdate {
                    full_rebuild: true,
                    tool_state_changed,
                }
            } else {
                // Extend cache with new messages only (same width/config)
                let prev_len = self.msg_height_cache.len();
                for m in session.messages.iter().skip(prev_len) {
                    let part_hs: Vec<u16> = m
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(p, max_w, config, &m.role, &self.tool_state)
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        m,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    self.part_heights_cache.push(part_hs);
                    self.msg_height_cache.push(msg_h);
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.msg_change_tokens
                    .extend(session.messages.iter().skip(prev_len).map(msg_change_token));
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                self.prev_mutable_msgs = mutable_msg_indices(session);
                // Incremental: add new messages' heights plus a gap for each.
                // All new messages have idx > 0 (prev_len >= 1), so gap = 1 per message.
                let new_count = self.msg_height_cache.len() - prev_len;
                let added_heights: i32 = self.msg_height_cache[prev_len..].iter().sum();
                self.cached_total_height += added_heights + new_count as i32;
                // Extend prefix_y: the previous last entry is the OLD total
                // (= the end of the last old message) — pop it, then each new
                // message starts one row below its predecessor's end (the
                // 1-row gap, except for the very first message of the
                // timeline) and ends after its own height. The final pushed
                // value is the new total.
                if self.prefix_y.is_empty() {
                    self.prefix_y.push(0);
                }
                let mut y = self.prefix_y.pop().unwrap_or(0);
                for (k, h) in self.msg_height_cache[prev_len..].iter().enumerate() {
                    if prev_len + k > 0 {
                        y += 1; // gap before this message
                    }
                    self.prefix_y.push(y); // its start position
                    y += h; // its end
                }
                self.prefix_y.push(y); // new total (no trailing gap)
                // Sync actual_total_height using .max() — see note above.
                self.actual_total_height = self.actual_total_height.max(self.cached_total_height);
                // The injection frame lands HERE (count grew): the pinned
                // streaming message is NOT one of the new messages, so the
                // loop above skipped it. Re-estimate it now — its deltas
                // since the last build would otherwise be clipped by the
                // stale height on this very frame, AND the walk would save
                // cells carrying the FRESH content token rendered with the
                // STALE heights, poisoning the per-message cell cache (the
                // next frame would then blit the clipped content as a hit).
                if let Some(pin) = &self.last_streamed_msg
                    && let Some(pi) = session.messages.iter().position(|m| m.id == *pin)
                    && pi < prev_len
                {
                    let msg = &session.messages[pi];
                    let part_hs: Vec<u16> = msg
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(
                                p,
                                max_w,
                                config,
                                &msg.role,
                                &self.tool_state,
                            )
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        msg,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    self.part_heights_cache[pi] = part_hs;
                    self.msg_height_cache[pi] = msg_h;
                    if pi < self.msg_change_tokens.len() {
                        self.msg_change_tokens[pi] = msg_change_token(msg);
                    }
                    // The pinned message's height changed inside the OLD
                    // range: recompute the whole prefix chain and the total
                    // (O(n) sums, injection frame only).
                    self.cached_total_height = self.rebuild_prefix_y();
                    self.actual_total_height =
                        self.actual_total_height.max(self.cached_total_height);
                }
                log::debug!(
                    "[PERF] msg_height_cache: extended prev={} now={}",
                    prev_len,
                    session.messages.len()
                );
                HeightCacheUpdate {
                    full_rebuild: false,
                    tool_state_changed,
                }
            }
        } else {
            // Incremental update: re-estimate ANY message that can still
            // change. Streaming text and tool completion normally touch only
            // the last message, but ToolResult/ToolError (app.rs) and glob/grep
            // ToolOutput (app.rs) search EVERY message for the running tool
            // part, so a box in an OLDER message can complete or grow while a
            // newer message streams below. Re-estimating it keeps the cached
            // height in sync with the box the render actually draws.
            //
            // Only messages that CAN change are re-hashed each pass: the last
            // message (always) plus any message with a Running tool / Running
            // compaction, plus those that had a running part LAST pass (a tool
            // completing in place between frames is no longer `Running`, but we
            // still catch it once so its final height lands). Everything else is
            // immutable, so hashing its full content per frame would pay
            // O(whole transcript) every frame for nothing.
            let mut any_changed = false;
            let mut changed_last = false;
            // When the streaming cache is live for the last message, its
            // per-part heights are authoritative and were already brought up to
            // date by `update_streaming_message_cache` this frame. Skip the
            // O(n) content hash + per-part re-estimate entirely.
            // The streaming cache's heights are authoritative for the LAST
            // message (refreshed by `update_streaming_message_cache` earlier
            // this frame). Cloned out so the scan below can mutate `self`.
            // Mouse paths (PaintedOnly) never reach this block.
            let stream_last: Option<(u16, Vec<u16>)> = self
                .streaming_msg
                .as_ref()
                .filter(|s| {
                    session
                        .messages
                        .last()
                        .is_some_and(|m| s.message_id == m.id)
                })
                .map(|s| (s.height, s.part_hs.clone()));
            if !self.msg_height_cache.is_empty() {
                let last_idx = session.messages.len() - 1;
                // A queued message injected below the still-streaming one
                // makes it no-longer-last; the pin keeps it a candidate so
                // its continued deltas re-estimate instead of freezing (the
                // "cut block" symptom). It settles itself once its content
                // stops changing.
                let pinned_idx = self
                    .last_streamed_msg
                    .as_ref()
                    .and_then(|id| session.messages.iter().position(|m| m.id == *id));
                // Take last pass's mutable set (carried over to catch an
                // in-place completion), then store this pass's for next time.
                let prev_mutable = std::mem::take(&mut self.prev_mutable_msgs);
                let curr_mutable = mutable_msg_indices(session);
                let is_candidate = |idx: usize| {
                    idx == last_idx
                        || Some(idx) == pinned_idx
                        || prev_mutable.binary_search(&idx).is_ok()
                        || curr_mutable.binary_search(&idx).is_ok()
                };
                for (idx, msg) in session.messages.iter().enumerate() {
                    if !is_candidate(idx) {
                        continue;
                    }
                    if idx == last_idx
                        && let Some((s_height, s_part_hs)) = stream_last.as_ref()
                    {
                        // Streaming message: use the cache's heights without
                        // hashing. The stored token is the cache height — a
                        // cheap monotonic stand-in; once streaming ends the
                        // normal `msg_change_token` path re-estimates once and
                        // replaces it.
                        if self.msg_height_cache.get(idx) != Some(&(*s_height as i32)) {
                            if idx < self.part_heights_cache.len() {
                                self.part_heights_cache[idx] = s_part_hs.clone();
                                self.msg_height_cache[idx] = *s_height as i32;
                            }
                            if idx < self.msg_change_tokens.len() {
                                self.msg_change_tokens[idx] = *s_height as u64;
                            }
                            changed_last = true;
                            any_changed = true;
                        }
                        continue;
                    }
                    let token = msg_change_token(msg);
                    if token == self.msg_change_tokens.get(idx).copied().unwrap_or(u64::MAX) {
                        continue;
                    }
                    let part_hs: Vec<u16> = msg
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(
                                p,
                                max_w,
                                config,
                                &msg.role,
                                &self.tool_state,
                            )
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        msg,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    if idx < self.part_heights_cache.len() {
                        self.part_heights_cache[idx] = part_hs;
                        self.msg_height_cache[idx] = msg_h;
                    }
                    if idx < self.msg_change_tokens.len() {
                        self.msg_change_tokens[idx] = token;
                    }
                    changed_last |= idx == last_idx;
                    any_changed = true;
                }
                self.prev_mutable_msgs = curr_mutable;
            }
            if !streaming {
                // Turn ended: the scan above just landed the pinned message's
                // final height (its streaming stand-in token no longer
                // matched the real content hash, forcing the one-shot
                // re-estimate). Retire the pin so no message stays a
                // per-frame re-hash candidate for the rest of the session.
                self.last_streamed_msg = None;
            }
            if any_changed {
                // The changed message's height moved, so every start position
                // after the first changed message shifts — rebuild the whole
                // prefix_y (O(n) sums) and the cached total.
                self.cached_total_height = self.rebuild_prefix_y();
                self.last_msg_change_token = if let Some((s_height, _)) = stream_last.as_ref() {
                    // Streaming: avoid the O(n) hash of the last message.
                    *s_height as u64
                } else {
                    session.messages.last().map(msg_change_token).unwrap_or(0)
                };
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                // Sync actual_total_height using .max() — see note above.
                self.actual_total_height = self.actual_total_height.max(self.cached_total_height);
                if self.render_frame.is_multiple_of(30) {
                    log::debug!(
                        "[PERF] msg_height_cache: incremental update (last={changed_last})"
                    );
                }
            }
            HeightCacheUpdate::default()
        }
    }
}
