//! Demonstrate the `tree_sitter` module: the LRU-cached parse lifecycle,
//! block and symbol resolution, and syntax highlighting.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example tree-sitter
//! ```

use cosh_sdk::tree_sitter::highlight::highlight;
use cosh_sdk::tree_sitter::{TreeSitter, tree_sitter};

fn main() {
    let src = "\
fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    let n = double(4);
    println!(\"doubled: {n}\");
}
";

    // ── 1. Block resolution ─────────────────────────────────────────────────
    println!("== 1. resolve_block ==");
    let ts = TreeSitter::new(16);
    // Only lines where a syntactic node BEGINS resolve. `fn double` starts
    // on line 1 -> the whole function, lines 1-3.
    let block = ts.resolve_block("demo.rs", src, 1).unwrap();
    println!("  block at line 1 (fn double): lines {}-{}", block.start, block.end);
    let main_block = ts.resolve_block("demo.rs", src, 5).unwrap();
    println!("  block at line 5 (fn main): lines {}-{}", main_block.start, main_block.end);
    // Interior lines (a body statement, line 2) and blank lines (line 4)
    // have no node STARTING there -> None.
    println!("  body statement line 2: {:?}", ts.resolve_block("demo.rs", src, 2));
    println!("  blank line 4: {:?}", ts.resolve_block("demo.rs", src, 4));
    println!();

    // ── 2. Symbol resolution ────────────────────────────────────────────────
    println!("== 2. resolve_symbol ==");
    let sym = ts.resolve_symbol("demo.rs", src, "double").unwrap();
    println!("  `double` definition: lines {}-{}", sym.start, sym.end);
    let missing = ts.resolve_symbol("demo.rs", src, "nope");
    println!("  unknown symbol: {missing:?}");
    println!();

    // ── 3. The cache lifecycle ──────────────────────────────────────────────
    println!("== 3. cache lifecycle (with_tree) ==");
    // `parse_count()` reports actual `parser.parse` invocations, so cache
    // hits (which skip parsing entirely) are visible.
    let read = |ts: &TreeSitter, text: &str| {
        ts.with_tree("demo.rs", text, |_tree, parsed_text| parsed_text.len())
            .unwrap()
    };
    // Demos 1-2 already parsed this file, but let's isolate a fresh cache.
    let ts2 = TreeSitter::new(16);
    let first = read(&ts2, src);
    println!(
        "  first call: parsed {first} bytes, parse count now {}",
        ts2.parse_count()
    );
    // Same text: served from cache, no re-parse.
    let second = read(&ts2, src);
    println!(
        "  same text again: parsed {second} bytes, parse count still {}",
        ts2.parse_count()
    );
    // Changed text: the old tree cannot be reused for a byte-identical
    // re-parse, so this is a full re-parse of the new content.
    let changed = src.replace("let n = double(4);", "let n = double(4) + 1;");
    let third = read(&ts2, &changed);
    println!(
        "  changed text: parsed {third} bytes, parse count now {}",
        ts2.parse_count()
    );
    // Writers invalidate the cache; the next call is a fresh full parse.
    ts2.invalidate("demo.rs");
    let fourth = read(&ts2, &changed);
    println!(
        "  after invalidate: parsed {fourth} bytes, parse count now {}",
        ts2.parse_count()
    );
    println!();

    // ── 4. Syntax highlighting ──────────────────────────────────────────────
    println!("== 4. highlight ==");
    let code = "fn main() {\n    let x = 42; // answer\n    println!(\"hi\");\n}\n";
    let spans = highlight(code, "rust").unwrap();
    let mut last = 0;
    for span in &spans {
        // Print the highlighted tokens with a (crude) color by category.
        let text = &code[span.start..span.end];
        let marker = match span.category {
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Keyword => "K",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::String => "S",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Comment => "C",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Type => "T",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Function => "F",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Number => "N",
            cosh_sdk::tree_sitter::highlight::HighlightCategory::Builtin => "B",
        };
        // show the gap between spans as plain text
        print!("{}", &code[last..span.start]);
        print!("[{marker}:{text}]");
        last = span.end;
    }
    println!("{}", &code[last..]);
    println!("  {} spans total", spans.len());
    let none = highlight(code, "not-a-language");
    println!("  unknown language: {none:?}");

    // ── 5. The process-global instance ──────────────────────────────────────
    println!();
    println!("== 5. tree_sitter() global ==");
    let global = tree_sitter();
    let b = global.resolve_block("demo.rs", src, 1).unwrap();
    println!("  global cache resolves line 1 to lines {}-{}", b.start, b.end);
    println!("  (the global instance is shared across the crate)");
}
