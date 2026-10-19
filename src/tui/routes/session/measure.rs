use super::*;

impl SessionView {
    pub(super) fn draw_text_wrap(
        buf: &mut Buffer,
        text: &str,
        x: u16,
        y_: u16,
        max_w: u16,
        max_h: u16,
        style: Style,
    ) -> u16 {
        let Some(right) = x.checked_add(max_w) else {
            return 1;
        };
        let Some(bottom) = y_.checked_add(max_h) else {
            return 1;
        };
        for row in y_..bottom {
            for col in x..right {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(style);
                    cell.set_char(' ');
                }
            }
        }
        let lines = cosh_tui::core::lib::unicode_util::word_wrap(text, max_w);
        let mut y = y_;
        for line in &lines {
            if y >= bottom {
                break;
            }
            let mut cx = x;
            for (grapheme, w) in cosh_tui::core::lib::unicode_util::graphemes_with_width(line) {
                if cx >= right {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    if grapheme.len() == 1 {
                        cell.set_char(grapheme.chars().next().unwrap());
                    } else {
                        cell.set_symbol(grapheme);
                    }
                    cell.set_style(style);
                }
                if w > 1 {
                    for dx in 1..w {
                        if let Some(cell) = buf.cell_mut((cx + dx, y)) {
                            cell.set_diff_option(CellDiffOption::Skip);
                        }
                    }
                }
                cx += w;
            }
            y += 1;
        }
        (y - y_).max(1)
    }
}

impl SessionView {
    /// True when the tool renders as a bordered block (box) rather than an
    /// inline single-line label. Mirrors what the renderers actually draw so
    /// the height estimate and the render agree on the box's external margins:
    /// `render_shell` boxes any bash with output (Running or Completed),
    /// `render_glob` boxes any glob with listable output, `render_write`
    /// boxes a completed write with non-empty input content and no warnings
    /// (its output is a short receipt — size comes from the input),
    /// `render_edit` boxes a completed edit with an extractable diff,
    /// `render_read` boxes any completed read with parseable content,
    /// `render_todo` boxes any non-running tool with listable output
    /// (Completed and Failed both draw).
    pub(super) fn tool_is_block(part: &ToolPart) -> bool {
        let display = tool_render::tool_display(&part.tool);
        let has_output = part.output.as_deref().is_some_and(|o| !o.trim().is_empty());
        match display {
            "bash" => has_output,
            "glob" => tool_render::glob_block_text(part).is_some(),
            "write" => tool_render::write_box_lines(part).is_some(),
            "edit" => tool_render::edit_diff_content(part).is_some(),
            "read" => {
                has_output
                    && matches!(part.status, ToolStatus::Completed)
                    && tool_render::read_block_text(part).is_some()
            }
            "todo" => {
                !matches!(part.status, ToolStatus::Running)
                    && has_output
                    && !tool_render::format_todo_output(part.output.as_deref().unwrap_or("").trim())
                        .is_empty()
            }
            _ => false,
        }
    }
}

