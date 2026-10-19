use super::*;

impl SessionView {
    pub(super) fn render_file_badge(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        theme: &Theme,
        file: &FilePart,
    ) {
        let is_dir = file.mime == "application/x-directory";
        let tag = if is_dir { " Directory " } else { " File " };
        let label = format!("{tag}{}", file.filename);
        let fg = if is_dir { theme.secondary } else { theme.info };
        let style = Style::default().fg(rgba_color(fg));
        draw_text_line(buf, &label, x, y, max_w, style);
    }
}

impl SessionView {
    /// Render the "Summarizing" box of the LLM compaction (the last-resort
    /// fallback): a
    /// bordered title row with a live/elapsed stopwatch, and the streamed
    /// summary body. Collapsed, the body is a fixed-height preview that shows
    /// only the LAST lines (the top lines visually leave the box as the stream
    /// grows — an LRU-like scroll-up; nothing is actually removed). Expanded,
    /// the box grows to fit the whole text. Returns the rendered height.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_summarizing_box(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        part: &CompactionPart,
        expanded: bool,
        theme: &Theme,
    ) -> u16 {
        let now = crate::types::now_ms();
        let elapsed = part
            .elapsed_ms
            .unwrap_or_else(|| now.saturating_sub(part.started_at));
        let secs = elapsed as f64 / 1000.0;
        let wrap_w = max_w.saturating_sub(3).max(1);
        let total_h = summarizing_height(part, expanded, max_w);
        let area = Rect::new(x, y, max_w.saturating_add(3), total_h);

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(blank_border_chars());
        border_box.render_self(buf, area);

        let x_off = x + 3;
        // 1 blank row of padding above the title (the bottom pad is the last
        // row of the box); matches SUMMARIZING_PAD_V in summarizing_height.
        let title_y = y + SUMMARIZING_PAD_V;
        // Keep the expanded-state collapse affordance without a marker on the
        // collapsed title.
        let mut title = if expanded { "- " } else { "" }.to_string();
        title.push_str(&format!("Summarizing · {secs:.3}"));
        let title_style = Style::default()
            .fg(rgba_color(theme.text_muted))
            .add_modifier(Modifier::ITALIC);
        draw_text_line(
            buf,
            &title,
            x_off,
            title_y,
            max_w.saturating_sub(3),
            title_style,
        );

        // Markdown-rendered body — the same renderer as the chat Text parts,
        // so the raw markdown syntax never reaches the screen. The bg is the
        // box's own `background_panel` (NOT the chat `background`): the border
        // box already fills its whole area with the panel color, so painting
        // the body with the chat background would leave a visible darker
        // strip inside the box (default cosh: #07070A vs #0B0B12).
        let body_h = estimate_height(&part.text, wrap_w).max(1);
        let content = sanitize_text(&part.text);
        let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(content));
        md.set_fg(Some(ColorInput::RGBA(theme.text)));
        md.set_bg(Some(ColorInput::RGBA(theme.background_panel)));
        crate::util::markdown::apply_theme(&mut md, theme);
        let overflow = !expanded && body_h > SUMMARIZING_COLLAPSED_LINES;
        if overflow {
            // Collapsed preview: the box shows the LAST
            // `SUMMARIZING_COLLAPSED_LINES` rows of the laid-out markdown (the
            // top rows leave the view as the stream grows — the LRU/scroll-up
            // effect). Markdown only lays out from the top, so the full body
            // is rendered into a scratch buffer first and only the final rows
            // are blitted into the box.
            let tmp_area = Rect::new(0, 0, wrap_w, body_h);
            let mut tmp = ratatui::buffer::Buffer::empty(tmp_area);
            md.render_self(&mut tmp, tmp_area);
            let src_start = body_h - SUMMARIZING_COLLAPSED_LINES;
            for (i, src_y) in (src_start..body_h).enumerate() {
                let dst_y = y + SUMMARIZING_PAD_V + 1 + i as u16;
                if dst_y >= area.bottom() {
                    break;
                }
                for cx in 0..wrap_w {
                    if let Some(src) = tmp.cell((cx, src_y))
                        && let Some(dst) = buf.cell_mut((x_off + cx, dst_y))
                    {
                        dst.set_symbol(src.symbol());
                        dst.set_style(src.style());
                    }
                }
            }
        } else {
            let body_area = Rect::new(
                x_off,
                y + SUMMARIZING_PAD_V + 1,
                wrap_w,
                body_h.min(total_h.saturating_sub(1 + SUMMARIZING_PAD_V)),
            );
            md.render_self(buf, body_area);
        }
        if overflow {
            let hint_y = y + SUMMARIZING_PAD_V + 1 + SUMMARIZING_COLLAPSED_LINES;
            draw_hint_button(buf, "Click to expand", x_off, hint_y, wrap_w, theme);
        }
        total_h
    }
}

