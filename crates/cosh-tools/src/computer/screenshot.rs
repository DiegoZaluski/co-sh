//! `computer_screenshot` — capture the screen (or a region / element) and deliver
//! the PNG inline as base64 through the connector's multimodal channel.
//!
//! Every xa11y capture is blocking (pixel copy from the compositor), so it
//! runs on tokio's blocking pool. Encoding to PNG happens on the same
//! blocking task — a full-screen RGBA buffer is large and the encode is
//! CPU-bound.
use std::fmt::Write as _;
use std::time::Duration;

use cosh_sdk::connector::{ChatMessage, ImageBlock, tool_result_message_with_images};
use xa11y::{
    App, AppExt, Rect, screenshot as xa11y_screenshot, screenshot_element,
    screenshot_region as xa11y_screenshot_region, screenshot_annotated, Annotated, Screenshot,
};

use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{
    ComputerScreenshot, LegendEntryOutput, OmissionOutput, ScreenshotOutput,
};

/// Default maximum width of the delivered image, in pixels.
///
/// Coordinates travel in the DELIVERED image's pixel space, so the output
/// always reports the final dimensions and the touch tool maps them back
/// to desktop coordinates using the reported `desktop_origin` and
/// `desktop_scale`. The cap keeps the base64 payload inside a tool-result
/// budget on HiDPI displays (a 4K+ Retina capture would otherwise exceed
/// it).
const DEFAULT_MAX_WIDTH: u32 = 1568;

/// Capture the screen and deliver the PNG inline (base64) for multimodal
/// models.
///
/// Three mutually exclusive modes: full display (no arguments), an explicit
/// display `region` (`[x, y, width, height]` in desktop pixels), or an
/// element capture (`app`/`pid` + `selector` — the pixels under the
/// matched element's current bounds; no auto-raise, so an occluded element
/// yields whatever pixels sit at those coordinates). Oversized captures
/// are downscaled to `max_width` (default 1568), preserving aspect ratio;
/// the output reports the DELIVERED dimensions plus the desktop-space
/// origin and scale so coordinate-based touch can map image points back
/// to the screen.
///
/// # Errors
///
/// Returns `Err` for invalid input (`region` with the wrong arity, element
/// capture without `selector`, `nth` of 0), when the app or selector does
/// not resolve, or when capture/encode fails (e.g. missing screen-recording
/// permission).
pub async fn screenshot(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    super::surface::validate_screenshot(input)?;
    if let Some(region) = &input.region {
        if input.annotate {
            return Err(
                "computer_screenshot: `annotate` boxes the app's matched elements and is \
                 mutually exclusive with `region`; omit `region` to capture the full display"
                    .into(),
            );
        }
        if region.len() != 4 {
            return Err(format!(
                "computer_screenshot: `region` takes exactly 4 numbers [x, y, width, height], got {}",
                region.len()
            ));
        }
        if input.app.is_some()
            || input.pid.is_some()
            || input.selector.is_some()
            || input.surface.is_some()
        {
            return Err(
                "computer_screenshot: use `region` OR element capture (`app`/`pid`/`surface` + `selector`), not both"
                    .into(),
            );
        }
    }
    // Annotated capture: each annotation group must be scoped to one root
    // (app or shell surface), so it requires a target, and the legend
    // covers every match — `nth` (which would pick one) has no meaning
    // there.
    if input.annotate {
        if input.app.is_none() && input.pid.is_none() && input.surface.is_none() {
            return Err(
                "computer_screenshot: `annotate` requires `app`, `pid` or `surface` — annotation \
                 groups must be scoped to one root"
                    .into(),
            );
        }
        if input.nth.is_some() {
            return Err(
                "computer_screenshot: `nth` does not apply to annotated captures — the \
                 legend covers every match; narrow `selector` instead"
                    .into(),
            );
        }
    }
    // Element capture REQUIRES a selector — `app`/`pid`/`surface` alone
    // must not silently fall back to a full-display capture (scope leak:
    // the model asked for one root and would receive pixels of everything
    // on screen). Annotated captures are exempt: they default `selector`
    // to `"*"` — box every element of the root — which is their whole
    // point.
    if !input.annotate
        && input.selector.is_none()
        && (input.app.is_some() || input.pid.is_some() || input.surface.is_some())
    {
        return Err(
            "computer_screenshot: element capture requires `selector` together with `app`, \
             `pid` or `surface`; omit all of them to capture the full display"
                .into(),
        );
    }
    // Comma alternations are rejected before anything runs: appending
    // `:nth(n)` to an alternation would bind to its last clause alone, so
    // the legend's selectors would name a different element than the box
    // they label (xa11y refuses them too, but only after the app resolves).
    // One selector per intent — each gets its own tag colour anyway.
    // Commas inside quotes are attribute values, not clause separators, so
    // quoted spans are blanked out before looking for a separator.
    if input
        .selector
        .as_deref()
        .map(without_quoted_spans)
        .is_some_and(|s| s.contains(','))
    {
        return Err(
            "computer_screenshot: comma alternation selectors (`\"button, link\"`) are \
             not supported — pass one selector per intent"
                .into(),
        );
    }
    if input.selector.is_some()
        && input.app.is_none()
        && input.pid.is_none()
        && input.surface.is_none()
    {
        return Err(
            "computer_screenshot: element capture requires `app`, `pid` or `surface` together with `selector`".into(),
        );
    }
    if input.nth == Some(0) {
        return Err("computer_screenshot: `nth` is 1-based; use 1 for the first match".into());
    }
    if input.nth.is_some() && input.selector.is_none() && !input.annotate {
        return Err("computer_screenshot: `nth` requires `selector`".into());
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || screenshot_blocking(&input))
        .await
        .map_err(|e| format!("computer_screenshot: blocking task failed: {e}"))?
}

