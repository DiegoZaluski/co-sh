//! Diagnostics engine tests: replace-whole semantics, aggregation across
//! servers, settle-wait timing and model formatting.

use std::{
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::Arc,
    time::Duration,
};

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, PublishDiagnosticsParams, Uri};

use crate::lsp::diagnostics::{
    DiagnosticsEngine, SETTLE_DEBOUNCE, SeverityFilter, format_for_model, uri_to_path,
};

fn uri(name: &str) -> Uri {
    crate::lsp::client::uri_from_path(Path::new(&format!("/tmp/{name}")))
        .expect("test path converts")
}

fn publish(
    server: &str,
    file: &str,
    messages: &[(&str, i32)],
) -> (String, PublishDiagnosticsParams) {
    let diagnostics: Vec<Diagnostic> = messages
        .iter()
        .map(|(message, severity)| Diagnostic {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(match *severity {
                1 => DiagnosticSeverity::ERROR,
                2 => DiagnosticSeverity::WARNING,
                3 => DiagnosticSeverity::INFORMATION,
                _ => DiagnosticSeverity::HINT,
            }),
            code: None,
            code_description: None,
            source: Some(server.to_owned()),
            message: (*message).to_owned(),
            related_information: None,
            tags: None,
            data: None,
        })
        .collect();

    (
        server.to_owned(),
        PublishDiagnosticsParams::new(uri(file), diagnostics, None),
    )
}

fn ingest(engine: &DiagnosticsEngine, (server, params): &(String, PublishDiagnosticsParams)) {
    engine.ingest(server, params);
}

fn diagnostic_count(params: &PublishDiagnosticsParams) -> usize {
    params.diagnostics.len()
}

#[tokio::test]
async fn replace_whole_document_per_server() {
    let engine = DiagnosticsEngine::new();
    let file = "a.rs";

    let first = publish("rust-analyzer", file, &[("e1", 1)]);
    ingest(&engine, &first);
    assert_eq!(
        engine
            .snapshot_for(&PathBuf::from(format!("/tmp/{file}")))
            .len(),
        1
    );

    // Same publisher, different set: replaces, never appends.
    let second = publish("rust-analyzer", file, &[("e1", 1), ("w2", 2)]);
    ingest(&engine, &second);
    let snapshot = engine.snapshot_for(&PathBuf::from(format!("/tmp/{file}")));
    assert_eq!(snapshot.len(), 2);

    // A second publisher coexists.
    let other = publish("clippy", file, &[("lint", 4)]);
    ingest(&engine, &other);
    assert_eq!(
        engine
            .snapshot_for(&PathBuf::from(format!("/tmp/{file}")))
            .len(),
        3
    );
}

/// Empty list from a server clears only that server; clearing the last one
/// removes the path entirely (spec semantics).
#[tokio::test]
async fn empty_publish_clears_publisher_then_path() {
    let engine = DiagnosticsEngine::new();
    let file = "b.rs";
    let path = PathBuf::from(format!("/tmp/{file}"));

    ingest(&engine, &publish("rust-analyzer", file, &[("e", 1)]));
    ingest(&engine, &publish("clippy", file, &[("l", 2)]));
    assert_eq!(engine.snapshot_for(&path).len(), 2);

    let (server, mut cleared) = publish("rust-analyzer", file, &[]);
    cleared.diagnostics.clear();
    engine.ingest(&server, &cleared);
    assert_eq!(engine.snapshot_for(&path).len(), 1, "only clippy remains");

    let (server, mut cleared) = publish("clippy", file, &[]);
    cleared.diagnostics.clear();
    engine.ingest(&server, &cleared);
    assert!(engine.snapshot_for(&path).is_empty(), "path fully gone");
    assert!(engine.snapshot_all(None).is_empty());
}

/// Identical re-publishes (tsserver/clangd churn) must not bump the version —
/// settle-wait depends on a stable version meaning "nothing changed".
#[tokio::test]
async fn identical_republish_does_not_bump_version() {
    let engine = DiagnosticsEngine::new();
    let params = publish("rust-analyzer", "stable.rs", &[("e", 1)]).1;

    engine.ingest("rust-analyzer", &params);
    let version = engine.version();

    engine.ingest("rust-analyzer", &params);
    engine.ingest("rust-analyzer", &params);
    assert_eq!(engine.version(), version, "identical set = no mutation");

    // A real change still bumps.
    ingest(
        &engine,
        &publish("rust-analyzer", "stable.rs", &[("e", 1), ("w", 2)]),
    );
    assert!(engine.version() > version);
}

#[tokio::test]
async fn remove_path_drops_entry_and_bumps() {
    let engine = DiagnosticsEngine::new();
    let path = PathBuf::from("/tmp/gone.rs");
    ingest(&engine, &publish("rust-analyzer", "gone.rs", &[("e", 1)]));
    assert!(!engine.snapshot_for(&path).is_empty());

    assert!(engine.remove_path(&path));
    assert!(engine.snapshot_for(&path).is_empty());
    assert!(!engine.remove_path(&path), "second removal is a no-op");
}

#[tokio::test]
async fn non_file_uris_are_ignored() {
    let engine = DiagnosticsEngine::new();
    let params = PublishDiagnosticsParams {
        uri: Uri::from_str("untitled:Untitled-1").unwrap(),
        diagnostics: vec![],
        version: None,
    };
    engine.ingest("x", &params);
    assert!(engine.snapshot_all(None).is_empty());
}

