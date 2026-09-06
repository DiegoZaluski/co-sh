//! Mouse drag selection shared by multi-field inputs (the RAG create-db
//! form, the hook/MCP registration panels).
//!
//! Only the field-agnostic kernel lives here: anchor/extend, normalise a
//! range, slice text. Coordinate mapping (click → byte), highlight
//! painting and edit semantics stay with each surface — the RAG fields
//! render through a markdown pipeline on a word-wrapped grid while the
//! registration panels paint a plain char grid, and typing replaces the
//! range in the panels but clears it in the RAG form.

/// A drag selection owned by one field: `anchor` is fixed at press time,
/// `end` follows the drag, so dragging backwards still selects the range
/// in between. Byte offsets; the mappers that build them
/// (`field_byte_at`, `form_panel_hit_field`) both land on char
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragSelection<F: Copy + PartialEq> {
    field: F,
    anchor: usize,
    end: usize,
}

impl<F: Copy + PartialEq> DragSelection<F> {
    /// Anchor a new selection: press parks the caret and starts an empty
    /// range there.
    pub const fn anchor(field: F, byte: usize) -> Self {
        Self {
            field,
            anchor: byte,
            end: byte,
        }
    }

    /// Extend the drag end, keeping the anchor fixed.
    pub fn extend(&mut self, byte: usize) {
        self.end = byte;
    }

    /// Which field owns this selection.
    pub const fn field(&self) -> F {
        self.field
    }

    /// Whether the range covers anything (`anchor != end`).
    pub const fn is_active(&self) -> bool {
        self.anchor != self.end
    }

    /// The range owned by `field`, normalised to `(start, end)` bytes and
    /// clamped to `len`. `None` for another field's selection or an empty
    /// range.
    pub fn range_for(&self, field: F, len: usize) -> Option<(usize, usize)> {
        if self.field != field || !self.is_active() {
            return None;
        }
        let (s, e) = (
            self.anchor.min(self.end).min(len),
            self.anchor.max(self.end).min(len),
        );
        (s != e).then_some((s, e))
    }

    /// The text covered by the range owned by `field` in `line`.
    pub fn slice_of(&self, field: F, line: &str) -> Option<String> {
        self.range_for(field, line.len())
            .map(|(s, e)| line[s..e].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::DragSelection;

    #[test]
    fn anchor_starts_empty_and_extend_grows() {
        let mut sel = DragSelection::anchor(0usize, 3);
        assert!(!sel.is_active());
        assert_eq!(sel.range_for(0, 10), None);
        sel.extend(7);
        assert!(sel.is_active());
        assert_eq!(sel.range_for(0, 10), Some((3, 7)));
    }

    #[test]
    fn backwards_drag_normalises() {
        let mut sel = DragSelection::anchor(1usize, 8);
        sel.extend(2);
        assert_eq!(sel.range_for(1, 10), Some((2, 8)));
        assert_eq!(sel.slice_of(1, "hello world"), Some("llo wo".into()));
    }

    #[test]
    fn other_field_never_matches() {
        let mut sel = DragSelection::anchor(0usize, 1);
        sel.extend(4);
        assert_eq!(sel.range_for(1, 10), None);
        assert_eq!(sel.slice_of(1, "hello"), None);
    }

    #[test]
    fn range_clamps_to_line_end() {
        let mut sel = DragSelection::anchor(0usize, 2);
        sel.extend(99);
        assert_eq!(sel.range_for(0, 5), Some((2, 5)));
        assert_eq!(sel.range_for(0, 2), None, "clamped empty is no range");
    }
}
