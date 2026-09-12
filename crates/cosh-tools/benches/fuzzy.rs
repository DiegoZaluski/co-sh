use cosh_tools::fs::fuzzy::{FindMatchOptions, find_match};
use criterion::{Criterion, criterion_group, criterion_main};

fn source_lines(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        match i % 4 {
            0 => out.push_str(&format!(
                "fn handle_{i}(request: &Request) -> Response {{\n"
            )),
            1 => out.push_str("    let payload = request.decode_payload();\n"),
            2 => out.push_str(&format!(
                "    metrics.record(\"line_{i}\", payload.len());\n"
            )),
            _ => out.push_str("}\n"),
        }
    }
    out
}

fn bench_miss(c: &mut Criterion, name: &str, lines: usize) {
    let content = source_lines(lines);
    // A plausible near-miss: right shape, wrong identifier — forces the full
    // window scan with per-window Levenshtein (the diagnostic worst case).
    let target =
        "    let payload = request.decode_data();\n    metrics.record(\"line_x\", payload.len());";
    let mut group = c.benchmark_group(name);
    group.throughput(criterion::Throughput::Bytes(content.len() as u64));
    group.bench_function("find_match_no_match", |b| {
        b.iter(|| {
            find_match(
                &content,
                target,
                &FindMatchOptions {
                    allow_fuzzy: true,
                    threshold: None,
                    excluded_ranges: &[],
                },
            )
        })
    });
    group.finish();
}

fn bench_exact(c: &mut Criterion, content: &str) {
    let mut group = c.benchmark_group("exact");
    group.bench_function("find_match_exact_hit", |b| {
        b.iter(|| {
            find_match(
                content,
                "    let payload = request.decode_payload();",
                &options(false),
            )
        })
    });
    group.finish();
}

fn options(allow_fuzzy: bool) -> FindMatchOptions<'static> {
    FindMatchOptions {
        allow_fuzzy,
        threshold: None,
        excluded_ranges: &[],
    }
}

fn bench_single_line_snapshot(c: &mut Criterion) {
    // The pathological shape for the work budget: one huge line (minified
    // JS/JSON) × a long single-line old_string. The budget must degrade the
    // fuzzy pass so the rejection stays fast; the bench pins that.
    let content = "x".repeat(512 * 1024);
    let target = format!("needle-{}-haystack", "y".repeat(100_000 / 2));
    let mut group = c.benchmark_group("single_line_512k_snapshot");
    group.bench_function("find_match_budget_degraded", |b| {
        b.iter(|| {
            find_match(
                std::hint::black_box(&content),
                std::hint::black_box(&target),
                &FindMatchOptions {
                    allow_fuzzy: true,
                    threshold: None,
                    excluded_ranges: &[],
                },
            )
        })
    });
    group.finish();
}

fn fuzzy_benches(c: &mut Criterion) {
    bench_miss(c, "miss_small_200_lines", 200);
    bench_miss(c, "miss_medium_2000_lines", 2000);
    bench_miss(c, "miss_large_5000_lines", 5000);
    bench_single_line_snapshot(c);
    bench_exact(c, &source_lines(2000));
}

criterion_group!(benches, fuzzy_benches);
criterion_main!(benches);
