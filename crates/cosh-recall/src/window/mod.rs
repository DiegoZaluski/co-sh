use std::collections::VecDeque;

/// A single entry in the context window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowEntry {
    /// The content of the entry (e.g., a summary, a tool result).
    pub content: String,
    /// How many tokens this entry contributes.
    pub token_count: usize,
}

/// A sliding context window backed by `VecDeque`.
///
/// Entries are pushed onto the back. When the accumulated token count exceeds
/// `max_tokens`, the oldest half of the entries are evicted from the front.
///
/// This keeps the most recent context intact for as long as possible, and
/// removal is subtle — only the oldest half disappears at once.
///
/// **Why `VecDeque`?**
/// - O(1) amortised push back and pop front.
/// - Cache-friendly linear storage.
/// - Naturally expresses the FIFO-with-eviction pattern.
#[derive(Debug, Clone)]
pub struct ContextWindow {
    entries: VecDeque<WindowEntry>,
    max_tokens: usize,
    total_tokens: usize,
}

impl ContextWindow {
    /// Create a new empty window with the given token budget.
    ///
    /// `max_tokens` must be greater than zero.
    ///
    /// # Panics
    ///
    /// Panics if `max_tokens` is 0.
    #[must_use]
    pub fn new(max_tokens: usize) -> Self {
        assert!(max_tokens > 0, "max_tokens must be greater than zero");
        Self {
            entries: VecDeque::new(),
            max_tokens,
            total_tokens: 0,
        }
    }

    /// Push a new entry onto the window.
    ///
    /// If the total token count now exceeds `max_tokens`, the oldest half of
    /// the entries are evicted from the front. The new entry always survives
    /// because it is pushed *before* the eviction check runs.
    pub fn push(&mut self, content: impl Into<String>, token_count: usize) {
        let entry = WindowEntry {
            content: content.into(),
            token_count,
        };
        self.total_tokens += entry.token_count;
        self.entries.push_back(entry);
        self.evict_if_needed();
    }

    /// How many entries are currently in the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the window contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current total token count across all entries.
    #[must_use]
    pub fn total_tokens(&self) -> usize {
        self.total_tokens
    }

    /// The configured token budget.
    #[must_use]
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// True when `total_tokens` >= `max_tokens`.
    #[must_use]
    pub fn is_over_budget(&self) -> bool {
        self.total_tokens >= self.max_tokens
    }

    /// Borrow the current entries (oldest first).
    #[must_use]
    pub fn entries(&self) -> &VecDeque<WindowEntry> {
        &self.entries
    }

    /// Iterate over entries from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &WindowEntry> {
        self.entries.iter()
    }

    /// Render the full context window as a formatted prompt block.
    ///
    /// Returns a string like:
    ///
    /// ```text
    /// ## Session Context
    ///
    /// - <content> (<token_count> tokens)
    /// - <content> (<token_count> tokens)
    /// ```
    ///
    /// Returns an empty string when the window is empty.
    #[must_use]
    pub fn format_context(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut out = String::from("\n## Session Context\n\n");
        for entry in &self.entries {
            use std::fmt::Write;
            let _ = writeln!(out, "- {} ({} tokens)", entry.content, entry.token_count);
        }
        out
    }

    /// Clear all entries and reset the token count.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_tokens = 0;
    }

    //.......... Private helpers ...

    /// If the window is over budget, evict the oldest ceil(half) entries in a
    /// single batch. Unlike a `while`-loop cascade, this guarantees at most one
    /// eviction per push — matching the "pop half" mental model.
    fn evict_if_needed(&mut self) {
        if self.total_tokens >= self.max_tokens && !self.entries.is_empty() {
            let remove_count = self.entries.len().div_ceil(2); // ceil(half)
            let mut evicted_tokens = 0usize;
            for _ in 0..remove_count {
                if let Some(evicted) = self.entries.pop_front() {
                    evicted_tokens += evicted.token_count;
                }
            }
            self.total_tokens = self.total_tokens.saturating_sub(evicted_tokens);
        }
    }
}

//.......... Tests ...

#[cfg(test)]
mod tests;