impl SessionView {
    pub(super) fn estimate_part_height(
        part: &Part,
        max_w: u16,
        config: &TuiConfig,
        role: &MessageRole,
        tool_state: &ToolRenderState,
    ) -> u16 {
        match part {
            Part::Text(t) if !t.synthetic && *role == MessageRole::Assistant => {
                estimate_height(&t.text, max_w)
            }
            Part::Text(t) if !t.synthetic => {
                let lines = cosh_tui::core::lib::unicode_util::word_wrap(&t.text, max_w);
                lines.len().max(1) as u16
            }
            Part::Install(_) => 1,
            Part::Tool(t) => {
                if !config.show_tool_details && matches!(t.status, ToolStatus::Completed) {
                    return 1;
                }
                if !config.show_generic_tool_output
                    && tool_render::tool_display(&t.tool) == "generic"
                {
                    return 1;
                }
                // Hide TODO tools that are running or have no output (cleared by dedup)
                // — they take no visual space and no copyable text. Only show on failure.
                if tool_render::tool_display(&t.tool) == "todo"
                    && !matches!(t.status, ToolStatus::Failed(_))
                    && (matches!(t.status, ToolStatus::Running)
                        || t.output.as_deref().unwrap_or("").trim().is_empty())
                {
                    return 0;
                }
                // Completed ask_questions renders compact question/answer
                // rows directly on the chat background.
                if tool_render::tool_display(&t.tool) == "question"
                    && let Some(height) = tool_render::question_height(t, max_w)
                {
                    return height;
                }
                // Only tools that render block-style output (shell, write, edit, read, todo, glob)
                // should allocate height for the full output block. All other tool
                // types render inline (1 line) regardless of whether they have output.
                // The conditions mirror each renderer EXACTLY, so the cached height
                // matches the drawn box even mid-loop (Running bash/glob with
                // streamed output already draw their box).
                let is_block = Self::tool_is_block(t);
                let notes_h = Self::lsp_notes_height(t, tool_state, config.diagnostics_mode);
                if is_block {
                    let output = t.output.as_deref().unwrap_or("").trim();
                    // Add 2 rows for the block's internal padding (top/bottom border lines),
                    // plus 2 rows for the external vertical margin that render_parts adds
                    // around block-type tools (1 top, 1 bottom).
                    if tool_render::tool_display(&t.tool) == "glob" {
                        // Glob block: preview collapsed, full list when expanded
                        // (mirrors render_glob's collapse_tool_output).
                        let id = t.tool_call_id.as_deref().unwrap_or("glob");
                        let body = self::tool_render::glob_block_text(t).unwrap_or_default();
                        let collapsed = crate::util::scroll::collapse_tool_output(&body, 10, 800);
                        let expanded = tool_state.is_expanded(id);
                        let display = if expanded || !collapsed.overflow {
                            &body
                        } else {
                            &collapsed.output
                        };
                        // Wrapped rows: the renderer folds long path lines at
                        // the box's content width (same helper as render_glob),
                        // so the estimate counts the visual rows the box
                        // paints, not raw source lines.
                        let rows = tool_render::tool_box_rows(
                            display.lines(),
                            None,
                            tool_render::tool_box_content_w(max_w),
                        );
                        let lines = rows.max(1) + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        lines + 5
                    } else if tool_render::tool_display(&t.tool) == "todo" {
                        let formatted = tool_render::format_todo_output(output);
                        // Wrapped rows: the renderer folds long descriptions
                        // at the box's content width (same helper as
                        // render_todo), so the estimate counts the visual
                        // rows the box paints, not raw source lines.
                        let lines = tool_render::tool_box_rows(
                            formatted.iter(),
                            None,
                            tool_render::tool_box_content_w(max_w),
                        )
                        .max(1);
                        lines + 4
                    } else if tool_render::tool_display(&t.tool) == "edit" {
                        // Same source as render_edit (shared helper) so the
                        // row count matches the drawn diff exactly. The count
                        // is PAINTED rows, not source lines: the split view
                        // (chat width ≥ 100) pairs consecutive `-`/`+` lines
                        // on one row, and sizing by raw line count left up to
                        // ~2× the diff height as blank bottom padding. Long
                        // content lines wrap at the body width (max_w - 2,
                        // the same area the diff paints into), so wrapped
                        // rows are counted too and none fold out of view.
                        let diff_rows = tool_render::edit_box_rows(t, max_w.saturating_sub(2));
                        // line_h = diff_rows + 2 (top padding + body +
                        // bottom padding; no title — the diff shows the path
                        // itself) + 2 for external margins (top + bottom)
                        diff_rows + 4
                    } else if tool_render::tool_display(&t.tool) == "write" {
                        // Same source as render_write (shared helper): the box
                        // previews `input.content` (capped at 20 rows), NOT the
                        // short JSON receipt in `output`. Using `output` here
                        // underestimated the box by up to ~19 rows, so the walk
                        // started the next part inside the box and the cached
                        // height clipped it. The count is WRAPPED rows at the
                        // code panel's wrap budget (write_box_rows), so long
                        // preview lines fold inside the box instead of
                        // overrunning its reserved height.
                        let display_lines = tool_render::write_box_rows(t, max_w).unwrap_or(0);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        display_lines + 5
                    } else if tool_render::tool_display(&t.tool) == "read" {
                        // Read block: preview collapsed to a few lines, full
                        // code when expanded (mirrors render_read). The count
                        // is WRAPPED rows at the code panel's wrap budget
                        // (code_panel_rows), so long file lines fold inside
                        // the box instead of overrunning its reserved height.
                        let id = t.tool_call_id.as_deref().unwrap_or("read");
                        let code = tool_render::read_block_text(t).unwrap_or_default();
                        let collapsed = crate::util::scroll::collapse_tool_output(&code, 10, 800);
                        let expanded = tool_state.is_expanded(id);
                        let display = if expanded || !collapsed.overflow {
                            &code
                        } else {
                            &collapsed.output
                        };
                        let rows = tool_render::code_panel_rows(display, max_w, usize::MAX).max(1)
                            + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        rows + 5
                    } else {
                        // bash (and write) block. When the tool is expanded, the full
                        // output is rendered, so the height must match render_shell.
                        let id = t.tool_call_id.as_deref().unwrap_or("shell");
                        let collapsed = crate::util::scroll::collapse_tool_output(output, 10, 800);
                        let display = if tool_state.is_expanded(id) {
                            output
                        } else {
                            &collapsed.output
                        };
                        // Wrapped rows: the renderer folds long lines at the
                        // box's content width (same helper as render_shell),
                        // so the estimate counts the visual rows the box
                        // paints, not raw source lines.
                        let rows = tool_render::tool_box_rows(
                            display.lines(),
                            None,
                            tool_render::tool_box_content_w(max_w),
                        );
                        let lines = rows.max(1) + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        lines + 5
                    }
                } else {
                    1
                }
                .saturating_add(notes_h)
            }
            Part::Reasoning(r) => {
                if r.text.is_empty() {
                    1
                } else {
                    let expanded = tool_state.is_expanded_or(
                        &r.text[..r.text.floor_char_boundary(32)],
                        config.thinking_mode,
                    );
                    if expanded {
                        // Header row + the wrap-aware markdown body height —
                        // the same estimate render_reasoning lays out with,
                        // so the cached height matches the drawn block even
                        // when long paragraphs wrap into extra rows.
                        let body_w = max_w.saturating_sub(2).max(1);
                        1u16.saturating_add(estimate_height(&sanitize_text(&r.text), body_w).max(1))
                    } else {
                        // Collapsed: just the "+ Thought" header row.
                        1
                    }
                }
            }
            Part::File(_) => 1,
            Part::Compaction(c) => {
                if !c.text.is_empty() {
                    // The "Summarizing" box: title row + wrapped body. Collapsed
                    // keeps a fixed preview height (scroll-up); expanded grows
                    // with the full streamed text. The width is the box's inner
                    // width, so wrapped rows match the render exactly.
                    let expanded = tool_state.is_expanded(&summarizing_id(c));
                    summarizing_height(c, expanded, max_w)
                } else {
                    1
                }
            }
            Part::Text(_) => 0,
        }
    }
}

