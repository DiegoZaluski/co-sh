use super::*;

impl SessionView {
    #[allow(clippy::too_many_lines, clippy::cast_sign_loss)]
    pub fn build_text_regions(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
    ) {
        self.build_text_regions_impl(session, inner_area, max_w, config, theme, None);
    }
}

impl SessionView {
    /// Like [`Self::build_text_regions`], but builds regions for an arbitrary
    /// content-space row span instead of the visible viewport. Used on
    /// drag-release so a selection that crossed scroll boundaries (auto-scroll)
    /// copies every row between anchor and focus, not just the last window.
    pub fn build_text_regions_for_content_range(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        content_range: (i32, i32),
    ) {
        self.build_text_regions_impl(
            session,
            inner_area,
            max_w,
            config,
            theme,
            Some(content_range),
        );
    }
}

impl SessionView {
    pub(super) fn build_text_regions_impl(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        content_range: Option<(i32, i32)>,
    ) {
        // With a content range, run the same walk against a virtual viewport
        // of exactly that span: `scroll` shifts the walk to the range start
        // and vp_top=0 keeps every `screen - vp_top + scroll` expression
        // yielding absolute content coordinates.
        let (scroll, vp_top, vp_bottom) = if let Some((cs_start, cs_end)) = content_range {
            // Cap the span so a pathological selection can't allocate
            // unbounded region rows.
            const MAX_SELECTION_SPAN_ROWS: i32 = 100_000;
            let span = (cs_end - cs_start).clamp(1, MAX_SELECTION_SPAN_ROWS);
            (cs_start, 0, span)
        } else {
            (
                self.scroll_y,
                i32::from(inner_area.y),
                i32::from(inner_area.bottom()),
            )
        };
        let x_off = inner_area.x + 3;

        // Same O(log n + visible) walk as the render: start at the first
        // message that can intersect the viewport and stop past its bottom.
        // Off-screen messages contribute no regions, so skipping them is
        // identical to the old full walk — just without the per-frame O(n)
        // scan of the whole timeline. Computed BEFORE the field borrows
        // below (`find_first_visible` needs `&self`).
        let first_vis = self.find_first_visible(scroll);
        let walk_start = if self.msg_height_cache.is_empty() {
            vp_top - scroll
        } else {
            vp_top - scroll + self.prefix_y[first_vis]
        };

        // Direct field borrows keep the reusable scratch buffer (a `&mut`
        // borrow of `self.scratch`) disjoint from `self.text_regions` pushes
        // and the other fields touched inside the per-part loop.
        let text_regions = &mut self.text_regions;
        let scratch = &mut self.scratch;
        text_regions.clear();

        // Tool render state for the region scratch renders: expansion maps
        // cloned from the live state (so collapsed/expanded boxes sample the
        // same rows the screen shows) but a FRESH spinner map — `dispatch_tool`
        // creates/advances spinners as a side effect, and the real ones must
        // not be touched by a copy-region build (the build also runs for
        // content rows that are currently off-screen).
        let mut region_tool_state = ToolRenderState {
            expanded: self.tool_state.expanded.clone(),
            error_expanded: self.tool_state.error_expanded.clone(),
            tool_spinners: std::collections::HashMap::new(),
            version: self.tool_state.version,
        };

        let mut y = walk_start;

        for (idx, msg) in session.messages.iter().enumerate().skip(first_vis) {
            if idx > first_vis {
                y += 1;
            }

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            if msg_top >= vp_bottom {
                break;
            }
            let msg_bottom = y + msg_h;

            // Only process messages that overlap with the viewport
            if msg_bottom > vp_top && msg_top < vp_bottom {
                let border_offset = match msg.role {
                    MessageRole::User => 1,
                    MessageRole::Assistant => 0,
                };
                let mut part_y = msg_top + border_offset;

                for (pi, part) in msg.parts.iter().enumerate() {
                    // No .max(1): hidden parts (e.g. plan_todo_write running/cleared) have
                    // height 0 and must not advance part_y to avoid scroll inconsistencies.
                    let raw_h = self
                        .part_heights_cache
                        .get(idx)
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(
                                part,
                                max_w,
                                config,
                                &msg.role,
                                &self.tool_state,
                            )
                        });
                    let part_h = i32::from(raw_h);
                    let p_top = part_y;
                    let p_bottom = part_y + part_h;

                    // Content-space position of this part's first row
                    let content_offset = p_top - vp_top + scroll;

                    // Only add regions for parts that overlap the viewport
                    if p_bottom > vp_top && p_top < vp_bottom {
                        match part {
                            crate::types::Part::Text(t) if !t.synthetic => {
                                // ── Streaming last message: regions straight from the
                                // incremental cache ── `update_streaming_message_cache`
                                // (which runs before this in render()) already refreshed
                                // the text-part cells this frame, so only the viewport
                                // rows are converted to regions (O(visible)); no re-parse
                                // and no full-content sanitize per frame.
                                if msg.role == MessageRole::Assistant
                                    && self.streaming_msg.as_ref().is_some_and(|sc| {
                                        sc.message_id == msg.id
                                            && sc
                                                .text
                                                .as_ref()
                                                .is_some_and(|tc| tc.part_index == pi)
                                    })
                                {
                                    let sc = self.streaming_msg.as_ref().unwrap();
                                    if let Some(tc) = sc.text.as_ref() {
                                        let tc_cells = tc.content();
                                        let tc_w = tc.width as usize;
                                        let screen_start = p_top.max(vp_top) as u16;
                                        let screen_end = p_bottom.min(vp_bottom) as u16;
                                        if screen_start < screen_end {
                                            let first_dy = (screen_start as i32 - p_top) as u16;
                                            for (screen_line_y, dy) in
                                                (screen_start..).zip(first_dy..tc.height)
                                            {
                                                if screen_line_y >= screen_end {
                                                    break;
                                                }
                                                let base = dy as usize * tc_w;
                                                let trimmed =
                                                    text_from_cell_row(&tc_cells[base..], tc_w);
                                                let cy = (screen_line_y as i32) - vp_top + scroll;
                                                text_regions.push(TextRegion::one_row(
                                                    cy,
                                                    x_off,
                                                    x_off + max_w,
                                                    trimmed,
                                                ));
                                            }
                                        }
                                    }
                                    part_y += part_h;
                                    continue;
                                }

                                let content = if config.conceal {
                                    conceal_text(&t.text)
                                } else {
                                    sanitize_text(&t.text)
                                };
                                if content.chars().all(char::is_whitespace) {
                                    part_y += part_h;
                                    continue;
                                }

                                // ── Assistant text: render markdown into temp buffer and scan ──
                                // First tries cached text regions (from the render loop on a previous
                                // frame); falls back to a direct markdown render into a temp buffer.
                                if msg.role == MessageRole::Assistant {
                                    let used_cache = self
                                        .msg_cache_text_regions
                                        .get(idx)
                                        .and_then(|r| r.as_ref())
                                        .is_some_and(|cached| {
                                            let p_start_cs = content_offset;
                                            let p_end_cs = content_offset + part_h;
                                            let vp_start_cs = scroll;
                                            let vp_end_cs = scroll + (vp_bottom - vp_top);
                                            let cs_start = p_start_cs.max(vp_start_cs);
                                            let cs_end = p_end_cs.min(vp_end_cs);
                                            for region in cached {
                                                if region.y1 >= cs_start && region.y1 < cs_end {
                                                    text_regions.push(TextRegion::one_row(
                                                        region.y1,
                                                        x_off,
                                                        x_off + max_w,
                                                        region.text.clone(),
                                                    ));
                                                }
                                            }
                                            cached.iter().any(|r| r.y1 >= cs_start && r.y1 < cs_end)
                                        });

                                    if !used_cache {
                                        let generous_h =
                                            ((part_h as u16).saturating_mul(3)).clamp(200, 5000);
                                        let scan_area = Rect::new(0, 0, max_w, generous_h);
                                        // Reuse the scratch buffer instead of allocating a
                                        // fresh multi-thousand-row buffer every frame while
                                        // the last message streams.
                                        let temp = scratch.get_or_insert_with(|| {
                                            ratatui::buffer::Buffer::empty(scan_area)
                                        });
                                        if *temp.area() != scan_area {
                                            temp.resize(scan_area);
                                        }

                                        let empty_style = Style::default()
                                            .bg(rgba_color(theme.background))
                                            .fg(rgba_color(theme.text));
                                        for ty in 0..generous_h {
                                            for tx in 0..max_w {
                                                if let Some(cell) = temp.cell_mut((tx, ty)) {
                                                    cell.set_char(' ');
                                                    cell.set_style(empty_style);
                                                }
                                            }
                                        }

                                        let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                                            Some(content.clone()),
                                        );
                                        md.set_fg(Some(ColorInput::RGBA(theme.text)));
                                        md.set_bg(Some(ColorInput::RGBA(theme.background)));
                                        crate::util::markdown::apply_theme(&mut md, theme);
                                        md.render_self(temp, scan_area);

                                        let screen_end = p_bottom.min(vp_bottom) as u16;
                                        // Rows of this part scrolled off ABOVE the viewport
                                        // still exist in the temp buffer: start scanning at
                                        // the first VISIBLE row, not at row 0. Pairing
                                        // screen rows with temp row 0 re-mapped the head of
                                        // the message onto the viewport (shifted copy) and
                                        // dropped the tail past the scan window (truncated
                                        // copy). Same fix as the streaming path's `first_dy`.
                                        let first_ty = (vp_top - p_top).max(0) as u16;
                                        for (screen_line_y, ty) in
                                            (p_top.max(vp_top) as u16..).zip(first_ty..generous_h)
                                        {
                                            if screen_line_y >= screen_end {
                                                break;
                                            }

                                            let temp_cells = temp.content();
                                            let row = ty as usize * max_w as usize;
                                            let trimmed = text_from_cell_row(
                                                &temp_cells[row..],
                                                max_w as usize,
                                            );
                                            let cy = (screen_line_y as i32) - vp_top + scroll;
                                            text_regions.push(TextRegion::one_row(
                                                cy,
                                                x_off,
                                                x_off + max_w,
                                                trimmed,
                                            ));
                                        }
                                    }
                                } else {
                                    // ── User text: width-aware wrapping (CJK, emoji, flags = 2 cols) ──
                                    let mut screen_line_y = p_top.max(vp_top) as u16;
                                    let screen_end = p_bottom.min(vp_bottom) as u16;

                                    // Visual rows this part has scrolled off ABOVE the
                                    // viewport. Computed ONCE from the RAW part top — NOT
                                    // from `screen_line_y`, which starts pre-clamped at
                                    // `p_top.max(vp_top)` and would zero the skip — then
                                    // consumed across empty lines and wrapped rows alike,
                                    // so screen row `vp_top` pairs with the part's visual
                                    // row `vp_top - p_top`. Pairing from row 0 re-mapped
                                    // the head of every straddling part onto the viewport
                                    // (shifted copy) and dropped the tail (truncated copy).
                                    let mut skip = (vp_top - p_top).max(0) as usize;

                                    for logical_line in content.lines() {
                                        if logical_line.is_empty() {
                                            // `draw_text_wrap` advances one row per wrapped
                                            // line, blanks included — the walk must match or
                                            // every row after a blank line copies shifted.
                                            if skip > 0 {
                                                skip -= 1;
                                            } else if screen_line_y < screen_end {
                                                let cy = (screen_line_y as i32) - vp_top + scroll;
                                                text_regions.push(TextRegion::one_row(
                                                    cy,
                                                    x_off,
                                                    x_off + max_w,
                                                    String::new(),
                                                ));
                                                screen_line_y += 1;
                                            }
                                            continue;
                                        }
                                        let lines = cosh_tui::core::lib::unicode_util::word_wrap(
                                            logical_line,
                                            max_w,
                                        );
                                        // Consume this logical line's share of the
                                        // off-screen rows before pairing the rest.
                                        let take = skip.min(lines.len());
                                        skip -= take;
                                        for visual_line in &lines[take..] {
                                            if screen_line_y >= screen_end {
                                                break;
                                            }
                                            let cy = (screen_line_y as i32) - vp_top + scroll;
                                            text_regions.push(TextRegion::one_row(
                                                cy,
                                                x_off,
                                                x_off + max_w,
                                                visual_line.clone(),
                                            ));
                                            screen_line_y += 1;
                                        }
                                    }
                                }
                            }
                            crate::types::Part::Tool(t) => {
                                // Skip hidden TODO tools entirely (no text regions, no spacing)
                                if self::tool_render::tool_display(&t.tool) == "todo"
                                    && !matches!(t.status, crate::types::ToolStatus::Failed(_))
                                    && (matches!(t.status, crate::types::ToolStatus::Running)
                                        || t.output.as_deref().unwrap_or("").trim().is_empty())
                                {
                                    continue;
                                }
                                if !config.show_tool_details
                                    && matches!(t.status, crate::types::ToolStatus::Completed)
                                {
                                    part_y += part_h;
                                    continue;
                                }
                                if !config.show_generic_tool_output
                                    && self::tool_render::tool_display(&t.tool) == "generic"
                                {
                                    part_y += part_h;
                                    continue;
                                }
                                // ── Sample the cells the REAL renderer paints ──
                                // The old builder re-derived box geometry by hand
                                // (hardcoded label/body offsets) and synthesized the
                                // copy text with `tool_copy_text`, which drifted from
                                // the renderers: the diff/read line-number gutters and
                                // +/- signs sit several columns right of where the
                                // regions claimed content started (copies shifted or
                                // empty), long lines truncate instead of wrapping, the
                                // split diff view pairs two source lines per row, and
                                // single-line generic labels were followed by
                                // whole-output regions that collided with the NEXT
                                // part's rows — copying text the user never selected.
                                // Rendering the part through the same `dispatch_tool`
                                // the screen uses and sampling the painted cells keeps
                                // copy, highlight and screen in sync for every
                                // renderer, with no per-tool geometry to maintain.
                                let is_block = Self::tool_is_block(t);
                                // The scratch mirrors the message's inner content area:
                                // the box paints at x=3 (screen x_off) and the widest
                                // box spans max_w + 3 columns from there.
                                let scan_area =
                                    Rect::new(0, 0, max_w.saturating_add(6), part_h.max(1) as u16);
                                let temp = scratch.get_or_insert_with(|| {
                                    ratatui::buffer::Buffer::empty(scan_area)
                                });
                                if *temp.area() != scan_area {
                                    temp.resize(scan_area);
                                }
                                temp.reset();
                                // Block boxes start one row below the part top — the
                                // external top margin render_parts adds — so mirror it
                                // here and the sampled rows line up with the screen.
                                let mut y_cur = u16::from(is_block);
                                let mut line_h = 0u16;
                                self::tool_render::dispatch_tool(
                                    &mut self::tool_render::ToolRenderCtx {
                                        buf: temp,
                                        x: 3,
                                        y: y_cur,
                                        line_h: &mut line_h,
                                        max_w,
                                        state: &mut region_tool_state,
                                        theme,
                                    },
                                    t,
                                    pi as u16,
                                );
                                y_cur = y_cur.saturating_add(line_h);
                                // Bottom margin, then the passive LSP findings — the
                                // same geometry render_parts paints below the box.
                                if is_block && y_cur.saturating_add(1) < scan_area.height {
                                    y_cur += 1;
                                }
                                if let Some(notes) = &t.lsp_notes
                                    && matches!(t.status, crate::types::ToolStatus::Completed)
                                    && Self::lsp_notes_count(notes) > 0
                                {
                                    let expanded = region_tool_state.is_expanded_or(
                                        &Self::lsp_notes_id(t),
                                        config.diagnostics_mode,
                                    );
                                    let header = if expanded {
                                        "- Diagnostics"
                                    } else {
                                        "+ Diagnostics"
                                    };
                                    let header_style = Style::default().fg(rgba_color(theme.error));
                                    draw_text_line(temp, header, 3, y_cur, max_w, header_style);
                                    if expanded {
                                        Self::render_lsp_notes(
                                            temp,
                                            3,
                                            y_cur + 1,
                                            max_w,
                                            theme,
                                            notes,
                                        );
                                    }
                                }
                                let temp_cells = temp.content();
                                let stride = scan_area.width as usize;
                                for k in 0..scan_area.height as usize {
                                    // Strip the 3 left columns (message margin): the
                                    // sampled text starts at screen column x_off.
                                    let base = k * stride + 3;
                                    let trimmed =
                                        text_from_cell_row(&temp_cells[base..], max_w as usize);
                                    push_sampled_row(
                                        text_regions,
                                        content_offset + k as i32,
                                        x_off,
                                        max_w,
                                        &trimmed,
                                    );
                                }
                            }
                            crate::types::Part::Reasoning(r) => {
                                let expanded = self.tool_state.is_expanded_or(
                                    &r.text[..r.text.floor_char_boundary(32)],
                                    config.thinking_mode,
                                );
                                let header = if expanded { "- Thought" } else { "+ Thought" };
                                if p_top >= vp_top {
                                    text_regions.push(TextRegion::one_row(
                                        content_offset,
                                        x_off,
                                        x_off + max_w,
                                        header.to_string(),
                                    ));
                                }
                                if expanded && !r.text.is_empty() {
                                    // The body is drawn through the SAME
                                    // markdown pipeline as the screen
                                    // (render_reasoning): word-wrapped, so
                                    // VISUAL rows — not source lines — are
                                    // what a selection must map to. Render
                                    // into the scratch buffer and scan one
                                    // region per visual row, exactly like
                                    // the Summarizing body below. The old
                                    // per-source-line mapping (capped at 10
                                    // lines, blank lines skipped) desynced
                                    // copy regions from the screen, so a
                                    // small selection inside a Thought
                                    // copied nothing or the wrong slice.
                                    let body_w = max_w.saturating_sub(2).max(1);
                                    let content = sanitize_text(&r.text);
                                    let body_h = estimate_height(&content, body_w).max(1);
                                    let scan_area = Rect::new(0, 0, body_w, body_h);
                                    let temp = scratch.get_or_insert_with(|| {
                                        ratatui::buffer::Buffer::empty(scan_area)
                                    });
                                    if *temp.area() != scan_area {
                                        temp.resize(scan_area);
                                    }
                                    let mut md = cosh_tui::core::renderables::markdown::
                                        MarkdownRenderable::new(Some(content));
                                    md.set_fg(Some(ColorInput::RGBA(theme.text_muted)));
                                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                                    crate::util::markdown::apply_theme(&mut md, theme);
                                    md.render_self(temp, scan_area);

                                    // Body rows start one row below the
                                    // "+ Thought" header. A part may straddle
                                    // the viewport top: skip the rows scrolled
                                    // off above instead of mapping body line 0
                                    // onto the first visible row (that offset
                                    // made small selections near the viewport
                                    // top copy from the wrong line).
                                    let body_top = p_top + 1;
                                    let row_start = (vp_top - body_top).max(0);
                                    let row_end = (p_bottom.min(vp_bottom) - body_top)
                                        .max(0)
                                        .min(i32::from(body_h));
                                    for k in row_start..row_end {
                                        let temp_cells = temp.content();
                                        let row = k as usize * body_w as usize;
                                        let trimmed =
                                            text_from_cell_row(&temp_cells[row..], body_w as usize);
                                        let cy = body_top + k - vp_top + scroll;
                                        text_regions.push(TextRegion::one_row(
                                            cy,
                                            x_off + 2,
                                            x_off + max_w,
                                            trimmed,
                                        ));
                                    }
                                }
                            }
                            crate::types::Part::Compaction(c) => {
                                if !c.text.is_empty() {
                                    // The "Summarizing" box: sample the cells the real
                                    // renderer paints (title, wrapped body rows, collapse
                                    // hint) so copy text matches the screen. The old
                                    // hand-mirrored layout labeled rows one column off
                                    // (body paints at x+3, regions claimed x+2) and
                                    // re-derived the collapsed-tail row window instead of
                                    // reusing the renderer's own.
                                    let expanded = self.tool_state.is_expanded(&summarizing_id(c));
                                    // The box paints at the part top with no external
                                    // margin (render_parts advances straight past it),
                                    // and its width is max_w + 3 inside the message's
                                    // max_w + 6 content area — mirror that exactly.
                                    let scan_area =
                                        Rect::new(0, 0, max_w.saturating_add(6), part_h as u16);
                                    let temp = scratch.get_or_insert_with(|| {
                                        ratatui::buffer::Buffer::empty(scan_area)
                                    });
                                    if *temp.area() != scan_area {
                                        temp.resize(scan_area);
                                    }
                                    temp.reset();
                                    Self::render_summarizing_box(
                                        temp, 3, 0, max_w, c, expanded, theme,
                                    );
                                    let temp_cells = temp.content();
                                    let stride = scan_area.width as usize;
                                    // Clamp to the visible band like the other arms:
                                    // rows the viewport cannot show are not
                                    // selectable, so they get no region (and no scan).
                                    let first_k = (vp_top - p_top).max(0) as usize;
                                    let last_k = (p_bottom.min(vp_bottom) - p_top).max(0) as usize;
                                    for k in first_k..last_k.min(part_h as usize) {
                                        // Strip the 3 box-margin columns so the sampled
                                        // text starts at screen column x_off.
                                        let base = k * stride + 3;
                                        let trimmed =
                                            text_from_cell_row(&temp_cells[base..], max_w as usize);
                                        push_sampled_row(
                                            text_regions,
                                            content_offset + k as i32,
                                            x_off,
                                            max_w,
                                            &trimmed,
                                        );
                                    }
                                } else if p_top >= vp_top {
                                    let text = compaction_line(c, crate::types::now_ms());
                                    text_regions.push(TextRegion::one_row(
                                        content_offset,
                                        x_off,
                                        x_off + max_w,
                                        text,
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }

                    part_y += part_h;
                }
            }

            y += msg_h;
        }
    }
}

impl SessionView {
    /// Content-space row span covered by the current drag selection: the
    /// anchor converted with the scroll offset captured at mouse-down, the
    /// focus with the current one. Returned as (start, end), end exclusive.
    pub fn selection_content_range(&self, anchor_y: u16, focus_y: u16) -> (i32, i32) {
        let vp_top = self.session_area.map_or(0, |(_, y, _, _)| i32::from(y));
        let content_anchor = (i32::from(anchor_y)) - vp_top + self.mouse_down_scroll_y;
        let content_focus = (i32::from(focus_y)) - vp_top + self.scroll_y;
        (
            content_anchor.min(content_focus),
            content_anchor.max(content_focus),
        )
    }
}

impl SessionView {
    pub fn get_text_in_region(
        &self,
        anchor_x: u16,
        anchor_y: u16,
        focus_x: u16,
        focus_y: u16,
    ) -> String {
        // Convert screen-space anchor/focus to content-space coordinates.
        // The anchor was captured at mouse_down_scroll_y; the focus at the
        // current scroll_y.  Content-space allows the selection to survive
        // scroll changes during drag (auto-scroll).
        let vp_top = self.session_area.map_or(0, |(_, y, _, _)| i32::from(y));
        let content_anchor = (anchor_y as i32) - vp_top + self.mouse_down_scroll_y;
        let content_focus = (focus_y as i32) - vp_top + self.scroll_y;

        let start_content_y = content_anchor.min(content_focus);
        let end_content_y = content_anchor.max(content_focus);

        // Determine x-bounds from the content-space drag direction.
        let (start_x, end_x) = if content_anchor <= content_focus {
            (anchor_x, focus_x)
        } else {
            (focus_x, anchor_x)
        };

        extract_text_in_region(
            &self.text_regions,
            start_content_y,
            end_content_y,
            start_x,
            end_x,
        )
    }
}
