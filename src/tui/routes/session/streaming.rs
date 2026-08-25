//! Incremental render cache for the streaming (still-growing) last assistant
//! message.
//!
//! While tokens append to the last `Text` part, the previous implementation
//! re-parsed and re-rendered the whole growing message on every frame: a full
//! pulldown parse for the height estimate, a full markdown render for the
//! cells, and full-content hashes for cache invalidation — O(n) per frame
//! (so O(n²) over a long stream), which made the TUI grind to a halt while
//! streaming and stayed slow after the stream ended because the last-message
//! hash kept running on every frame.
//!
//! This module keeps a persistent render of the streaming text part and only
//! re-renders the trailing open block each frame — the current paragraph line,
//! an open code fence, a list, or a blockquote — from a stable split point
//! (see [`tail_scan`]); the prefix before it is rendered once and reused
//! verbatim. Per-frame cost is therefore O(delta) for the common shapes:
//! paragraphs (split at the last line) and multi-block tails (closed blocks
//! are reused through the renderer's per-block cache). A SINGLE open block
//! that grows without bound (one huge fence/list) is still re-rendered whole
//! per frame — linear, but bounded by that block and unchanged for
//! realistic snippet sizes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::lib::rgba::ColorInput;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::{
    MarkdownRenderable, boundary_blank_rows, estimate_height, estimate_height_interior_slice,
};

use crate::theme::Theme;

use super::{conceal_text, rgba_color, sanitize_text};

/// Kind of the trailing open block the split point re-renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TailBlock {
    /// Plain paragraph: split at the start of the last line.
    Paragraph,
    /// Open code fence: split at the fence opener.
    Fence,
    /// Markdown list: split at the first item of the list.
    List,
    /// Blockquote: split at the first line of the quote.
    Blockquote,
    /// Table region: full re-render (tables are buffered until they close, so
    /// the split cannot be incremental without re-parsing the whole thing).
    Table,
}

/// Incremental render cache for the streaming text part of the last message.
pub(crate) struct StreamingTextCache {
    pub(crate) message_id: String,
    pub(crate) part_index: usize,
    /// Content width the text is rendered at (max_w).
    pub(crate) width: u16,
    pub(crate) config_token: u64,
    /// The rendered (sanitized/concealed) text held by the cache.
    rendered_text: String,
    /// Raw (pre-sanitize) text length consumed so far.
    raw_len: usize,
    /// Byte offset of the trailing open block inside `rendered_text`.
    split: usize,
    /// Rows occupied by the stable prefix `rendered_text[..split]`.
    prefix_h: u16,
    /// Rows occupied by the re-rendered tail `rendered_text[split..]`.
    tail_h: u16,
    /// Kind of the trailing open block.
    block: TailBlock,
    /// Rendered cells of the whole text part at `width` columns.
    cells: Buffer,
    /// Total height of the text part (`prefix_h + connector + tail_h`).
    pub(crate) height: u16,
    /// Persistent tail renderer.
    ///
    /// A fresh `MarkdownRenderable` per frame would throw away its
    /// incremental parser state and per-block render cache every frame, so
    /// an open list/fence/quote tail (one huge growing top-level block, or
    /// many stabilized blocks) was fully re-parsed and re-rendered each
    /// frame — O(tail) per frame, O(n²) over a long stream. Persisting the
    /// instance keeps closed blocks' rendered rows cached across frames;
    /// only the unstable trailing blocks re-render per frame (O(delta)).
    tail_md: MarkdownRenderable,
}

impl StreamingTextCache {
    pub(crate) fn new(
        message_id: String,
        part_index: usize,
        width: u16,
        config_token: u64,
        theme: &Theme,
    ) -> Self {
        let mut cache = Self {
            message_id,
            part_index,
            width,
            config_token,
            rendered_text: String::new(),
            raw_len: 0,
            split: 0,
            prefix_h: 0,
            tail_h: 0,
            block: TailBlock::Paragraph,
            cells: Buffer::empty(Rect::new(0, 0, width.max(1), 1)),
            height: 0,
            tail_md: {
                let mut md = MarkdownRenderable::new(None);
                md.set_fg(Some(ColorInput::RGBA(theme.text)));
                md.set_bg(Some(ColorInput::RGBA(theme.background)));
                crate::util::markdown::apply_theme(&mut md, theme);
                md
            },
        };
        cache.update("", width, config_token, false);
        cache
    }

