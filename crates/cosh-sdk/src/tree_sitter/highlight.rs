use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HighlightCategory {
    Keyword,
    String,
    Comment,
    Type,
    Function,
    Number,
    Builtin,
}

#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub category: HighlightCategory,
}

fn lang_from_name(name: &str) -> Option<Language> {
    Some(match name {
        "rust" | "rs" => tree_sitter_rust::LANGUAGE.into(),
        "python" | "py" => tree_sitter_python::LANGUAGE.into(),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "c#" | "csharp" | "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" | "golang" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "haskell" | "hs" | "lhs" => tree_sitter_haskell::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "zig" | "zon" => tree_sitter_zig::LANGUAGE.into(),
        "kotlin" | "kt" | "kts" => tree_sitter_kotlin::LANGUAGE.into(),
        _ => return None,
    })
}

#[allow(clippy::too_many_lines)]
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
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => {
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
        "c#" | "csharp" | "cs" | "go" | "java" | "haskell" | "hs" | "lhs" | "swift" | "zig"
        | "zon" | "kotlin" | "kt" | "kts" => {
            "(string_literal) @string
             (comment) @comment
             (type_identifier) @type
             (integer_literal) @number
             (float_literal) @number"
        }
        _ => return None,
    })
}

#[must_use]
pub fn highlight(source: &str, lang: &str) -> Option<Vec<HighlightSpan>> {
    let language = lang_from_name(lang)?;
    let query_str = query_for_language(lang)?;

    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    let query = Query::new(&language, query_str).ok()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());

    let mut spans: Vec<HighlightSpan> = Vec::new();
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let node = capture.node;
            let range = node.byte_range();
            let name = query.capture_names()[capture.index as usize];
            let category = match name {
                "keyword" => HighlightCategory::Keyword,
                "string" => HighlightCategory::String,
                "comment" => HighlightCategory::Comment,
                "type" => HighlightCategory::Type,
                "function" => HighlightCategory::Function,
                "number" => HighlightCategory::Number,
                "builtin" => HighlightCategory::Builtin,
                _ => continue,
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