fn screenshot_blocking(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    if input.annotate {
        annotated_blocking(input)
    } else {
        plain_blocking(input)
    }
}

/// Annotated capture: boxes + tags drawn on the matched elements, legend
/// mapping every tag back to a round-trippable selector.
///
/// `screenshot_annotated` captures the full display (the plan's shape: the
/// model sees the whole UI, acts only through selectors); the selector
/// defaults to `"*"` — box every element of the app. Selectors resolve
/// before the capture, so a bad selector costs no pixels.
fn annotated_blocking(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    let selector = input
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("*");
    let locator = target_locator(input, selector, None)?;
    let annotated: Annotated = screenshot_annotated(None, &[locator])
        .map_err(|e| super::errors::render("computer_screenshot", "annotated capture", &e))?;

    // Downscale oversized captures exactly like the plain path; xa11y's
    // resize preserves the desktop mapping (tags are drawn before the
    // downscale, at capture resolution, so they stay legible).
    let max_width = input.max_width.unwrap_or(DEFAULT_MAX_WIDTH).max(256);
    let shot: Screenshot = if annotated.screenshot.width > max_width {
        let target_height = ((f64::from(annotated.screenshot.height) * f64::from(max_width)
            / f64::from(annotated.screenshot.width))
            .round() as u32)
            .max(1);
        annotated
            .screenshot
            .resize(max_width, target_height)
            .map_err(|e| super::errors::render("computer_screenshot", "resize capture", &e))?
    } else {
        annotated.screenshot
    };

    let (origin, desktop_scale) = delivered_mapping(&shot);
    let png = shot
        .to_png()
        .map_err(|e| super::errors::render("computer_screenshot", "encode PNG", &e))?;

    Ok(ScreenshotOutput {
        width: shot.width,
        height: shot.height,
        desktop_origin: origin,
        desktop_scale,
        bytes: png.len(),
        legend: annotated
            .legend
            .iter()
            .map(|entry| LegendEntryOutput {
                tag: entry.tag.clone(),
                selector: entry.selector.clone(),
                role: entry.role.clone(),
                name: entry.name.clone(),
                color: entry.color,
            })
            .collect(),
        omitted: annotated
            .omitted
            .iter()
            .map(|o| OmissionOutput {
                selector: o.selector.clone(),
                role: o.role.clone(),
                name: o.name.clone(),
                reason: o.reason.as_str().to_string(),
            })
            .collect(),
        truncated: annotated.truncated,
        images: vec![ImageBlock::png(&png)],
    })
}