impl SessionView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_parts(
        buf: &mut Buffer,
        x: u16,
        y_start: u16,
        max_w: u16,
        max_h: u16,
        parts: &[Part],
        role: &MessageRole,
        theme: &Theme,
        tool_state: &mut ToolRenderState,
        config: &TuiConfig,
        opt_heights: Option<&[u16]>,
        streaming: bool,
    ) -> u16 {
        let mut y = y_start;
        let bottom = y_start + max_h;
        let fg_color = if config.conceal {
            theme.text_muted
        } else {
            theme.text
        };

        for (pi, part) in parts.iter().enumerate() {
            if y >= bottom {
                break;
            }

            match part {
                Part::Text(t) if !t.synthetic && *role == MessageRole::Assistant => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        sanitize_text(&t.text)
                    };
                    let est_h = opt_heights
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(part, max_w, config, role, tool_state)
                        });
                    let render_h = if streaming {
                        // During streaming, use the exact estimated height without padding.
                        // estimate_height uses the same pulldown_cmark layout algorithm as
                        // the render, so it's already accurate. Adding padding would inflate
                        // the total height and create scroll oscillation frame-to-frame.
                        est_h.max(1).min(bottom.saturating_sub(y))
                    } else {
                        // Small +1 min(3) padding for non-streaming messages to provide a
                        // safety margin against estimate_height being slightly off (CJK widths,
                        // code block spacing, etc.). This prevents content clipping without
                        // causing the oscillation that the old +5/min(10) padding caused.
                        // scan_content_height will return the actual content height, and
                        // without .max() inflation at end of render, no oscillation occurs
                        // even if actual_h slightly exceeds est_h.
                        (est_h.saturating_add(1))
                            .max(3)
                            .min(bottom.saturating_sub(y))
                    };
                    let area = Rect::new(x, y, max_w, render_h);
                    let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                        Some(content),
                    );
                    md.set_fg(Some(ColorInput::RGBA(fg_color)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                    crate::util::markdown::apply_theme(&mut md, theme);
                    md.render_self(buf, area);
                    // During streaming, skip the expensive scan_content_height
                    // for the last message since it will be re-rendered next
                    // frame. The estimated height from the same markdown layout
                    // algorithm is already accurate, so no padding is needed.
                    let actual_h = if streaming {
                        render_h
                    } else {
                        // The glyph scan ignores the code block's padding rows
                        // (top gap, bottom padding and blank separator), so
                        // advancing purely by the scan would make the next
                        // part/message overlap them — which visually erases the
                        // block's bottom padding (code blocks without a
                        // language tag ended up with 2 blank rows on top and
                        // none at the bottom). The estimate mirrors the
                        // renderer's full cursor advance, so advance by
                        // whichever is taller.
                        let scanned = Self::scan_content_height(buf, x, y, max_w, render_h);
                        scanned.max(est_h)
                    };
                    y += actual_h.max(1);
                }
                Part::Text(t) if !t.synthetic => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        sanitize_text(&t.text)
                    };
                    let h = opt_heights
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(part, max_w, config, role, tool_state)
                        })
                        .min(bottom - y)
                        .max(1);
                    let text_style = Style::default()
                        .fg(rgba_color(fg_color))
                        .bg(rgba_color(theme.background_panel));
                    let rendered = Self::draw_text_wrap(buf, &content, x, y, max_w, h, text_style);
                    y += rendered;
                }
                Part::Tool(tool) => {
                    // Skip hidden TODO tools entirely (no visual space, no copyable text)
                    if tool_render::tool_display(&tool.tool) == "todo"
                        && !matches!(tool.status, ToolStatus::Failed(_))
                        && (matches!(tool.status, ToolStatus::Running)
                            || tool.output.as_deref().unwrap_or("").trim().is_empty())
                    {
                        continue;
                    }
                    if !config.show_tool_details && matches!(tool.status, ToolStatus::Completed) {
                        y += 1;
                        continue;
                    }
                    if !config.show_generic_tool_output
                        && tool_render::tool_display(&tool.tool) == "generic"
                    {
                        y += 1;
                        continue;
                    }

                    // Determine if this tool renders as a block (bordered box)
                    // so we can add a small vertical margin around it. Uses the
                    // same predicate as the height estimate (`tool_is_block`),
                    // so the drawn box plus margins matches the cached height.
                    let is_block = Self::tool_is_block(tool);

                    // Top margin — always added for block tools (the estimate
                    // reserves it even for the first part of the message), but
                    // skip if there isn't room for at least 1 row after it.
                    if is_block && y + 1 < bottom {
                        y += 1;
                    }

                    let mut line_h = 0u16;
                    tool_render::dispatch_tool(
                        &mut tool_render::ToolRenderCtx {
                            buf: &mut *buf,
                            x,
                            y,
                            line_h: &mut line_h,
                            max_w,
                            state: &mut *tool_state,
                            theme,
                        },
                        tool,
                        pi as u16,
                    );
                    let available = bottom.saturating_sub(y);
                    line_h = line_h.min(available);
                    y += line_h;

                    // Bottom margin
                    if is_block && y < bottom {
                        y += 1;
                    }

                    // Passive LSP findings: a "+"/"- LSP" affordance in the
                    // Thought style, collapsed by default — a click expands
                    // one clipped line per finding below the code/diff block.
                    if let Some(notes) = &tool.lsp_notes
                        && matches!(tool.status, ToolStatus::Completed)
                        && Self::lsp_notes_count(notes) > 0
                    {
                        let expanded = tool_state
                            .is_expanded_or(&Self::lsp_notes_id(tool), config.diagnostics_mode);
                        let header = if expanded {
                            "- Diagnostics"
                        } else {
                            "+ Diagnostics"
                        };
                        let header_style = Style::default().fg(rgba_color(theme.error));
                        draw_text_line(buf, header, x, y, max_w, header_style);
                        y += 1;
                        if expanded {
                            y = Self::render_lsp_notes(buf, x, y, max_w, theme, notes);
                        }
                    }
                }
                Part::Reasoning(r) => {
                    let expanded = tool_state.is_expanded_or(
                        &r.text[..r.text.floor_char_boundary(32)],
                        config.thinking_mode,
                    );
                    let mut line_h = 0u16;
                    Self::render_reasoning(buf, x, y, &mut line_h, max_w, r, expanded, theme);
                    y += line_h.max(1);
                }
                Part::File(f) => {
                    Self::render_file_badge(buf, x, y, max_w, theme, f);
                    y += 1;
                }
                Part::Install(p) => {
                    // One-line status rendered with the SAME bar component
                    // as the context budget bar (`render_budget_bar`): the
                    // download progress mirrors it in the chat flow. While
                    // running the elapsed time ticks (the activity signal);
                    // the terminal event freezes the line at 100%/failed.
                    let text = install_line(p, crate::types::now_ms());
                    let color = if p.failed == Some(true) {
                        theme.error
                    } else if p.is_running() {
                        theme.text_muted
                    } else {
                        theme.success
                    };
                    let style = Style::default().fg(rgba_color(color));
                    draw_text_line(buf, &text, x, y, max_w, style);
                    y += 1;
                }
                Part::Compaction(c) => {
                    if !c.text.is_empty() {
                        let expanded = tool_state.is_expanded(&summarizing_id(c));
                        let mut line_h =
                            Self::render_summarizing_box(buf, x, y, max_w, c, expanded, theme);
                        // Clamp to the remaining viewport like the tool blocks,
                        // so an expanded long summary never draws past it.
                        let available = bottom.saturating_sub(y);
                        line_h = line_h.min(available);
                        y += line_h;
                    } else {
                        // One-line status: the "Summarizing" line ticks live
                        // (computed from `started_at` vs now) until it
                        // finalizes. Height is always 1, so the line never
                        // reflows while the stopwatch runs.
                        let text = compaction_line(c, crate::types::now_ms());
                        let style = Style::default().fg(rgba_color(compaction_color(theme)));
                        draw_text_line(buf, &text, x, y, max_w, style);
                        y += 1;
                    }
                }
                Part::Text(_) => {}
            }
        }

        y - y_start
    }
}

