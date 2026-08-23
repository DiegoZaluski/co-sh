use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

/// Number of trailing blocks kept "unstable" during incremental parsing.
///
/// The last blocks of a streaming message may still be incomplete (e.g. an
/// open fenced code block), so they are re-parsed on every update even when
/// their source prefix has not changed (`trailingUnstable = 2`).
pub(crate) const TRAILING_UNSTABLE_BLOCKS: usize = 2;

/// Semantic classification of a top-level block. Rendering re-parses the
/// block's raw slice generically, so this is only used for introspection and
/// future fast paths (e.g. per-kind caches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Paragraph,
    Heading,
    CodeBlock,
    Table,
    List,
    BlockQuote,
    Rule,
    Other,
}

/// A top-level markdown block, identified by its byte range in the full
/// source document.
#[derive(Debug, Clone)]
pub(crate) struct BlockInfo {
    pub range: Range<usize>,
    pub kind: BlockKind,
}

/// The result of parsing a markdown document into top-level blocks.
///
/// Blocks are the unit of reuse for both incremental parsing and
/// per-block render caching: when content is appended during
/// streaming, every block whose raw slice is unchanged keeps its cached
/// render; only the changed tail is re-parsed/re-rendered.
#[derive(Debug, Clone, Default)]
pub(crate) struct MdBlocks {
    pub content: String,
    pub blocks: Vec<BlockInfo>,
    /// Number of leading blocks reused from the previous parse (the stable
    /// prefix). Purely informational/observability.
    pub stable_count: usize,
}

impl MdBlocks {
    /// The raw markdown source of `block`.
    pub(crate) fn source(&self, block: &BlockInfo) -> &str {
        &self.content[block.range.clone()]
    }

    /// Number of line breaks separating the previous block's last VISIBLE
    /// character from the next block's start.
    ///
    /// Trailing newlines are stripped off the previous block first, so the
    /// result is invariant to how much trailing whitespace pulldown's ranges
    /// happen to swallow — which differs between a fresh parse and an
    /// incremental one whose stable blocks keep their original (shorter)
    /// ranges.
    pub(crate) fn inter_block_gap(&self, prev: &BlockInfo, cur: &BlockInfo) -> usize {
        let bytes = self.content.as_bytes();
        let mut p = prev.range.end.min(bytes.len());
        while p > 0 && matches!(bytes[p - 1], b'\n' | b'\r' | b' ' | b'\t') {
            p -= 1;
        }
        let end = cur.range.start.min(bytes.len()).max(p);
        bytes[p..end].iter().filter(|&&b| b == b'\n').count()
    }
}

fn parser_options() -> Options {
    // Must match the options used by the renderer / estimator exactly.
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options
}

impl BlockKind {
    /// "Separated" kinds always get a blank row to their neighbours; only
    /// paragraph-to-paragraph adjacency depends on the source's blank line.
    pub(crate) fn is_separated(self) -> bool {
        !matches!(self, Self::Paragraph | Self::Other)
    }
}

/// Whether a blank separator row belongs between two consecutive top-level
/// blocks. Separated kinds (headings, lists, code fences, tables, quotes,
/// rules) are always spaced from their neighbours; adjacent paragraphs are
/// separated only when the source had an explicit blank line between them.
///
/// Mirrors the reference renderer's top-level margin rule: spacing is a
/// property of the block PAIR, never of the cursor feed alone.
pub(crate) fn needs_inter_block_margin(
    prev: BlockKind,
    cur: BlockKind,
    gap_newlines: usize,
) -> bool {
    if prev.is_separated() || cur.is_separated() {
        return true;
    }
    gap_newlines > 1
}

fn classify(tag: &Tag<'_>) -> BlockKind {
    match tag {
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::CodeBlock(_) => BlockKind::CodeBlock,
        Tag::Table(_) => BlockKind::Table,
        Tag::List(_) => BlockKind::List,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        _ => BlockKind::Other,
    }
}

