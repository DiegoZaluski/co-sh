//! Tests for the annotated screenshot: legend rendering, validation, output shaping.
#[cfg(test)]
mod screenshot_annotate_tests {
    use crate::computer::screenshot::{screenshot, screenshot_tool_result};
    use crate::computer::types::{
        ComputerScreenshot, LegendEntryOutput, OmissionOutput, ScreenshotOutput,
    };

    fn input() -> ComputerScreenshot {
        ComputerScreenshot {
            annotate: true,
            app: Some("TestApp".into()),
            ..ComputerScreenshot::default()
        }
    }

    // Input validation

    /// Annotate without an app scope is rejected: annotation groups must be
    /// application-rooted (a rootless `:nth` resolves per-application and
    /// would disagree with the legend).
    #[tokio::test]
    async fn annotate_requires_app_or_pid() {
        let input = ComputerScreenshot {
            annotate: true,
            ..ComputerScreenshot::default()
        };
        let err = screenshot(&input).await.unwrap_err();
        // With rootless targeting now legal (review round 1: full display
        // and `region` are real forms), the ANNOTATE clause is what fires
        // for a targetless annotate — its "requires" wording is what the
        // model sees.
        assert!(err.contains("requires `app`, `pid` or `surface`"), "{err}");
    }

    /// `nth` has no meaning for annotated captures — the legend covers every
    /// match. Rejected before any capture.
    #[tokio::test]
    async fn annotate_rejects_nth() {
        let mut input = input();
        input.nth = Some(2);
        let err = screenshot(&input).await.unwrap_err();
        assert!(err.contains("does not apply to annotated"), "{err}");
    }

    /// `region` + `annotate` is rejected: the annotated capture covers the
    /// full display (boxes are drawn at capture resolution).
    #[tokio::test]
    async fn annotate_rejects_region() {
        let mut input = input();
        input.region = Some(vec![0, 0, 100, 100]);
        let err = screenshot(&input).await.unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    /// Plain element capture keeps its old constraint: `app`/`pid` without
    /// `selector` is a scope leak and must still fail — only `annotate`
    /// unlocks the `app`-without-`selector` form.
    #[tokio::test]
    async fn plain_app_without_selector_still_rejected() {
        let input = ComputerScreenshot {
            app: Some("TestApp".into()),
            ..ComputerScreenshot::default()
        };
        let err = screenshot(&input).await.unwrap_err();
        assert!(err.contains("requires `selector`"), "{err}");
    }

    /// Alternation selectors are rejected up front (appending `:nth(n)` to a
    /// group would bind to its last clause alone, so legend selectors would
    /// name a different element than the box they label) — before the app
    /// even resolves.
    #[tokio::test]
    async fn annotate_rejects_alternation_selector() {
        let mut input = input();
        input.selector = Some("button, link".into());
        let err = screenshot(&input).await.unwrap_err();
        assert!(err.contains("alternation"), "unexpected error: {err}");
    }

    /// A comma INSIDE a quoted attribute value is not an alternation
    /// separator — `button[name='Save, As']` must reach xa11y (it fails on
    /// the unknown app, NOT on an alternation error).
    #[tokio::test]
    async fn comma_inside_quoted_name_is_not_an_alternation() {
        let mut input = input();
        input.selector = Some("button[name='Save, As']".into());
        let err = screenshot(&input).await.unwrap_err();
        assert!(
            !err.contains("alternation"),
            "quoted comma must not trip the alternation check: {err}"
        );
    }

    /// `without_quoted_spans` blanks quoted spans so clause structure
    /// remains: separators outside quotes survive, values inside do not.
    #[test]
    fn quoted_spans_are_blanked_for_clause_detection() {
        // The separating comma survives; the comma inside the quoted value
        // does not (it is blanked with the rest of the span).
        let blanked =
            crate::computer::screenshot::without_quoted_spans("button[name='Save, As'], link");
        assert!(blanked.contains(", link"), "{blanked}");
        assert!(!blanked.contains(", As"), "{blanked}");
        // Unclosed quote: everything after it is treated as quoted.
        let blanked = crate::computer::screenshot::without_quoted_spans("button[name='a");
        assert!(!blanked.contains(','), "{blanked}");
    }

    // ── Tool-result text rendering ──────────────────────────────────────

    fn output(legend: Vec<LegendEntryOutput>) -> ScreenshotOutput {
        ScreenshotOutput {
            width: 800,
            height: 600,
            desktop_origin: (0, 0),
            desktop_scale: (1.0, 1.0),
            bytes: 42,
            legend,
            omitted: Vec::new(),
            truncated: 0,
            images: Vec::new(),
        }
    }

    #[test]
    fn plain_result_has_no_legend_text() {
        let msg = screenshot_tool_result("t1", &output(Vec::new()));
        let text = msg.content.as_deref().unwrap_or_default();
        assert!(!text.contains("Annotated elements"));
        assert!(text.contains("screenshot 800x600"));
    }

    #[test]
    fn annotated_result_renders_legend_lines() {
        let msg = screenshot_tool_result(
            "t1",
            &output(vec![LegendEntryOutput {
                tag: "B7".into(),
                selector: "button[name='Export']:nth(7)".into(),
                role: "button".into(),
                name: Some("Export".into()),
                color: [255, 0, 0],
            }]),
        );
        let text = msg.content.as_deref().unwrap_or_default();
        assert!(text.contains("Annotated elements"), "{text}");
        assert!(
            text.contains("B7  button[name='Export']:nth(7)  button 'Export'"),
            "{text}"
        );
    }

    #[test]
    fn annotated_result_renders_omission_and_truncation_lines() {
        let mut out = output(vec![LegendEntryOutput {
            tag: "A1".into(),
            selector: "button:nth(1)".into(),
            role: "button".into(),
            name: None,
            color: [0, 255, 0],
        }]);
        out.omitted.push(OmissionOutput {
            selector: "list_item:nth(3)".into(),
            role: "list_item".into(),
            name: Some("Hidden".into()),
            reason: "outside_capture".into(),
        });
        out.truncated = 12;
        let text = screenshot_tool_result("t1", &out)
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();
        assert!(text.contains("A1  button:nth(1)  button"), "{text}");
        assert!(
            text.contains("not drawn: list_item:nth(3)  list_item  (outside_capture)"),
            "{text}"
        );
        assert!(text.contains("legend truncated: 12"), "{text}");
    }

    /// A capture whose every match was omitted has an EMPTY legend but must
    /// still say why — omission reporting is independent of the legend block
    /// (picture and legend cannot silently disagree).
    #[test]
    fn empty_legend_still_reports_omissions_and_truncation() {
        let mut out = output(Vec::new());
        out.omitted.push(OmissionOutput {
            selector: "group:nth(1)".into(),
            role: "group".into(),
            name: None,
            reason: "zero_area".into(),
        });
        out.truncated = 4;
        let text = screenshot_tool_result("t1", &out)
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();
        assert!(!text.contains("Annotated elements"), "{text}");
        assert!(
            text.contains("not drawn: group:nth(1)  group  (zero_area)"),
            "{text}"
        );
        assert!(text.contains("legend truncated: 4"), "{text}");
    }
}