/// Resolve the capture root — an application or a shell surface, exactly
/// one (validated) — and build the locator for `selector` under it.
/// Shared by the annotated and plain element paths.
fn target_locator(
    input: &ComputerScreenshot,
    selector: &str,
    nth: Option<usize>,
) -> Result<xa11y::Locator, String> {
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let nth = nth.unwrap_or(1);
    if let Some(surface_kind) = input.surface {
        let surface = super::surface::resolve(surface_kind, timeout)
            .map_err(|e| format!("computer_screenshot: {e}"))?;
        Ok(surface.locator(selector).nth(nth))
    } else {
        let app = resolve_app(input)?;
        Ok(app.locator(selector).nth(nth))
    }
}

fn plain_blocking(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    let capture = match (&input.region, &input.selector) {
        // Explicit region of the display.
        (Some(region), _) => {
            let rect = Rect {
                x: region[0],
                y: region[1],
                width: u32::try_from(region[2])
                    .map_err(|_| "computer_screenshot: region width must be >= 0".to_string())?,
                height: u32::try_from(region[3])
                    .map_err(|_| "computer_screenshot: region height must be >= 0".to_string())?,
            };
            xa11y_screenshot_region(rect)
        }
        // Element capture: resolve the root (app or shell surface), match
        // the selector, shoot the element's current bounds.
        (None, Some(selector)) => {
            let locator = target_locator(input, selector, input.nth)?;
            let element = locator
                .element()
                .map_err(|e| super::errors::render("computer_screenshot", "resolve selector", &e))?;
            screenshot_element(&element)
        }
        // Full display.
        (None, None) => xa11y_screenshot(),
    }
    .map_err(|e| super::errors::render("computer_screenshot", "capture", &e))?;

    // Downscale oversized captures so the base64 payload stays inside a
    // tool-result budget. xa11y's resize preserves the desktop mapping, so
    // re-derive origin/scale from the FINAL image below.
    let max_width = input.max_width.unwrap_or(DEFAULT_MAX_WIDTH).max(256);
    let capture = if capture.width > max_width {
        let target_height = ((f64::from(capture.height) * f64::from(max_width)
            / f64::from(capture.width))
            .round() as u32)
            .max(1);
        capture
            .resize(max_width, target_height)
            .map_err(|e| super::errors::render("computer_screenshot", "resize capture", &e))?
    } else {
        capture
    };

    // Re-derive the desktop transform from the FINAL delivered image so
    // computer_pointer can map image pixels back to screen coordinates
    // exactly.
    let (origin, desktop_scale) = delivered_mapping(&capture);

    let png = capture
        .to_png()
        .map_err(|e| super::errors::render("computer_screenshot", "encode PNG", &e))?;
    let image = ImageBlock::png(&png);

    Ok(ScreenshotOutput {
        width: capture.width,
        height: capture.height,
        desktop_origin: origin,
        desktop_scale,
        bytes: png.len(),
        legend: Vec::new(),
        omitted: Vec::new(),
        truncated: 0,
        images: vec![image],
    })
}

