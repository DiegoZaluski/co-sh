//! Tree-sitter helpers used by the applier's boundary machinery — a port of
//! upstream `packages/hashline/src/syntax.ts` (now
//! `crates/pi-edit/src/modes/hashline/syntax.rs` in oh-my-pi).
//!
//! Three queries, all keyed by the file's display path (→ language by
//! extension):
//!
//! - [`parses_cleanly`] — does this text parse without syntax errors? The
//!   oracle that replaced the old hand-rolled delimiter-balance scanner:
//!   a candidate result is only trusted when the real grammar accepts it.
//! - [`node_chain`] — named-node chain containing a 1-indexed line,
//!   innermost first (used to classify lines and find enclosing constructs).
//! - [`enclosing_boundaries`] — for each multi-line named node whose span
//!   crosses the edge of a line window, the boundary line outside that
//!   window (the "matching bracket" generalization).
//!
//! Every query parses the FULL text and caches the result keyed by
//! (text hash, length, path, window) with a FIFO eviction, exactly like
//! upstream's syntax cache. The shared [`crate::tree_sitter::TreeSitter`]
//! pool is deliberately NOT used here: it re-parses incrementally off the
//! previous tree for a path, which is only sound when the caller also feeds
//! the matching `tree.edit()` ranges — boundary probes swap whole arbitrary
//! texts, and a stale-tree incremental parse yields a corrupt syntax tree.
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::{LazyLock, Mutex},
};

use tree_sitter::Point;

const CACHE_MAX: usize = 256;

#[derive(PartialEq, Eq, Hash, Clone)]
struct Key {
    text_hash: u64,
    text_len: usize,
    path: String,
    start: u32,
    end: u32,
}

fn key(text: &str, path: &str, start: u32, end: u32) -> Key {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    Key {
        text_hash: hasher.finish(),
        text_len: text.len(),
        path: path.to_string(),
        start,
        end,
    }
}

#[derive(Default)]
struct SyntaxCache {
    chains: HashMap<Key, Vec<NodeSpan>>,
    chain_order: VecDeque<Key>,
    boundaries: HashMap<Key, Vec<u32>>,
    boundary_order: VecDeque<Key>,
    parses: HashMap<Key, bool>,
    parse_order: VecDeque<Key>,
}

static CACHE: LazyLock<Mutex<SyntaxCache>> = LazyLock::new(|| Mutex::new(SyntaxCache::default()));

fn insert_fifo<T>(map: &mut HashMap<Key, T>, order: &mut VecDeque<Key>, key: Key, value: T) {
    if map.len() >= CACHE_MAX
        && let Some(oldest) = order.pop_front()
    {
        map.remove(&oldest);
    }
    order.push_back(key.clone());
    map.insert(key, value);
}

fn parse(path: &str, text: &str) -> Option<tree_sitter::Tree> {
    let language = crate::tree_sitter::language::detect_language(path)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(text, None)
}

/// A named node's line span (1-indexed, inclusive) and grammar kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpan {
    /// First line of the node.
    pub start_line: u32,
    /// Last line containing a content byte of the node (a node ending in a
    /// newline does not "end" on the following blank line).
    pub end_line: u32,
    /// Tree-sitter grammar node kind (e.g. `attribute_item`, `function_item`).
    pub kind: String,
}

/// Whether `text` parses cleanly for the language inferred from `path`.
///
/// `None`/empty path or an unrecognized extension returns `false`: there is
/// no grammar to prove the text, so no repair may claim syntax validity
/// (mirrors upstream's `parsed: false` for unparsed sources).
#[must_use]
pub fn parses_cleanly(path: Option<&str>, text: &str) -> bool {
    let Some(path) = path else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    let cache_key = key(text, path, 0, 0);
    if let Ok(cache) = CACHE.lock()
        && let Some(cached) = cache.parses.get(&cache_key)
    {
        return *cached;
    }
    let parsed = parse(path, text)
        .is_some_and(|tree| !tree.root_node().has_error());
    if let Ok(mut cache) = CACHE.lock() {
        let SyntaxCache {
            parses, parse_order, ..
        } = &mut *cache;
        insert_fifo(parses, parse_order, cache_key, parsed);
    }
    parsed
}

