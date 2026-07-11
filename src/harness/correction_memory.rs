use std::fmt::Write;
use xxhash_rust::xxh32::xxh32;

struct CorrectionEntry {
    hash: u32,
    text: String,
    count: usize,
}

/// Bounded, deduplicated memory of tool correction errors for the current session.
///
/// Persists across agent loop iterations so the model sees its full error
/// history rather than only the most recent failure. Duplicate errors are
/// coalesced (count incremented) via xxh32 content hash.
pub struct CorrectionMemory {
    entries: Vec<CorrectionEntry>,
    max_entries: usize,
}

impl CorrectionMemory {
    /// Create a new correction memory with the given maximum number of entries.
    #[must_use]
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max_entries),
            max_entries,
        }
    }

    /// Record an error.  If an identical error already exists its count is
    /// incremented and it is moved to the front (most recent).  Otherwise a new
    /// entry is inserted; if the memory is at capacity the oldest entry is
    /// evicted.
    pub fn push(&mut self, text: &str) {
        let hash = xxh32(text.as_bytes(), 0);

        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.hash == hash && e.text == text)
        {
            let mut entry = self.entries.remove(pos);
            entry.count += 1;
            self.entries.insert(0, entry);
        } else {
            self.entries.insert(
                0,
                CorrectionEntry {
                    hash,
                    text: text.to_string(),
                    count: 1,
                },
            );
            if self.entries.len() > self.max_entries {
                self.entries.pop();
            }
        }
    }

    /// Render the memory as a compact Markdown block, or an empty string if
    /// there are no entries.
    #[must_use]
    pub fn format(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut out = "\n## Correction History\n".to_string();
        for entry in &self.entries {
            let _ = write!(
                out,
                "\n- {}{}",
                entry.text,
                if entry.count > 1 {
                    format!(" (x{})", entry.count)
                } else {
                    String::new()
                },
            );
        }
        out
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Whether there are any entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedup_same_error() {
        let mut m = CorrectionMemory::new(5);
        m.push("error A");
        m.push("error A");
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].count, 2);
    }

    #[test]
    fn push_two_distinct_errors() {
        let mut m = CorrectionMemory::new(5);
        m.push("error A");
        m.push("error B");
        assert_eq!(m.entries.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let mut m = CorrectionMemory::new(2);
        m.push("error A");
        m.push("error B");
        m.push("error C");
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.entries[0].text, "error C");
        assert_eq!(m.entries[1].text, "error B");
    }

    #[test]
    fn dedup_reorders_to_front() {
        let mut m = CorrectionMemory::new(3);
        m.push("error A");
        m.push("error B");
        m.push("error A");
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.entries[0].text, "error A");
        assert_eq!(m.entries[0].count, 2);
        assert_eq!(m.entries[1].text, "error B");
    }

    #[test]
    fn format_empty() {
        let m = CorrectionMemory::new(5);
        assert_eq!(m.format(), "");
    }

    #[test]
    fn format_single_entry() {
        let mut m = CorrectionMemory::new(5);
        m.push("error A");
        let out = m.format();
        assert!(out.contains("error A"));
        assert!(!out.contains("(x"));
    }

    #[test]
    fn format_repeated_entry_shows_count() {
        let mut m = CorrectionMemory::new(5);
        m.push("error A");
        m.push("error A");
        let out = m.format();
        assert!(out.contains("x2"));
    }

    #[test]
    fn format_entries_have_correction_header() {
        let mut m = CorrectionMemory::new(5);
        m.push("error A");
        assert!(m.format().contains("## Correction History"));
    }
}