/// Split `content` into top-level blocks by tracking container depth over the
/// pulldown_cmark event stream (with byte offsets via `into_offset_iter`).
fn parse_all(content: &str) -> Vec<BlockInfo> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut open_start: Option<usize> = None;
    let mut open_kind = BlockKind::Other;

    for (event, range) in Parser::new_ext(content, parser_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 && open_start.is_none() {
                    open_start = Some(range.start);
                    open_kind = classify(&tag);
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(start) = open_start.take()
                {
                    blocks.push(BlockInfo {
                        range: start..range.end,
                        kind: open_kind,
                    });
                }
            }
            // Atomic events at container depth 0 form standalone blocks.
            // Everything else at depth 0 produces no visual output.
            Event::Rule if depth == 0 => {
                blocks.push(BlockInfo {
                    range: range.clone(),
                    kind: BlockKind::Rule,
                });
            }
            _ => {}
        }
    }

    blocks
}

/// Length of the shared byte prefix of `a` and `b`, aligned down to a char
/// boundary of `b`.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let max = a.len().min(b.len());
    let mut i = 0;
    while i < max && a.as_bytes()[i] == b.as_bytes()[i] {
        i += 1;
    }
    while i > 0 && !b.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Parse `new_content`, reusing leading blocks from `prev` when their raw