    /// Append streamed text (or resize/config changes) and re-render only the
    /// changed tail. Returns the row range `(row, end)` of the text part that
    /// was (re)rendered this frame, so the caller can patch the surrounding
    /// message cache.
    ///
    /// The theme is applied once at construction; a theme change carries a new
    /// `config_token` (it includes `theme_gen`), which makes the caller drop
    /// this cache and build a fresh one.
    pub(crate) fn update(
        &mut self,
        raw_text: &str,
        width: u16,
        config_token: u64,
        conceal: bool,
    ) -> (u16, u16) {
        // Sanitize only the appended delta: `sanitize`/`conceal` are per-char,
        // so `f(prefix) + f(delta) == f(whole)`.
        let delta = &raw_text[self.raw_len.min(raw_text.len())..];
        if !delta.is_empty() {
            let d = if conceal {
                conceal_text(delta)
            } else {
                sanitize_text(delta)
            };
            self.rendered_text.push_str(&d);
            self.raw_len = raw_text.len();
        }

        // Width/config/theme changes invalidate everything (re-render all).
        if self.width != width || self.config_token != config_token {
            self.width = width;
            self.config_token = config_token;
            self.split = 0;
            self.prefix_h = 0;
            self.block = TailBlock::Paragraph;
        }

        let text = &self.rendered_text;
        let prev_split = self.split;
        let prev_prefix_h = self.prefix_h;

        let (block, split) = tail_scan(text, prev_split, self.block);

        // Tables are buffered by the renderer until they close, so the split
        // cannot be incremental: fall back to a full re-render while a table
        // region is streaming.
        let (block, split) = if block == TailBlock::Table {
            (TailBlock::Paragraph, 0usize)
        } else {
            (block, split)
        };

        let new_prefix_h = if split == 0 {
            0
        } else if split >= prev_split {
            if split > prev_split {
                // Slice heights are additive EXCEPT for each internal slice
                // boundary's inter-block separator: fold in both the boundary
                // consumed by this advance (at prev_split) and its estimate.
                // The stabilized segment keeps its trailing feed row — it is
                // an INTERIOR slice; what follows positions itself after it.
                prev_prefix_h
                    .saturating_add(boundary_blank_rows(text, prev_split))
                    .saturating_add(estimate_height_interior_slice(
                        &text[prev_split..split],
                        width,
                    ))
            } else {
                prev_prefix_h
            }
        } else {
            // Full recompute of a non-final prefix: keep its trailing feed.
            estimate_height_interior_slice(&text[..split], width)
        };

        let tail = &text[split..];
        let tail_h = if tail.is_empty() {
            0
        } else {
            estimate_height(tail, width).max(1)
        };

        // Inter-block separator owed at a slice boundary: slice heights are
        // additive EXCEPT for the margin between the two sides' boundary
        // blocks, which only exists in the combined document. The re-rendered
        // region therefore starts one row below the prefix when the pair of
        // blocks straddling `render_from` triggers a separator.
        //
        // Invariant: `prefix_h` equals the standalone estimate of
        // `rendered_text[..split]`, and each advance folds in BOTH the
        // consumed boundary's separator and the stabilized segment's estimate.
        let render_row = if split >= prev_split {
            // The slice starts at the OLD boundary: place it after that
            // boundary's separator (the newly stabilized segment sits between
            // the old prefix start and the current one).
            prev_prefix_h.saturating_add(boundary_blank_rows(text, prev_split))
        } else {
            new_prefix_h
        };
        let connector = if split == 0 {
            0
        } else {
            boundary_blank_rows(text, split)
        };

        let height = new_prefix_h
            .saturating_add(connector)
            .saturating_add(tail_h)
            .max(1);

        // ── Render ──
        // Render from the earlier of the previous/current split so a block
        // that just closed (fence/list ending in the tail) is refreshed in
        // place. The renderer instance PERSISTS across frames, so its
        // per-block render cache survives: closed blocks of the slice are
        // reused verbatim and only the unstable trailing blocks re-render,
        // instead of re-rendering the whole slice from a cold cache like a
        // fresh-per-frame instance did.
        //
        // The theme setters were applied once at construction — a theme or
        // width change replaces this whole cache via the caller's identity
        // check (config_token includes theme_gen), and cached block entries
        // additionally validate width + style fingerprint on every hit.
        let render_from = if split >= prev_split {
            prev_split
        } else {
            split
        };
        let area_h = height.saturating_sub(render_row);

        let new_area = Rect::new(0, 0, width, height);
        if self.cells.area() != &new_area {
            self.cells.resize(new_area);
        }
        if area_h > 0 {
            // `render_self` fills the whole area with the background first,
            // so no separate clearing is needed.
            let area = Rect::new(0, render_row, width, area_h);
            self.tail_md.set_content(text[render_from..].to_string());
            self.tail_md.render_self(&mut self.cells, area);
        }
        self.split = split;
        self.prefix_h = new_prefix_h;
        self.tail_h = tail_h;
        self.block = block;
        self.height = height;

        (render_row, height)
    }

