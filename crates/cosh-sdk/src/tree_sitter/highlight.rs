use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use std::cell::RefCell;
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightCategory {
    Keyword,
    String,
    Comment,
    Type,
    Function,
    Number,
    Builtin,
}

#[derive(Debug)]
struct CompiledHighlighter {
    language: Language,
    query: Query,
    /// Capture index → category (`None` for captures we do not style).
    categories: Vec<Option<HighlightCategory>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub category: HighlightCategory,
}

fn lang_from_name(name: &str) -> Option<Language> {
    Some(match name {
        "rust" | "rs" => tree_sitter_rust::LANGUAGE.into(),
        "python" | "py" => tree_sitter_python::LANGUAGE.into(),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "json" | "typescript" | "ts" | "tsx"
        | "php" | "lua" | "dart" => tree_sitter_javascript::LANGUAGE.into(),
        "c#" | "csharp" | "cs" | "c" | "h" | "cpp" | "c++" | "cxx" | "hpp" | "objectivec"
        | "objc" | "m" | "mm" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" | "golang" => tree_sitter_go::LANGUAGE.into(),
        "java" | "scala" | "groovy" => tree_sitter_java::LANGUAGE.into(),
        "haskell" | "hs" | "lhs" => tree_sitter_haskell::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "zig" | "zon" => tree_sitter_zig::LANGUAGE.into(),
        "kotlin" | "kt" | "kts" => tree_sitter_kotlin::LANGUAGE.into(),
        _ => return None,
    })
}

fn query_for_language(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "rust" | "rs" => {
            "\"fn\" @keyword
             \"let\" @keyword
             \"use\" @keyword
             \"return\" @keyword
             \"if\" @keyword
             \"else\" @keyword
             \"match\" @keyword
             \"struct\" @keyword
             \"enum\" @keyword
             \"impl\" @keyword
             \"pub\" @keyword
             \"ref\" @keyword
             \"const\" @keyword
             \"static\" @keyword
             \"trait\" @keyword
             \"where\" @keyword
             \"while\" @keyword
             \"for\" @keyword
             \"in\" @keyword
             \"loop\" @keyword
             \"break\" @keyword
             \"continue\" @keyword
             \"mod\" @keyword
             \"as\" @keyword
             \"async\" @keyword
             \"await\" @keyword
             \"unsafe\" @keyword
             \"extern\" @keyword
             \"type\" @keyword
             \"union\" @keyword
             \"move\" @keyword
             \"dyn\" @keyword
             (string_literal) @string
             (raw_string_literal) @string
             (line_comment) @comment
             (block_comment) @comment
             (type_identifier) @type
             (integer_literal) @number
             (float_literal) @number
             (function_item name: (identifier) @function)
             (call_expression function: (identifier) @function)
             (macro_invocation macro: (identifier) @function)
             (boolean_literal) @builtin
             (mutable_specifier) @keyword
             (self) @keyword
             (super) @keyword
             (crate) @keyword"
        }
        "python" | "py" => {
            "\"def\" @keyword
             \"class\" @keyword
             \"return\" @keyword
             \"if\" @keyword
             \"elif\" @keyword
             \"else\" @keyword
             \"for\" @keyword
             \"in\" @keyword
             \"while\" @keyword
             \"import\" @keyword
             \"from\" @keyword
             \"as\" @keyword
             \"with\" @keyword
             \"try\" @keyword
             \"except\" @keyword
             \"finally\" @keyword
             \"raise\" @keyword
             \"yield\" @keyword
             \"lambda\" @keyword
             \"pass\" @keyword
             \"break\" @keyword
             \"continue\" @keyword
             \"and\" @keyword
             \"or\" @keyword
             \"not\" @keyword
             \"is\" @keyword
             (string) @string
             (comment) @comment
             (integer) @number
             (float) @number
             (function_definition name: (identifier) @function)
             (call function: (identifier) @function)"
        }
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "json" | "typescript" | "ts" | "tsx"
        | "php" | "lua" | "dart" => {
            "\"function\" @keyword
             \"const\" @keyword
             \"let\" @keyword
             \"var\" @keyword
             \"return\" @keyword
             \"if\" @keyword
             \"else\" @keyword
             \"for\" @keyword
             \"while\" @keyword
             \"do\" @keyword
             \"import\" @keyword
             \"export\" @keyword
             \"from\" @keyword
             \"class\" @keyword
              \"new\" @keyword
              \"throw\" @keyword
             \"try\" @keyword
             \"catch\" @keyword
             \"finally\" @keyword
             \"async\" @keyword
             \"await\" @keyword
             \"yield\" @keyword
             \"typeof\" @keyword
             \"instanceof\" @keyword
              \"delete\" @keyword
              \"in\" @keyword
              \"of\" @keyword
              \"switch\" @keyword
              \"case\" @keyword
              \"default\" @keyword
              \"break\" @keyword
              \"continue\" @keyword
              (null) @builtin
              (undefined) @builtin
              (true) @builtin
              (false) @builtin
              (this) @keyword
              (string) @string
             (comment) @comment
             (number) @number
             (function_declaration name: (identifier) @function)
             (call_expression function: (identifier) @function)"
        }
        "c#" | "csharp" | "cs" | "go" | "java" | "scala" | "groovy" | "haskell" | "hs" | "lhs"
        | "swift" | "zig" | "zon" | "kotlin" | "kt" | "kts" | "c" | "h" | "cpp" | "c++" | "cxx"
        | "hpp" | "objectivec" | "objc" | "m" | "mm" => {
            "(string_literal) @string
             (comment) @comment
             (type_identifier) @type
             (integer_literal) @number
             (float_literal) @number"
        }
        _ => return None,
    })
}

