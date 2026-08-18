//! Manages the lifecycle and reuse of Tree-sitter syntax trees via an LRU cache.
//!
//! - **Creation:** If the file is new, detects the language and performs a full parse.
//! - **Reuse:** If the text hasn't changed, retrieves the tree from the cache instantly.
//! - **Update:** If the text changed, performs an incremental parse reusing the old tree.
//!
//! Finally, uses the resulting tree to resolve the `BlockSpan` for the given line.
pub mod block;
pub mod highlight;
pub mod language;

use crate::hashline::types::BlockSpan;
use lru::LruCache;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tree_sitter::{Language, Parser, Tree};

const DEFAULT_CAPACITY: usize = 128;

struct CachedEntry {
    language: Language,
    tree: Tree,
    text: String,
}

pub struct TreeSitter {
    inner: Mutex<LruCache<String, CachedEntry>>,
    /// Number of actual `parser.parse` calls (full + incremental), for
    /// observability of the cache lifecycle. Incremented under the cache
    /// lock; read via [`parse_count`](Self::parse_count).
    parses: AtomicU64,
}

impl TreeSitter {
    /// Create a new LRU-cached tree-sitter parser pool.
    ///
    /// # Panics
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(
                #[allow(clippy::expect_used)]
                std::num::NonZeroUsize::new(capacity).expect("capacity > 0"),
            )),
            parses: AtomicU64::new(0),
        }
    }

    /// Total number of `parser.parse` invocations performed so far (full
    /// parses plus incremental re-parses). Cache hits do not increment it.
    /// Useful for observing the parse lifecycle (e.g. in examples and tests).
    #[must_use]
    pub fn parse_count(&self) -> u64 {
        self.parses.load(Ordering::Relaxed)
    }

    /// Acquires the cache entry for `path`, re-parsing if `text` changed.
    ///
    /// `f` receives the entry (which holds the parsed tree) plus `text`.
    /// Returns `None` if the language is unsupported or parsing fails.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned.
    #[allow(clippy::significant_drop_tightening)]
    fn with_entry<R>(
        &self,
        path: &str,
        text: &str,
        f: impl FnOnce(&CachedEntry, &str) -> R,
    ) -> Option<R> {
        #[allow(clippy::unwrap_used)]
        let mut guard = self.inner.lock().unwrap();

        // `Tree` is not `Clone`, so we must pop to gain ownership even on a cache hit.
        let entry = match guard.pop(path) {
            Some(old) if old.text == text => old,
            Some(old) => {
                let mut parser = Parser::new();
                if parser.set_language(&old.language).is_err() {
                    guard.put(path.to_string(), old);
                    return None;
                }
                self.parses.fetch_add(1, Ordering::Relaxed);
                let Some(tree) = parser.parse(text, Some(&old.tree)) else {
                    guard.put(path.to_string(), old);
                    return None;
                };
                CachedEntry {
                    language: old.language,
                    tree,
                    text: text.to_string(),
                }
            }
            None => {
                let mut parser = Parser::new();
                let language = language::detect_language(path)?;
                parser.set_language(&language).ok()?;
                self.parses.fetch_add(1, Ordering::Relaxed);
                let tree = parser.parse(text, None)?;
                CachedEntry {
                    language,
                    tree,
                    text: text.to_string(),
                }
            }
        };
        let result = f(&entry, &entry.text);
        guard.put(path.to_string(), entry);
        Some(result)
    }

    pub fn resolve_block(&self, path: &str, text: &str, line: u32) -> Option<BlockSpan> {
        self.with_entry(path, text, |entry, _| {
            block::resolve_block(&entry.tree, text, line)
        })
        .flatten()
    }

    /// Resolves the syntactic block of the first definition whose declared `name`
    /// matches `name` (e.g. a symbol, struct, or module).
    ///
    /// Uses the same cache and parsing lifecycle as [`resolve_block`].
    /// Returns `None` when the language is unsupported, parsing fails, or no
    /// matching definition is found.
    pub fn resolve_symbol(&self, path: &str, text: &str, name: &str) -> Option<BlockSpan> {
        self.with_entry(path, text, |entry, _| {
            block::resolve_symbol_name(&entry.tree, text.as_bytes(), name)
        })
        .flatten()
    }

    /// Run `f` with the parsed [`Tree`] and its source text for `path`,
    /// re-parsing (incrementally) when the text changed.
    ///
    /// Unlike the higher-level [`resolve_block`]/[`resolve_symbol`] helpers,
    /// this exposes the raw tree so advanced consumers (e.g. the AST rewrite
    /// engine) can walk and match nodes themselves. Returns `None` when the
    /// language is unsupported or parsing fails.
    pub fn with_tree<R>(
        &self,
        path: &str,
        text: &str,
        f: impl FnOnce(&Tree, &str) -> R,
    ) -> Option<R> {
        self.with_entry(path, text, |entry, _| f(&entry.tree, &entry.text))
    }

    pub fn invalidate(&self, path: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.pop(path);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear();
        }
    }
}

static CACHE: OnceLock<TreeSitter> = OnceLock::new();
/// Instantiate a tree-sitter parser only once.
///
/// Manages parse tree lifecycle internally:
/// - **Miss:** detects language from path extension, performs a full parse, stores the result.
/// - **Hit (unchanged):** returns the cached tree instantly, no re-parse.
/// - **Hit (modified):** performs an incremental re-parse reusing the old tree.
///
/// Returns `None` if the language is unsupported or parsing fails.
///
/// # Supported languages
///
/// `Rust`, `Python`, `JavaScript`, `C#`, `Go`, `Java`, `Haskell`, `Swift`, `Zig`, `Kotlin`
pub fn tree_sitter() -> &'static TreeSitter {
    CACHE.get_or_init(|| TreeSitter::new(DEFAULT_CAPACITY))
}
