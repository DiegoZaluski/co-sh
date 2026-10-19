use super::*;

impl SessionView {
    /// Scan a buffer rectangle [x, x+w) × [y, y+h) and return the
    /// number of rows from y that contain at least one non-space glyph.
    /// Returns 0 if the rectangle is empty or all-space.
    pub(super) fn scan_content_height(buf: &Buffer, x: u16, y: u16, w: u16, h: u16) -> u16 {
        let mut last_row: Option<u16> = None;
        for check_y in y..y + h {
            for cx in x..x + w {
                if let Some(cell) = buf.cell((cx, check_y))
                    && cell.symbol().chars().next().unwrap_or(' ') != ' '
                {
                    last_row = Some(check_y);
                    break;
                }
            }
        }
        last_row.map(|r| r - y + 1).unwrap_or(0)
    }
}

impl SessionView {
    /// Approximate live bytes of one cached entry (rendered cells + region
    /// strings). Only the byte-heavy `Vec<Cell>` and the region text are
    /// counted — the fixed per-message overhead is negligible. An associated
    /// function (no `&self`) so the store sites can compute it from disjoint
    /// field borrows while the reusable `scratch` buffer is still borrowed.
    pub(super) fn cache_entry_bytes_of(
        cells: &Option<Vec<ratatui::buffer::Cell>>,
        regions: &Option<Vec<TextRegion>>,
    ) -> usize {
        let cells = cells.as_ref().map_or(0, |c| {
            c.len()
                .saturating_mul(std::mem::size_of::<ratatui::buffer::Cell>())
        });
        let regions = regions.as_ref().map_or(0, |r| {
            r.iter().fold(0usize, |acc, reg| {
                acc.saturating_add(reg.text.capacity())
                    .saturating_add(std::mem::size_of::<TextRegion>())
            })
        });
        cells.saturating_add(regions)
    }
}