/// Compiled query + capture table per fence language, built once.
///
/// `Query::new` compiles the pattern list from source text and dominated the
/// cost of every highlight call (measured ~2ms per fenced block); a chat
/// stream discovers several new fences per frame, so this cache is what
/// makes incremental streaming affordable. Unknown languages are cached as
/// `None` so repeated misses stay cheap too. The language tag comes from the
/// fence info string (model-controlled), so entries are LRU-capped instead
/// of growing without bound.
type CompiledCache = lru::LruCache<String, Option<Arc<CompiledHighlighter>>>;

fn compiled_cache() -> &'static Mutex<CompiledCache> {
    static COMPILED: LazyLock<Mutex<CompiledCache>> = LazyLock::new(|| {
        Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(256).expect("non-zero"),
        ))
    });
    &COMPILED
}

fn compiled_highlighter(lang: &str) -> Option<Arc<CompiledHighlighter>> {
    // Poisoning follows the codebase's other render caches (panic on a
    // poisoned lock rather than silently losing highlighting forever).
    #[allow(clippy::unwrap_used)]
    let mut map = compiled_cache().lock().unwrap();
    if let Some(cached) = map.get(lang) {
        return cached.clone();
    }
    let compiled = lang_from_name(lang).and_then(|language| {
        let query = Query::new(&language, query_for_language(lang)?).ok()?;
        let categories = query
            .capture_names()
            .iter()
            .map(|name| match *name {
                "keyword" => Some(HighlightCategory::Keyword),
                "string" => Some(HighlightCategory::String),
                "comment" => Some(HighlightCategory::Comment),
                "type" => Some(HighlightCategory::Type),
                "function" => Some(HighlightCategory::Function),
                "number" => Some(HighlightCategory::Number),
                "builtin" => Some(HighlightCategory::Builtin),
                _ => None,
            })
            .collect();
        Some(Arc::new(CompiledHighlighter {
            language,
            query,
            categories,
        }))
    });
    map.put(lang.to_string(), compiled.clone());
    compiled
}

// One reusable parser per thread (parsers are not `Sync`; the TUI render
// path is single-threaded, so a thread-local avoids re-allocating the
// tree-sitter parser on every fenced block).
thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

