use super::*;

impl SessionView {
    /// Locate the message index under content row `click_y` using the
    /// prefix-y binary search. Shared by the click handler and the hover
    /// tracker.
    pub(super) fn message_index_at_y(&self, vp_top: i32, click_y: i32, len: usize) -> Option<usize> {
        let mut low = 0;
        let mut high = len;
        while low < high {
            let mid = low + (high - low) / 2;
            let msg_top = vp_top - self.scroll_y + self.prefix_y[mid];
            let msg_bottom = msg_top + self.msg_height_cache[mid];
            if click_y < msg_top {
                high = mid;
            } else if click_y >= msg_bottom {
                low = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }
}

impl SessionView {
    /// Track the hovered user message on mouse-move events (opencode-style).
    /// Returns `true` when the hover target changed so the caller redraws.
    pub fn update_hover(
        &mut self,
        x: u16,
        y: u16,
        area: Rect,
        state: &AppState,
        config: &TuiConfig,
    ) -> bool {
        let Some(session) = state.current_session() else {
            return self.clear_hover();
        };
        // Hover stays active while the agent works: the highlight is purely
        // visual and Copy remains available during streaming.
        if self.drag_selection.is_some() {
            return self.clear_hover();
        }

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );
        // The renderer leaves a two-column margin around the session boxes.
        // Hovering that margin must not make a user message look actionable.
        if x < inner_area.x || x >= inner_area.right() {
            return self.clear_hover();
        }
        let max_w = inner_area.width.saturating_sub(6).max(2);
        // PaintedOnly: hover maps the cursor against the layout the last
        // render painted — never mutates the shared caches between frames.
        self.ensure_height_caches_fresh(
            session,
            max_w,
            config,
            HeightCacheMode::PaintedOnly,
            state.status == SessionStatus::Working,
        );

        let next = self
            .message_index_at_y(
                i32::from(inner_area.y),
                i32::from(y),
                session.messages.len(),
            )
            .filter(|&idx| session.messages[idx].role == MessageRole::User);
        if next != self.hovered_msg_idx {
            self.hovered_msg_idx = next;
            return true;
        }
        false
    }
}

impl SessionView {
    /// Clear the hovered user-message highlight (the opencode-style row tint).
    /// `true` when the hover target was set and is now cleared.
    pub fn clear_hover(&mut self) -> bool {
        if self.hovered_msg_idx.is_some() {
            self.hovered_msg_idx = None;
            return true;
        }
        false
    }
}

impl SessionView {
    /// Recolor the hovered user message's cells with the element background —
    /// the same visual swap opencode applies via
    /// `backgroundColor={hover() ? theme.backgroundElement : ...}`.
    pub(super) fn render_hover_highlight(
        &self,
        buf: &mut Buffer,
        inner_area: Rect,
        session: &crate::types::Session,
        theme: &Theme,
    ) {
        let Some(idx) = self.hovered_msg_idx else {
            return;
        };
        if idx >= self.prefix_y.len().saturating_sub(1) || idx >= self.msg_height_cache.len() {
            return;
        }
        if session
            .messages
            .get(idx)
            .is_none_or(|m| m.role != MessageRole::User)
        {
            return;
        }
        let vp_top = i32::from(inner_area.y);
        let vp_bottom_excl = i32::from(inner_area.y + inner_area.height);
        let msg_top = (vp_top - self.scroll_y + self.prefix_y[idx]).max(vp_top);
        let msg_bottom = (vp_top - self.scroll_y + self.prefix_y[idx] + self.msg_height_cache[idx])
            .min(vp_bottom_excl);
        for cy in msg_top..msg_bottom {
            // Light the BAND only, not the rail columns: the rails stand on
            // the transcript background and the band is one column inside
            // each rail — lighting the rails too made the box read a
            // couple of columns wider under the cursor.
            for cx in inner_area.x + 1..inner_area.x + inner_area.width - 1 {
                if let Some(cell) = buf.cell_mut((cx, u16::try_from(cy).unwrap_or(0))) {
                    cell.set_bg(rgba_color(theme.background_element));
                }
            }
        }
    }
}

impl SessionView {
    #[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        state: &AppState,
        config: &TuiConfig,
    ) -> bool {
        let Some(session) = state.current_session() else {
            return false;
        };

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );
        let max_w = inner_area.width.saturating_sub(6).max(2);
        let x_off = i32::from(inner_area.x + 3);

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());
        let click_x = i32::from(mouse.x);
        let click_y = i32::from(mouse.y);

        // Message hit-testing below is based on vertical layout. Keep it
        // bounded by the same horizontal area that render() uses for the
        // message boxes; otherwise a click in the side margin on the same row
        // is incorrectly treated as a click on the user message.
        if click_x < i32::from(inner_area.x) || click_x >= i32::from(inner_area.right()) {
            return false;
        }

        // PaintedOnly: the click maps against the layout the last render
        // painted — never mutates the shared caches between frames.
        self.ensure_height_caches_fresh(
            session,
            max_w,
            config,
            HeightCacheMode::PaintedOnly,
            state.status == SessionStatus::Working,
        );

        // Binary search for the message that contains the click position,
        // using the prefix-y array directly (no O(n) per-click allocation of
        // a position vector). Message `i` starts at
        // `vp_top - scroll_y + prefix_y[i]` in screen space — the same layout
        // the render walk produces.
        let mut low = 0;
        let mut high = session.messages.len();
        let mut target_msg_idx = None;
        while low < high {
            let mid = low + (high - low) / 2;
            let msg_top = vp_top - self.scroll_y + self.prefix_y[mid];
            let msg_h = self.msg_height_cache[mid];
            let msg_bottom = msg_top + msg_h;

            if click_y < msg_top {
                high = mid;
            } else if click_y >= msg_bottom {
                low = mid + 1;
            } else {
                target_msg_idx = Some(mid);
                break;
            }
        }

        // 3. Confirm if message is visible in viewport
        if let Some(idx) = target_msg_idx {
            let msg = &session.messages[idx];
            let msg_h = self.msg_height_cache[idx];
            let msg_top = vp_top - self.scroll_y + self.prefix_y[idx];
            let msg_bottom = msg_top + msg_h;

            // Check if click is within this message and message is visible
            if click_y >= msg_top
                && click_y < msg_bottom
                && msg_bottom > vp_top
                && msg_top < vp_bottom
            {
                // Click on a user message → request the Message Actions
                // dialog (app.rs opens it). Assistant messages keep the
                // per-part click handling below.
                if msg.role == MessageRole::User {
                    self.pending_message_action = Some(msg.id.clone());
                    return true;
                }
                let border_offset: i32 = match msg.role {
                    MessageRole::User => 1,
                    MessageRole::Assistant => 0,
                };
                let mut part_y = msg_top + border_offset;

                for (pi, part) in msg.parts.iter().enumerate() {
                    let part_h = i32::from(
                        self.part_heights_cache[idx]
                            .get(pi)
                            .copied()
                            .unwrap_or(1)
                            .max(1),
                    );

                    if click_y >= part_y && click_y < part_y + part_h {
                        // Passive LSP findings: the last rows of the part
                        // belong to the `+`/`- LSP` block — any click there
                        // toggles it, ahead of the tool block's own toggle.
                        // Mirrors the renderer's visibility gates so hidden
                        // parts never claim clicks for absent rows.
                        if let crate::types::Part::Tool(tool) = part
                            && tool.lsp_notes.is_some()
                            && matches!(tool.status, ToolStatus::Completed)
                            && config.show_tool_details
                            && (config.show_generic_tool_output
                                || tool_render::tool_display(&tool.tool) != "generic")
                        {
                            let notes_rows = i32::from(Self::lsp_notes_height(
                                tool,
                                &self.tool_state,
                                config.diagnostics_mode,
                            ));
                            if notes_rows > 0 && click_y >= part_y + part_h - notes_rows {
                                let id = Self::lsp_notes_id(tool);
                                self.tool_state
                                    .toggle_with_default(&id, config.diagnostics_mode);
                                return true;
                            }
                        }

                        if let crate::types::Part::Tool(tool) = part {
                            let display = tool_render::tool_display(&tool.tool);
                            if display == "bash" || display == "glob" || display == "read" {
                                let output =
                                    tool.output.as_deref().unwrap_or("").trim().to_string();
                                if !output.is_empty() {
                                    let id =
                                        tool.tool_call_id.as_deref().unwrap_or(match display {
                                            "glob" => "glob",
                                            "read" => "read",
                                            _ => "shell",
                                        });
                                    let text = if display == "glob" {
                                        tool_render::glob_block_text(tool).unwrap_or_default()
                                    } else if display == "read" {
                                        tool_render::read_block_text(tool).unwrap_or_default()
                                    } else {
                                        output
                                    };
                                    let collapsed =
                                        crate::util::scroll::collapse_tool_output(&text, 10, 800);
                                    if collapsed.overflow {
                                        // Toggle on any click inside the block. The part's
                                        // row range already matches the rendered block because the
                                        // height caches are rebuilt with the expansion state.
                                        self.tool_state.toggle_expanded(id);
                                        return true;
                                    }
                                }
                            }
                        }

                        if let crate::types::Part::Reasoning(r) = part {
                            let part_id = &r.text[..r.text.floor_char_boundary(32)];
                            if click_y == part_y {
                                let header_x_end = x_off + 8;
                                if click_x >= x_off && click_x < header_x_end {
                                    // Flip relative to the effective state so
                                    // the first click on a block expanded via
                                    // the global thinking mode collapses it.
                                    self.tool_state
                                        .toggle_with_default(part_id, config.thinking_mode);
                                    return true;
                                }
                            }
                        }

                        if let crate::types::Part::Compaction(c) = part
                            && !c.text.is_empty()
                        {
                            // Toggle the "Summarizing" box on any click
                            // inside it (the part's row range matches the
                            // rendered box — height caches are rebuilt with
                            // the expansion state).
                            let id = summarizing_id(c);
                            self.tool_state.toggle_expanded(&id);
                            return true;
                        }

                        return true;
                    }

                    part_y += part_h;
                }
            }
        }

        false
    }
}