impl SessionView {
    /// Draw the passive LSP findings below a tool's output block: errors in
    /// the theme's error color, warnings in its warning color.
    pub(super) fn render_lsp_notes(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        theme: &Theme,
        notes: &cosh_tools::fs::LspNotes,
    ) -> u16 {
        let error_style = Style::default().fg(rgba_color(theme.error));
        let warning_style = Style::default().fg(rgba_color(theme.warning));
        let mut y = y;
        for note in &notes.errors {
            let line = format!("\u{2717} {}:{} {}", note.path, note.line, note.message);
            draw_text_line(buf, &line, x, y, max_w, error_style);
            y += 1;
        }
        for note in &notes.warnings {
            let line = format!("\u{26a0} {}:{} {}", note.path, note.line, note.message);
            draw_text_line(buf, &line, x, y, max_w, warning_style);
            y += 1;
        }
        y
    }
}

impl SessionView {
    pub(super) fn render_queued_badge(buf: &mut Buffer, x: u16, y: u16, theme: &Theme) {
        let badge_style = Style::default().fg(rgba_color(theme.warning));
        draw_text_line(buf, " [QUEUED]", x, y, 16, badge_style);
    }
}

impl SessionView {
    pub(super) fn render_compaction_banner(buf: &mut Buffer, x: u16, y: u16, max_w: u16, theme: &Theme) {
        let line_style = Style::default().fg(rgba_color(theme.text_muted));
        let dash = '\u{2500}';
        let right = x + max_w;
        for cx in x..right {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(dash);
                cell.set_style(line_style);
            }
        }
        let label = " Compaction ";
        let label_x = x + max_w.saturating_sub(label.len() as u16 + 2);
        let label_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, label, label_x, y, label.len() as u16, label_style);
    }
}