#[must_use]
pub fn highlight(source: &str, lang: &str) -> Option<Vec<HighlightSpan>> {
    let compiled = compiled_highlighter(lang)?;

    PARSER.with_borrow_mut(|parser| {
        parser.set_language(&compiled.language).ok()?;
        let tree = parser.parse(source, None)?;
        let root = tree.root_node();

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&compiled.query, root, source.as_bytes());

        let mut spans: Vec<HighlightSpan> = Vec::new();
        while let Some(match_) = matches.next() {
            for capture in match_.captures {
                let node = capture.node;
                let range = node.byte_range();
                let Some(category) = compiled.categories[capture.index as usize] else {
                    continue;
                };
                spans.push(HighlightSpan {
                    start: range.start,
                    end: range.end,
                    category,
                });
            }
        }

        spans.sort_by_key(|s| s.start);
        Some(spans)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_rust() {
        let source = "fn main() {\n    let x = 1;\n    println!(\"hello\");\n}\n";
        let result = highlight(source, "rust");
        assert!(result.is_some(), "highlight should return Some for rust");
        let spans = result.unwrap();
        println!("Rust spans: {:?}", spans);
        assert!(!spans.is_empty(), "should have at least one span");

        // Check for keyword spans (fn, let)
        let keywords: Vec<_> = spans
            .iter()
            .filter(|s| s.category == HighlightCategory::Keyword)
            .collect();
        assert!(!keywords.is_empty(), "should find keywords like fn and let");

        // Check that span positions are valid
        for span in &spans {
            assert!(span.start < span.end, "span start < end");
            assert!(span.end <= source.len(), "span end <= source.len");
        }
    }

    #[test]
    fn test_highlight_python() {
        let source = "def hello():\n    print('world')\n";
        let result = highlight(source, "python");
        assert!(result.is_some(), "highlight should return Some for python");
        let spans = result.unwrap();
        println!("Python spans: {:?}", spans);
        assert!(!spans.is_empty(), "should have at least one span");
    }

    #[test]
    fn test_highlight_json() {
        let source = r#"{
  "name": "test",
  "count": 42,
  "active": true,
  "data": null
}"#;
        let result = highlight(source, "json");
        assert!(
            result.is_some(),
            "highlight should return Some for json (uses JS grammar)"
        );
        let spans = result.unwrap();
        println!("JSON spans: {:?}", spans);
        assert!(!spans.is_empty(), "should have at least one span for JSON");

        // JSON should have strings (the keys and values)
        let strings: Vec<_> = spans
            .iter()
            .filter(|s| s.category == HighlightCategory::String)
            .collect();
        assert!(!strings.is_empty(), "JSON should have string spans");

        // JSON should have numbers (42)
        let numbers: Vec<_> = spans
            .iter()
            .filter(|s| s.category == HighlightCategory::Number)
            .collect();
        assert!(!numbers.is_empty(), "JSON should have number spans");

        // JSON should have builtins (true, null)
        let builtins: Vec<_> = spans
            .iter()
            .filter(|s| s.category == HighlightCategory::Builtin)
            .collect();
        assert!(
            !builtins.is_empty(),
            "JSON should have builtin spans (true, null)"
        );
    }

    #[test]
    fn test_highlight_javascript() {
        let source = "function hello() {\n    const x = 1;\n    return x;\n}\n";
        let result = highlight(source, "javascript");
        assert!(
            result.is_some(),
            "highlight should return Some for javascript"
        );
        let spans = result.unwrap();
        println!("JS spans: {:?}", spans);
        assert!(!spans.is_empty(), "should have at least one span");
    }

    #[test]
    fn test_highlight_unknown_lang() {
        let source = "some text";
        let result = highlight(source, "unknown_lang");
        assert!(
            result.is_none(),
            "highlight should return None for unknown lang"
        );
    }

    /// The compiled-highlighter cache stores unknown languages as an
    /// explicit `None`: repeated misses must keep returning `None` (a bug
    /// here would silently start highlighting garbage) and stay cheap.
    #[test]
    fn test_highlight_unknown_lang_repeated_is_stable() {
        for _ in 0..3 {
            assert!(highlight("some text", "unknown_lang").is_none());
            assert!(highlight("other text", "unknown_lang2").is_none());
        }
    }

    /// The second call for the same language hits the compiled-query cache;
    /// results must be byte-identical to the cold first call.
    #[test]
    fn test_highlight_cached_call_matches_cold() {
        let source = "fn main() {\n    let x = \"s\";\n}\n";
        let cold = highlight(source, "rust");
        let warm = highlight(source, "rust");
        assert_eq!(cold, warm, "cached highlighter changed the output");
    }

    #[test]
    fn test_highlight_empty_lang() {
        let source = "some text";
        let result = highlight(source, "");
        assert!(
            result.is_none(),
            "highlight should return None for empty lang"
        );
    }

    #[test]
    fn test_highlight_lang_with_spaces() {
        // This simulates what would happen if pulldown_cmark passes " rust" (with leading space)
        let source = "fn main() {}";
        let result = highlight(source, "rust");
        assert!(result.is_some(), "highlight should return Some for 'rust'");

        let result2 = highlight(source, " rust");
        assert!(
            result2.is_none(),
            "highlight should return None for ' rust' (leading space)"
        );

        let result3 = highlight(source, "rust ");
        assert!(
            result3.is_none(),
            "highlight should return None for 'rust ' (trailing space)"
        );
    }
}
