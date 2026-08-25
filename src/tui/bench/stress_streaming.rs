//! Streaming-path probe: measures `StreamingTextCache::update` cost as the
//! streamed document grows, per tail kind (paragraph / list / fence).
//!
//! The incremental design targets O(delta) per frame. If any path rescans or
//! re-renders the whole open tail per frame, cost grows linearly with the
//! tail (O(n) per frame ⇒ O(n²) over the stream) and becomes visible as
//! progressive jank during long agent answers.
//!
//! Run: cargo test --release stress_streaming -- --ignored --nocapture

use crate::routes::session::streaming::StreamingTextCache;

fn drive(name: &str, doc_builder: impl Fn(usize) -> String, units: usize) {
    let th = crate::theme::ThemeRegistry::new().default_theme().clone();
    let width: u16 = 120;
    // Build the FULL document first, then feed it in chunks like Token events.
    let full = doc_builder(units);
    let bytes = full.len();
    let mut cache = StreamingTextCache::new("m".into(), 0, width, 0, &th);

    let mut pos = 0usize;
    let mut worst = 0.0f64;
    let mut sum = 0.0f64;
    let mut n = 0usize;
    let mut last_print = 0usize;
    while pos < bytes {
        // ~512B chunks, snapped to char boundaries (like coalesced tokens).
        let mut e = (pos + 512).min(bytes);
        while e < bytes && !full.is_char_boundary(e) {
            e += 1;
        }
        let t = std::time::Instant::now();
        let (_row, _h) = cache.update(&full[..e], width, 0, false);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        sum += ms;
        n += 1;
        pos = e;
        if pos * 4 >= bytes * (last_print + 1) {
            println!(
                "[STREAM] {name:<10} progress {:>3}%: avg={:.3}ms worst={:.3}ms h={} rows",
                (last_print + 1) * 25,
                sum / n as f64,
                worst,
                cache.height,
            );
            last_print += 1;
        }
    }
    println!(
        "[STREAM] {name:<10} FINAL size={bytes:>7}B frames={n:<5} avg={:.3}ms worst={:.3}ms height={}",
        sum / n as f64,
        worst,
        cache.height
    );
}

#[test]
#[ignore]
fn stress_streaming_paragraph() {
    drive(
        "paragraph",
        |_| "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod\n"
            .repeat(2000),
        2000,
    );
}

#[test]
#[ignore]
fn stress_streaming_long_list() {
    drive(
        "list",
        |n| {
            (0..n)
                .map(|i| format!("- item number {i} with some trailing explanation text here\n"))
                .collect()
        },
        1500,
    );
}

#[test]
#[ignore]
fn stress_streaming_code_fence() {
    drive(
        "fence",
        |n| {
            let body: String = (0..n)
                .map(|i| format!("pub fn generated_{i}() -> u32 {{ {i} }}\n"))
                .collect();
            format!("```rust\n{body}```\n")
        },
        1500,
    );
}

/// Same fence body but with an UNKNOWN language tag: no tree-sitter
/// highlighting runs, isolating the highlight share of the cost.
#[test]
#[ignore]
fn stress_streaming_code_fence_nohighlight() {
    drive(
        "fence-nohl",
        |n| {
            let body: String = (0..n)
                .map(|i| format!("pub fn generated_{i}() -> u32 {{ {i} }}\n"))
                .collect();
            format!("```notalanguage\n{body}```\n")
        },
        1500,
    );
}

#[test]
#[ignore]
fn stress_streaming_mixed_doc() {
    drive(
        "mixed",
        |n| {
            (0..n)
                .map(|i| match i % 4 {
                    0 => format!("## Section {i}\n\nParagraph with `inline code` and **bold**.\n\n"),
                    1 => format!("- bullet {i}\n- bullet {i}b\n"),
                    2 => format!("```rust\ndef f{i}(x):\n    return x * {i}\n```\n\n"),
                    _ => format!("> quoted wisdom {i}\n\n"),
                })
                .collect()
        },
        1000,
    );
}