impl SessionView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_user_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_color: RGBA,
        tool_state: &mut ToolRenderState,
        config: &TuiConfig,
        is_queued: bool,
        is_compacted: bool,
        part_heights: Option<&[u16]>,
        streaming: bool,
    ) {
        // The sent-prompt frame: extracted verbatim into
        // `component::prompt_message_box` so the `/agents` pane mirrors the
        // EXACT same box (byte-identical by the component's own test).
        crate::component::prompt_message_box::render(buf, area, agent_color, theme);

        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6);
        let border_line = 1u16;
        let padding_bottom = 1u16;
        let inner_h = area
            .height
            .saturating_sub(border_line)
            .saturating_sub(padding_bottom);

        if is_compacted {
            Self::render_compaction_banner(buf, x_off, area.y + 1, max_w, theme);
        }

        let inner_y = area.y + border_line + u16::from(is_compacted);
        let remaining = inner_h.saturating_sub(u16::from(is_compacted));

        Self::render_parts(
            buf,
            x_off,
            inner_y,
            max_w,
            remaining,
            &msg.parts,
            &MessageRole::User,
            theme,
            tool_state,
            config,
            part_heights,
            streaming,
        );

        if is_queued {
            Self::render_queued_badge(buf, x_off, area.y, theme);
        }
    }
}

impl SessionView {
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cast_sign_loss
    )]
    pub(super) fn render_assistant_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        tool_state: &mut ToolRenderState,
        config: &TuiConfig,
        is_queued: bool,
        is_compacted: bool,
        part_heights: &[u16],
        streaming: bool,
    ) -> u16 {
        let is_error = msg.id.starts_with("msg-err-");
        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6).max(2);

        if is_error {
            // The sent-prompt frame in the error tone — the SAME extracted
            // component the user message uses.
            crate::component::prompt_message_box::render(buf, area, theme.error, theme);

            let error_style = Style::default()
                .fg(rgba_color(theme.text_muted))
                .bg(rgba_color(theme.background_panel));
            let error_text: String = msg
                .parts
                .iter()
                .filter_map(|p| {
                    if let crate::types::Part::Text(t) = p {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .filter(|ch| !ch.is_control())
                .collect();
            let lines = cosh_tui::core::lib::unicode_util::word_wrap(&error_text, max_w);
            for (line_y, line) in (area.y + 1..).zip(lines.iter()) {
                if line_y >= area.bottom() {
                    break;
                }
                let mut line_x = x_off;
                for (grapheme, w) in cosh_tui::core::lib::unicode_util::graphemes_with_width(line) {
                    if line_x > x_off + max_w {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((line_x, line_y)) {
                        if grapheme.len() == 1 {
                            cell.set_char(grapheme.chars().next().unwrap());
                        } else {
                            cell.set_symbol(grapheme);
                        }
                        cell.set_style(error_style);
                    }
                    if w > 1 {
                        for dx in 1..w {
                            if let Some(cell) = buf.cell_mut((line_x + dx, line_y)) {
                                cell.set_diff_option(CellDiffOption::Skip);
                            }
                        }
                    }
                    line_x += w;
                }
            }
            return 0;
        }

        let banner_h = u16::from(is_compacted);
        let inner_y = area.y + banner_h;
        let net_h = area.height.saturating_sub(banner_h);

        let parts_h = Self::render_parts(
            buf,
            x_off,
            inner_y,
            max_w,
            net_h,
            &msg.parts,
            &MessageRole::Assistant,
            theme,
            tool_state,
            config,
            Some(part_heights),
            streaming,
        );

        if is_compacted {
            Self::render_compaction_banner(buf, x_off, area.y, max_w, theme);
        }

        if is_queued {
            Self::render_queued_badge(buf, x_off, area.y, theme);
        }

        banner_h + parts_h
    }
}