/// Named-node chain containing 1-indexed `line`, innermost first, excluding
/// the whole-file root. ERROR and MISSING recovery nodes are skipped so an
/// unrelated syntax error elsewhere in the file does not blank the chain.
///
/// The chain descends through the line's first content character, so
/// single-line nodes beginning on the line (attributes, decorators,
/// one-line statements) come first, followed by every enclosing construct.
#[must_use]
pub fn node_chain(lines: &[String], path: &str, line: u32) -> Vec<NodeSpan> {
    let text = lines.join("\n");
    if line == 0 || text.is_empty() {
        return Vec::new();
    }
    let cache_key = key(&text, path, line, line);
    if let Ok(cache) = CACHE.lock()
        && let Some(cached) = cache.chains.get(&cache_key)
    {
        return cached.clone();
    }
    let Some(row_line) = text.split('\n').nth((line - 1) as usize) else {
        return Vec::new();
    };
    let Some(col) = row_line
        .bytes()
        .position(|byte| byte != b' ' && byte != b'\t')
    else {
        return Vec::new();
    };
    let chain = parse(path, &text)
        .map(|tree| {
            let root = tree.root_node();
            // One-column-wide range so zero-width nodes at the line start are
            // still found.
            let point = Point::new((line - 1) as usize, col);
            let point_end = Point::new((line - 1) as usize, col + 1);
            let Some(mut node) = root.named_descendant_for_point_range(point, point_end) else {
                return Vec::new();
            };
            let mut chain = Vec::new();
            loop {
                if node.id() == root.id() {
                    break;
                }
                if node.is_named() && !node.is_error() && !node.is_missing() {
                    chain.push(NodeSpan {
                        start_line: node_start_line(node),
                        end_line: node_content_end_line(node),
                        kind: node.kind().to_string(),
                    });
                }
                let Some(parent) = node.parent() else {
                    break;
                };
                node = parent;
            }
            chain
        })
        .unwrap_or_default();
    if let Ok(mut cache) = CACHE.lock() {
        let SyntaxCache {
            chains, chain_order, ..
        } = &mut *cache;
        insert_fifo(chains, chain_order, cache_key, chain.clone());
    }
    chain
}

/// Syntax block boundaries enclosing the line window `[start, end]`: for
/// every multi-line named node whose span crosses a window edge, the boundary
/// line sitting outside the window (opener shown, closer off-window → its
/// closing line, and vice versa). Returns the sorted unique boundary lines.
///
/// An unrecognized language or a file that does not parse returns an empty
/// list — boundaries are advisory hints for repair heuristics, never trusted
/// without a parse-provable file.
#[must_use]
pub fn enclosing_boundaries(lines: &[String], path: &str, start: u32, end: u32) -> Vec<u32> {
    let text = lines.join("\n");
    if text.is_empty() || start == 0 || end < start {
        return Vec::new();
    }
    let cache_key = key(&text, path, start, end);
    if let Ok(cache) = CACHE.lock()
        && let Some(cached) = cache.boundaries.get(&cache_key)
    {
        return cached.clone();
    }
    let boundaries = parse(path, &text)
        .map(|tree| {
            // A file-level syntax error makes error-recovery spans unreliable.
            let root = tree.root_node();
            if root.has_error() {
                return Vec::new();
            }
            let mut boundaries = std::collections::BTreeSet::new();
            collect_boundaries(&mut tree.walk(), start, end, &mut boundaries);
            boundaries.into_iter().collect()
        })
        .unwrap_or_default();
    if let Ok(mut cache) = CACHE.lock() {
        let SyntaxCache {
            boundaries: values,
            boundary_order,
            ..
        } = &mut *cache;
        insert_fifo(values, boundary_order, cache_key, boundaries.clone());
    }
    boundaries
}

fn collect_boundaries(
    cursor: &mut tree_sitter::TreeCursor<'_>,
    start: u32,
    end: u32,
    out: &mut std::collections::BTreeSet<u32>,
) {
    let node = cursor.node();
    // Prune subtrees that cannot contribute: every descendant's span is
    // contained in this node's raw row span.
    let raw_start = node_start_line(node);
    let raw_end = node_raw_end_line(node);
    if raw_end < start || raw_start > end {
        return;
    }
    // Skip the whole-file root: its only "boundary" is EOF.
    if node.is_named() && node.parent().is_some() {
        let content_end = node_content_end_line(node);
        if content_end > raw_start {
            let start_visible = raw_start >= start && raw_start <= end;
            let end_visible = content_end >= start && content_end <= end;
            if start_visible && !end_visible {
                out.insert(content_end);
            } else if end_visible && !start_visible {
                out.insert(raw_start);
            }
        }
    }
    if cursor.goto_first_child() {
        loop {
            collect_boundaries(cursor, start, end, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn node_start_line(node: tree_sitter::Node<'_>) -> u32 {
    u32::try_from(node.start_position().row).unwrap_or(u32::MAX - 1) + 1
}

fn node_raw_end_line(node: tree_sitter::Node<'_>) -> u32 {
    u32::try_from(node.end_position().row).unwrap_or(u32::MAX - 1) + 1
}

/// Last source line containing a content byte from `node`: tree-sitter
/// reports `end_position` one past the last byte, so an end at column 0
/// means the node's last content row is the previous one.
fn node_content_end_line(node: tree_sitter::Node<'_>) -> u32 {
    let pos = node.end_position();
    if pos.column == 0 && pos.row > 0 {
        u32::try_from(pos.row).unwrap_or(u32::MAX)
    } else {
        node_raw_end_line(node)
    }
}
