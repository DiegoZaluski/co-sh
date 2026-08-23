use super::md::{estimate_height_ext, estimate_height_impl};
use super::parser::{BlockKind, needs_inter_block_margin};

/// Estimate the number of terminal rows required to render `text` as markdown
/// within a viewport `max_w` columns wide.
///
/// Callers (e.g. `SessionView`) use this to pre-allocate the correct area
/// height before rendering.
///
/// The estimate is produced by measuring each top-level markdown block with
/// the renderer's own block pipeline ([`super::md`]) — see
/// [`estimate_height_impl`] — so it is exact by construction: any change to
/// the renderer's wrapping or spacing automatically applies to this estimate.
/// Results are memoized in an LRU keyed by (content, width).
#[must_use]
pub fn estimate_height(text: &str, max_w: u16) -> u16 {
    estimate_height_impl(text, max_w)
}

/// Like [`estimate_height`], but the slice's LAST block keeps its trailing
/// feed row. Use for INTERIOR slices of a larger document (e.g. stabilized
/// streaming segments): the blocks that follow position themselves after
/// that row, so dropping it would lose one row of separation per segment.
#[must_use]
pub fn estimate_height_interior_slice(text: &str, max_w: u16) -> u16 {
    estimate_height_ext(text, max_w, true)
}

/// Blank separator rows owed at a SLICE boundary of one streamed document:
/// the inter-block margin between the block ending just before `boundary`
/// and whatever block follows it. Mirrors the renderer's top-level rule —
/// separated kinds (headings, lists, fences, tables, quotes, rules) always
/// space from their neighbours; adjacent paragraphs only after an explicit
/// blank line. The result is 0 or 1.
///
/// Block kinds are derived from each side's first visible glyph, which is
/// exact for every kind that can start (or interrupt) a top-level block;
/// misreading an exotic case degrades to the conservative margin because any
/// gap of two or more newlines forces a separator regardless of kind.
#[must_use]
pub fn boundary_blank_rows(doc: &str, boundary: usize) -> u16 {
    let bytes = doc.as_bytes();
    let at = boundary.min(bytes.len());

    // Previous block: walk back over trailing whitespace/newlines, then find
    // the start of its last visible line.
    let mut p = at;
    while p > 0 && matches!(bytes[p - 1], b'\n' | b'\r' | b' ' | b'\t') {
        p -= 1;
    }
    if p == 0 {
        // Boundary sits before any content: the tail is the document head,
        // and no separator precedes the first block.
        return 0;
    }
    let mut line_start = p;
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let prev_kind = block_kind_from_head(&doc[line_start..p]);

    let cur_kind = block_kind_from_head(doc[at..].trim_start_matches([' ', '\t']));

    let gap = bytes[p..at].iter().filter(|&&b| b == b'\n').count();
    // An ORDERED list interrupts a paragraph only when its first marker
    // number is 1; otherwise pulldown merges the lines into the paragraph.
    // Bullets (-/*/+) always interrupt. CommonMark caps ordered markers at
    // 9 digits: longer runs never start a list, so they demote too (and the
    // fold below cannot overflow).
    let cur_kind = if gap < 2 && cur_kind == BlockKind::List {
        let head = doc[at..].trim_start();
        if !head.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            cur_kind
        } else {
            let digit_bytes =
                &head.as_bytes()[..head.bytes().take_while(u8::is_ascii_digit).count()];
            let starts_list = digit_bytes.len() <= 9
                && digit_bytes.iter().fold(0u32, |n, d| {
                    n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
                }) == 1;
            if starts_list {
                cur_kind
            } else {
                BlockKind::Paragraph
            }
        }
    } else {
        cur_kind
    };

    u16::from(needs_inter_block_margin(prev_kind, cur_kind, gap))
}

/// Classify a top-level block from its first glyph. Exact for every marker
/// that can start a top-level block; anything else falls back to Paragraph,
/// whose only spacing rule (explicit blank line) is decided by the gap.
fn block_kind_from_head(head: &str) -> BlockKind {
    let t = head.trim_start();
    let b = t.as_bytes();
    match b.first() {
        // ATX heading: '#' run followed by space/EOL.
        Some(b'#') => {
            let hashes = b.iter().take_while(|&&c| c == b'#').count();
            if hashes <= 6 && b.get(hashes).is_none_or(|&c| c == b' ' || c == b'\t') {
                BlockKind::Heading
            } else {
                BlockKind::Paragraph
            }
        }
        Some(b'>') if b.len() == 1 || b[1] == b' ' || b[1] == b'\t' => BlockKind::BlockQuote,
        // Fenced code needs a run of >=3 (matches tail_scan's fence check):
        // a line starting with inline code "`foo`" or "~~strike~~" is a
        // Paragraph continuation and must not force a separator.
        Some(b'`') | Some(b'~') => {
            let run = b.iter().take_while(|&&c| c == b[0]).count();
            if run >= 3 {
                BlockKind::CodeBlock
            } else {
                BlockKind::Paragraph
            }
        }
        Some(c @ (b'-' | b'*' | b'+')) => {
            let rest = &t[1..];
            if rest.starts_with(' ') || rest.starts_with('\t') || rest.is_empty() {
                BlockKind::List
            } else if (*c == b'-' || *c == b'*')
                && b.iter().take(3).all(|&x| x == *c)
                && b.len() >= 2
            {
                // "---" thematic break / "--" setext-H2 underline.
                BlockKind::Rule
            } else {
                BlockKind::Paragraph
            }
        }
        Some(b'=') => BlockKind::Heading, // setext underline
        Some(c) if c.is_ascii_digit() => {
            // Ordered marker: digits, then '.' or ')', then space/tab/EOL.
            let digits = b.iter().take_while(|&&x| x.is_ascii_digit()).count();
            match b.get(digits) {
                Some(b'.') | Some(b')') => match b.get(digits + 1) {
                    None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => BlockKind::List,
                    _ => BlockKind::Paragraph,
                },
                _ => BlockKind::Paragraph,
            }
        }
        _ => BlockKind::Paragraph,
    }
}
