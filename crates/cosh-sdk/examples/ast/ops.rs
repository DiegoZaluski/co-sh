//! Demonstrate the `ast` module: language resolution, pattern compilation
//! with metavariables, match collection, rewriting, raw edit application,
//! and the error paths.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example ast-ops
//! ```

use cosh_sdk::ast::{
    AstMatchStrictness, apply_edits, collect_matches, compile_pattern,
    compile_rewrite_rules, compile_search_patterns, rewrite_source,
    resolve_language, resolve_supported_lang,
};
use std::path::Path;

fn main() {
    // ── 1. Language resolution ──────────────────────────────────────────────
    println!("== 1. language resolution ==");
    let lang = resolve_language(None, Path::new("demo.rs")).unwrap();
    println!("  demo.rs -> {lang}");
    let by_alias = resolve_supported_lang("py").unwrap();
    println!("  alias \"py\" -> {by_alias}");
    let err = resolve_supported_lang("cobol");
    println!(
        "  unknown alias -> {}",
        err.unwrap_err().to_string().split("Supported:").next().unwrap().trim()
    );
    println!();

    // ── 2. Compile + match ──────────────────────────────────────────────────
    println!("== 2. compile_pattern + collect_matches ==");
    let src = "\
fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    println!(\"sum: {sum}\");
    sum
}

fn main() {
    let x = add(1, 2);
    let y = add(3, 4);
    let z = x + y;
    println!(\"total: {z}\");
}
";
    // $A + $B matches every binary addition expression.
    let pattern = compile_pattern(
        "$A + $B",
        None,
        &AstMatchStrictness::Smart.into(),
        lang,
    )
    .unwrap();
    let matches = collect_matches(src, lang, &[pattern]);
    println!("  `$A + $B` matched {} expressions:", matches.len());
    for m in &matches {
        println!("    line {}: {}", m.line, m.text.trim());
    }
    println!();

    // ── 3. Search patterns: Rust gets a contextual bonus ────────────────────
    println!("== 3. compile_search_patterns ==");
    let search = compile_search_patterns("add($A, $B)", lang).unwrap();
    println!("  `add($A, $B)` compiled to {} pattern(s)", search.len());
    let hits = collect_matches(src, lang, &search);
    println!("  matched {} call site(s):", hits.len());
    for m in &hits {
        println!("    line {}: {}", m.line, m.text.trim());
    }
    println!();

    // ── 4. Rewriting ────────────────────────────────────────────────────────
    println!("== 4. rewrite_source ==");
    // Rename every `add(...)` call to `sum(...)`.
    let rules = vec![("add($A, $B)".to_string(), "sum($A, $B)".to_string())];
    let compiled = compile_rewrite_rules(&rules, lang).unwrap();
    let (rewritten, count) = rewrite_source(src, lang, &compiled).unwrap();
    println!("  {count} replacement(s); rewritten source:");
    for line in rewritten.lines() {
        println!("    {line}");
    }
    println!();

    // ── 5. Raw edit application ─────────────────────────────────────────────
    println!("== 5. apply_edits ==");
    // ast-grep's `Edit` type: replace bytes [0, 1) of \"abcdef\" with \"X\".
    let edits = vec![ast_grep_core::source::Edit {
        position: 1,
        deleted_length: 1,
        inserted_text: b"X".to_vec(),
    }];
    let out = apply_edits("abcdef", &edits).unwrap();
    println!("  single edit: \"abcdef\" -> \"{out}\"");
    // Two identical edits collapse into one deterministic edit.
    let dup = vec![
        ast_grep_core::source::Edit { position: 0, deleted_length: 2, inserted_text: b"Y".to_vec() },
        ast_grep_core::source::Edit { position: 0, deleted_length: 2, inserted_text: b"Y".to_vec() },
    ];
    println!("  duplicate edits dedupe: \"abcdef\" -> \"{}\"", apply_edits("abcdef", &dup).unwrap());
    // Overlapping divergent edits are rejected.
    let overlap = vec![
        ast_grep_core::source::Edit { position: 1, deleted_length: 3, inserted_text: b"a".to_vec() },
        ast_grep_core::source::Edit { position: 2, deleted_length: 1, inserted_text: b"b".to_vec() },
    ];
    let err = apply_edits("abcdef", &overlap).unwrap_err();
    println!("  overlapping edits rejected: {err}");
}