/// Desktop-space origin and scale of a DELIVERED capture, derived from the
/// capture's own mapping when available (xa11y preserves it through resize),
/// falling back to the capture's scale factor. Pixel-center mapping: the
/// span between the centers of the first and last image pixel covers
/// (dim-1) pixels, so divide by dim-1 — dividing by dim would
/// systematically underestimate the scale.
fn delivered_mapping(capture: &Screenshot) -> ((i32, i32), (f64, f64)) {
    if !capture.mapping_available() {
        return ((0, 0), (f64::from(capture.scale), f64::from(capture.scale)));
    }
    let origin = capture
        .image_to_desktop(xa11y::Point::new(0, 0))
        .map(|p| (p.x, p.y))
        .unwrap_or((0, 0));
    let corner = capture.image_to_desktop(xa11y::Point::new(
        i32::try_from(capture.width.saturating_sub(1)).unwrap_or(i32::MAX),
        i32::try_from(capture.height.saturating_sub(1)).unwrap_or(i32::MAX),
    ));
    match corner {
        Ok(corner) => {
            let dx = (corner.x - origin.0).abs().max(1);
            let dy = (corner.y - origin.1).abs().max(1);
            (
                origin,
                (
                    f64::from(dx) / f64::from(capture.width.saturating_sub(1).max(1)),
                    f64::from(dy) / f64::from(capture.height.saturating_sub(1).max(1)),
                ),
            )
        }
        Err(_) => ((0, 0), (f64::from(capture.scale), f64::from(capture.scale))),
    }
}
/// Build the connector message for a screenshot tool result: short text
/// plus the inline PNG (the harness replays this in the conversation so a
/// multimodal model sees the image). Annotated captures append the legend —
/// one `TAG  selector  role 'name'` line per box — so the model can act on
/// a tag it read off the image without any other tool call; omissions and
/// truncation are reported so picture and legend cannot silently disagree.
#[must_use]
pub fn screenshot_tool_result(tool_call_id: &str, output: &ScreenshotOutput) -> ChatMessage {
    let mut text = format!(
        "screenshot {}x{} px ({} bytes base64 PNG), covering desktop pixels \
         from ({}, {}) at {:.3}x{:.3} desktop px per image px (dx, dy). \
         Pointer coordinates are in THIS image's pixel space; map to the \
         display as desktop = origin + image_coord * scale.",
        output.width,
        output.height,
        output.bytes,
        output.desktop_origin.0,
        output.desktop_origin.1,
        output.desktop_scale.0,
        output.desktop_scale.1,
    );
    if !output.legend.is_empty() {
        text.push_str("\n\nAnnotated elements — act on a box by passing its \
selector to computer_act:\n");
        for entry in &output.legend {
            match &entry.name {
                Some(name) => {
                    let _ = writeln!(text, "{}  {}  {} '{}'", entry.tag, entry.selector, entry.role, name);
                }
                None => {
                    let _ = writeln!(text, "{}  {}  {}", entry.tag, entry.selector, entry.role);
                }
            }
        }
    }
    // Omission and truncation reporting sits OUTSIDE the legend block: a
    // capture whose every match was omitted (or capped) has an empty legend
    // but must still say why — the picture and the legend cannot silently
    // disagree.
    for omission in &output.omitted {
        let _ = writeln!(
            text,
            "not drawn: {}  {}  ({})",
            omission.selector, omission.role, omission.reason
        );
    }
    if output.truncated > 0 {
        let _ = writeln!(
            text,
            "legend truncated: {} more matched elements are not described — \
narrow the selector",
            output.truncated
        );
    }
    tool_result_message_with_images(tool_call_id, &text, output.images.clone())
}

/// Blank out `'…'` / `"…"` quoted spans of a selector, leaving clause
/// structure only. A comma inside quotes is an attribute value
/// (`button[name='Save, As']`), not an alternation separator.
fn without_quoted_spans(selector: &str) -> String {
    let mut out = String::with_capacity(selector.len());
    let mut quote: Option<char> = None;
    for ch in selector.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => out.push(' '),
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    out.push(' ');
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out
}

/// Resolve the target application (validating `app`/`pid` exclusivity).
fn resolve_app(input: &ComputerScreenshot) -> Result<App, String> {
    use std::time::Duration;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let name = input.app.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout)
            .map_err(|e| super::errors::render("computer_screenshot", "resolve application", &e)),
        (None, Some(pid)) => App::by_pid(pid, timeout)
            .map_err(|e| super::errors::render("computer_screenshot", "resolve application", &e)),
        (None, None) => {
            Err("computer_screenshot: element capture requires `app`, `pid` or `surface`".into())
        }
        (Some(_), Some(_)) => {
            Err("computer_screenshot: provide `app` or `pid`, not both".into())
        }
    }
}

#[cfg(test)]
mod screenshot_annotate_tests {
    use super::super::types::{ComputerScreenshot, LegendEntryOutput, OmissionOutput, ScreenshotOutput};
    use super::{screenshot, screenshot_tool_result};

    fn input() -> ComputerScreenshot {
        ComputerScreenshot {
            annotate: true,
            app: Some("TestApp".into()),
            ..ComputerScreenshot::default()
        }
    }

    // ── Input validation ────────────────────────────────────────────────

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
        assert!(
            err.contains("requires `app`, `pid` or `surface`"),
            "{err}"
        );
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
        let blanked = super::without_quoted_spans("button[name='Save, As'], link");
        assert!(blanked.contains(", link"), "{blanked}");
        assert!(!blanked.contains(", As"), "{blanked}");
        // Unclosed quote: everything after it is treated as quoted.
        let blanked = super::without_quoted_spans("button[name='a");
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
        assert!(text.contains("not drawn: group:nth(1)  group  (zero_area)"), "{text}");
        assert!(text.contains("legend truncated: 4"), "{text}");
    }
}