/// source is provably unchanged.
///
/// Strategy:
/// 1. Compute the common byte prefix between the old and new documents.
/// 2. Walk the previous blocks and keep those that lie entirely inside the
///    shared prefix — their bytes cannot have changed.
/// 3. Back off [`TRAILING_UNSTABLE_BLOCKS`] blocks so incomplete tails
///    (streaming) are always re-parsed.
/// 4. Re-parse only `&new[stable_end..]` and append its blocks.
pub(crate) fn parse_blocks_incremental(new_content: &str, prev: Option<&MdBlocks>) -> MdBlocks {
    let Some(prev) = prev.filter(|p| !p.blocks.is_empty()) else {
        return MdBlocks {
            content: new_content.to_string(),
            blocks: parse_all(new_content),
            stable_count: 0,
        };
    };

    if prev.content == new_content {
        return MdBlocks {
            content: new_content.to_string(),
            blocks: prev.blocks.clone(),
            stable_count: prev.blocks.len(),
        };
    }

    let prefix = common_prefix_len(&prev.content, new_content);

    // Count how many previous blocks lie fully inside the shared prefix.
    let mut usable: usize = 0;
    for block in &prev.blocks {
        if block.range.end <= prefix {
            usable += 1;
        } else {
            break;
        }
    }

    let stable_count = usable.saturating_sub(TRAILING_UNSTABLE_BLOCKS);
    let (head, tail_start) = if stable_count > 0 {
        (
            prev.blocks[..stable_count].to_vec(),
            prev.blocks[stable_count].range.start,
        )
    } else {
        (Vec::new(), 0)
    };

    let mut blocks = head;
    let tail = &new_content[tail_start..];
    for block in parse_all(tail) {
        blocks.push(BlockInfo {
            range: (block.range.start + tail_start)..(block.range.end + tail_start),
            kind: block.kind,
        });
    }

    MdBlocks {
        content: new_content.to_string(),
        blocks,
        stable_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(md: &MdBlocks) -> Vec<BlockKind> {
        md.blocks.iter().map(|b| b.kind).collect()
    }

    #[test]
    fn splits_top_level_blocks() {
        let text = "# Title\n\nSome *para*.\n\n```rust\nfn x() {}\n```\n\n- a\n- b\n\n---\n";
        let md = parse_blocks_incremental(text, None);
        assert_eq!(
            kinds(&md),
            vec![
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::CodeBlock,
                BlockKind::List,
                BlockKind::Rule,
            ]
        );
        // Ranges must slice back to non-empty sources.
        for b in &md.blocks {
            assert!(!md.source(b).is_empty());
        }
        // Fenced code block ranges cover the fences but not the trailing
        // blank line after them.
        assert_eq!(md.source(&md.blocks[2]), "```rust\nfn x() {}\n```");
    }

    #[test]
    fn table_is_one_block() {
        let text = "| A | B |\n|---|---|\n| 1 | 2 |\n\nafter\n";
        let md = parse_blocks_incremental(text, None);
        assert_eq!(kinds(&md), vec![BlockKind::Table, BlockKind::Paragraph]);
    }

    #[test]
    fn lazy_continuation_stays_inside_list_block() {
        // A lazy paragraph continuation belongs to the list item, not to a
        // new top-level paragraph.
        let text = "- item\ncontinued\n";
        let md = parse_blocks_incremental(text, None);
        assert_eq!(kinds(&md), vec![BlockKind::List]);
    }

    #[test]
    fn incremental_append_reuses_prefix() {
        let base = "# H\n\npara one\n\n- l1\n- l2\n\ntail";
        let first = parse_blocks_incremental(base, None);

        let extended = "# H\n\npara one\n\n- l1\n- l2\n\ntail grows longer";
        let second = parse_blocks_incremental(extended, Some(&first));

        assert_eq!(second.content, extended);
        // 4 usable blocks minus TRAILING_UNSTABLE_BLOCKS (the "tail" paragraph
        // is deliberately re-parsed while streaming).
        assert_eq!(second.stable_count, 2);
        // Stable blocks must cover identical ranges/content.
        for (a, b) in first.blocks[..second.stable_count]
            .iter()
            .zip(&second.blocks)
        {
            assert_eq!(a.range, b.range);
            assert_eq!(a.kind, b.kind);
        }
        // Full structure equals a from-scratch parse.
        let fresh = parse_blocks_incremental(extended, None);
        assert_eq!(second.blocks.len(), fresh.blocks.len());
        assert_eq!(kinds(&second), kinds(&fresh));
    }

    #[test]
    fn incremental_change_reparses_from_edit_point() {
        let base = "aaa bbb ccc\n\nddd eee fff\n\nggg hhh iii";
        let first = parse_blocks_incremental(base, None);
        assert_eq!(first.blocks.len(), 3);

        // Edit only the last paragraph.
        let edited = "aaa bbb ccc\n\nddd eee fff\n\nGGG hhh iii";
        let second = parse_blocks_incremental(edited, Some(&first));
        assert_eq!(second.blocks.len(), 3);
        // The edit is inside the trailing-unstable window, so nothing stays
        // stable — but the re-parse must still produce identical structure
        // with the edit applied.
        assert_eq!(second.source(&second.blocks[0]), "aaa bbb ccc\n");
        assert_eq!(second.source(&second.blocks[1]), "ddd eee fff\n");
        assert!(second.source(second.blocks.last().unwrap()).contains("GGG"));
    }

    #[test]
    fn incremental_shrink_and_replace() {
        let long = "one\n\ntwo\n\nthree\n\nfour";
        let first = parse_blocks_incremental(long, None);
        let short = "one\n\nTWO";
        let second = parse_blocks_incremental(short, Some(&first));
        assert_eq!(second.blocks.len(), 2);
        assert!(second.source(second.blocks.last().unwrap()).contains("TWO"));

        // Completely different content falls back to a full parse.
        let other = "> quote";
        let third = parse_blocks_incremental(other, Some(&second));
        assert_eq!(third.stable_count, 0);
        assert_eq!(kinds(&third), vec![BlockKind::BlockQuote]);
    }

    #[test]
    fn identical_content_short_circuits() {
        let text = "hello world";
        let first = parse_blocks_incremental(text, None);
        let second = parse_blocks_incremental(text, Some(&first));
        assert_eq!(second.stable_count, 1);
        assert_eq!(second.blocks.len(), 1);
    }

    #[test]
    fn multibyte_content_does_not_split_chars() {
        let base = "héllo wörld ✓";
        let first = parse_blocks_incremental(base, None);
        // Append a multibyte char: common prefix ends mid-char-safe point.
        let extended = "héllo wörld ✓✓";
        let second = parse_blocks_incremental(extended, Some(&first));
        assert!(!second.blocks.is_empty());
        assert_eq!(second.source(second.blocks.last().unwrap()), extended);
    }
}