impl SessionView {
    /// Rows the passive LSP findings occupy for a completed tool: the
    /// `+`/`- LSP` header, plus one clipped line per finding when expanded.
    /// Must stay in sync with the notes drawn at the end of the
    /// `Part::Tool` branch in `render_parts`.
    pub(super) fn lsp_notes_height(t: &ToolPart, tool_state: &ToolRenderState, default_expanded: bool) -> u16 {
        match (&t.lsp_notes, &t.status) {
            (Some(n), ToolStatus::Completed) => {
                let count = Self::lsp_notes_count(n);
                if count == 0 {
                    0
                } else if tool_state.is_expanded_or(&Self::lsp_notes_id(t), default_expanded) {
                    1 + count
                } else {
                    1
                }
            }
            _ => 0,
        }
    }
}

impl SessionView {
    /// Stable expansion key for a tool part's LSP block.
    pub(super) fn lsp_notes_id(t: &ToolPart) -> String {
        format!("lsp:{}", t.tool_call_id.as_deref().unwrap_or(&t.tool))
    }
}

impl SessionView {
    /// Total findings across severities.
    pub(super) fn lsp_notes_count(n: &cosh_tools::fs::LspNotes) -> u16 {
        (n.errors.len() + n.warnings.len()).min(u16::MAX as usize) as u16
    }
}