#[test]
fn uri_to_path_decodes_percent_escapes() {
    let encoded = Uri::from_str("file:///tmp/my%20project/caf%C3%A9.rs").unwrap();
    assert_eq!(
        uri_to_path(&encoded),
        Some(PathBuf::from("/tmp/my project/café.rs"))
    );
    assert_eq!(uri_to_path(&Uri::from_str("https://x/y").unwrap()), None);
}

// ── Settle-wait ──────────────────────────────────────────────────────────

#[tokio::test]
async fn settle_returns_immediately_when_quiet() {
    let engine = DiagnosticsEngine::new();
    let started = std::time::Instant::now();
    // Nothing ever changes: the FIRST debounce window decides. We assert the
    // wait finishes within debounce + generous slack instead of the full cap.
    let quiet = tokio::time::timeout(
        SETTLE_DEBOUNCE + Duration::from_millis(250),
        engine.wait_for_settle(Duration::from_secs(30)),
    )
    .await
    .expect("quiet store settles fast");
    assert!(quiet);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn settle_hits_cap_when_mutations_keep_coming() {
    let engine = Arc::new(DiagnosticsEngine::new());

    let feeder = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            for round in 0..50 {
                let (_, params) = publish(
                    "feeder",
                    "churn.rs",
                    &[(Box::leak(format!("m{round}").into_boxed_str()), 1)],
                );
                engine.ingest("feeder", &params);
                tokio::time::sleep(SETTLE_DEBOUNCE / 4).await;
            }
        })
    };

    let quiet = engine.wait_for_settle(Duration::from_secs(1)).await;
    feeder.abort();
    assert!(!quiet, "continuous churn must hit the cap");
}

#[tokio::test]
async fn settle_waits_out_burst_before_returning() {
    let engine = Arc::new(DiagnosticsEngine::new());
    let file = "burst.rs";

    // Feed a burst right after starting the wait: the wait must NOT return
    // during the burst (early versions would pass on the pre-burst quiet).
    let waiter = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move { engine.wait_for_settle(Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    for round in 0..3 {
        ingest(
            &engine,
            &publish(
                "burst",
                file,
                &[(Box::leak(format!("b{round}").into_boxed_str()), 1)],
            ),
        );
        tokio::time::sleep(SETTLE_DEBOUNCE / 3).await;
    }

    let quiet = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("settles after the burst")
        .expect("ends quiet");
    assert!(quiet);
    assert!(
        !engine
            .snapshot_for(&PathBuf::from("/tmp/burst.rs"))
            .is_empty()
    );
}

// ── Formatting ───────────────────────────────────────────────────────────

fn diag_unspecified_severity(message: &str) -> Diagnostic {
    let mut diag = diag_at(0, 1, message);
    diag.severity = None;
    diag
}

fn diag_at(line: u32, severity: i32, message: &str) -> Diagnostic {
    let severity = match severity {
        1 => DiagnosticSeverity::ERROR,
        2 => DiagnosticSeverity::WARNING,
        3 => DiagnosticSeverity::INFORMATION,
        _ => DiagnosticSeverity::HINT,
    };
    Diagnostic {
        range: lsp_types::Range {
            start: lsp_types::Position { line, character: 2 },
            end: lsp_types::Position { line, character: 8 },
        },
        severity: Some(severity),
        code: Some(NumberOrString::Number(42)),
        code_description: None,
        source: None,
        message: message.to_owned(),
        related_information: None,
        tags: None,
        data: None,
    }
}

#[test]
fn format_orders_by_severity_then_position() {
    let diags = vec![
        diag_at(5, 3, "info later"),
        diag_at(2, 1, "error at 2"),
        diag_at(7, 1, "error at 7"),
        diag_at(1, 2, "warning"),
    ];

    let out = format_for_model(&diags, SeverityFilter::All, 10);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4);
    // Rendering is 1-based for both line and character.
    assert!(lines[0].contains("error 3:3"), "first line: {out}");
    assert!(lines[1].contains("error 8:3"));
    assert!(lines[2].contains("warning 2:3"));
    assert!(lines[3].contains("info 6:3"));
    assert!(out.contains("(42)"), "code rendered");
}

#[test]
fn format_filters_by_severity() {
    let diags = vec![
        diag_at(0, 1, "err"),
        diag_at(1, 2, "warn"),
        diag_at(2, 4, "hint"),
    ];

    let errors_only = format_for_model(&diags, SeverityFilter::ErrorsOnly, 10);
    assert_eq!(errors_only.lines().count(), 1);
    assert!(errors_only.contains("err"));

    let warn_up = format_for_model(&diags, SeverityFilter::WarningAndUp, 10);
    assert_eq!(warn_up.lines().count(), 2);
}

#[test]
fn format_dedupes_cross_server_duplicates_and_caps() {
    let dup_a = diag_at(3, 1, "same problem");
    let dup_b = diag_at(3, 1, "same problem");
    let unique = diag_at(9, 1, "other");
    let diags = vec![dup_a, dup_b, unique];

    let out = format_for_model(&diags, SeverityFilter::All, 1);
    assert_eq!(out.lines().count(), 2, "unique + truncation marker");
    assert!(out.contains("… and 1 more diagnostics"));
    assert!(
        out.lines().next().unwrap().contains("same problem"),
        "severity order picks it first"
    );

    // Unfiltered count is zero when there is nothing to show.
    assert_eq!(format_for_model(&[], SeverityFilter::All, 5), "");
}

#[test]
fn format_counts_reflect_filters_not_raw_input() {
    let diags = vec![
        diag_at(0, 4, "hint"),
        diag_at(1, 4, "hint"),
        diag_at(2, 4, "hint"),
    ];
    let out = format_for_model(&diags, SeverityFilter::ErrorsOnly, 10);
    assert_eq!(out, "", "hints filtered out → empty, no truncation marker");
}
