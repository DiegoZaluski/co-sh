//! Returns the line range (`BlockSpan`) that defines the code block of the current line.
//!
//! Finds the deepest node at the target byte and climbs the Tree-sitter tree to capture
//! the largest possible structure that still starts on the given line.
use tree_sitter::Tree;

use crate::hashline::types::BlockSpan;

/// `text` is needed to convert line numbers to byte offsets — tree-sitter
/// stores positions as byte offsets, not line numbers.
pub(crate) fn resolve_block(tree: &Tree, text: &str, line: u32) -> Option<BlockSpan> {
    let row = line.checked_sub(1)? as usize;
    let byte = byte_of_row(text, row)?;
    let root = tree.root_node();

    let mut node = root.named_descendant_for_byte_range(byte, byte)?;

    // Climbs the syntax tree to encompass the full block of the current line.
    // Stops before entering the previous line or hitting the file root.
    loop {
        let parent = node.parent()?;
        if parent.start_position().row != row || parent == root {
            break;
        }
        node = parent;
    }

    // Confirm that `named_descendant_for_byte_range` has not moved to another line.
    // Scenarios in which this happens:
    // 1. Blank lines
    // 2. End of file
    if node.start_position().row != row {
        return None;
    }

    Some(BlockSpan {
        start: node.start_position().row as u32 + 1,
        end: node.end_position().row as u32 + 1,
    })
}

/// Returns the `BlockSpan` of the first definition node whose `name` field matches `name`.
///
/// Walks the tree in pre-order so the outermost matching definition is found first
/// (e.g. a module before a nested symbol).
pub(crate) fn resolve_symbol_name(tree: &Tree, source: &[u8], name: &str) -> Option<BlockSpan> {
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();

        if node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            == Some(name)
        {
            return Some(BlockSpan {
                start: node.start_position().row as u32 + 1,
                end: node.end_position().row as u32 + 1,
            });
        }

        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

fn byte_of_row(text: &str, target_row: usize) -> Option<usize> {
    if target_row == 0 {
        return Some(0);
    }
    let mut seen: usize = 0;
    for (i, &b) in text.as_bytes().iter().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == target_row {
                return Some(i + 1);
            }
        }
    }
    None
}
