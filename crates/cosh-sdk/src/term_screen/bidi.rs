//! Bidirectional text support.
//! Stripped-down replacement for wezterm-bidi.
//! This is a minimal stub that always assumes Left-to-Right text direction.
//! Full bidi support (UAX #9) is not needed for terminal output parsing;
//! if needed in the future, consider using the unic-bidi crate.

/// Text direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
}

/// Hint for paragraph direction in bidi resolution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphDirectionHint {
    AutoLeftToRight,
    AutoRightToLeft,
    LeftToRight,
    RightToLeft,
}

impl Default for ParagraphDirectionHint {
    fn default() -> Self {
        ParagraphDirectionHint::LeftToRight
    }
}

impl ParagraphDirectionHint {
    pub fn direction(self) -> Direction {
        match self {
            ParagraphDirectionHint::AutoLeftToRight | ParagraphDirectionHint::LeftToRight => {
                Direction::LeftToRight
            }
            ParagraphDirectionHint::AutoRightToLeft | ParagraphDirectionHint::RightToLeft => {
                Direction::RightToLeft
            }
        }
    }
}

/// Bidi context stub.
/// In the full implementation this would resolve the Unicode Bidirectional Algorithm.
/// Here it's a no-op that preserves Left-to-Right ordering.
#[derive(Debug, Clone)]
pub struct BidiContext;

/// Information about a bidi run (contiguous range of same direction)
#[derive(Debug, Clone)]
pub struct BidiRun {
    /// The range of indices in the logical order
    pub range: std::ops::Range<usize>,
    /// The indices in visual order
    pub indices: Vec<usize>,
}

/// A reordered run for visual display
#[derive(Debug, Clone)]
pub struct ReorderedRun {
    /// The range of indices in the logical order
    pub range: std::ops::Range<usize>,
    /// The indices in visual order
    pub indices: Vec<usize>,
    /// The text direction for this run
    pub direction: Direction,
}

impl BidiContext {
    pub fn new() -> Self {
        BidiContext
    }

    /// Resolve paragraph direction and bidi levels.
    /// Stub: does nothing, assumes LTR.
    pub fn resolve_paragraph(&mut self, _paragraph: &[char], _hint: ParagraphDirectionHint) {
        // No-op: always LTR
    }

    /// Get reordered visual runs.
    /// Stub: returns a single LTR run covering the entire range.
    pub fn reordered_runs(&self, range: std::ops::Range<usize>) -> Vec<ReorderedRun> {
        let indices: Vec<usize> = range.clone().collect();
        vec![ReorderedRun { range, indices, direction: Direction::LeftToRight }]
    }
}