    /// Content cells of the text part (width × height, row-major).
    pub(crate) fn content(&self) -> &[ratatui::buffer::Cell] {
        self.cells.content()
    }

    pub(crate) fn split(&self) -> usize {
        self.split
    }

    /// Raw (pre-sanitize) bytes consumed so far — a cheap O(1) monotonic
    /// token that changes exactly when streamed content grows.
    pub(crate) fn raw_len(&self) -> usize {
        self.raw_len
    }
}

/// Split `text` at the start of the trailing open block. Returns the block
/// kind and the byte offset the re-rendered tail starts at.
///
/// The split point is chosen so the prefix before it is *stable*: rendering
/// the prefix once and re-rendering `text[split..]` each frame produces the
/// same cells (and the same total height) as rendering the whole text. Lines
/// are scanned (no pulldown parse) from the previous split, carrying the
/// previous block state so a split inside an open fence/list/quote survives.
fn tail_scan(text: &str, prev_split: usize, prev_block: TailBlock) -> (TailBlock, usize) {
    let mut fence_open = prev_block == TailBlock::Fence;
    let mut fence_start = if fence_open { prev_split } else { usize::MAX };
    let mut in_list = prev_block == TailBlock::List;
    let mut list_start = if in_list { prev_split } else { usize::MAX };
    let mut in_quote = prev_block == TailBlock::Blockquote;
    let mut quote_start = if in_quote { prev_split } else { usize::MAX };
    let mut in_table = false;
    let mut list_blank = false;
    let mut quote_blank = false;
    let mut para_start = prev_split;

    // When carrying an open fence, start scanning after its opener line so the
    // opener is not mistaken for a closer.
    let scan = if fence_open {
        match text[prev_split..].find('\n') {
            Some(nl) => prev_split + nl + 1,
            None => text.len(),
        }
    } else {
        prev_split
    };

    let mut line = scan;
    loop {
        let line_end = match text[line..].find('\n') {
            Some(nl) => line + nl,
            None => text.len(),
        };
        let raw = &text[line..line_end];
        let trimmed = raw.trim();
        let blank = trimmed.is_empty();
        let indented = !raw.is_empty() && (raw.starts_with(' ') || raw.starts_with('\t'));

        if blank {
            if in_list {
                list_blank = true;
            }
            if in_quote {
                quote_blank = true;
            }
        } else {
            let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
            let is_marker = is_list_marker(trimmed);
            let is_quote_line = trimmed.starts_with('>');
            let is_table_line = raw.starts_with('|');

            if fence_open {
                if is_fence {
                    fence_open = false;
                }
            } else if is_fence {
                fence_open = true;
                fence_start = line;
            } else if in_table {
                if !is_table_line {
                    in_table = false;
                }
            } else if is_table_line {
                in_table = true;
            } else if is_marker {
                if !in_list {
                    in_list = true;
                    list_start = line;
                }
                list_blank = false;
            } else if in_list {
                if list_blank && !indented {
                    // Blank line followed by a non-indented paragraph: the
                    // list ended.
                    in_list = false;
                    list_blank = false;
                    para_start = line;
                } else {
                    // Continuation of the current item.
                    list_blank = false;
                }
            } else if is_quote_line {
                if !in_quote {
                    in_quote = true;
                    quote_start = line;
                }
                quote_blank = false;
            } else if in_quote {
                if quote_blank {
                    in_quote = false;
                    quote_blank = false;
                    para_start = line;
                } else {
                    quote_blank = false;
                }
            } else {
                para_start = line;
            }
        }

        if line_end == text.len() {
            break;
        }
        line = line_end + 1;
    }

    if in_table {
        return (TailBlock::Table, 0);
    }

    // Re-render from the earliest open block so nested structures (a list
    // inside a quote, a fence inside a list item, ...) render with full
    // context.
    let mut block = TailBlock::Paragraph;
    let mut split = para_start;
    if fence_open {
        block = TailBlock::Fence;
        split = fence_start;
    }
    if in_list && list_start < split {
        block = TailBlock::List;
        split = list_start;
    }
    if in_quote && quote_start < split {
        block = TailBlock::Blockquote;
        split = quote_start;
    }
    (block, split)
}