impl SessionView {
    /// Evict per-message render-cache entries (cells + text regions) LRU-style
    /// until the cache is back under [`RENDER_CACHE_LOW_WATER`]. Messages
    /// inside a generous row guard around the viewport are never evicted, so
    /// ordinary scrolling stays re-render-free; only old content far from the
    /// current position sheds its cached cells. The transcript is untouched —
    /// an evicted message simply re-renders if the user scrolls back to it.
    ///
    /// The guard window is located with two binary searches over `prefix_y`
    /// (O(log n)) instead of a full timeline walk with a running row counter:
    /// [`find_first_visible`](Self::find_first_visible) bounds the messages
    /// entirely above the guard and a mirror search bounds the messages
    /// entirely below it. The candidate scan is then limited to the two
    /// evictable ranges with a single cached-cell check per message (no
    /// position arithmetic), but it still scales with the number of messages
    /// OUTSIDE the guard — so when the viewport sits at the bottom of a long
    /// timeline the front range covers most of it. The win is the O(log n)
    /// boundaries, the O(1) per-item work, and skipping the guard interior.
    /// The eviction then sizes and orders only the entries it will evict: a
    /// partial selection finds the smallest recency-order window that covers
    /// the overshoot and sorts that window alone, instead of sorting every
    /// candidate.
    pub(super) fn prune_render_cache(&mut self, vp_top: i32, vp_bottom: i32) {
        if self.msg_cache_tokens.is_empty() {
            return;
        }
        let n = self.msg_cache_tokens.len();
        // The render path resizes every render-cache vector and `prefix_y`
        // together, but guard cheaply against direct use elsewhere. The
        // assert surfaces invariant drift in debug builds instead of the
        // early return masking it silently.
        debug_assert_eq!(self.msg_height_cache.len(), n);
        debug_assert_eq!(self.prefix_y.len(), n + 1);
        if self.msg_height_cache.len() != n || self.prefix_y.len() != n + 1 {
            return;
        }
        // Content-space guard: the visible content window is
        // [scroll_y, scroll_y + viewport_height], and the message walk below
        // is in content rows. `vp_top`/`vp_bottom` are SCREEN rows — only
        // their difference (the viewport height) is coordinate-independent.
        let visible_h = vp_bottom - vp_top;
        let guard_top = self.scroll_y - RENDER_CACHE_GUARD_ROWS;
        let guard_bottom = self.scroll_y + visible_h + RENDER_CACHE_GUARD_ROWS;

        // Guard window via binary search over `prefix_y` (both O(log n)):
        // `above_guard_end` is the first message whose bottom crosses
        // `guard_top` (messages [0, above_guard_end) sit entirely above the
        // guard); `below_guard_start` is the first message whose top is at or
        // after `guard_bottom` (messages [below_guard_start, n) sit entirely
        // below). Messages in [above_guard_end, below_guard_start) intersect
        // the guard and are never evicted. `prefix_y` is monotonic (heights
        // are non-negative), so the two boundaries never cross.
        let above_guard_end = self.find_first_visible(guard_top);
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.prefix_y[mid] >= guard_bottom {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let below_guard_start = lo;

        // Only the two evictable ranges are scanned, with the guard test
        // already baked into the boundaries — per-message work is a single
        // cached-cell check (no position arithmetic).
        let mut candidates: Vec<usize> =
            Vec::with_capacity(above_guard_end + (n - below_guard_start));
        for idx in 0..above_guard_end {
            if self.msg_cache_cells[idx].is_some() {
                candidates.push(idx);
            }
        }
        for idx in below_guard_start..n {
            if self.msg_cache_cells[idx].is_some() {
                candidates.push(idx);
            }
        }
        if candidates.is_empty() {
            return;
        }
        let overshoot = self.msg_cache_bytes.saturating_sub(RENDER_CACHE_LOW_WATER);
        if overshoot == 0 {
            return;
        }
        // Evict oldest-first until back under the low-water mark. Only a
        // prefix of the recency order is ever evicted (the byte crossing
        // lands inside the first `m` candidates), so instead of sorting the
        // whole candidate list (O(k log k)) find the smallest power-of-two
        // window whose cumulative size covers the overshoot: each
        // `select_nth_unstable_by_key` partitions so the `window` oldest
        // candidates are up front (O(k) each), and the window doubles
        // between probes. Only that window is then sorted and walked.
        // With distinct stamps the result matches a stable full sort
        // exactly; equal stamps (messages rendered in the same frame share
        // the render-frame stamp) are broken arbitrarily by the partition,
        // which is fine for an LRU — the guard, the outside-guard-only
        // eviction and the crossing semantics are preserved either way.
        // The window degrades to the full list only when nearly every
        // candidate is evicted (covered == k).
        let mut window = 1usize;
        let covered = loop {
            if window >= candidates.len() {
                break candidates.len();
            }
            candidates.select_nth_unstable_by_key(window, |&i| self.msg_cache_last_used[i]);
            let sum: usize = candidates[..window]
                .iter()
                .map(|&i| {
                    Self::cache_entry_bytes_of(
                        &self.msg_cache_cells[i],
                        &self.msg_cache_text_regions[i],
                    )
                })
                .sum();
            if sum >= overshoot {
                break window;
            }
            window *= 2;
        };
        candidates[..covered].sort_by_key(|&i| self.msg_cache_last_used[i]);
        for &idx in &candidates[..covered] {
            if self.msg_cache_bytes <= RENDER_CACHE_LOW_WATER {
                break;
            }
            self.msg_cache_bytes = self
                .msg_cache_bytes
                .saturating_sub(Self::cache_entry_bytes_of(
                    &self.msg_cache_cells[idx],
                    &self.msg_cache_text_regions[idx],
                ));
            self.msg_cache_tokens[idx] = !0;
            self.msg_cache_w[idx] = 0;
            self.msg_cache_h[idx] = 0;
            self.msg_cache_cells[idx] = None;
            self.msg_cache_text_regions[idx] = None;
        }
    }
}

impl SessionView {
    /// Maintain the persistent render cache for the streaming last message.
    ///
    /// While tokens append to the last `Text` part, this renders the message's
    /// stable non-text parts once and patches only the streaming text part's
    /// tail each frame (O(delta)), so the walk can blit the whole message from
    /// `self.streaming_msg.cells` instead of re-parsing and re-rendering the
    /// growing message every frame. Returns the per-part heights for the
    /// height cache, or `None` when nothing is streaming (the caller falls
    /// back to the regular per-message cache path).
    pub(super) fn update_streaming_message_cache(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        streaming: bool,
    ) -> Option<streaming::StreamState> {
        if !streaming {
            self.streaming_msg = None;
            // Turn ended. The height-cache pin (`last_streamed_msg`) is
            // deliberately KEPT here: the scan in `ensure_height_caches_fresh`
            // still needs it this frame to land the streamed message's final
            // height (its token changed since the last pass), and it retires
            // there once the estimate is up to date.
            return None;
        }
        let Some(msg) = session.messages.last() else {
            self.streaming_msg = None;
            return None;
        };
        // Only a non-error assistant message whose LAST part is a growing Text
        // part can be rendered incrementally. Anything else (a running tool
        // box, an error message, ...) falls back to the regular full-render
        // path.
        if msg.role != MessageRole::Assistant
            || msg.id.starts_with("msg-err-")
            || !matches!(msg.parts.last(), Some(Part::Text(t)) if !t.synthetic)
        {
            self.streaming_msg = None;
            return None;
        }
        let config_tok = config_token(config);
        let width = inner_area.width;
        let text_pi = msg.parts.len() - 1;
        // Track which message the streaming cache is rendering: while deltas
        // keep arriving to it AFTER a queued message was injected below it
        // (it is no longer `last`), the height cache must keep re-estimating
        // it or its frozen estimate clips the painted content.
        self.last_streamed_msg = Some(msg.id.clone());

        let identity_ok = self.streaming_msg.as_ref().is_some_and(|sc| {
            sc.message_id == msg.id
                && sc.width == width
                && sc.max_w == max_w
                && sc.config_token == config_tok
                && sc.tool_state_version == self.tool_state.version
                && sc.part_hs.len() == msg.parts.len()
                && sc.text.as_ref().is_some_and(|t| t.part_index == text_pi)
        });

        let Part::Text(text_part) = &msg.parts[text_pi] else {
            unreachable!();
        };

        if !identity_ok {
            // ── Fresh cache: render the whole message once ──
            let mut part_hs: Vec<u16> = msg
                .parts
                .iter()
                .enumerate()
                .map(|(pi, p)| {
                    if pi == text_pi {
                        1 // placeholder; patched below from the text cache
                    } else {
                        Self::estimate_part_height(p, max_w, config, &msg.role, &self.tool_state)
                    }
                })
                .collect();
            let mut text_cache = streaming::StreamingTextCache::new(
                msg.id.clone(),
                text_pi,
                max_w,
                config_tok,
                theme,
            );
            let (_, text_h) = text_cache.update(&text_part.text, max_w, config_tok, config.conceal);
            part_hs[text_pi] = text_h;
            let msg_h: u16 = part_hs.iter().sum::<u16>().max(1);
            let full_area = Rect::new(0, 0, width, msg_h);
            let temp = self.scratch.get_or_insert_with(|| Buffer::empty(full_area));
            if *temp.area() != full_area {
                temp.resize(full_area);
            }
            temp.reset();
            temp.set_style(full_area, Style::default().bg(rgba_color(theme.background)));
            Self::render_assistant_message(
                temp,
                full_area,
                msg,
                theme,
                &mut self.tool_state,
                config,
                false,
                false,
                &part_hs,
                true,
            );
            let part_offset: u16 = part_hs[..text_pi].iter().sum();
            let mut cells = Vec::with_capacity(width as usize * msg_h as usize);
            let temp_cells = temp.content();
            let stride = width as usize;
            for dy in 0..msg_h as usize {
                cells.extend_from_slice(&temp_cells[dy * stride..(dy + 1) * stride]);
            }
            self.streaming_msg = Some(streaming::StreamingMessageCache {
                message_id: msg.id.clone(),
                width,
                max_w,
                config_token: config_tok,
                tool_state_version: self.tool_state.version,
                part_hs,
                cells,
                height: msg_h,
                text: Some(text_cache),
                last_tail: Some((part_offset, part_offset + text_h)),
            });
        } else {
            // ── Incremental: patch the streaming text part's tail ──
            let sc = self.streaming_msg.as_mut().unwrap();
            let text_cache = sc.text.as_mut().unwrap();
            let (render_row, text_h) =
                text_cache.update(&text_part.text, max_w, config_tok, config.conceal);
            sc.part_hs[text_pi] = text_h;
            let msg_h: u16 = sc.part_hs.iter().sum::<u16>().max(1);
            let old_h = sc.height;
            let x_off = 3u16; // text column within the message cells (area-relative)
            let part_offset: u16 = sc.part_hs[..text_pi].iter().sum();
            let row_end = part_offset + text_h;

            // Grow the cell buffer, preserving content; fill the new rows with
            // the background and carry the border glyph down. Row-slice fills
            // replace the per-cell `set_bg`/`set_char` calls (each clone is an
            // inline `CompactString`, so the fill stays allocation-free).
            let stride = width as usize;
            let new_cap = stride * msg_h as usize;
            if new_cap > sc.cells.len() {
                let border = sc.cells.first().cloned().unwrap_or_default();
                let mut bg_cell = Cell::default();
                bg_cell.set_bg(rgba_color(theme.background));
                bg_cell.set_char(' ');
                sc.cells.resize(new_cap, Cell::default());
                for dy in old_h..msg_h {
                    let base = dy as usize * stride;
                    sc.cells[base..base + stride].fill(bg_cell.clone());
                    sc.cells[base] = border.clone();
                }
            }
            // Patch the re-rendered text rows from the text cache. The text
            // cache's buffer rows are indexed absolutely (the re-render was
            // written into `self.cells` at `area.y = render_row`), so text row
            // `dy` lives at `dy * tc_w` in the cache's content.
            let tc_cells = text_cache.content();
            let tc_w = max_w as usize;
            let mut bg_cell = Cell::default();
            bg_cell.set_bg(rgba_color(theme.background));
            bg_cell.set_char(' ');
            for dy in render_row..text_h {
                let dst_row = part_offset + dy;
                if dst_row >= msg_h {
                    break;
                }
                let dst_base = dst_row as usize * stride;
                let src_base = dy as usize * tc_w;
                sc.cells[dst_base + 1..dst_base + stride].fill(bg_cell.clone());
                sc.cells[dst_base + x_off as usize..dst_base + x_off as usize + tc_w]
                    .clone_from_slice(&tc_cells[src_base..src_base + tc_w]);
            }
            sc.height = msg_h;
            sc.last_tail = Some((part_offset + render_row, row_end));
        }

        let sc = self.streaming_msg.as_ref().unwrap();
        Some(streaming::StreamState {
            message_id: sc.message_id.clone(),
            part_hs: sc.part_hs.clone(),
            height: sc.height,
            raw_len: sc.text.as_ref().map_or(0, |t| t.raw_len()),
        })
    }
}
