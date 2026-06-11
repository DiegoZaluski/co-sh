//! Manages the lifecycle and reuse of Tree-sitter syntax trees via an LRU cache.
//!
//! - **Creation:** If the file is new, detects the language and performs a full parse.
//! - **Reuse:** If the text hasn't changed, retrieves the tree from the cache instantly.
//! - **Update:** If the text changed, performs an incremental parse reusing the old tree.
//!
//! Finally, uses the resulting tree to resolve the `BlockSpan` for the given line.
pub mod block;
pub mod language;

use crate::hashline::types::BlockSpan;
use lru::LruCache;
use std::sync::Mutex;
use std::sync::OnceLock;
use tree_sitter::{Language, Parser, Tree};

const DEFAULT_CAPACITY: usize = 128;

struct CachedEntry {
    language: Language,
    tree: Tree,
    text: String,
}

pub struct TreeSitter {
    inner: Mutex<LruCache<String, CachedEntry>>,
}

impl TreeSitter {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(capacity).expect("capacity > 0"),
            )),
        }
    }

    /// Acquires the cache entry for `path`, re-parsing if `text` changed.
    ///
    /// `f` receives the entry (which holds the parsed tree) plus `text`.
    /// Returns `None` if the language is unsupported or parsing fails.
    fn with_entry<R>(
        &self,
        path: &str,
        text: &str,
        f: impl FnOnce(&CachedEntry, &str) -> R,
    ) -> Option<R> {
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
                let tree = match parser.parse(text, Some(&old.tree)) {
                    Some(t) => t,
                    None => {
                        guard.put(path.to_string(), old);
                        return None;
                    }
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
/// `Rust`, `Python`, `JavaScript`, `C#`, `Go`, `Java`, `Haskell`, `Swift`, `Zig`
pub fn syntax() -> &'static TreeSitter {
    CACHE.get_or_init(|| TreeSitter::new(DEFAULT_CAPACITY))
}
