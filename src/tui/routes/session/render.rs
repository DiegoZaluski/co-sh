use super::*;

impl SessionView {
    #[allow(
        clippy::cast_sign_loss,
        clippy::too_many_lines,
        clippy::too_many_arguments
    )]
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        config: &TuiConfig,
        delta_time: f64,
    ) -> i32 {
        let Some(session) = state.current_session() else {
            return 0;
        };

        self.session_area = Some((area.x, area.y, area.right(), area.bottom()));

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );

        let unique_agents = state.unique_agents();
        let agent_colors = AgentColors::from_theme(theme);
        let max_w = inner_area.width.saturating_sub(6).max(2);
        let streaming = state.status == SessionStatus::Working;

        let _frame_start = Instant::now();

        let config_tok = config_token(config);
        let stream_state = self
            .update_streaming_message_cache(session, inner_area, max_w, config, theme, streaming);
        let height_update = self.ensure_height_caches_fresh(
            session,
            max_w,
            config,
            // Render mode: the walk may rebuild/extend/re-estimate freely;
            // the streaming snapshot is read from `self.streaming_msg`
            // inside (already refreshed by the call above).
            HeightCacheMode::Render,
            streaming,
        );

        // Ensure render cache vectors match message count
        let n_msgs = session.messages.len();
        if height_update.full_rebuild {
            self.msg_cache_tokens.clear();
            self.msg_cache_cells.clear();
            self.msg_cache_w.clear();
            self.msg_cache_h.clear();
            self.msg_cache_text_regions.clear();
            self.msg_cache_last_used.clear();
            self.msg_cache_bytes = 0;
        }
        // Only resize when message count actually changed (avoids O(n) fill per frame).
        if n_msgs != self.msg_cache_tokens.len() {
            self.msg_cache_tokens.resize(n_msgs, !0);
            self.msg_cache_cells.resize(n_msgs, None);
            self.msg_cache_w.resize(n_msgs, 0);
            self.msg_cache_h.resize(n_msgs, 0);
            self.msg_cache_text_regions.resize(n_msgs, None);
            self.msg_cache_last_used.resize(n_msgs, 0);
        }

        self.render_frame = self.render_frame.wrapping_add(1);

        let total_height = self.cached_total_height;
        let visible_height = i32::from(inner_area.height);
        let max_scroll = (self.actual_total_height - visible_height).max(0);
        self.scroll_y = self.scroll_y.clamp(0, max_scroll);

        // Cache dimensions for app.rs
        self.total_height = total_height;
        self.visible_height = visible_height;

        // ── Sticky scroll: recalculateBarProps on content size change ─────────
        // Mirrors OpenCode's `recalculateBarProps()` which calls `applyStickyStart`
        // when content size changes and user hasn't manually scrolled.
        //
        // Expand/collapse is a content size change like any other — it must
        // NOT be special-cased (the old branch called `sync_manual_scroll_state`
        // instead, which poisoned `has_manual_scroll` on every toggle and killed
        // the auto-follow, leaving the view stranded mid-history: the
        // "collapsing and expanding jumps to the wrong location" bug). With
        // per-part expansion keys (see `Session::ensure_tool_call_ids`), a
        // toggle changes ONLY the clicked box's height, so:
        //   - sticky-bottom users stay glued to the bottom through BOTH
        //     collapse and expand, and a collapse→expand cycle restores the
        //     exact pre-toggle scroll position;
        //   - manually-scrolled users are untouched (`recalculate_bar_props`
        //     only re-aims when sticky), and since content ABOVE the clicked
        //     box is unmoved, their raw `scroll_y` keeps the viewport anchored.
        if total_height != self.last_content_height {
            self.recalculate_bar_props(total_height, visible_height);
            self.last_content_height = total_height;
        }

        let stream_token = stream_state.as_ref().map_or_else(
            // Not streaming: use the render cache's stored content token
            // for the last message. `msg_cache_tokens[last]` is refreshed
            // by the walk whenever a non-streaming message re-renders
            // (content change or eviction), so an in-place edit of the last
            // message still rebuilds its selection regions — one frame
            // later, without the O(n) per-frame hash this generation used
            // to pay. Streaming ignores it (the streaming cache keeps the
            // streaming message's regions fresh incrementally).
            || self.msg_cache_tokens.last().copied().unwrap_or(0),
            |s| s.raw_len as u64,
        );
        let regions_gen = text_regions_generation(
            session,
            config,
            max_w,
            self.tool_state.version,
            stream_token,
        );
        if regions_gen != self.text_regions_gen {
            let _regions_start = Instant::now();
            self.build_text_regions(session, inner_area, max_w, config, theme);
            self.text_regions_gen = regions_gen;
            log::debug!(
                "[PERF] text_regions: {}us (built)",
                _regions_start.elapsed().as_micros()
            );
        } else if self.render_frame.is_multiple_of(30) {
            // Rate-limited: this fires on EVERY idle frame otherwise.
            log::debug!("[PERF] text_regions: skipped (no change)");
        }

        if config.show_scrollbar {
            let scrollbar_area = Rect::new(
                inner_area.right().saturating_sub(1),
                inner_area.y,
                1,
                inner_area.height,
            );
            let mut scrollbar = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
            scrollbar.set_scroll_size(f64::from(self.actual_total_height.max(1)));
            scrollbar.set_viewport_size(f64::from(visible_height));
            scrollbar.set_scroll_position(f64::from(self.scroll_y));
            scrollbar.set_track_color(Some(theme.background.into()));
            scrollbar.set_thumb_color(Some(theme.text_muted.into()));
            scrollbar.render_self(buf, scrollbar_area);
        }

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());

        // Start the message walk at the FIRST message that can intersect the
        // viewport (binary search over `prefix_y`, O(log n)) instead of
        // walking the whole timeline from message 0: on sessions with 500+
        // messages the old per-frame O(n) walk — thousands of tight
        // iterations per frame just to skip off-screen messages — was the
        // dominant frame cost even with the render cache hot. The walk below
        // then STOPS once `msg_top` passes the viewport bottom, so the frame
        // cost is O(log n + visible) instead of O(n). Computed BEFORE the
        // `scratch` field borrow below (`find_first_visible` needs `&self`).
        let first_vis = self.find_first_visible(self.scroll_y);
        let walk_start = if self.msg_height_cache.is_empty() {
            i32::from(inner_area.y) - self.scroll_y
        } else {
            // `prefix_y[first_vis]` already accounts for the 1-row gaps
            // before it; the walk's own `idx > 0` gap logic resumes exactly
            // like the full walk would.
            i32::from(inner_area.y) - self.scroll_y + self.prefix_y[first_vis]
        };

        // When the walk stops past the viewport bottom, the remaining
        // (invisible) messages still contribute their cached heights to the
        // total; `prefix_y[n] - prefix_y[idx]` is exactly that remainder.
        let n_msgs = session.messages.len();
        let mut broke_at: Option<usize> = None;

        // Reuse one scratch buffer across all temp renders in this frame (and
        // across frames — `Buffer::resize` keeps the allocation). Removes the
        // per-frame large allocations that accumulated heap churn and
        // fragmented the allocator over long sessions. A direct field borrow
        // keeps `scratch` disjoint from the `tool_state`/cache writes below.
        let scratch = &mut self.scratch;

        let mut y = walk_start;

        for (idx, msg) in session.messages.iter().enumerate().skip(first_vis) {
            // `prefix_y[first_vis]` already includes the 1-row gaps before the
            // first visited message, so the walk adds gaps only AFTER it.
            if idx > first_vis {
                y += 1;
            }

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            if msg_top >= vp_bottom {
                // Nothing below can intersect the viewport — stop the walk.
                broke_at = Some(idx);
                break;
            }
            let msg_bottom = y + msg_h;
            let is_assistant_non_error =
                matches!(msg.role, MessageRole::Assistant) && !msg.id.starts_with("msg-err-");
            let mut render_actual_h = msg_h;

            // Check if message overlaps with viewport (using i32, no u16 wrap)
            if stream_state.is_some() && idx == n_msgs - 1 {
                // ── Streaming last message: blit the incremental cache ──
                // `update_streaming_message_cache` refreshed the cells this
                // frame (before the height cache ran), so the blit is O(visible
                // rows) with no re-parse and no full re-render. Text regions
                // for this message are built from the same cache in
                // `build_text_regions`.
                self.msg_cache_last_used[idx] = self.render_frame;
                if let Some(sc) = self.streaming_msg.as_ref() {
                    let cached_w = sc.width as usize;
                    let cached_h = sc.height;
                    let src_y = (vp_top - msg_top).max(0) as u16;
                    let dst_y = msg_top.max(vp_top) as u16;
                    let vis_h =
                        ((cached_h as i32).min(vp_bottom - msg_top) - src_y as i32).max(0) as u16;
                    let dst_x = inner_area.x;
                    blit_cell_rows(
                        buf,
                        dst_x,
                        dst_y,
                        &sc.cells,
                        cached_w,
                        usize::from(src_y),
                        usize::from(vis_h),
                        cached_w,
                    );
                    render_actual_h = cached_h as i32;
                }
            } else if msg_bottom > vp_top && msg_top < vp_bottom {
                // Recency stamp for the render-cache LRU: any message the user
                // can currently see counts as recently used.
                self.msg_cache_last_used[idx] = self.render_frame;
                let is_top_clipped = msg_top < vp_top;

                if is_top_clipped && is_assistant_non_error {
                    // ── Top-clipped assistant (non-error) ──
                    let is_streaming_msg = idx == session.messages.len() - 1 && streaming;
                    // Bypass the cell cache only when THIS message contains an
                    // active/finishing spinner so its beam advances every frame.
                    // A global "any spinner in the session" check forced every
                    // top-clipped message to re-render fully on every frame
                    // while any tool animated — the main per-frame cost during
                    // agent bursts. The key is assembled into a reused buffer
                    // instead of a per-part `format!` allocation per frame.
                    let spinner_key = &mut self.spinner_key_scratch;
                    let tool_spinners = &self.tool_state.tool_spinners;
                    let has_active_spinner = msg.parts.iter().enumerate().any(|(pi, part)| {
                        matches!(part, Part::Tool(t) if {
                            tool_render::renderer_spinner_key_into(
                                spinner_key,
                                tool_render::tool_display(&t.tool),
                                t.tool_call_id.as_deref(),
                                pi as u16,
                            ) && tool_spinners
                                .get(spinner_key.as_str())
                                .is_some_and(|s| !s.is_idle())
                        })
                    });
                    let token = msg_content_token(msg, config_tok, max_w, &self.tool_state);
                    let cache_hit = !is_streaming_msg
                        && !has_active_spinner
                        && !msg_has_running_compaction(msg)
                        && !msg_has_running_install(msg)
                        && token == self.msg_cache_tokens[idx]
                        && self.msg_cache_w[idx] == inner_area.width
                        && self.msg_cache_h[idx] > 0
                        && self.msg_cache_cells[idx].is_some();

                    if cache_hit {
                        // ── Use cached cells ──
                        let cached_cells = self.msg_cache_cells[idx].as_ref().unwrap();
                        let cached_h = self.msg_cache_h[idx];
                        let cached_w = self.msg_cache_w[idx] as usize;
                        let src_y = (vp_top - msg_top) as u16;
                        let dst_y = vp_top as u16;
                        let vis_h = ((cached_h as i32).min(vp_bottom - msg_top) - src_y as i32)
                            .max(0) as u16;
                        blit_cell_rows(
                            buf,
                            inner_area.x,
                            dst_y,
                            cached_cells,
                            cached_w,
                            usize::from(src_y),
                            usize::from(vis_h),
                            cached_w,
                        );
                        render_actual_h = cached_h as i32;
                    } else {
                        // ── Fall back: temp buffer render (scratch reused across frames) ──
                        let src_y = (vp_top - msg_top) as u16;
                        let dst_y = vp_top as u16;
                        let generous_h =
                            ((msg_h as u16).saturating_add(inner_area.height)).clamp(100, 5000);
                        let full_area = Rect::new(0, 0, inner_area.width, generous_h);
                        let temp = scratch.get_or_insert_with(|| Buffer::empty(full_area));
                        if *temp.area() != full_area {
                            temp.resize(full_area);
                        }
                        // `Buffer::resize` keeps existing cell content, so clear it
                        // before rendering: un-written cells would otherwise show
                        // glyphs from the previous message rendered into this buffer.
                        temp.reset();
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

                        let mut actual_h = Self::render_assistant_message(
                            temp,
                            full_area,
                            msg,
                            theme,
                            &mut self.tool_state,
                            config,
                            false,
                            false,
                            &self.part_heights_cache[idx],
                            streaming,
                        ) as i32;
                        actual_h = actual_h.min(generous_h as i32);

                        let msg_content_top = msg_top - vp_top + self.scroll_y;
                        if actual_h > 0 {
                            let x_off_text = inner_area.x + 3;
                            let temp_cells = temp.content();
                            let total_stride = inner_area.width as usize;
                            let ah = actual_h as u16;
                            let regions = regions_from_full_width_cells(
                                temp_cells,
                                total_stride,
                                3,
                                x_off_text,
                                x_off_text + max_w,
                                (0..ah as usize).zip(msg_content_top..),
                            );
                            self.msg_cache_text_regions[idx] = Some(regions);
                        }

                        let vis_h =
                            (actual_h.min(vp_bottom - msg_top) - src_y as i32).max(0) as u16;
                        let temp_cells = temp.content();
                        let total_stride = inner_area.width as usize;
                        let dst_x = inner_area.x;
                        blit_cell_rows(
                            buf,
                            dst_x,
                            dst_y,
                            temp_cells,
                            total_stride,
                            usize::from(src_y),
                            usize::from(vis_h),
                            total_stride,
                        );

                        // Save non-streaming messages to cache
                        if !is_streaming_msg && actual_h > 0 {
                            let ah = actual_h as u16;
                            let temp_cells = temp.content();
                            let total_stride = inner_area.width as usize;
                            // Store full-width cells (including border/margin) for consistent
                            // alignment regardless of scroll state.
                            let mut cells = Vec::with_capacity(total_stride * ah as usize);
                            for dy in 0..ah {
                                let base = dy as usize * total_stride;
                                cells.extend_from_slice(&temp_cells[base..base + total_stride]);
                            }
                            // Text regions are still content-only (no border/margin text).
                            let x_off_text = inner_area.x + 3;
                            let msg_content_top = msg_top - vp_top + self.scroll_y;
                            let regions = regions_from_full_width_cells(
                                temp_cells,
                                total_stride,
                                3,
                                x_off_text,
                                x_off_text + max_w,
                                (0..ah as usize).zip(msg_content_top..),
                            );
                            // Re-account the entry's byte cost (old subtracted,
                            // new added) and stamp it most-recently-used. The
                            // writes are per-field so the borrow of the reusable
                            // `scratch` buffer stays disjoint.
                            self.msg_cache_bytes =
                                self.msg_cache_bytes
                                    .saturating_sub(Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ));
                            self.msg_cache_tokens[idx] = token;
                            self.msg_cache_w[idx] = inner_area.width;
                            self.msg_cache_h[idx] = ah;
                            self.msg_cache_cells[idx] = Some(cells);
                            self.msg_cache_text_regions[idx] = Some(regions);
                            self.msg_cache_bytes =
                                self.msg_cache_bytes
                                    .saturating_add(Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ));
                            self.msg_cache_last_used[idx] = self.render_frame;
                        }

                        render_actual_h = actual_h.max(1);
                    }
                } else if is_top_clipped {
                    // ── Top-clipped user/error: fixed-size temp buffer ──
                    let src_y = (vp_top - msg_top) as u16;
                    let dst_y = vp_top as u16;
                    let vis_h = (msg_bottom.min(vp_bottom) - vp_top) as u16;

                    if vis_h > 0 && msg_h > 0 {
                        let full_area = Rect::new(0, 0, inner_area.width, msg_h as u16);
                        let temp = scratch.get_or_insert_with(|| Buffer::empty(full_area));
                        if *temp.area() != full_area {
                            temp.resize(full_area);
                        }
                        // `Buffer::resize` keeps existing cell content, so clear it
                        // before rendering: un-written cells would otherwise show
                        // glyphs from the previous message rendered into this buffer.
                        temp.reset();
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

                        match msg.role {
                            MessageRole::User => {
                                let agent_name = msg.agent.as_deref().unwrap_or("default");
                                let agent_color = agent_colors.get(agent_name, &unique_agents);
                                Self::render_user_message(
                                    temp,
                                    full_area,
                                    msg,
                                    theme,
                                    agent_color,
                                    &mut self.tool_state,
                                    config,
                                    false,
                                    false,
                                    Some(&self.part_heights_cache[idx]),
                                    streaming,
                                );
                            }
                            MessageRole::Assistant => {
                                Self::render_assistant_message(
                                    temp,
                                    full_area,
                                    msg,
                                    theme,
                                    &mut self.tool_state,
                                    config,
                                    false,
                                    false,
                                    &self.part_heights_cache[idx],
                                    streaming,
                                );
                            }
                        }

                        let temp_cells = temp.content();
                        let line_stride = full_area.width as usize;
                        let dst_x = inner_area.x;
                        blit_cell_rows(
                            buf,
                            dst_x,
                            dst_y,
                            temp_cells,
                            line_stride,
                            usize::from(src_y),
                            usize::from(vis_h),
                            line_stride,
                        );
                    }
                } else if is_assistant_non_error && msg_bottom <= vp_bottom {
                    // ── Fully visible assistant: render directly, use actual height ──
                    let visible_top = msg_top as u16;
                    let visible_bottom = msg_bottom.min(vp_bottom);
                    let visible_h = (visible_bottom - i32::from(visible_top)) as u16;

                    if visible_h > 0 {
                        let is_streaming_msg = idx == session.messages.len() - 1 && streaming;
                        let token = msg_content_token(msg, config_tok, max_w, &self.tool_state);
                        let cache_hit = !is_streaming_msg
                            && !msg_has_running_compaction(msg)
                            && !msg_has_running_install(msg)
                            && token == self.msg_cache_tokens[idx]
                            && self.msg_cache_w[idx] == inner_area.width
                            && self.msg_cache_h[idx] > 0
                            && self.msg_cache_cells[idx].is_some();

                        if cache_hit {
                            if let Some(ref cached_cells) = self.msg_cache_cells[idx] {
                                let w = self.msg_cache_w[idx] as usize;
                                let h = self.msg_cache_h[idx];
                                blit_cell_rows(
                                    buf,
                                    inner_area.x,
                                    visible_top,
                                    cached_cells,
                                    w,
                                    0,
                                    visible_h.min(h) as usize,
                                    w,
                                );
                                render_actual_h = h as i32;

                                // Ensure text regions are cached for this message.
                                // `msg_cache_cells` stores FULL-width rows (border/margin
                                // included) for alignment, but region text must be
                                // content-only: it is labeled `x1 = inner_area.x + 3`, so
                                // sampling from column 0 would inject 3 ghost columns at
                                // the head of every line — the copy then leads with 3
                                // spaces and loses the last 3 painted characters. Strip
                                // the 3 leading columns, mirroring the temp-buffer sites
                                // (`temp_cells[base + 3 + dx]`).
                                if self.msg_cache_text_regions[idx].is_none() {
                                    let x_off_text = inner_area.x + 3;
                                    let msg_content_top = msg_top - vp_top + self.scroll_y;
                                    let regions = regions_from_full_width_cells(
                                        cached_cells,
                                        w,
                                        3,
                                        x_off_text,
                                        x_off_text + max_w,
                                        (0..h as usize).zip(msg_content_top..),
                                    );
                                    self.msg_cache_text_regions[idx] = Some(regions);
                                }
                            }
                        } else {
                            let msg_area =
                                Rect::new(inner_area.x, visible_top, inner_area.width, visible_h);

                            let actual_h = Self::render_assistant_message(
                                buf,
                                msg_area,
                                msg,
                                theme,
                                &mut self.tool_state,
                                config,
                                false,
                                false,
                                &self.part_heights_cache[idx],
                                streaming,
                            ) as i32;

                            render_actual_h = actual_h.max(1);

                            // Save non-streaming messages to cache (cells + text regions)
                            if !is_streaming_msg {
                                let full_ah = render_actual_h as u16;
                                let w = inner_area.width as usize;
                                let buf_w = buf.area.width as usize;
                                let buf_h = buf.area.height as usize;
                                let buf_x = inner_area.x as usize;
                                // Rows must fit inside the buffer: a render clipped
                                // at the viewport bottom can leave
                                // `visible_top + dy` past the last buffer row, and
                                // `buf.content[base..base + w]` would panic. Clamp
                                // to the rows that actually fit — mirroring the
                                // guards in `blit_cell_rows` — instead of indexing
                                // past the end.
                                let fit_rows = if buf_x.saturating_add(w) <= buf_w {
                                    buf_h.saturating_sub(usize::from(visible_top))
                                } else {
                                    0
                                };
                                let ah = full_ah.min(fit_rows as u16);
                                // Cache only complete renders: a clamp here means
                                // the render was clipped at the buffer bottom, so
                                // fewer rows exist in `buf` than the logical height.
                                // Caching the clamped count would make cache-hit
                                // frames advance the layout by less than this fill
                                // frame did (render_actual_h divergence → scroll
                                // jitter), so skip caching instead; `msg_cache_h`
                                // stays stale, and the token check in `cache_hit`
                                // makes the next frame re-render.
                                if ah == full_ah {
                                    // Row-slice read: the saved rows are fully
                                    // inside the buffer's area (they were just
                                    // painted there), so one bounds-checked slice
                                    // per row replaces a per-cell `buf.cell(..)`.
                                    let mut cells = Vec::with_capacity(w * ah as usize);
                                    for dy in 0..ah {
                                        let base = (visible_top + dy) as usize * buf_w + buf_x;
                                        cells.extend_from_slice(&buf.content[base..base + w]);
                                    }
                                    // Region text is content-only (x1 = inner_area.x + 3):
                                    // the border/margin columns are stripped by
                                    // regions_from_full_width_cells, so they never enter
                                    // the text as ghost leading columns (they would
                                    // shift the copy 3 chars left of the painted
                                    // selection and drop the last 3 chars).
                                    let x_off_text = inner_area.x + 3;
                                    let msg_content_top = msg_top - vp_top + self.scroll_y;
                                    let regions = regions_from_full_width_cells(
                                        &cells,
                                        w,
                                        3,
                                        x_off_text,
                                        x_off_text + max_w,
                                        (0..ah as usize).zip(msg_content_top..),
                                    );
                                    self.msg_cache_bytes = self.msg_cache_bytes.saturating_sub(
                                        Self::cache_entry_bytes_of(
                                            &self.msg_cache_cells[idx],
                                            &self.msg_cache_text_regions[idx],
                                        ),
                                    );
                                    self.msg_cache_tokens[idx] = token;
                                    self.msg_cache_w[idx] = inner_area.width;
                                    self.msg_cache_h[idx] = ah;
                                    self.msg_cache_cells[idx] = Some(cells);
                                    self.msg_cache_text_regions[idx] = Some(regions);
                                    self.msg_cache_bytes = self.msg_cache_bytes.saturating_add(
                                        Self::cache_entry_bytes_of(
                                            &self.msg_cache_cells[idx],
                                            &self.msg_cache_text_regions[idx],
                                        ),
                                    );
                                    self.msg_cache_last_used[idx] = self.render_frame;
                                }
                            }
                        }
                    }
                } else {
                    // ── Not top-clipped user/error or bottom-clipped: render directly ──
                    let visible_top = msg_top as u16;
                    let visible_bottom = msg_bottom.min(vp_bottom);
                    let visible_h = (visible_bottom - i32::from(visible_top)) as u16;

                    if visible_h > 0 {
                        let msg_area =
                            Rect::new(inner_area.x, visible_top, inner_area.width, visible_h);

                        match msg.role {
                            MessageRole::User => {
                                let agent_name = msg.agent.as_deref().unwrap_or("default");
                                let agent_color = agent_colors.get(agent_name, &unique_agents);
                                Self::render_user_message(
                                    buf,
                                    msg_area,
                                    msg,
                                    theme,
                                    agent_color,
                                    &mut self.tool_state,
                                    config,
                                    false,
                                    false,
                                    Some(&self.part_heights_cache[idx]),
                                    streaming,
                                );
                            }
                            MessageRole::Assistant => {
                                let is_streaming_msg =
                                    idx == session.messages.len() - 1 && streaming;
                                let token =
                                    msg_content_token(msg, config_tok, max_w, &self.tool_state);
                                let cache_hit = !is_streaming_msg
                                    && !msg_has_running_compaction(msg)
                                    && !msg_has_running_install(msg)
                                    && token == self.msg_cache_tokens[idx]
                                    && self.msg_cache_w[idx] == inner_area.width
                                    && self.msg_cache_h[idx] > 0
                                    && self.msg_cache_cells[idx].is_some();

                                if cache_hit {
                                    let cached_cells = self.msg_cache_cells[idx].as_ref().unwrap();
                                    let cached_h = self.msg_cache_h[idx];
                                    let cached_w = self.msg_cache_w[idx] as usize;
                                    for dy in 0..visible_h.min(cached_h) {
                                        let base = dy as usize * cached_w;
                                        for dx in 0..cached_w {
                                            if let Some(cell) = buf.cell_mut((
                                                inner_area.x + dx as u16,
                                                visible_top + dy,
                                            )) {
                                                *cell = cached_cells[base + dx].clone();
                                            }
                                        }
                                    }
                                    render_actual_h = cached_h as i32;
                                } else if msg.id.starts_with("msg-err-") {
                                    // Error messages: render directly to buf.
                                    // render_assistant_message returns 0 for errors
                                    // (content is rendered as a side-effect), so the
                                    // temp buffer approach would lose the output.
                                    Self::render_assistant_message(
                                        buf,
                                        msg_area,
                                        msg,
                                        theme,
                                        &mut self.tool_state,
                                        config,
                                        false,
                                        false,
                                        &self.part_heights_cache[idx],
                                        streaming,
                                    );
                                    render_actual_h = msg_h;
                                } else {
                                    // Render full message to temp buffer, then copy
                                    // visible portion to buf. This ensures the full
                                    // message height is cached, not just the clipped
                                    // viewport portion — preventing partial cut-off
                                    // on subsequent frames (e.g. after theme change).
                                    let generous_h = ((msg_h as u16)
                                        .saturating_add(inner_area.height))
                                    .clamp(100, 5000);
                                    let full_area = Rect::new(0, 0, inner_area.width, generous_h);
                                    let temp =
                                        scratch.get_or_insert_with(|| Buffer::empty(full_area));
                                    if *temp.area() != full_area {
                                        temp.resize(full_area);
                                    }
                                    // `Buffer::resize` keeps existing cell content, so clear it
                                    // before rendering: un-written cells would otherwise show
                                    // glyphs from the previous message rendered into this buffer.
                                    temp.reset();
                                    temp.set_style(
                                        full_area,
                                        Style::default().bg(rgba_color(theme.background)),
                                    );

                                    let mut actual_h = Self::render_assistant_message(
                                        temp,
                                        full_area,
                                        msg,
                                        theme,
                                        &mut self.tool_state,
                                        config,
                                        false,
                                        false,
                                        &self.part_heights_cache[idx],
                                        streaming,
                                    ) as i32;
                                    actual_h = actual_h.min(generous_h as i32);

                                    let msg_content_top = msg_top - vp_top + self.scroll_y;
                                    if actual_h > 0 {
                                        let x_off_text = inner_area.x + 3;
                                        let temp_cells = temp.content();
                                        let total_stride = inner_area.width as usize;
                                        let ah = actual_h as u16;
                                        let regions = regions_from_full_width_cells(
                                            temp_cells,
                                            total_stride,
                                            3,
                                            x_off_text,
                                            x_off_text + max_w,
                                            (0..ah as usize).zip(msg_content_top..),
                                        );
                                        self.msg_cache_text_regions[idx] = Some(regions);
                                    }

                                    // Copy visible portion from temp to buf
                                    let dst_y = visible_top;
                                    let vis_h = actual_h.min(i32::from(visible_h)) as u16;
                                    let temp_cells = temp.content();
                                    let total_stride = inner_area.width as usize;
                                    let dst_x = inner_area.x;
                                    blit_cell_rows(
                                        buf,
                                        dst_x,
                                        dst_y,
                                        temp_cells,
                                        total_stride,
                                        0,
                                        usize::from(vis_h),
                                        total_stride,
                                    );

                                    render_actual_h = actual_h.max(1);

                                    // Save non-streaming messages to cache (full height)
                                    if !is_streaming_msg && actual_h > 0 {
                                        let ah = render_actual_h as u16;
                                        let w = inner_area.width as usize;
                                        let mut cells = Vec::with_capacity(w * ah as usize);
                                        for dy in 0..ah {
                                            let base = dy as usize * total_stride;
                                            cells.extend_from_slice(&temp_cells[base..base + w]);
                                        }
                                        // Region text is content-only (x1 = inner_area.x
                                        // + 3): the border/margin columns are stripped
                                        // by regions_from_full_width_cells, so they
                                        // never enter the text as ghost leading
                                        // columns (they would shift the copy 3 chars
                                        // left of the painted selection and drop the
                                        // last 3).
                                        let x_off_text = inner_area.x + 3;
                                        let regions = regions_from_full_width_cells(
                                            &cells,
                                            w,
                                            3,
                                            x_off_text,
                                            x_off_text + max_w,
                                            (0..ah as usize).zip(msg_content_top..),
                                        );
                                        self.msg_cache_bytes = self.msg_cache_bytes.saturating_sub(
                                            Self::cache_entry_bytes_of(
                                                &self.msg_cache_cells[idx],
                                                &self.msg_cache_text_regions[idx],
                                            ),
                                        );
                                        self.msg_cache_tokens[idx] = token;
                                        self.msg_cache_w[idx] = inner_area.width;
                                        self.msg_cache_h[idx] = ah;
                                        self.msg_cache_cells[idx] = Some(cells);
                                        self.msg_cache_text_regions[idx] = Some(regions);
                                        self.msg_cache_bytes = self.msg_cache_bytes.saturating_add(
                                            Self::cache_entry_bytes_of(
                                                &self.msg_cache_cells[idx],
                                                &self.msg_cache_text_regions[idx],
                                            ),
                                        );
                                        self.msg_cache_last_used[idx] = self.render_frame;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            y += render_actual_h;
        }

        // Record actual rendered total height (accurate, for scrollbar sizing).
        // Do NOT inflate cached_total_height from the render scan — the cache
        // is the stable source of truth for height estimates. Inflating it would
        // cause total_height != last_content_height on the next frame, triggering
        // recalculate_bar_props which changes scroll_y and creates visible
        // scroll jumps mid-stream.
        //
        // When the walk stopped past the viewport bottom, the invisible
        // messages below it never render (render_actual_h stays = msg_h), so
        // they contribute exactly their cached heights + gaps: the prefix-y
        // span from the stop index to the end. The messages before the stop
        // already contributed their real (possibly diverging) heights to `y`.
        let actual_total = match broke_at {
            Some(idx) => (y - (vp_top - self.scroll_y))
                .saturating_add(self.prefix_y[n_msgs] - self.prefix_y[idx]),
            None => y - (vp_top - self.scroll_y),
        };
        self.actual_total_height = actual_total;
        self.total_height = self.cached_total_height;
        self.last_content_height = self.cached_total_height;

        // ── Hover highlight for user messages (opencode-style) ────────────────
        self.render_hover_highlight(buf, inner_area, session, theme);

        if let Some((anchor_x, _anchor_screen_y, focus_x, _focus_screen_y)) = self.drag_selection
            && inner_area.height > 0
        {
            // Convert content-space anchor and focus to current screen position.
            // Both are stored in content space so the visual highlight follows
            // content during auto-scroll.
            //
            // A zero-height viewport (e.g. the session area briefly collapsing
            // to 0 rows during a terminal resize) has no visible rows to
            // highlight, and the clamp bound `vp_top + height - 1` would fall
            // below `vp_top` — panicking with `min > max`. Skip it entirely.
            let vp_top = i32::from(inner_area.y);
            let vp_bottom = vp_top + i32::from(inner_area.height) - 1;
            let anchor_screen_y = (self.selection_anchor_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_bottom) as u16;
            let focus_screen_y = (self.selection_focus_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_bottom) as u16;

            let start_y = anchor_screen_y.min(focus_screen_y);
            let end_y = anchor_screen_y.max(focus_screen_y);

            // Direction and row bands are matched against CONTENT rows, not
            // the clamped screen rows (mirrors `get_text_in_region`): an
            // endpoint whose row scrolled OUT of the viewport is clamped to
            // the edge for the iteration bounds, but that edge row is then a
            // MIDDLE row of the selection and must be highlighted full-width.
            // Matching clamped screen rows instead froze the initial
            // mouse-down x on the first/last visible line for the whole drag
            // (e.g. an upward drag past the top edge kept the partial band on
            // the last line).
            let anchor_content_y = self.selection_anchor_content_y;
            let focus_content_y = self.selection_focus_content_y;
            let (top_x, bottom_x) = if anchor_content_y <= focus_content_y {
                (anchor_x, focus_x)
            } else {
                (focus_x, anchor_x)
            };
            let top_content_y = anchor_content_y.min(focus_content_y);
            let bottom_content_y = anchor_content_y.max(focus_content_y);

            let content_min_x = inner_area.x + 3;
            let content_max_x = (inner_area.x + 3 + max_w).saturating_sub(1);

            for cy in start_y..=end_y {
                // Content row under this screen row. Rows outside the true
                // content span (selection fully clamped off-screen) are not
                // part of the selection — skip them.
                let row_content_y = i32::from(cy) - vp_top + self.scroll_y;
                if row_content_y < top_content_y || row_content_y > bottom_content_y {
                    continue;
                }
                let (lx1, lx2) =
                    if row_content_y == top_content_y && row_content_y == bottom_content_y {
                        (top_x.min(bottom_x), top_x.max(bottom_x))
                    } else if row_content_y == top_content_y {
                        (top_x, content_max_x)
                    } else if row_content_y == bottom_content_y {
                        (content_min_x, bottom_x)
                    } else {
                        (content_min_x, content_max_x)
                    };

                let lx1 = lx1.max(content_min_x);
                let lx2 = lx2.min(content_max_x);
                if lx1 > lx2 {
                    continue;
                }

                for cx in lx1..=lx2 {
                    if let Some(cell) = buf.cell_mut((cx, cy)) {
                        let fg = cell.fg;
                        let bg = cell.bg;
                        cell.set_fg(bg);
                        cell.set_bg(fg);
                    }
                }
            }
        }

        // ── Bounded process memory on long sessions ──────────────────────────
        // The render cache is the fattest in-memory copy (up to ~40 bytes per
        // rendered cell). Once it exceeds the generous budget, shed the oldest
        // entries outside the viewport guard; idle tool spinners (never
        // rendered) are dropped every frame so the map stays bounded by the
        // number of tools currently animating.
        if self.msg_cache_bytes > RENDER_CACHE_BUDGET {
            self.prune_render_cache(vp_top, vp_bottom);
        }
        self.tool_state.sweep_idle_spinners();

        self.handle_auto_scroll(delta_time, total_height, visible_height);

        let _frame_us = _frame_start.elapsed().as_micros();
        // Rate-limited: during sustained slow phases (e.g. streaming a huge
        // message) this would otherwise fire on every frame. Mildly-slow
        // frames (>5ms) are sampled at 1/30; genuinely bad frames (>50ms) are
        // always logged so an isolated freeze is never silently missed.
        if _frame_us > 5000 && (_frame_us > 50_000 || self.render_frame.is_multiple_of(30)) {
            log::debug!(
                "[PERF] session_render_total: {_frame_us}us msgs={} scroll_y={}",
                session.messages.len(),
                self.scroll_y
            );
        }

        total_height
    }
}