/// True when the trimmed line looks like a CommonMark list item marker.
fn is_list_marker(trimmed: &str) -> bool {
    let b = trimmed.as_bytes();
    match b.first().copied() {
        Some(b'-') | Some(b'*') | Some(b'+') => b.get(1).is_some_and(|c| *c == b' ' || *c == b'\t'),
        Some(c) if c.is_ascii_digit() => {
            let mut i = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if b.get(i).is_some_and(|c| *c == b'.' || *c == b')') {
                i += 1;
                i == b.len() || b.get(i).is_some_and(|c| *c == b' ' || *c == b'\t')
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Cheap style for background rows (used when resizing the message cache).
pub(crate) fn bg_style(theme: &Theme) -> Style {
    Style::default().bg(rgba_color(theme.background))
}

/// Lightweight snapshot of the streaming message's per-part heights, handed to
/// the height-cache update so it can skip the O(n) estimate/hash per frame.
#[derive(Debug, Clone)]
pub(crate) struct StreamState {
    pub(crate) message_id: String,
    pub(crate) part_hs: Vec<u16>,
    pub(crate) height: u16,
    /// Monotonic raw-byte count of the streamed text — a cheap O(1) token
    /// that changes exactly when streamed content grows.
    pub(crate) raw_len: usize,
}

/// Persistent full render of the streaming last message: stable non-text parts
/// rendered once, the streaming text part patched incrementally each frame.
pub(crate) struct StreamingMessageCache {
    pub(crate) message_id: String,
    /// Full message width (inner_area.width) the cells are laid out at.
    pub(crate) width: u16,
    pub(crate) max_w: u16,
    pub(crate) config_token: u64,
    /// Tool-state expansion version the cells were rendered with.
    pub(crate) tool_state_version: u64,
    /// Per-part heights (mirrors `part_heights_cache[idx]`).
    pub(crate) part_hs: Vec<u16>,
    /// Full-width (w × h) cells of the message (border + margins + parts).
    pub(crate) cells: Vec<ratatui::buffer::Cell>,
    /// Total message height.
    pub(crate) height: u16,
    /// Incremental text-part cache (the message's last part is a Text part).
    pub(crate) text: Option<StreamingTextCache>,
    /// Rows `(start, end)` of the text part re-rendered by the last update;
    /// the caller uses this to keep text regions fresh (O(tail)).
    pub(crate) last_tail: Option<(u16, u16)>,
}
